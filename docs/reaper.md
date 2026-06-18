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

Then fully quit and reopen REAPER — the daemon auto-starts via `__startup.lua`
(see the daemon section).

---

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `REAPER_HOST` | `localhost` | Host of the REAPER web server. Use `127.0.0.1` on WSL2. |
| `REAPER_PORT` | `8080` | Port of the REAPER web server. |
| `REAPER_WSL_SCRIPTS_PATH` | WSL2: `/mnt/c/Users/$USER/AppData/Roaming/REAPER/Scripts`; macOS: `$HOME/Library/Application Support/REAPER/Scripts` | WSL-visible path to REAPER's Scripts folder, where the daemon bridge command files live. The default derives from the runtime user (`default_scripts_dir()`); set this explicitly if the Windows and Linux usernames differ. |
| `REAPER_SCRIPT_TIMEOUT_MS` | `5000` | How long the agent waits for the daemon to write `ai_mesh_result.txt` before reporting the daemon unresponsive. |

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
run Lua we use a small daemon inside REAPER, installed as REAPER's native
`__startup.lua` so it auto-starts on every launch:

```
reaper_script(code)
    │  agent writes:
    │    Scripts/ai_mesh_cmd.lua  ← the Lua to run
    │    Scripts/ai_mesh_id.txt   ← a fresh id (request_id / epoch-ns)
    ▼
__startup.lua  (reaper.defer loop, polls ai_mesh_id.txt)
    │  on id change: pcall(dofile, ai_mesh_cmd.lua)
    │  writes Scripts/ai_mesh_result.txt = "<id>\t<ok|err>\t<message>"
    │  (via a temp file + os.rename, so the agent never reads a half-written result)
    ▼
    │  agent reads ai_mesh_result.txt whole and matches the leading id to its request
    ▼
ReaperScriptResult  (ok, the Lua error, or "daemon did not respond")
```

REAPER automatically runs `Scripts/__startup.lua` at startup (a native feature, no
SWS required), so the daemon comes up with REAPER and needs no manual registration.
It re-schedules itself via `reaper.defer` and runs for the life of the REAPER process.

The bridge is **request/response**, not fire-and-forget. After writing the trigger,
the agent polls `ai_mesh_result.txt` (matching its request id) for up to **5 s**
(`REAPER_SCRIPT_TIMEOUT_MS` overrides). It parses the whole file with
`splitn(3, '\t')` — `<id>`, `<status>`, then the message — so a `return` value or Lua
error spanning **multiple lines** (e.g. a `reaper_get_project` track listing) is relayed
intact. It returns one of:
- `<summary>` — the daemon ran the Lua cleanly. The daemon forwards the script's
  `return` value (a string) as the result message, so a structured tool can report
  what it did (e.g. `Added 'Vocals 2' as track 5 (armed)`). A bare script that
  returns nothing yields an empty message, which the coordinator shows as `ok`.
- `REAPER Lua error: <msg>` — the Lua raised (e.g. `attempt to index a nil value`),
- `REAPER daemon did not respond within 5s …` — no result appeared, i.e. the daemon
  isn't running (missing/stale `__startup.lua`, or REAPER was opened before setup).

The daemon captures the return via `local ok, ret = pcall(dofile, cmd_file)` and writes
`<id>\tok\t<ret>` on success. The agent relays this message; the coordinator's
`run_reaper_lua` falls back to `ok` only when the message is empty.

So a dead daemon now surfaces in chat instead of a false `ok`. Lua errors are also
printed to REAPER's console (`ReaScript console output`, which auto-opens), prefixed
`[ai-mesh]`.

### One-time setup

```bash
just setup-reaper-daemon
```

This writes `__startup.lua` (plus the seed `ai_mesh_cmd.lua` / `ai_mesh_id.txt` /
`ai_mesh_result.txt`) into REAPER's Scripts folder. Then **fully quit and reopen
REAPER** — a `ReaScript console output` window should appear on launch printing
`[ai-mesh] daemon started (via __startup.lua)`.

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

