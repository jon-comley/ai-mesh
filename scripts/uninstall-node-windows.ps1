param(
    [switch]$RemoveBinary
)

$ErrorActionPreference = "Stop"

$agentService  = "ai-mesh-agent"
$ollamaService = "ollama-serve"
$aiMeshRoot    = "C:\Users\$env:USERNAME\ai-mesh"

function Get-Nssm {
    $nssm = Get-Command nssm.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
    if (-not $nssm) { throw "nssm.exe not found on PATH." }
    return $nssm
}

function Remove-NssmService {
    param([string]$Name)
    $existing = Get-Service -Name $Name -ErrorAction SilentlyContinue
    if (-not $existing) {
        Write-Host ">>> Service $Name not found - skipping."
        return
    }
    Write-Host ">>> Stopping $Name..."
    & sc.exe stop $Name 2>&1 | Out-Null
    Start-Sleep -Seconds 2
    Write-Host ">>> Removing $Name NSSM service..."
    $nssm = Get-Nssm
    & $nssm remove $Name confirm 2>&1 | Out-Null
    Write-Host ">>> $Name removed."
}

Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force
Get-Process nssm  -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1

Remove-NssmService -Name $agentService

if ($RemoveBinary) {
    Write-Host ">>> Removing agent.exe from $aiMeshRoot..."
    Remove-Item -Path (Join-Path $aiMeshRoot "agent.exe") -ErrorAction SilentlyContinue
    Write-Host ">>> Binary removed."
}

Write-Host ">>> Uninstall complete."
