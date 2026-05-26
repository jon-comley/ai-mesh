param(
    [Parameter(Mandatory = $true)]
    [string]$CoordinatorIp,

    [string]$Role = "compute"
)

$ErrorActionPreference = "Stop"

$aiMeshRoot      = "C:\Users\$env:USERNAME\ai-mesh"
$agentPath       = Join-Path $aiMeshRoot "agent.exe"
$logDir          = Join-Path $aiMeshRoot "logs"
$agentService    = "ai-mesh-agent"

# Resolve latest release tag dynamically with fallback to verified base version.
try {
    Write-Host ">>> Fetching latest llama.cpp release tag from GitHub..."
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/ggml-org/llama.cpp/releases/latest" -UseBasicParsing -TimeoutSec 5
    $llamaVersion = $release.tag_name
} catch {
    Write-Host ">>> Warning: Failed to query GitHub API. Falling back to static tag."
    $llamaVersion = "b9251"
}
$llamaZipUrl     = "https://github.com/ggml-org/llama.cpp/releases/download/$llamaVersion/llama-$llamaVersion-bin-win-vulkan-x64.zip"
$llamaInstallDir = "$env:LOCALAPPDATA\Programs\llama.cpp"
$llamaHost       = "http://127.0.0.1:8080"


# Select the best model based on available GPU VRAM or system RAM.
function Select-DefaultModel {
    $memMb = 0
    $gpu = $false

    # GPU VRAM — handles 32-bit overflow (4294967295 = iGPU with >= 4 GB allocated)
    $gpuObj = Get-WmiObject Win32_VideoController |
        Where-Object { $_.AdapterRAM -gt 0 } |
        Sort-Object AdapterRAM -Descending |
        Select-Object -First 1
    if ($gpuObj) {
        if ($gpuObj.AdapterRAM -eq 4294967295) {
            $memMb = 8192  # 32-bit overflow sentinel — GPU reports max uint32 when VRAM > 4 GB
        } else {
            $memMb = [int]($gpuObj.AdapterRAM / 1MB)
        }
        $gpu = $true
    }

    # Fall back to system RAM
    if ($memMb -eq 0) {
        $memMb = [int]((Get-WmiObject Win32_ComputerSystem).TotalPhysicalMemory / 1MB)
    }

    if ($gpu) {
        if     ($memMb -ge 22000) { "qwen2.5:32b" }
        elseif ($memMb -ge 9000)  { "qwen2.5:14b" }
        elseif ($memMb -ge 4000)  { "qwen2.5:7b" }
        elseif ($memMb -ge 1000)  { "qwen2.5:1.5b" }
        else                      { "qwen2.5:0.5b" }
    } else {
        if     ($memMb -ge 44000) { "qwen2.5:32b" }
        elseif ($memMb -ge 18000) { "qwen2.5:14b" }
        elseif ($memMb -ge 10000) { "qwen2.5:7b" }
        elseif ($memMb -ge 3000)  { "qwen2.5:1.5b" }
        else                      { "qwen2.5:0.5b" }
    }
}

function Ensure-Directory {
    param([string]$Path)
    if (-not (Test-Path $Path)) {
        New-Item -ItemType Directory -Path $Path | Out-Null
    }
}

function Ensure-WingetPackage {
    param(
        [string]$Id,
        [string]$Name
    )
    $installed = winget list --id $Id --source winget 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $installed) {
        winget install --id $Id -e --source winget --accept-package-agreements --accept-source-agreements
    }
}

function Ensure-LlamaCpp {
    $llamaExe = Join-Path $llamaInstallDir "llama-server.exe"
    if (Test-Path $llamaExe) {
        Write-Host ">>> llama-server already present at $llamaExe - skipping download."
        return
    }

    Write-Host ">>> Installing llama.cpp $llamaVersion (Vulkan) from ZIP..."
    $zipPath = Join-Path $env:TEMP "llama-win-vulkan.zip"
    Write-Host ">>> Downloading $llamaZipUrl (this may take a few minutes)..."
    Invoke-WebRequest -Uri $llamaZipUrl -OutFile $zipPath -UseBasicParsing -TimeoutSec 600

    Ensure-Directory -Path $llamaInstallDir
    Write-Host ">>> Extracting to $llamaInstallDir..."
    Expand-Archive -Path $zipPath -DestinationPath $llamaInstallDir -Force
    Remove-Item $zipPath -Force

    Write-Host ">>> llama.cpp $llamaVersion installed at $llamaInstallDir."
}

