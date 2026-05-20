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
    Write-Host ">>> Downloading $llamaZipUrl..."
    Invoke-WebRequest -Uri $llamaZipUrl -OutFile $zipPath -UseBasicParsing

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

Enable-SshElevation

Ensure-LlamaCpp
Ensure-Nssm

Ensure-AgentBinary
Ensure-AgentService

Write-Host ">>> Provisioning complete. This node is ready as an ai-mesh $Role node."
