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
    │  HTTP GET http://127.0.0.1:8080/_/{id};   (numeric transport actions)
    ▼
REAPER Web Browser Control (Windows, port 8080)
    │
    ▼
REAPER DAW
```

For arbitrary Lua (track creation, arming, project state) the web server is not
enough — `csurf` can only dispatch **numeric** action IDs, not named (`RS...`)
script actions. The `reaper_script` tool instead drives a **Lua daemon** running
inside REAPER (see [ReaScript daemon bridge](#reascript-daemon-bridge)).

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

### 5. Install the ReaScript daemon

Required for the `reaper_script` tool and `just test-record` (anything beyond the
numeric transport actions). See [ReaScript daemon bridge](#reascript-daemon-bridge).

```bash
just setup-reaper-daemon
```

Then register `ai_mesh_daemon.lua` in REAPER's action list and **add it to startup
actions** (required — see the daemon section), as the recipe instructs.

---

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `REAPER_HOST` | `localhost` | Host of the REAPER web server. Use `127.0.0.1` on WSL2. |
| `REAPER_PORT` | `8080` | Port of the REAPER web server. |
| `REAPER_WSL_SCRIPTS_PATH` | `/mnt/c/Users/jonno/AppData/Roaming/REAPER/Scripts` | WSL-visible path to REAPER's Scripts folder, where the daemon bridge command files live. |

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

## ReaScript daemon bridge

The csurf web server executes **numeric action IDs only** — it cannot run a named
`RS...` ReaScript action, and it has no endpoint for evaluating arbitrary Lua. To
run Lua we use a small long-lived daemon registered inside REAPER:

```
reaper_script(code)
    │  agent writes:
    │    Scripts/ai_mesh_cmd.lua  ← the Lua to run
    │    Scripts/ai_mesh_id.txt   ← a fresh id (request_id / epoch-ns)
    ▼
ai_mesh_daemon.lua  (reaper.defer loop, polls ai_mesh_id.txt)
    │  on id change: pcall(dofile, ai_mesh_cmd.lua)
    ▼
REAPER DAW
```

The daemon re-schedules itself via `reaper.defer` and runs for the life of the
REAPER process. It is **fire-and-forget**: the agent returns `ok` once the files
are written; it does not read a result back from the Lua. Errors inside the Lua
are printed to REAPER's console (`Actions → Show REAPER console`), prefixed
`[ai-mesh]`.

### One-time setup

```bash
just setup-reaper-daemon
```

This writes `ai_mesh_daemon.lua` (plus the seed `ai_mesh_cmd.lua` / `ai_mesh_id.txt`)
into REAPER's Scripts folder and prints the registration steps:

1. `Actions → Show Action List → Load` → select `ai_mesh_daemon.lua`
2. Run it once — the console prints `[ai-mesh] daemon started`
3. **Right-click the action → `Add to startup actions`.** This is required — the
   daemon lives in the REAPER process, so without a startup action it dies on every
   REAPER restart and `reaper_script` goes silent until you re-run it by hand.

### Verifying end-to-end

```bash
just test-record
```

Creates an armed mic track, records 5 s from the default input, stops, rewinds,
and plays back — exercising both the numeric transport path and the daemon bridge.

---

## Supported Actions

### Named transport actions (`reaper_transport` tool)

| Action | REAPER command ID |
|---|---|
| `play` | 1008 |
| `stop` | 1007 |
| `pause` | 1016 |
| `record` | 1013 |
| `rewind` | 40042 |
| `new_project` | 40023 |
| `save` | 40022 |

> Note: `record` is **1013**, not 1009 — `1009` is *Play/stop* and will silently
> play instead of record. `rewind` is **40042** (*Go to start of project*).

### Arbitrary actions (`reaper_action` tool)

Pass any numeric command ID (as a string) or named action string:

```
"action_id": "40075"       # toggle repeat
"action_id": "_SWS_ABOUT"  # SWS extension action
```

The action is sent as `GET http://127.0.0.1:8080/_/{action_id};`. The trailing
semicolon is the csurf command separator — without it the request is ignored.

### Arbitrary Lua (`reaper_script` tool)

Runs Lua/ReaScript inside REAPER via the daemon bridge. Used for anything the
numeric actions can't express — creating/naming/arming tracks, setting record
inputs, querying project state. Example: create an armed mono vocal track.

```lua
reaper.InsertTrackAtIndex(0, true)
local t = reaper.GetTrack(0, 0)
reaper.GetSetMediaTrackInfo_String(t, "P_NAME", "Vocals", true)
reaper.SetMediaTrackInfo_Value(t, "I_RECINPUT", 0)   -- 0 = mono input 1
reaper.SetMediaTrackInfo_Value(t, "I_RECARM", 1)
reaper.UpdateArrange()
```

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
- Result read-back from `reaper_script` (currently fire-and-forget; errors only surface in REAPER's console)
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