function Ensure-Nssm {
    $nssm = Get-Command "nssm.exe" -ErrorAction SilentlyContinue
    if (-not $nssm) {
        Ensure-WingetPackage -Id "NSSM.NSSM" -Name "NSSM"
    }
}

function Get-Nssm {
    $nssm = Get-Command nssm.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
    if (-not $nssm) {
        throw "NSSM is installed but nssm.exe was not found on PATH. Try opening a new PowerShell session or reinstalling NSSM."
    }
    return $nssm
}


function Ensure-AgentBinary {
    if (-not (Test-Path $agentPath)) {
        throw "agent.exe not found at $agentPath"
    }
}

function Disable-Sleep {
    # Prevents Windows from sleeping while idle. The SER8 does not recover
    # cleanly from sleep (BIOS shows an unrecoverable screen requiring a CMOS
    # reset). Must be re-applied after every CMOS reset.
    powercfg /h off
    powercfg /change standby-timeout-ac 0
    powercfg /change monitor-timeout-ac 0
    Write-Host ">>> Sleep and hibernate disabled."
}

function Harden-Stability {
    # Two registry fixes for the SER8 hang pattern:
    #   1. Fast Startup — powercfg /h off disables hibernate but not Fast
    #      Startup (hybrid shutdown); can produce a half-suspended state.
    #   2. NIC power management — adapter should stay live across idle periods.
    #
    # NOTE: TdrDelay intentionally NOT set here. Setting it to 60s was tried
    # and correlated with severe GPU overheating (DPC_WATCHDOG_VIOLATION crashes
    # followed by thermal shutdown). The Windows default (2s) is safer — it
    # resets a hung GPU driver quickly rather than letting it cook.
    reg add "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Power" /v HiberbootEnabled /t REG_DWORD /d 0 /f | Out-Null
    Get-ChildItem "HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4D36E972-E325-11CE-BFC1-08002bE10318}" -ErrorAction SilentlyContinue | ForEach-Object {
        $props = Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue
        if ($props -and $props.DriverDesc) {
            Set-ItemProperty $_.PSPath -Name PnPCapabilities -Value 24 -Type DWord -ErrorAction SilentlyContinue
        }
    }
    Write-Host ">>> Stability hardening applied (Fast Startup off, NIC power save off)."
}

function Ensure-StartupHardeningTask {
    # Writes a small hardening script to C:\ai-mesh\harden-boot.ps1 and registers
    # a Scheduled Task that runs it as SYSTEM at every boot.  This re-applies
    # Harden-Stability after Windows driver updates or BSOD-triggered driver resets
    # which can silently restore NIC power-save settings and GPU TDR values.
    $hardenScript = Join-Path $aiMeshRoot "harden-boot.ps1"
    $taskName     = "ai-mesh-harden-boot"

    $scriptContent = @'
# Re-applied at every boot by the ai-mesh-harden-boot Scheduled Task.
# Mirrors the Harden-Stability block in install-node-windows.ps1.
#
# TdrDelay is intentionally NOT set — 60s was correlated with GPU overheating.
# Windows default (2s) resets a hung driver quickly; leave it alone.

# Disable Fast Startup (hybrid shutdown can leave NIC and GPU in a half-suspended state).
reg add "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Power" /v HiberbootEnabled /t REG_DWORD /d 0 /f | Out-Null

# NIC power management: PnPCapabilities=24 tells Windows not to power down the
# adapter or use it as a wake source.  Applied to every adapter in the NIC class
# so it survives driver updates that reset the per-adapter registry key.
Get-ChildItem "HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4D36E972-E325-11CE-BFC1-08002bE10318}" -ErrorAction SilentlyContinue | ForEach-Object {
    $props = Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue
    if ($props -and $props.DriverDesc) {
        Set-ItemProperty $_.PSPath -Name PnPCapabilities -Value 24 -Type DWord -ErrorAction SilentlyContinue
    }
}

# Belt-and-braces: also clear WoL flags via the NetAdapter cmdlets.
# This affects the live driver state in addition to the registry key above.
Get-NetAdapter | Where-Object { $_.Status -eq "Up" } | ForEach-Object {
    Set-NetAdapterPowerManagement -Name $_.Name `
        -WakeOnMagicPacket Disabled `
        -WakeOnPattern Disabled `
        -ErrorAction SilentlyContinue
}

Write-EventLog -LogName Application -Source "ai-mesh" -EventId 1 -EntryType Information `
    -Message "ai-mesh-harden-boot: stability hardening re-applied." -ErrorAction SilentlyContinue
'@

    Set-Content -Path $hardenScript -Value $scriptContent -Encoding UTF8

    # Register event log source so Write-EventLog above doesn't throw.
    if (-not [System.Diagnostics.EventLog]::SourceExists("ai-mesh")) {
        New-EventLog -LogName Application -Source "ai-mesh" -ErrorAction SilentlyContinue
    }

    $action  = New-ScheduledTaskAction -Execute "powershell.exe" `
                   -Argument "-NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$hardenScript`""
    $trigger = New-ScheduledTaskTrigger -AtStartup
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 2) `
                    -MultipleInstances IgnoreNew
    $principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -RunLevel Highest

    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger `
        -Settings $settings -Principal $principal -Force | Out-Null

    Write-Host ">>> Startup hardening task registered: '$taskName' (runs as SYSTEM at every boot)."
}

