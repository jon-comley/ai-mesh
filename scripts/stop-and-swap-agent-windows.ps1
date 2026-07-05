param(
    [Parameter(Mandatory = $true)]
    [string]$WinPath
)

# Stops the ai-mesh-agent NSSM service and swaps in the freshly-uploaded
# binary. Split out of the deploy-node justfile recipe because the naive
# inline version (taskkill agent.exe, then sc.exe stop, then a flat 1s
# sleep, then copy) raced NSSM's own restart logic: taskkill kills the
# process directly rather than going through the Service Control Manager,
# which NSSM treats as an unexpected exit and restarts per its configured
# AppRestartDelay, re-locking agent.exe before the copy could complete
# ("The process cannot access the file because it is being used by another
# process"). Fix: stop via SCM first (NSSM recognises this as an
# intentional stop, so it won't auto-restart), poll for the process to
# actually disappear instead of guessing a fixed delay, only then fall back
# to taskkill for anything left over, and retry the copy itself a few times
# as a last safety net.
#
# ASCII only, deliberately: a non-ASCII character (e.g. an em dash) inside
# a string literal here can get misread if this file is parsed on the
# Windows side with a different encoding than it was written in (no BOM),
# corrupting the string's quote boundary and cascading into a bogus
# "Missing closing brace" parse error for the rest of the file.
# Deliberately NOT setting $ErrorActionPreference = "Stop" globally: sc.exe
# and taskkill are native commands that routinely "fail" harmlessly here
# (service already stopped, process not running to kill) and that's fine,
# best-effort by design. On PowerShell 7.3+, $PSNativeCommandUseErrorAction
# Preference (default true) makes a native command's stderr output respect
# $ErrorActionPreference too, not just its exit code -- with Stop set
# globally, taskkill's routine "process not found" message becomes a
# terminating NativeCommandError and kills the whole script. Explicitly
# disabling it here restores classic best-effort native-command semantics
# regardless of PowerShell version (harmless no-op variable on 5.1, where
# this behavior never existed). Copy-Item below still opts into throwing
# via its own -ErrorAction Stop, which is unaffected by any of this.
$PSNativeCommandUseErrorActionPreference = $false
$agentService = "ai-mesh-agent"

Write-Host ">>> Stopping $agentService via SCM..."
sc.exe stop $agentService 2>&1 | Out-Null

$deadline = (Get-Date).AddSeconds(15)
while ((Get-Process -Name agent -ErrorAction SilentlyContinue) -and ((Get-Date) -lt $deadline)) {
    Start-Sleep -Milliseconds 300
}

# Fallback for anything NSSM didn't clean up (llama-server is a child
# process the agent spawns, not part of the service itself).
taskkill /F /IM llama-server.exe /T 2>&1 | Out-Null
taskkill /F /IM agent.exe /T 2>&1 | Out-Null
Start-Sleep -Milliseconds 500

$source = Join-Path $WinPath "agent_next.exe"
$dest = Join-Path $WinPath "agent.exe"
$maxAttempts = 6
$attempt = 0
$done = $false
$lastError = $null
while (-not $done -and $attempt -lt $maxAttempts) {
    $attempt++
    try {
        Copy-Item -Path $source -Destination $dest -Force -ErrorAction Stop
        $done = $true
    } catch {
        $lastError = $_
        # Print the real exception, not a guess -- "file in use" was an
        # assumption carried over from one early manual test and never
        # actually confirmed for every subsequent failure since.
        Write-Host ">>> Copy attempt $attempt/$maxAttempts failed: $($_.Exception.GetType().FullName): $($_.Exception.Message)"
        Start-Sleep -Seconds 1
    }
}
if (-not $done) {
    Write-Host ">>> Diagnostics: who has $dest open right now:"
    Write-Host (Get-Process | Where-Object { $_.Path -eq $dest } | Format-Table -AutoSize | Out-String)
    Write-Host ">>> Source file state:"
    Write-Host (Get-Item $source -ErrorAction SilentlyContinue | Format-List * | Out-String)
    Write-Host ">>> Destination file state:"
    Write-Host (Get-Item $dest -ErrorAction SilentlyContinue | Format-List * | Out-String)
    Write-Error "Failed to swap agent.exe after $maxAttempts attempts. Last error: $($lastError.Exception.Message)"
    exit 1
}
Write-Host ">>> Binary swapped successfully."
