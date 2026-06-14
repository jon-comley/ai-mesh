param(
    [int]$ReaperPort = 8080,

    # Set to $true to register a per-user Scheduled Task that launches REAPER
    # automatically when you log in.  Leave $false if you prefer to open it manually.
    [switch]$AutoStart = $false
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
$ProgressPreference = 'SilentlyContinue'   # faster Invoke-WebRequest

$reaperConfig = Join-Path $env:APPDATA "REAPER"
$reaperIni    = Join-Path $reaperConfig "reaper.ini"
$reaperWebIni = Join-Path $reaperConfig "reaper-webbrd.ini"
$reaperExe    = "C:\Program Files\REAPER (x64)\reaper.exe"

# ── Download ──────────────────────────────────────────────────────────────────

function Get-LatestReaperVersion {
    # Scrape the reaper.fm download page for the current x64 installer filename.
    try {
        $html = Invoke-WebRequest -Uri "https://www.reaper.fm/download.php" -UseBasicParsing -TimeoutSec 10
        $match = [regex]::Match($html.Content, 'reaper(\d+)_x64-install\.exe')
        if ($match.Success) { return $match.Groups[1].Value }
    } catch { }
    # Fall back to a known-good recent version.
    return "729"
}

function Install-Reaper {
    if (Test-Path $reaperExe) {
        Write-Host ">>> REAPER already installed at $reaperExe - skipping download."
        return
    }

    $ver = Get-LatestReaperVersion
    $url = "https://www.reaper.fm/files/$($ver.Substring(0,1)).x/reaper${ver}_x64-install.exe"
    $tmp = Join-Path $env:TEMP "reaper-install.exe"

    Write-Host ">>> Downloading REAPER $($ver.Insert(1,'.')) from $url ..."
    Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing -TimeoutSec 300
    Write-Host ">>> Running silent install..."
    Start-Process -FilePath $tmp -ArgumentList "/S" -Wait
    Remove-Item $tmp -Force
    Write-Host ">>> REAPER installed."
}

# ── Web server config ─────────────────────────────────────────────────────────

function Configure-WebServer {
    # Ensure the REAPER config dir exists (created on first REAPER launch, but we
    # may be writing it before the first launch).
    if (-not (Test-Path $reaperConfig)) {
        New-Item -ItemType Directory -Path $reaperConfig | Out-Null
    }

    # reaper-webbrd.ini - REAPER's web browser control surface config.
    # Binds to 0.0.0.0 so WSL2 (and other LAN devices) can reach it.
    $webIniContent = @"
[webbrd]
port=$ReaperPort
pass=
bindip=0.0.0.0
"@
    Set-Content -Path $reaperWebIni -Value $webIniContent -Encoding ASCII
    Write-Host ">>> Web server config written to $reaperWebIni (port $ReaperPort, bind 0.0.0.0)."

    # reaper.ini - register the Web Browser Control as a control surface so REAPER
    # actually loads the web server plugin on startup.
    # Format: [csurf] n=<count> <index>=WSC <midi_in> <midi_out> <name> <flags> <port>
    if (-not (Test-Path $reaperIni)) {
        # First-run stub - REAPER will merge its own defaults on first launch.
        Set-Content -Path $reaperIni -Value "" -Encoding ASCII
    }

    $ini = Get-Content $reaperIni -Raw -ErrorAction SilentlyContinue
    if ($ini -notmatch '\(web browser control\)') {
        # Append the csurf block.  If REAPER already has a [csurf] section with
        # other surfaces, this will add a second section; REAPER merges them on
        # load.  A full ini parser is out of scope here - verify in REAPER's UI.
        $csurf = @"

[csurf]
nummidi=0
n=1
0=WSC 0 "" "" "(web browser control)" 0 $ReaperPort
"@
        Add-Content -Path $reaperIni -Value $csurf -Encoding ASCII
        Write-Host ">>> Web Browser Control surface registered in reaper.ini."
    } else {
        Write-Host ">>> Web Browser Control already present in reaper.ini - skipping."
    }
}

# ── Firewall ──────────────────────────────────────────────────────────────────

function Open-Firewall {
    $ruleName = "REAPER Web Server (ai-mesh)"
    $existing = Get-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue
    if ($existing) {
        Write-Host ">>> Firewall rule already exists - skipping."
        return
    }
    New-NetFirewallRule `
        -DisplayName $ruleName `
        -Direction Inbound `
        -Protocol TCP `
        -LocalPort $ReaperPort `
        -Action Allow `
        -Profile Any | Out-Null
    Write-Host ">>> Firewall: TCP $ReaperPort opened for inbound connections."
}

# ── Optional auto-start ───────────────────────────────────────────────────────

function Register-AutoStart {
    $taskName = "REAPER - ai-mesh autostart"
    $existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($existing) {
        Write-Host ">>> Autostart task already registered - skipping."
        return
    }
    $action   = New-ScheduledTaskAction -Execute $reaperExe
    $trigger  = New-ScheduledTaskTrigger -AtLogOn
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Hours 0) -MultipleInstances IgnoreNew
    # Run as current user (REAPER needs a desktop session for audio + GUI).
    Register-ScheduledTask `
        -TaskName $taskName `
        -Action $action `
        -Trigger $trigger `
        -Settings $settings `
        -RunLevel Limited | Out-Null
    Write-Host ">>> Autostart task registered: REAPER will launch at next login."
}

# ── WSL2 host IP hint ─────────────────────────────────────────────────────────

function Show-Wsl2Hint {
    # WSL2 mirrored networking is assumed (networkingMode=mirrored in .wslconfig).
    # With mirrored mode, WSL2 shares the Windows network stack, so localhost
    # works from both sides without chasing the vEthernet (WSL) address.
    Write-Host ""
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    Write-Host "  WSL2 agent env vars (add to agent service drop-in)"
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    Write-Host "    REAPER_HOST=127.0.0.1"
    Write-Host "    REAPER_PORT=$ReaperPort"
    Write-Host ""
    Write-Host "  Requires WSL2 mirrored networking (.wslconfig already set)."
    Write-Host "  If not yet applied: run 'wsl --shutdown' then reopen WSL2."
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
}

# ── Manual verification reminder ──────────────────────────────────────────────

function Show-VerificationSteps {
    Write-Host ""
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    Write-Host "  VERIFY WEB SERVER AFTER FIRST REAPER LAUNCH"
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    Write-Host "  The reaper.ini config written by this script should be"
    Write-Host "  picked up automatically, but REAPER's ini merge behaviour"
    Write-Host "  can vary by version.  After first launch:"
    Write-Host ""
    Write-Host "  1. Open REAPER"
    Write-Host "  2. Options → Preferences → Control/OSC/web"
    Write-Host "  3. Confirm 'Web Browser Control' is listed and enabled"
    Write-Host "     on port $ReaperPort, bound to 0.0.0.0"
    Write-Host "  4. If not present: click Add → Web Browser Control,"
    Write-Host "     set port $ReaperPort, bind address 0.0.0.0, click OK"
    Write-Host "  5. Test: curl http://localhost:$ReaperPort/_/TRANSPORT"
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
}

# ── Main ──────────────────────────────────────────────────────────────────────

Install-Reaper
Configure-WebServer
Open-Firewall
if ($AutoStart) { Register-AutoStart }
Show-Wsl2Hint
Show-VerificationSteps

Write-Host ""
Write-Host ">>> Done. Launch REAPER, verify the web server (steps above),"
Write-Host ">>> then rebuild the OmniLink1 agent with --features reaper."