function Enable-SshElevation {
    # LocalAccountTokenFilterPolicy = 1 allows SSH sessions for local admin
    # accounts to run with a full (elevated) token rather than the default
    # filtered token. Without this, remote PowerShell via SSH cannot install
    # services or write to HKLM - even when logged in as an Administrator.
    $regPath = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System"
    $name = "LocalAccountTokenFilterPolicy"

    $exists = Get-ItemProperty -Path $regPath -ErrorAction SilentlyContinue | Select-Object -ExpandProperty $name -ErrorAction SilentlyContinue

    if ($exists -eq 1) {
        Write-Host ">>> SSH elevation already enabled."
        return
    }

    Write-Host ">>> Enabling SSH elevation (LocalAccountTokenFilterPolicy = 1)..."
    New-ItemProperty -Path $regPath -Name $name -Value 1 -PropertyType DWord -Force | Out-Null
    Write-Host ">>> SSH elevation enabled."
}

function Ensure-AgentService {
    # NSSM AppEnvironmentExtra takes each env var as a separate argument, not
    # semicolon-separated. Passing a single string with a semicolon produces a
    # leading colon in the registry and the vars are never set.
    $nssm = Get-Nssm
    $agentLog = Join-Path $logDir "agent.log"

    Write-Host ">>> Using NSSM at: $nssm"

    $existing = Get-Service -Name $agentService -ErrorAction SilentlyContinue
    if (-not $existing) {
        & $nssm install $agentService $agentPath
    }
    & $nssm set $agentService AppDirectory $aiMeshRoot
    & $nssm set $agentService AppEnvironmentExtra `
        "COORDINATOR_IP=$CoordinatorIp" `
        "AGENT_ROLE=$Role" `
        "LLAMA_MODEL_DIR=$env:USERPROFILE\.ai-mesh\models" `
        "LLAMA_SERVER_BIN=$(Join-Path $llamaInstallDir 'llama-server.exe')" `
        "LLAMA_GPU_LAYERS=99" `
        "LLAMA_FLASH_ATTN=1" `
        "DEFAULT_MODEL=$defaultModel"
    & $nssm set $agentService Start SERVICE_AUTO_START
    & $nssm set $agentService AppStdout $agentLog
    & $nssm set $agentService AppStderr $agentLog
    & $nssm set $agentService AppRotateFiles 1
    & $nssm set $agentService AppRotateOnline 1
    & $nssm set $agentService AppRotateBytes 10485760
    & $nssm set $agentService AppThrottle 1500
    & $nssm set $agentService AppRestartDelay 5000
    & $nssm set $agentService AppKillProcessTree 1

    & sc.exe stop $agentService 2>&1 | Out-Null
    Start-Sleep -Seconds 2
    & sc.exe start $agentService 2>&1 | Out-Null
    Write-Host ">>> Agent service start triggered."
}

# Main provisioning sequence

$defaultModel = Select-DefaultModel
Write-Host ">>> Detected hardware → default model: $defaultModel"
Write-Host ">>> To load it after provisioning: just auto-load-model <node-name>"

Ensure-Directory -Path $aiMeshRoot
Ensure-Directory -Path $logDir

Disable-Sleep
Harden-Stability
Ensure-StartupHardeningTask
Enable-SshElevation

Ensure-LlamaCpp
Ensure-Nssm

Ensure-AgentBinary
Ensure-AgentService

Write-Host ">>> Provisioning complete. This node is ready as an ai-mesh $Role node."
