# REAPER DAW Integration

LLM control of the [REAPER](https://www.reaper.fm/) digital audio workstation via the ai-mesh coordinator intent pipeline.

---

## Architecture

```
User intent ("play the track")
    │
    ▼
Coordinator (pi1) — intent router
    │  ReaperCommand message
    ▼
OmniLink1 agent (WSL2, --features reaper)
    │  HTTP GET http://127.0.0.1:8080/_/command/{id}
    ▼
REAPER Web Browser Control (Windows, port 8080)
    │
    ▼
REAPER DAW
```

REAPER runs on the Windows side of OmniLink1. The WSL2 agent reaches it over loopback via mirrored networking (`.wslconfig` `networkingMode=mirrored`). Note: use `127.0.0.1` explicitly — `localhost` resolves to IPv6 first on WSL2 and will fail.

---

## Installation

### 1. Windows — run the installer script

From a Windows PowerShell (Administrator):

```powershell
powershell -ExecutionPolicy Bypass -File "\\wsl.localhost\Ubuntu\home\jonno\repos\ai-mesh\scripts\install-reaper-windows.ps1"
```

The script:
- Downloads and silently installs the latest REAPER x64
- Writes `%APPDATA%\REAPER\reaper-webbrd.ini` (port 8080, bind `0.0.0.0`)
- Registers the Web Browser Control surface in `reaper.ini`
- Opens a Windows Firewall inbound rule for TCP 8080

Optional: add `-AutoStart` to register a per-user Scheduled Task that launches REAPER at login.

### 2. Verify REAPER web server

Launch REAPER, then check **Options → Preferences → Control/OSC/web**. The Web Browser Control entry should show status **up** with 1 listener on port 8080.

Test from WSL:

```bash
curl http://127.0.0.1:8080/_/TRANSPORT
```

Expected response (tab-delimited):

```
TRANSPORT	0	0.000000	0	0:00.000	1.1.00	1.1.00
```

### 3. Build and install the agent

The REAPER capability is a compile-time feature. Build the agent on OmniLink1:

```bash
cargo build --release -p agent --features reaper
cp target/release/agent ~/agent
sudo bash scripts/install-node-linux.sh 192.168.1.11 controller jonno
```

Add the REAPER environment drop-in:

```bash
sudo mkdir -p /etc/systemd/system/ai-mesh-agent.service.d
printf '[Service]\nEnvironment=REAPER_HOST=127.0.0.1\nEnvironment=REAPER_PORT=8080\n' \
  | sudo tee /etc/systemd/system/ai-mesh-agent.service.d/reaper.conf
sudo systemctl daemon-reload && sudo systemctl restart ai-mesh-agent
```

### 4. Push credentials from the coordinator

```bash
just set-fingerprint omnilink1
```

This runs locally (no SSH) when `NODE_HOST=127.0.0.1`.

---

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `REAPER_HOST` | `localhost` | Host of the REAPER web server. Use `127.0.0.1` on WSL2. |
| `REAPER_PORT` | `8080` | Port of the REAPER web server. |

---

## Node Configuration

`nodes/omnilink1.env`:

```
NODE_HOST=127.0.0.1
NODE_USER=jonno
NODE_OS=linux
NODE_ROLE=controller
NODE_FEATURES=reaper
```

---

## Capability Behaviour

**Transport poller** — `capability-reaper` spawns a background task on `start()` that polls `/_/TRANSPORT` every 2 seconds and sends a `ReaperStatus` message to the coordinator. The coordinator forwards this to the dashboard WebSocket as a `ReaperUpdate` event.

**Response parsing** — tries JSON first (newer REAPER builds), falls back to the standard tab-delimited format:

```
TRANSPORT \t play_state \t play_rate \t repeat \t position \t loop_mode \t tempo \t ts_num \t ts_denom
```

**Offline reporting** — when the web server is unreachable (REAPER closed, firewall, etc.) the agent sends `reaper_online: false` with zeroed fields. The dashboard shows an **Offline** badge.

---

## Supported Actions

### Named transport actions (`reaper_transport` tool)

| Action | REAPER command ID |
|---|---|
| `play` | 1008 |
| `stop` | 1007 |
| `pause` | 1016 |
| `record` | 1009 |
| `rewind` | 40113 |

### Arbitrary actions (`reaper_action` tool)

Pass any numeric command ID (as a string) or named action string:

```
"action_id": "40075"       # toggle repeat
"action_id": "_SWS_ABOUT"  # SWS extension action
```

The action is sent as `GET http://127.0.0.1:8080/_/command/{action_id}`.

---

## Dashboard Panel

`coordinator/src/http/static/reaper.js` renders:

- Online/offline badge
- Play state (Stopped / Playing / Paused / Recording)
- Position (formatted as `mm:ss.sss`)
- Tempo (BPM)
- Time signature
- Command log (last 20 commands with ok/fail status)

All DOM elements are null-guarded so the panel is safe to render before the first WebSocket update arrives.

---

## Deferred

- Dashboard REAPER tab wired in nav (panel renders but tab link not yet added to `index.html`)
- Tempo / time-sig write via intent (read-only today)
- Track list / project state queries
- REAPER on macOS (planned for next machine)
- Multi-REAPER instances (one per node, routed by node ID)

---

## Troubleshooting

**`curl http://127.0.0.1:8080/_/TRANSPORT` times out**
→ REAPER is not running, or the Web Browser Control surface is not enabled. Check Preferences → Control/OSC/web.

**`curl http://localhost:8080/_/TRANSPORT` fails but `127.0.0.1` works**
→ WSL2 resolves `localhost` to `::1` (IPv6) first. Always use `127.0.0.1` in env vars.

**Agent logs `reaper: REAPER web server unreachable`**
→ Check `REAPER_HOST` and `REAPER_PORT` are set in the systemd drop-in: `sudo systemctl show ai-mesh-agent --property=Environment`.

**`just set-fingerprint omnilink1` hangs**
→ Fixed in justfile — local nodes (`NODE_HOST=127.0.0.1`) skip SSH and write drop-ins directly. Pull latest and retry.
