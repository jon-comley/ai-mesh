param(
    [Parameter(Mandatory = $true)]
    [string]$CoordinatorIp,

    [string]$Role = "compute",

    # Override the Qwen2.5 model to pre-cache. If empty, the best variant
    # for this node's RAM is selected automatically.
    [string]$Model = ""
)

$ErrorActionPreference = "Stop"

$aiMeshRoot      = "C:\Users\$env:USERNAME\ai-mesh"
$agentPath       = Join-Path $aiMeshRoot "agent.exe"
$logDir          = Join-Path $aiMeshRoot "logs"
$agentService    = "ai-mesh-agent"
$ollamaService   = "ollama-serve"
$ollamaApiUrl    = "http://127.0.0.1:11434"

# Ollama is installed from GitHub ZIP rather than winget.
# The winget package (Ollama.Ollama) is pinned to 0.24.0 which does not support
# OLLAMA_VULKAN=1 and therefore cannot use AMD iGPUs via Vulkan.
# NOTE: AMD Adrenalin driver 26.5.2+ must be installed on the host BEFORE
# running this script for GPU acceleration to work. Download from:
# https://www.amd.com/en/support/download/drivers.html
$ollamaVersion    = "v0.30.0-rc21"
$ollamaZipUrl     = "https://github.com/ollama/ollama/releases/download/$ollamaVersion/ollama-windows-amd64.zip"
$ollamaInstallDir = "$env:LOCALAPPDATA\Programs\Ollama"

function Select-ModelForRam {
    $ramGb = [math]::Round((Get-WmiObject Win32_ComputerSystem).TotalPhysicalMemory / 1073741824)
    if     ($ramGb -lt 6)  { return "qwen2.5:1.5b" }
    elseif ($ramGb -lt 12) { return "qwen2.5:7b"   }
    elseif ($ramGb -lt 32) { return "qwen2.5:14b"  }
    else                   { return "qwen2.5:32b"  }
}

if ([string]::IsNullOrEmpty($Model)) {
    $Model = Select-ModelForRam
    $ramGb = [math]::Round((Get-WmiObject Win32_ComputerSystem).TotalPhysicalMemory / 1073741824)
    Write-Host ">>> Auto-selected model for ${ramGb}GB RAM: $Model"
} else {
    Write-Host ">>> Using specified model: $Model"
}
$modelName = $Model

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

function Ensure-Ollama {
    # The winget package is pinned to 0.24.0 which does not support OLLAMA_VULKAN.
    # Install from GitHub release ZIP so AMD iGPU Vulkan acceleration works.
    # Skip if a compatible version (0.30+) is already running.
    try {
        $resp = Invoke-RestMethod -Uri "$ollamaApiUrl/api/version" -TimeoutSec 3 -ErrorAction SilentlyContinue
        if ($resp -and ($resp.version -like "0.3*" -or $resp.version -notlike "0.2*")) {
            Write-Host ">>> Ollama $($resp.version) already installed — skipping download."
            return
        }
    } catch {}

    Write-Host ">>> Installing Ollama $ollamaVersion from ZIP (winget package lacks AMD Vulkan support)..."
    Write-Host ">>>   REMINDER: AMD Adrenalin 26.5.2+ must be installed on this machine for GPU to work."

    # Stop service and kill processes before overwriting the binary
    & sc.exe stop $ollamaService 2>&1 | Out-Null
    Start-Sleep -Seconds 3
    Get-Process -Name "ollama" -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 1

    $zipPath = Join-Path $env:TEMP "ollama-windows-amd64.zip"
    Write-Host ">>> Downloading $ollamaZipUrl..."
    Invoke-WebRequest -Uri $ollamaZipUrl -OutFile $zipPath -UseBasicParsing

    Ensure-Directory -Path $ollamaInstallDir
    Write-Host ">>> Extracting to $ollamaInstallDir..."
    Expand-Archive -Path $zipPath -DestinationPath $ollamaInstallDir -Force
    Remove-Item $zipPath -ErrorAction SilentlyContinue
    Write-Host ">>> Ollama $ollamaVersion installed at $ollamaInstallDir."
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

function Wait-OllamaApi {
    param([int]$MaxSeconds = 30)
    Write-Host ">>> Waiting for Ollama API at $ollamaApiUrl (up to ${MaxSeconds}s)..."
    $deadline = (Get-Date).AddSeconds($MaxSeconds)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 2
        try {
            $resp = Invoke-WebRequest -Uri $ollamaApiUrl -UseBasicParsing -TimeoutSec 2 -ErrorAction SilentlyContinue
            if ($resp.StatusCode -eq 200) {
                Write-Host ">>> Ollama API is responding."
                return
            }
        } catch {}
    }
    throw "Ollama did not start within ${MaxSeconds}s. Check logs or run 'ollama serve' manually."
}

