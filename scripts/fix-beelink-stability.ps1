# Beelink SER8 Stability Hardening Script
# Applies all software-fixable mitigations. fTPM must be disabled in BIOS separately.

$ErrorActionPreference = "Stop"

# 1. Admin check
if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error "Must be run as Administrator."; exit 1
}

Write-Host "=== Beelink SER8 Stability Hardening ===" -ForegroundColor Cyan

# 2. AMD GPU registry hardening (ULPS + shader deep sleep)
Write-Host ">>> Applying AMD GPU registry hardening..."
$gpuClass = "HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}"
Get-ChildItem $gpuClass | Where-Object { $_.PSChildName -match "^\d{4}$" } | ForEach-Object {
    Set-ItemProperty -Path $_.PSPath -Name "EnableUlps"              -Value 0 -Type DWord -EA SilentlyContinue
    Set-ItemProperty -Path $_.PSPath -Name "PP_SclkDeepSleepDisable" -Value 1 -Type DWord -EA SilentlyContinue
}
Write-Host ">>> EnableUlps=0, PP_SclkDeepSleepDisable=1"

# 3. Intel AX200 Wi-Fi power management — CORRECT NIC class GUID
# (Previous version used the GPU class GUID by mistake and silently did nothing.)
Write-Host ">>> Disabling Intel AX200 Wi-Fi power management..."
$nicClass = "HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4D36E972-E325-11CE-BFC1-08002BE10318}"
$ax200 = Get-ChildItem $nicClass -EA SilentlyContinue | Where-Object {
    $p = Get-ItemProperty $_.PSPath -EA SilentlyContinue
    $p.DriverDesc -like "*AX200*" -or $p.DriverDesc -like "*Wi-Fi 6*"
}
if ($ax200) {
    foreach ($k in $ax200) {
        Set-ItemProperty -Path $k.PSPath -Name "PnPCapabilities" -Value 24 -Type DWord -EA SilentlyContinue
    }
    Write-Host ">>> AX200 PnPCapabilities=24 (power save disabled)"
} else {
    Write-Warning ">>> AX200 not found in NIC class — skipped"
}

# 4. High Performance power plan
Write-Host ">>> Setting power plan to High Performance..."
powercfg /setactive 8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c
Write-Host ">>> Done"

# 5. Verify state
Write-Host ""
Write-Host "=== Verification ===" -ForegroundColor Cyan
$gpuKey = Get-ChildItem $gpuClass | Where-Object { $_.PSChildName -match "^\d{4}$" } | Select-Object -First 1
if ($gpuKey) {
    $p = Get-ItemProperty $gpuKey.PSPath
    Write-Host "EnableUlps              = $($p.EnableUlps)"
    Write-Host "PP_SclkDeepSleepDisable = $($p.PP_SclkDeepSleepDisable)"
}
$plan = powercfg /getactivescheme
Write-Host "Power plan: $plan"

Write-Host ""
Write-Host "=== Done — reboot to apply ===" -ForegroundColor Green
Write-Host "REMINDER: disable fTPM in BIOS (Advanced > AMD PBS > fTPM Switch = Disabled)" -ForegroundColor Yellow
