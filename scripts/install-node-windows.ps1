param(
    [Parameter(Mandatory = $true)]
    [string]$CoordinatorIp,

    [string]$Role = "compute",

    [string]$AuthorizedKey = ""
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

    # GPU VRAM - handles 32-bit overflow (4294967295 = iGPU with >= 4 GB allocated)
    $gpuObj = Get-WmiObject Win32_VideoController |
        Where-Object { $_.AdapterRAM -gt 0 } |
        Sort-Object AdapterRAM -Descending |
        Select-Object -First 1
    if ($gpuObj) {
        if ($gpuObj.AdapterRAM -eq 4294967295) {
            $memMb = 8192  # 32-bit overflow sentinel - GPU reports max uint32 when VRAM > 4 GB
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

# Reload $env:Path from the machine + user registry so packages installed by
# winget in *this* session are visible without opening a new PowerShell.
function Update-SessionPath {
    $machine = [System.Environment]::GetEnvironmentVariable('Path', 'Machine')
    $user    = [System.Environment]::GetEnvironmentVariable('Path', 'User')
    $paths = @($machine, $user) | Where-Object { $_ }
    $env:Path = $paths -join ';'
}

function Ensure-WingetPackage {
    param(
        [string]$Id,
        [string]$Name
    )
    # Check if already installed
    $check = winget list --id $Id --source winget 2>$null
    if ($LASTEXITCODE -eq 0 -and $check) {
        return
    }

    Write-Host ">>> Installing $Name via winget..."

    # Try install
    $out = winget install --id $Id -e --source winget --accept-package-agreements --accept-source-agreements 2>&1

    # Check for success. Some winget versions return 0 even when source is broken.
    # Error 0x8a15000f is "Data required by the source is missing".
    if ($LASTEXITCODE -ne 0 -or $out -match "0x8a15000f" -or $out -match "Failed when opening source") {
        Write-Host ">>> winget install failed or source is broken - attempting repair..."
        winget source reset --force 2>$null
        winget source update 2>$null
        $out = winget install --id $Id -e --source winget --accept-package-agreements --accept-source-agreements 2>&1

        if ($LASTEXITCODE -ne 0 -and $out -notmatch "already installed") {
            # Last ditch: try msstore
            Write-Host ">>> winget source still failing - trying msstore..."
            $out = winget install --id $Id -e --source msstore --accept-package-agreements --accept-source-agreements 2>&1
            
            if ($LASTEXITCODE -ne 0 -and $out -notmatch "already installed") {
                throw "Failed to install $Name via winget."
            }
        }
    }

    Update-SessionPath
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
    $localBin = Join-Path $aiMeshRoot "bin\nssm.exe"
    if (Get-Command "nssm.exe" -ErrorAction SilentlyContinue) { return }
    if (Test-Path $localBin) { return }

    # Ensure TLS 1.2+ is enabled for GitHub downloads
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13

    # Hide progress bar for faster downloads over SSH
    $oldProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'

    # Try direct download from mirrors first (faster and avoids winget source issues).
    # NOTE: nssm-2.24 is the last OFFICIAL STABLE (released 2014-08-31) - it predates
    # AppKillProcessTree (added in later dev builds, never cut as a stable release).
    # Prefer the 2.24-101 CI build, which supports AppKillProcessTree for clean
    # llama-server child cleanup; fall back to the 2.24 stable mirrors.
    # 1. nssm.cc CI build (2.24-101) - has AppKillProcessTree
    # 2. Official 2.24 stable (nssm.cc)
    # 3. fawno mirror / ONLYOFFICE mirror - 2.24 stable repackages
    $nssmZipUrls = @(
        "https://nssm.cc/ci/nssm-2.24-101-g897c7ad.zip",
        "https://nssm.cc/release/nssm-2.24.zip",
        "https://github.com/fawno/nssm.cc/releases/download/v2.24.1/nssm-v2.24.1-Win64.zip",
        "https://github.com/ONLYOFFICE/nssm/releases/download/v2.24.1/nssm_x64.zip"
    )
    $zipPath = Join-Path $env:TEMP "nssm.zip"
    $extractPath = Join-Path $env:TEMP "nssm_extract"

    $downloaded = $false
    foreach ($url in $nssmZipUrls) {
        try {
            Write-Host ">>> Downloading NSSM from $url..."
            Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing -ErrorAction Stop -TimeoutSec 30
            $downloaded = $true
            break
        } catch {
            Write-Host ">>> Warning: Failed to download from $url - trying next mirror..."
        }
    }

    $ProgressPreference = $oldProgress

    if ($downloaded) {
        if (Test-Path $extractPath) { Remove-Item $extractPath -Recurse -Force }
        Expand-Archive -Path $zipPath -DestinationPath $extractPath -Force

        # Search for nssm.exe; prefer 'win64' folder if present, otherwise take the first match.
        $allExes = Get-ChildItem -Path $extractPath -Filter nssm.exe -Recurse
        $nssmExe = $allExes | Where-Object { $_.FullName -match "win64" } | Select-Object -First 1
        if (-not $nssmExe) { $nssmExe = $allExes | Select-Object -First 1 }

        if ($nssmExe) {
            $binDir = Join-Path $aiMeshRoot "bin"
            Ensure-Directory -Path $binDir
            Copy-Item -Path $nssmExe.FullName -Destination $localBin -Force
            Write-Host ">>> NSSM installed manually to $binDir"
            return
        }
    }

    Write-Host ">>> Direct download failed. Attempting winget as final fallback..."
    try {
        Ensure-WingetPackage -Id "NSSM.NSSM" -Name "NSSM"
    } catch {
        throw "Failed to install NSSM via all available methods (Mirrors and Winget)."
    }
}

# Resolve nssm.exe robustly: PATH - refreshed PATH - known winget/install dirs.
# winget updates the registry PATH but not the running session, so a freshly
# installed nssm.exe is otherwise invisible until a new shell is opened.
function Get-Nssm {
    $cmd = Get-Command nssm.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    Update-SessionPath
    $cmd = Get-Command nssm.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    # Check local bin
    $localBin = Join-Path $aiMeshRoot "bin\nssm.exe"
    if (Test-Path $localBin) { return $localBin }

    # Fall back to the locations winget drops it: the Links shim dir and the
    # versioned package dir under WinGet\Packages.
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Links\nssm.exe'),
        (Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Packages')
    )
    foreach ($base in $candidates) {
        if (Test-Path $base) {
            $found = Get-ChildItem -Path $base -Recurse -Filter nssm.exe -ErrorAction SilentlyContinue |
                Where-Object { $_.FullName -match '\\win64\\' -or $_.Directory.Name -eq 'Links' } |
                Select-Object -First 1
            if ($found) { return $found.FullName }
        }
    }

    throw "nssm.exe not found after install. Please install NSSM manually or fix winget."
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
    # Registry fixes for Beelink stability (DPC_WATCHDOG and kernel hangs):
    #   1. Fast Startup - powercfg /h off disables hibernate but not Fast
    #      Startup (hybrid shutdown); can produce a half-suspended state.
    #   2. NIC power management - adapter should stay live across idle periods.
    #      Also disables WoL (Wake on LAN) to prevent network-triggered wake
    #      states that may lead to Type B hangs (process stuck, GPU unresponsive).
    #   3. GPU ULPS (Ultra Low Power State) - the AMD Radeon 780M DPC routine
    #      for waking out of ULPS overruns the watchdog timeout, causing
    #      0x00000133 BSODs at idle. Confirmed root cause 2026-05-28.
    #      AMD driver updates silently restore EnableUlps=1, so this must be
    #      re-applied at every boot via the Scheduled Task below.
    #
    # NOTE: TdrDelay intentionally NOT set here. Setting it to 60s was tried
    # and correlated with severe GPU overheating (DPC_WATCHDOG_VIOLATION crashes
    # followed by thermal shutdown). The Windows default (2s) is safer - it
    # resets a hung GPU driver quickly rather than letting it cook.

    # Fast Startup off
    reg add "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Power" /v HiberbootEnabled /t REG_DWORD /d 0 /f | Out-Null

    # NIC power management off
    Get-ChildItem "HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4D36E972-E325-11CE-BFC1-08002bE10318}" -ErrorAction SilentlyContinue | ForEach-Object {
        $props = Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue
        if ($props -and $props.DriverDesc) {
            Set-ItemProperty $_.PSPath -Name PnPCapabilities -Value 24 -Type DWord -ErrorAction SilentlyContinue
        }
    }

    # GPU ULPS off - disable on every display adapter subkey
    Get-ChildItem "HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4D36E968-E325-11CE-BFC1-08002bE10318}" -ErrorAction SilentlyContinue | ForEach-Object {
        $props = Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue
        if ($props -and $props.DriverDesc) {
            Set-ItemProperty $_.PSPath -Name EnableUlps              -Value 0 -Type DWord -ErrorAction SilentlyContinue
            Set-ItemProperty $_.PSPath -Name PP_SclkDeepSleepDisable -Value 1 -Type DWord -ErrorAction SilentlyContinue
        }
    }

    # WoL (Wake on LAN) off - prevents network-triggered wake or half-sleep states
    # that may contribute to Type B hangs (process stuck, GPU unresponsive).
    Get-NetAdapter | Where-Object { $_.Status -eq "Up" } | ForEach-Object {
        Set-NetAdapterPowerManagement -Name $_.Name `
            -WakeOnMagicPacket Disabled `
            -WakeOnPattern Disabled `
            -ErrorAction SilentlyContinue
    }

    # High Performance power plan — prevents CPU/GPU from throttling under 24/7 load
    powercfg /setactive 8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c | Out-Null

    Write-Host ">>> Stability hardening applied (Fast Startup off, NIC power save off, GPU ULPS off, WoL off, High Performance power plan)."
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
# TdrDelay is intentionally NOT set - 60s was correlated with GPU overheating.
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

# GPU ULPS off - AMD driver updates silently restore EnableUlps=1, causing
# 0x00000133 DPC_WATCHDOG_VIOLATION at idle (confirmed root cause 2026-05-28).
# Re-apply on every boot to survive driver reinstalls.
Get-ChildItem "HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4D36E968-E325-11CE-BFC1-08002bE10318}" -ErrorAction SilentlyContinue | ForEach-Object {
    $props = Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue
    if ($props -and $props.DriverDesc) {
        Set-ItemProperty $_.PSPath -Name EnableUlps              -Value 0 -Type DWord -ErrorAction SilentlyContinue
        Set-ItemProperty $_.PSPath -Name PP_SclkDeepSleepDisable -Value 1 -Type DWord -ErrorAction SilentlyContinue
    }
}

Write-EventLog -LogName Application -Source "ai-mesh" -EventId 1 -EntryType Information `
    -Message "ai-mesh-harden-boot: stability hardening re-applied (Fast Startup off, NIC power save off, GPU ULPS off)." -ErrorAction SilentlyContinue
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

function Enable-SshKeyAuthorization {
    if (-not $AuthorizedKey) { return }

    $authFile = "C:\ProgramData\ssh\administrators_authorized_keys"
    Write-Host ">>> Authorizing SSH key..."

    if (-not (Test-Path $authFile)) {
        New-Item -ItemType File -Path $authFile -Force | Out-Null
    }

    $content = Get-Content $authFile -ErrorAction SilentlyContinue
    if ($content -notcontains $AuthorizedKey) {
        Add-Content -Path $authFile -Value $AuthorizedKey
        Write-Host ">>> SSH key added to $authFile"
    } else {
        Write-Host ">>> SSH key already authorized."
    }

    # Windows OpenSSH requires strict permissions on this file:
    # Only Administrators and SYSTEM should have access.
    & icacls.exe $authFile /inheritance:r /grant "Administrators:(F)" /grant "SYSTEM:(F)" | Out-Null
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
    # AppKillProcessTree (kills the spawned llama-server on stop) only exists in
    # NSSM builds newer than the 2.24 stable. The script runs under
    # $ErrorActionPreference = 'Stop', so nssm writing 'Invalid parameter' to
    # stderr becomes a terminating NativeCommandError - swallow it via try/catch
    # so an older nssm.exe doesn't abort provisioning. Applied when supported.
    try {
        & $nssm set $agentService AppKillProcessTree 1 2>&1 | Out-Null
    } catch {
        Write-Host ">>> (skipping AppKillProcessTree - not supported by this NSSM build)"
    }

    & sc.exe stop $agentService 2>&1 | Out-Null
    Start-Sleep -Seconds 2
    & sc.exe start $agentService 2>&1 | Out-Null
    Write-Host ">>> Agent service start triggered."
}

# Main provisioning sequence

$defaultModel = Select-DefaultModel
Write-Host ">>> Detected hardware - default model: $defaultModel"
Write-Host ">>> To load it after provisioning: just auto-load-model (node-name)"

Ensure-Directory -Path $aiMeshRoot
Ensure-Directory -Path $logDir

Disable-Sleep
Harden-Stability
Ensure-StartupHardeningTask
Enable-SshElevation
Enable-SshKeyAuthorization

Ensure-LlamaCpp
Ensure-Nssm

Ensure-AgentBinary
Ensure-AgentService

Write-Host ">>> Provisioning complete. This node is ready as an ai-mesh $Role node."