function Ensure-OllamaService {
    $nssm = Get-Nssm
    $ollamaExe = Join-Path $ollamaInstallDir "ollama.exe"
    if (-not (Test-Path $ollamaExe)) {
        throw "ollama.exe not found at $ollamaInstallDir — Ensure-Ollama must run first."
    }
    $ollamaLog = Join-Path $logDir "ollama.log"

    Write-Host ">>> Ensuring Ollama NSSM service ($ollamaService) using $ollamaExe..."

    $existing = Get-Service -Name $ollamaService -ErrorAction SilentlyContinue
    if (-not $existing) {
        & $nssm install $ollamaService $ollamaExe "serve"
    } else {
        & $nssm set $ollamaService Application $ollamaExe
        & $nssm set $ollamaService AppParameters "serve"
    }

    & $nssm set $ollamaService AppDirectory (Split-Path $ollamaExe)
    & $nssm set $ollamaService Start SERVICE_AUTO_START
    & $nssm set $ollamaService AppStdout $ollamaLog
    & $nssm set $ollamaService AppStderr $ollamaLog
    & $nssm set $ollamaService AppRotateFiles 1
    & $nssm set $ollamaService AppRotateBytes 10485760
    & $nssm set $ollamaService AppKillProcessTree 1
    # OLLAMA_VULKAN=1: enables Vulkan backend, required for AMD iGPUs (e.g. Radeon 780M)
    # which are not detected by ROCm but are Vulkan-capable. Without this Ollama
    # reports 0 B VRAM and falls back to CPU-only inference.
    & $nssm set $ollamaService AppEnvironmentExtra "OLLAMA_VULKAN=1"

    & sc.exe stop $ollamaService 2>&1 | Out-Null
    Start-Sleep -Seconds 2
    & sc.exe start $ollamaService 2>&1 | Out-Null

    Wait-OllamaApi -MaxSeconds 30
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
    & $nssm set $agentService AppEnvironmentExtra "COORDINATOR_IP=$CoordinatorIp" "AGENT_ROLE=$Role"
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

function Ensure-ModelCached {
    param([string]$Model)
    Write-Host ">>> Pre-caching model $Model..."
    & ollama pull $Model

    try {
        $resp = Invoke-WebRequest -Uri "$ollamaApiUrl/api/tags" -UseBasicParsing -TimeoutSec 5
        if ($resp.Content -notmatch [regex]::Escape($Model.Split(":")[0])) {
            throw "Model $Model not found in /api/tags response after pull."
        }
    } catch {
        throw "Could not verify model $Model via Ollama API: $_"
    }
    Write-Host ">>> Model $Model cached and verified."
}

# Main provisioning sequence

Ensure-Directory -Path $aiMeshRoot
Ensure-Directory -Path $logDir

Enable-SshElevation

Ensure-Ollama
Ensure-Nssm

Ensure-OllamaService

Ensure-ModelCached -Model $modelName

Ensure-AgentBinary
Ensure-AgentService

Write-Host ">>> Provisioning complete. This node is ready as an ai-mesh $Role node."