### Add a track (`reaper_add_track` tool)

Structured track creation — **preferred over `reaper_script` for "add a track called X"**,
because the coordinator generates guaranteed-correct Lua (small models routinely mishandle
`InsertTrackAtIndex`, which returns nothing, and produce a blank-named track). Args:

- `name` (required) — the label **only**, without the word "track" (for "add another
  vocal track" the name is `vocal`, not `vocal track`; the schema description steers the
  model to this).
- `rec_input` (optional) — mono record input index (`0` = input 1, `1` = input 2).
- `arm` (optional, default `true`) — arm the new track for recording, enable input
  monitoring, and **disarm all other tracks** so only the new one is record-ready.

Behaviour (built in `build_add_track_lua`, `coordinator/src/intent.rs`):

- **Title-cases** the name (`vocal` → `Vocal`, `vocal bus` → `Vocal Bus`).
- Resolves a unique name: if the name is taken it appends ` 2`, ` 3`, … (`Vocal`,
  `Vocal 2`, `Vocal 3`).
- Gets the new track via `GetTrack` (never the nil `InsertTrackAtIndex` return).
- Returns a summary, e.g. `Added 'Vocal 2' as track 5 (armed)`.

### Remove a track (`reaper_remove_track` tool)

Deletes the first track whose name matches `name`, **case-insensitively** (tracks are
stored Title Case but you'll usually type lowercase), via `build_remove_track_lua`. Returns
`Removed track 'Vocal' (was track 4)` or `No track named 'vocal' found`.

### Arbitrary Lua (`reaper_script` tool)

Runs Lua/ReaScript inside REAPER via the daemon bridge. Used for anything the numeric
actions and structured track tools can't express — setting record inputs on existing
tracks, querying project state, etc. A script may `return` a string to report a result.

```lua
reaper.InsertTrackAtIndex(0, true)
local t = reaper.GetTrack(0, 0)
reaper.GetSetMediaTrackInfo_String(t, "P_NAME", "Vocals", true)
reaper.SetMediaTrackInfo_Value(t, "I_RECINPUT", 0)   -- 0 = mono input 1
reaper.SetMediaTrackInfo_Value(t, "I_RECARM", 1)
reaper.UpdateArrange()
return "Vocals track created"
```

### Tempo & time signature (`reaper_set_tempo` tool)

Sets the project tempo (BPM) and/or time signature (built in `build_set_tempo_lua`). Args
(at least one required): `tempo` (number), `ts_num` + `ts_denom` (integers). Numeric strings
are accepted too, since small models often emit `"120"` rather than `120`.

- **Tempo only** ("set the tempo to 120") → `reaper.SetCurrentBPM(0, …, true)`, which sets
  the master tempo without injecting a tempo/time-sig marker.
- **Time signature** ("set it to 3/4", "6/8 at 100bpm") → reuses the tempo/time-sig marker at
  the project start if one exists, else inserts one. Any field you don't supply is read back
  from the project's current value (`TimeMap_GetTimeSigAtTime`), so "change to 3/4" keeps the
  existing tempo and vice versa.
- Returns a summary, e.g. `Set tempo to 100 BPM, 6/8`.

### Project state (`reaper_get_project` tool)

Read-only introspection (built in `build_project_info_lua`) for questions like "what tracks
are in the project?" or "what's the tempo?". Returns a **multi-line** summary (relayed via the
whole-file result read): project name, tempo + time signature, transport state, and the track
list with per-track name and `[armed]` flag. Example:

```
Project: Song.rpp
Tempo: 120 BPM, 4/4
Transport: stopped
Tracks: 2
  1. Vocals [armed]
  2. Guitar
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

- REAPER on macOS — the agent code is macOS-ready (`default_scripts_dir()` resolves the
  `~/Library/Application Support/REAPER/Scripts` path), but provisioning (install script + node
  setup + testing) is pending the Mac mini.
- Multi-REAPER instances (one per node, routed by node ID).

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
