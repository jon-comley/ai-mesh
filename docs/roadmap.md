# ai-mesh Roadmap

---

## Phase 6 — Model Scheduling ✓ Complete

- Model registry (`ModelAllocation` + `update_model_status`)
- Wire protocol messages (`ModelLoad`, `ModelUnload`, `ModelStatus`, `RequestModelInference`, `ModelInferenceResult`) with `wire_version` compatibility
- Allocation-aware scheduler: `select_node_for_model(mb)` (capacity) + `select_node_for_inference(name)` (Ready model)
- Connection routing map — per-connection `mpsc::Sender` registered on `Heartbeat`, purged on disconnect
- ModelLoad forwarding and agent-side `ModelStatus` replies
- CLI `mesh load`, `mesh nodes` Models column
- 54 tests across all four crates

---

## Phase 7 — Inference Routing ✓ Complete

- `RequestModelInference` routed through scheduler to selected agent
- Agent calls llama-server `POST /v1/chat/completions` (`stream: false`); returns real output, token count, duration
- `mesh infer <model-name> <prompt>` CLI command
- `ModelUnload` forwarding; agent reports `Unloaded`
- Oneshot channel per inference request; coordinator waits up to 300s for result
- `model_is_loading` registry query; coordinator polls for up to 300s before dispatching inference

---

## Phase 8 — Production Hardening ✓ Complete

- **Inference timeout tuning** — split into 300s pull-wait (Phase 1) + 120s generate (Phase 2); distinct error strings per phase
- **SQLite persistent registry** — `rusqlite` (bundled); `Registry::open(path)` for prod, `Registry::new()` (in-memory) for tests; state survives coordinator restarts
- **Pi (ARM64) compute node** — cross-compiled with `rustls-tls`; `just deploy-node pi1` fully self-provisioning
- **Beelink SER8 (Windows 11) compute node** — cross-compiled (`x86_64-pc-windows-gnu`); NSSM service; `sysinfo` crate for hardware detection (no child-process spawning); `just update-node beelink1` for OTA updates
- **Generic node provisioning** — `nodes/<name>.env` inventory; `deploy-node`, `update-node`, `uninstall-node` work for any Linux or Windows node without justfile changes
- **Agent reconnect loop** — graceful channel handling; retries TCP connection every 5s on disconnect
- **Cross-platform agent** — conditional compilation for hardware detection (Windows: `sysinfo`; Linux/macOS: `/proc`); hostname detection (Windows: `COMPUTERNAME`; Linux: `/etc/hostname`)

---

## Phase 8.5 — llama-server Migration ✓ Complete

- Replaced Ollama with llama-server (llama.cpp) across all nodes
- Agent downloads GGUF shards from Hugging Face on `ModelLoad`; no pre-caching during provisioning
- Inference switched to `POST /v1/chat/completions` with system + user message format
- `--flash-attn on` enabled; `LLAMA_GPU_LAYERS=99` offloads all layers to GPU where available
- Windows: Vulkan-enabled llama.cpp ZIP; AMD Radeon 780M at 29/29 GPU layers, 17.6 t/s (qwen2.5:7b)
- Linux: architecture-aware tarball download (x86_64 or ARM64)
- `just load-model <node> <model>` replaces `change-model`; `just update-llama <node>` for llama-server updates

---

## Lighting MVP ✓ Complete

- **Phase A — pi1 infrastructure**: Mosquitto 2.x (remote listener), Zigbee2MQTT with SLZB-06 PoE coordinator (192.168.1.16, EmberZNet 8.0.2 / EZSP v14, adapter `ember`), Z2M as systemd service
- **Phase B — `capability-zigbee` crate**: rumqttc 0.24 MQTT client; `ZigbeeClient::connect()` spawns EventLoop poll task internally; broadcast channel for `ZigbeeEvent` (StateChanged, DeviceListUpdated, GroupListUpdated, ConnectionLost, ConnectionRestored); `DeviceRegistry` parses `zigbee2mqtt/bridge/devices`; unit tests
- **Phase C — `capability-lighting` wired**: reads `MQTT_HOST`/`MQTT_PORT` from env; stubs gracefully when unset (tests pass); forwards `LightState` events back on the mesh tx channel; `handle(LightCommand)` publishes via `ZigbeeClient`
- **Phase D — end-to-end**: `just intent "turn test_bulb on/off"` → LLM tool call → MQTT → Zigbee → bulb responds; brightness (`50% → 127`) and colour temperature (`candlelight → 1500K`) working
- **Pairing**: `just pair-bulb` recipe; first Hue White and Color Ambiance B22 paired (IEEE `0x00178801024c077c`, renamed `test_bulb`)
- **Z2M groups**: `all` group created; `just intent "turn all bulbs off"` broadcasts to all members
- **Robustness fixes**: 5s reconnect delay (prevents Mosquitto storm); truncated JSON from 0.5b models repaired; empty target falls back to Group(1); node-id in MQTT client ID avoids same-ID collision

---

## Lighting — Device Awareness ✓ Complete

- **`MeshMessage::LightDeviceList`** — lighting node sends full device + group name list to coordinator on every MQTT connect; coordinator stores in registry
- **`bridge/groups` subscription** — Z2M groups (e.g. `all`) discovered automatically alongside devices
- **Re-subscribe on reconnect** — subscriptions re-issued in `ConnAck` handler so they survive Mosquitto restarts and network blips; Z2M retained topics re-deliver immediately
- **Coordinator registry persistence** — device/group lists stored in SQLite `light_devices` table; survive coordinator restarts; LLM has valid targets immediately after `just restart-coordinator` before pi1 reconnects
- **LLM system prompt injection** — known devices and groups listed in system prompt; LLM uses exact Z2M friendly names rather than guessing
- **Target validation** — `dispatch_tool` checks LLM-chosen target against known list before sending to MQTT; returns `unknown target 'x' — known targets: ...` on mismatch; skips validation when list is empty (fail-open)
- **Brightness clamped to 0–254** — Zigbee spec reserves 255; LLM-supplied values are clamped at dispatch
- **Z2M coordinator filtered** — `bridge/devices` entries with `type: Coordinator` excluded from device list regardless of whether `ieee_address` is present (newer Z2M versions include it)

---

## Phase 9 — Remaining Cluster Nodes (In Progress)

- **Mac mini M4** ⚠️ _hardware not available until ~end of July 2026_ — cross-compile for `aarch64-apple-darwin`, provision as compute node, add `just deploy-node mac1`
- **Multi-node routing validation** ✓ — `just validate-routing` confirms `qwen2.5:1.5b` → Pi and `qwen2.5:7b` → Beelink; `mesh infer` output now includes a `served-by:` line showing the serving node; load-balancing across identical-model nodes is a future concern when a second GPU node joins
- **`just start-cluster` recipe** ✓ — starts coordinator + controller + all remote agents, then calls `auto-load-model` on every compute node; leaves mesh in a ready-to-use inference state
- **`just auto-load-model <node>`** ✓ — SSHes into node, detects GPU VRAM or CPU RAM, selects best-fit model, loads it with hardware-filtered fallback hints
- **Automatic model placement (coordinator side)** ✓ — `ModelLoadRequest.node_id` is now `Option<String>`; when absent the coordinator calls `select_node_for_model(mb)` and picks the node with the most headroom; `mesh load <model> <size>` works without `--node-id`; `just load-model` still passes explicit node for predictable placement

---

## Lighting — Phase 2 ✓ Complete

- **State report debounce** — 75ms per-device `AbortHandle` debounce in the Z2M event loop; Z2M burst updates (state/brightness/colour temp) collapse to one `StateChanged` per action; map stores `AbortHandle` (not `JoinHandle`) so the task is cleanly detached
- **`LightingCapability` node_id** — capability now receives `node_id` at construction (from `build_capabilities`) instead of reading a missing env var; device list reports correctly keyed by pi1's persistent UUID in the coordinator registry

---

## Intent Routing — Phase 2 ✓ Complete

- **System prompt in correct role** — `InferenceRequest` carries `system_prompt: Option<String>`; intent handler sends the tool schema in the `system` role and the user text in the `user` role; previously both were concatenated into the user role, which instruction-following models (7b+) ignored
- **Special-tag suppression** — system prompt explicitly forbids `<tool_call>` and XML tags; Qwen 7b emits these when detecting schemas in the system prompt, which llama-server strips to empty — causing silent no-ops
- **Prefer largest model for intents** — `any_ready_llm_model` now selects the largest ready LLM by `size_mb` so intents always route to BEELINK1 7b over pi1 1.5b
- **Device name deduplication** — `all_light_device_names` uses `HashSet` to collapse duplicates; stale SQLite rows from old node UUIDs no longer show devices twice in the system prompt and target validation list
- **`temperature=0`, `max_tokens=128` for intents** — greedy decoding for deterministic JSON; 128-token cap prevents runaway generation on a short tool call response
- **`cache_prompt=true`** — llama-server reuses KV state for a stable system prompt; back-to-back intents skip prefill after the first, cutting latency noticeably
- **Compact schema JSON** — switched from pretty-printed to compact JSON in the system prompt; fewer prefill tokens on a cache miss
- **`just load <model>` recipe** — coordinator auto-placement without SSH; useful when a node is registered but SSH is unavailable (e.g. Windows node after network blip)

---

## Lighting — Deferred Improvements

- **Device availability tracking** — `zigbee2mqtt/+/availability` is subscribed but payloads (`{"state":"online"|"offline"}`) are not yet parsed. Add an `online: bool` field to `DeviceRegistry` entries, updated on each availability message. Two design choices to evaluate: (a) suppress offline devices from the LLM system prompt entirely so the model never attempts to control them; (b) include them but mark as `[offline]` so the model can report the device is unreachable. Option (a) is simpler but risks the LLM not knowing the device exists at all; option (b) gives better user feedback. Also affects target validation — an offline device should probably return a distinct error (`device 'test_bulb' is currently offline`) rather than the generic unknown-target error.
- **`bridge/event` subscription** — Subscribe to `zigbee2mqtt/bridge/event` for real-time device join/leave announcements. Currently `bridge/devices` (retained) covers discovery on connect, but live pairing events require this topic. Low priority until live-pair UX is needed.
- **ZigbeeClient lifecycle hardening** — `connect()` spawns one event loop task internally and is safe via `OnceCell`. If multiple lighting nodes or dynamic reconnect logic is added, consider an explicit `shutdown()` signal and replacing `OnceCell<Arc<ZigbeeClient>>` with `ArcSwap<ZigbeeClient>` to allow reconnect without structural changes.
- **Auto-placement structured error** — coordinator currently returns a silent `Acknowledge` when no node has sufficient headroom for a `ModelLoad`. Replace with a distinct `MeshMessage::Error { reason }` variant so the CLI can surface a clear "no node with enough VRAM" message instead of silently doing nothing.
- **Debounce map pruning** — completed debounce tasks leave a dead `AbortHandle` in the map until the same device fires again. Low impact (map stays small), but a periodic sweep or a completion callback could keep it clean in long-running deployments.
- **Distributed lighting capability** — the `capability-lighting` crate connects to an MQTT broker by address (`MQTT_HOST`/`MQTT_PORT`). Any node (e.g. beelink1) could run the lighting capability pointed at pi1's Mosquitto broker, saving the coordinator→pi1 LightCommand mesh hop when beelink1 is already serving the intent. Estimated saving: ~1–2ms LAN RTT. Not worth building until the Zigbee RF round-trip (~300–500ms) is no longer the dominant latency.
- **Zigbee latency ceiling** — the observed ~300–500ms bulb switch time is inherent to the Zigbee protocol (RF transmission + bulb processing + optional ACK). The SLZB-06 is network-attached (Ethernet, 192.168.1.16), so moving Z2M ownership to a different machine would not reduce this — the RF path is identical. Meaningful latency improvements would require: Zigbee direct binding (bypass Z2M for switch→bulb, not applicable to software commands), or migrating to Matter/Thread hardware (typically ~50–100ms).

---

## Phase 10 — Security & Auth ✓ Complete

- **TLS on coordinator TCP listener** — self-signed cert generated with `rcgen`, persisted at `~/.config/ai-mesh/coordinator.crt`; SHA-256 fingerprint logged on startup
- **TOFU fingerprint verification** — agents and CLI verify coordinator cert against `MESH_TLS_FINGERPRINT` env var; wrong fingerprint → hard connection failure; `MESH_INSECURE=1` escape hatch with loud warning
- **Node authentication** — `AuthToken` first-frame message; dual-token rotation (`MESH_AUTH_TOKEN` + `MESH_AUTH_TOKEN_NEXT`) for zero-downtime key rotation
- **Shared CLI connection helper** (`cli/src/connection.rs`) — TLS + auth extracted from all 10 commands
- **`just set-fingerprint <node>`** — reads fingerprint from coordinator log, pushes to node (systemd override on Linux, NSSM AppEnvironmentExtra on Windows); called automatically by `just restart-coordinator`
- **`just restart-coordinator`** auto-writes `MESH_TLS_FINGERPRINT` to `~/.bashrc` — no manual env var management on the controller machine
- **Linux nodes** — `install-node-linux.sh` grants passwordless `sudo tee` + `sudo systemctl` via `/etc/sudoers.d/ai-mesh-agent` so fingerprint pushes work non-interactively
- **Coordinator state file** ✓ — coordinator writes `~/.config/ai-mesh/coordinator.state` (shell-sourceable KEY=VALUE, `0600`) on startup with `MESH_TLS_FINGERPRINT` and `MESH_AUTH_TOKEN`; `set-fingerprint`, `set-auth-token`, and `restart-coordinator` source this file instead of grepping `/tmp/mesh-coordinator.log`, eliminating the log-rotation race condition
- **Per-message heartbeat auth token** ✓ — `HeartbeatPayload` carries `auth_token: Option<String>`; agent populates it from `MESH_AUTH_TOKEN`; coordinator rejects heartbeats with a missing or wrong token when auth is configured (defence-in-depth on top of connection-level `AuthToken` first-frame check)
- Signed wire messages (HMAC) ✓ — implemented as Phase 10.5 (`shared/src/frame.rs`, `SignedFrame` + HKDF key derivation)

### Phase 10 — Complete ✓

- **Auth token auto-distribution** ✓ — coordinator auto-generates `MESH_AUTH_TOKEN` on first run (no env var required); token is written to `coordinator.state`; `restart-coordinator` and `start-cluster` read the state file and push credentials to all compute nodes via `set-fingerprint` before starting agents; `deploy-node` also pushes credentials immediately when the coordinator is already running.

---

## Phase 10.5 — HMAC Message Signing (Defence-in-Depth) ✓ Complete

The existing TLS + token auth stops unauthenticated connections. HMAC goes one layer deeper: every wire message is signed with a shared secret so that even a rogue process with a valid token cannot forge arbitrary messages (e.g., a compromised agent cannot send a crafted `ModelLoad` to another node).

### Implementation

- **Signing key** — derived from `MESH_AUTH_TOKEN` via HKDF-SHA256 (label `"ai-mesh-hmac-v1"`); no new credential distribution needed.
- **Wire envelope** — `SignedFrame { ts: u64, payload: Vec<u8>, sig: Vec<u8> }` wraps every `MeshMessage` after the initial `AuthToken` handshake. The `AuthToken` first-frame is always sent unsigned (it IS the key establishment step).
- **Timestamp replay protection** — receiver rejects frames whose timestamp differs from now by more than 30 seconds.
- **All paths covered** — coordinator (reader + writer tasks), agent (reader task + writer loop), CLI (`send_recv`). HMAC is active whenever `MESH_AUTH_TOKEN` is configured; dev mode (no token) sends plain frames.
- **Protocol downgrade protection** — coordinator rejects plain `MeshMessage` JSON after auth (fails `from_slice::<SignedFrame>`); old agents fail fast with a clear error.
- **Key rotation** — inherits existing dual-token rotation; HMAC key re-derived from the active token.
- **Chaos validation** — `just chaos` fires 6 adversarial scenarios against the live coordinator (no-auth, wrong token, unsigned frame after auth, corrupted HMAC, stale timestamp, valid request sanity check); all must pass before `just validate-routing` proceeds.

---

## Phase 11 — Web Dashboard & Health Reporter (In Progress)

Full design spec: `plans/phase11-dashboard.md`

### Phase A — axum shell + PWA ✓ Complete

- `axum` 0.8 HTTP server embedded in coordinator, default port 9001 (`MESH_HTTP_PORT` to override)
- 6 tab panels: Nodes, Health, Models, Lighting, Security, Errors
- Mobile-first CSS with bottom tab bar; CSS grid desktop sidebar at ≥ 900 px
- `manifest.json` + service worker — installable as PWA today
- All static assets embedded via `include_str!` (single binary, zero runtime file I/O)
- `DashboardModule` trait in plan for per-capability panel extensibility

### Phase B ✓ Complete — WebSocket + live topology

- `/ws` WebSocket endpoint with `?token=` Bearer auth; `DashboardState` wraps a `tokio::sync::broadcast` channel
- `DashboardEvent::TopologyUpdate` pushed on every heartbeat from `process_message`
- `NodeDashInfo` fields: id, name, role, ip, last_seen_secs, health ("green" / "amber" / "red")
- Nodes panel in `topology.js` renders live node cards; health dot + role badge + IP + age
- 9 new unit tests: `auth_ok` logic, health colour thresholds, `push_topology` no-op, WS endpoint 400
- Chaos binary extended: scenario 7 verifies dashboard `/ws` returns 401 for a wrong token (plain TCP, no new deps)

### Phase C — Health timeline (In Progress)

- **C1 ✓** — Wire protocol: `cpu_usage_pct`, `ram_used_gb`, `ram_total_gb` added to `HeartbeatPayload`; `SetHeartbeatInterval { secs }` added to `MeshMessage`; backward-compat shims subsequently removed in C2
- **C2 ✓** — Agent `sysinfo` metrics: `refresh_cpu_usage()` + `refresh_memory()` on each heartbeat; `Arc<AtomicU64>` interval updated live when coordinator pushes `SetHeartbeatInterval`; backward compat removed (`Option<f32>` → `f32`; pre-C2 agents now fail fast); 278 tests
- **C3 ✓** — Coordinator `HealthStore` (`HashMap<node_id, VecDeque<HealthSample>>`, capped at 60); coordinator-stamped `ts_ms`; `push_health()` broadcasts `DashboardEvent::HealthUpdate` on every heartbeat; 288 tests
- **C4 ✓** — `POST /api/nodes/{id}/heartbeat-interval` HTTP endpoint; `NodeConnections` shared between TCP server and HTTP layer via `DashboardState`; `send_to_node()` uses `try_send` with `warn!` on full channel; 9 new unit tests; 297 tests passing
- **C5 ✓** — `health.js` ES module: SVG sparklines (CPU %, RAM %) per node in the Health panel; mini CPU sparkline in each Nodes-panel node card; "Set interval" button per node calls `POST /api/nodes/{id}/heartbeat-interval`; `get_all_health_snapshots()` on `DashboardState` pushes the full HealthStore to new WS clients on connect so sparklines populate immediately; `repaintAll()` refills mini sparklines after each `TopologyUpdate`; 5 new tests (point-in-time copy, sample values, single-sample, order, content-type); 304 tests passing
- **C7 ✓** — GPU metrics: `gpu_usage_pct`, `gpu_vram_used_gb`, `gpu_vram_total_gb` added to `HeartbeatPayload` as `Option<f32>` with `serde(default)` (CPU-only nodes omit them; pre-C7 agents remain compatible). New `agent/src/gpu.rs`: Linux reads amdgpu sysfs (`/sys/class/drm/card0/device/gpu_busy_percent` etc.); Windows reads GPU perf counters via PowerShell subprocess (no extra crates). `HealthSample` gains matching `Option<f32>` GPU fields; `push_health()` extended to 7 params. Dashboard `health.js` renders GPU% + VRAM sparklines beneath RAM row, hidden when all samples have `None` GPU data. Live-tested on beelink1 (AMD Radeon 780M): GPU% and VRAM visible. 315 tests passing.
- **C6 ✓** — `mesh set-heartbeat <node> <secs>` CLI command + `just set-heartbeat` recipe; health.js "Set interval" button shows current interval (`Set interval · Ns`)

### Phase D — Model management panel

- **D1 ✓** — `DashboardEvent::ModelUpdate { nodes: Vec<NodeModelInfo> }` added to `DashboardState`; `NodeModelInfo` + `ModelEntry` structs carry node metadata and per-model state string; `model_snapshot` ring stores latest state; `push_model_update()` always stores, broadcasts only when WS clients exist; `get_model_snapshot()` for point-in-time copies; snapshot pushed to new WS clients on connect (mirrors health snapshot-on-connect); coordinator patches `HardwareReport` + `ModelStatus` handlers to call `push_model_update(build_model_snapshot(&registry))`; 5 new tests. 329 tests.
- **D2 ✓** — `POST /api/models/load` + `POST /api/models/unload` HTTP endpoints in `coordinator/src/http/api.rs`; `gen_request_id()` generates `"http-{ms}"` request IDs; validates empty `node_id`, empty `model_name`, and `size_mb == 0` → 400; sends `MeshMessage::ModelLoad` / `MeshMessage::ModelUnload` via `send_to_node()`; routes registered in `mod.rs`; 10 new unit tests. 334 tests.
- **D3 ✓** — `models.js` ES module: per-node card with VRAM + RAM capacity bars (read from `getLatestSample()` in health.js), model rows with state badges (Ready/Loading/Failed — `.toLowerCase()` fix so badges colour correctly) and Unload button, "Load model…" button with prompt dialog; `dashboard.js` wired for `ModelUpdate` + `HealthUpdate` repaint; `style.css` extended with model card layout and drag-to-reorder styles; `/static/models.js` route added. Drag-to-reorder (HTML5 DnD, `localStorage` persistence, re-render guard during drag) added to Nodes, Health, and Models panels. `run-coordinator` recipe kills any stale process and sources the state file so the auth token is preserved on restart. 1 new content-type test. 335 tests.

### Phase E — Error feed + diagnostic panel

### Phase F — Lighting OS. Full spec in `plans/phase11f-lighting.md`.

#### The Vision

> Everyone else built a light switch you can control from your phone. We're building a lighting system that understands what's happening in your home — including inside your AI cluster — and responds to it.

**Room layout context:** 3×3 grid of lights. Bottom row: 3 white-temperature-only bulbs. Main grid: 8 colour spots (4×2 arrangement). This asymmetry shapes how scenes, effects, and games are designed — colour commands must gracefully degrade to brightness-only on the CT bulbs.

**The innovations:**
1. **Telemetry Lighting** — the room is the dashboard. GPU inference running → subtle blue pulse on the desk lamp. Node offline → brief red flash. Model loading → soft green fade. No commercial system has ever tied infrastructure telemetry to ambient lighting because no commercial system runs AI inference in your home.
2. **Intent-First Adaptive Scenes** — "cozy" at 7am means something different than at 10pm. Scenes are prompts, not presets. The LLM resolves the intent using time of day, room state, and known devices.
3. **Temporal Scene Composer** — scenes that exist across time. A morning routine defined as a drag-and-drop timeline (warm red → cool white over 40 minutes); coordinator executes transitions as a background task.
4. **Reactive Graph** — plain-language automation rules. *"If no motion for 5 minutes after sunset, fade to 10% over 2 minutes."* LLM parses each rule once into a structured `{ trigger, action }` pair; coordinator evaluates triggers on every event. No YAML, no Home Assistant complexity.
5. **Presence-Weighted Circadian** — not just "shift colour temp with the sun" but per-room based on occupancy. Motion sensor active in the study at midnight → keep it cool and bright. Room empty → start wind-down regardless of global schedule.
6. **Photo Colour Picker** — a grab-box tool in the dashboard: load a photo (or in future, a live screen capture), drag a selection box over any region, and the average colour of that region is extracted client-side via Canvas API and sent to the selected lights as CIE XY. Useful for matching artwork, a sunset photo, or the palette of whatever is on screen.
7. **Light Games** — the 4×2 colour spot grid is a natural game board. Ideas: Simon Says (flash a colour sequence on specific spots, granddaughter taps to repeat), Colour Match (LLM picks a "mystery" colour across two lights, guess which bulb matches a reference), Disco Party (choreographed random colour sweeps triggered by a "dance!" intent). These are creative uses of infrastructure we're already building.

**Future — Effects Engine (F9+):**
- **Solar / Sunset Engine** — coordinator uses device location (lat/lon) + the `sunrise` or `spa` Rust crate to derive exact sunrise/sunset times and solar elevation; drives a smooth colour-temperature curve matching the real sky throughout the day. Configurable per-group.
- **Hue Entertainment Fast Path** — MQTT round-trip (~100ms) is too slow for fluid effects. Philips' Entertainment API uses UDP streaming at ~50 Hz. A future `capability_effects` crate would open a UDP stream directly to the Zigbee coordinator (or a Hue bridge if one is ever added) for music sync, game sync, and reactive effects that feel instantaneous.
- **Screen / Monitor Sync** — screen capture (`scrap` or `xcap` Rust crate) → divide frame into regions → compute average colour per region → stream to the corresponding light in the grid. An Ambilight-style experience without the proprietary hardware. Requires the Hue Entertainment fast path (MQTT is too slow for 50 Hz updates).

---

- **F1 ✓** — Live state feed. `DashboardEvent::LightingUpdate { devices }` + `light_snapshot` in `DashboardState`; `push_lighting_update()` stores per-device and broadcasts; snapshot pushed to new WS clients on connect; `server.rs` `LightState` handler wired to dashboard; new `lighting.js` renders per-device cards (on/off badge, brightness bar %, colour temp in K, XY→RGB colour swatch, drag-to-reorder); `/static/lighting.js` route + test; `dashboard.js` wired for `LightingUpdate`. Three bugs fixed during live testing: Z2M publishes device state to base topic `zigbee2mqtt/<device>` not `/state` suffix (subscription corrected); Z2M bridge status filtered out; action events (`{"action":"toggle"}`) filtered to prevent ghost entries. 342 tests.
- **F2 ✓** — Individual device controls. `POST /api/lights/{device}/command` endpoint in `coordinator/src/http/api.rs`: `LightCommandBody { action, value?, x?, y? }` + `build_light_action()` dispatches on/off/toggle/brightness/color_temp/color_xy; `get_node_for_device()` added to `DashboardState` to resolve device→node routing; 404 for unknown device, 400 for malformed action, 503 if node not connected. `lighting.js` rewritten with interactive controls: toggle button (optimistic flip + re-render), brightness range slider (live label, send on release), colour temp slider (154–500 mireds range). Auth token read from `localStorage` and appended to all command requests. Slider drag bug fixed: `pointerdown` on input/button temporarily sets `draggable="false"` on the card. Error toast: non-2xx responses show a red pill notification above the tab bar for 4 s. 12 new tests (get_node_for_device ×2, light_command ×6, build_light_action_maps_all_variants, helpers). 354 tests.
- **F3 ✓** — Display polish. `formatDeviceName()` in `lighting.js` converts raw Z2M device IDs to title case (`test_bulb` → `Test Bulb`, `kitchen-light` → `Kitchen Light`). Node ID shown as a small muted badge beneath the device name so it's clear which physical node owns each device. Pure JS/CSS change — no Rust changes, 354 tests.
- **F4 ✓** — Colour picker. Swatch button in card header (shown for any bulb reporting `color_xy` or `color_temp`) toggles an inline picker: 12 px rainbow hue strip + saturation slider; `rgbToHsl()` initialises sliders from device's current XY state; `hslToXy()` converts back via Philips Wide Gamut D65 matrix (L fixed at 50% — brightness stays independent); CSS transition animates open/close. Also fixed startup visibility: on `ConnAck`, agent publishes to `zigbee2mqtt/bridge/request/devices`; on `bridge/devices` receipt, spawns GET requests to `zigbee2mqtt/{device}/get` for every discovered device — Z2M responds with current state immediately on connect. **Z2M config note:** set `mqtt.retain: true` in Z2M `configuration.yaml` for belt-and-braces (broker then holds last state per device topic; agent gets it on subscribe even before GET responses land). 354 tests (new logic is frontend JS + async MQTT wiring — not unit-testable without a mock broker).
- **F5** — Groups / Rooms. Wire Z2M groups as collapsible room cards in the dashboard. Group-level on/off, brightness, and scene selection. Dashboard shows `GroupListUpdated` names alongside device cards; `POST /api/lights/group/{name}/command` routes to the Z2M group topic (`zigbee2mqtt/{group}/set`). Colour commands degrade gracefully — CT bulbs in the group ignore XY fields; Z2M handles this transparently.
- **F6** — Scenes (basic). Snapshot current full state → save as a named scene → one-tap recall. `POST /api/scenes/save { name, group? }` snapshots the current `LightSnapshot`; `POST /api/scenes/recall { name }` replays each device command in sequence. Scenes stored as JSON in the coordinator state file (no new DB table). Dashboard: "Save scene" button per group card, scene list with recall and delete.
- **F7** — Switches / Input Devices. New `capability_switches` crate. Subscribes to Z2M button, remote, motion sensor, and contact sensor events. Emits `SwitchEvent { device_id, action }` to coordinator; coordinator broadcasts `DashboardEvent::SwitchEvent`. Dashboard: live switch activity feed. Foundation for the Reactive Graph (switch event → trigger evaluation).
- **F8** — Creative Features. See *The Innovations* above. Delivery order within F8: (1) Telemetry Lighting (lowest lift — watches existing mesh events); (2) Photo Colour Picker (client-side Canvas API, no new Rust); (3) Intent-First Scenes (routes through existing intent handler); (4) Circadian/Sunset Engine (solar position crate + background task); (5) Temporal Composer (timeline UI + coordinator scheduler); (6) Reactive Graph (LLM rule parser + trigger evaluator); (7) 2D Room Layout (SVG drag canvas, `localStorage` persistence). **Room layout note:** the existing 3×3 grid (3 CT-only + 8 colour spots) should inform the default layout template.
- **F9** — Light Games. Interactive games designed around the 4×2 colour spot grid. MVP: **Simon Says** — coordinator generates a random colour sequence, flashes each spot in turn, player must repeat the sequence via the dashboard (or physical switch events from F7). Stretch: **Colour Match**, **Disco Party** (intent-triggered choreography). LLM involvement optional — game logic can be pure Rust for determinism and speed.
- **F10** — Effects Engine. Prerequisite: investigate whether SLZB-06 + Z2M supports Zigbee direct binding or a lower-latency command path for ≥10 Hz updates. If yes: `capability_effects` crate with UDP/low-latency path. If no: evaluate adding a Hue Bridge as a parallel light controller (Entertainment API at 50 Hz). Screen sync deferred until the fast path exists.

### Phase G — Security panel

### Phase H — Polish, icons, desktop layout pass

### Deferred dashboard polish (raised post-C5)

These are non-blocking UX improvements for after C6 ships:

- **RAM% / CPU% colour thresholds** — colour the metric-value text amber (> 75%) or red (> 90%) so overloaded nodes stand out without reading the number.
- **Sparkline tooltip with exact timestamp** — on `mouseover` a `<title>` element or a floating tooltip shows `cpu: X.X%  ram: Y.Y%  at HH:MM:SS` for the hovered data point; helps operators correlate a spike with a specific inference request.
- **Per-node collapse/expand in Health panel** — each health card has a `▾ / ▸` toggle; collapsed cards show only the last value, not the full sparkline; useful when the cluster grows beyond 4–5 nodes and the panel gets long.
- **Sparkline fill area** — shade the area under the CPU sparkline with a low-opacity accent fill for faster visual triage.

### Deferred chaos / QA scenarios (raised post-Phase B)

These are not blocking but should be added to the chaos binary before Phase 11 ships as complete:

- **WS auth edge cases** — token rotation mid-session (valid token replaced; existing WS connections should remain alive until they close naturally), simultaneous connect with old and new token during rotation window
- **Lagged broadcast receiver** — connect N WS clients, then flood the coordinator with heartbeats faster than clients can consume; verify no panics, no disconnects, just the `Lagged(n)` path in `ws.rs` firing and clients catching back up cleanly
- **Phase B chaos coverage audit** — walk every branch in `ws.rs` and `state.rs` and confirm each has either a unit test or a chaos scenario; the `Channel closed` arm (coordinator shutdown mid-session) is the main gap

---

A lightweight web interface embedded in the coordinator process (no separate service). Primary goal: **observable mesh** — operators can see the state of the cluster at a glance and drill into errors without SSHing into nodes.

### Core views

- **Topology** — live graph of connected nodes, their model assignments, and heartbeat latency; nodes colour-coded (green / amber / red) by health status.
- **Health timeline** — time-series strip per node showing CPU %, GPU %, RAM %, heartbeat jitter. Data sourced from the existing heartbeat `HardwareStatus` payloads; stored in a small ring buffer in the coordinator (no extra DB writes for MVP).
- **Error feed** — structured error log; whenever a node reports an error (inference failure, model load failure, Zigbee disconnect, etc.) an entry is created with: timestamp, node, error kind, severity.
- **Diagnostic panel** — clicking an error entry triggers the coordinator to request a log snapshot from the affected node (`DiagRequest` mesh message); the node responds with its last N lines of stderr/stdout and the web UI renders them inline. This avoids the operator having to SSH just to read a log.

### Model management UI

- Load / unload models on any ready node via button; mirrors `mesh load` / `mesh unload` CLI.
- Node capacity bars (VRAM / RAM) update live so the operator can see headroom before choosing a model.

### Tech choices (tentative)

- **HTTP server** — `axum` (already familiar in the Rust ecosystem; lightweight); single binary, no Node.js build step.
- **Real-time updates** — WebSocket endpoint (`/ws`) that streams `DashboardEvent` JSON; front-end subscribes and patches the DOM.
- **Front-end** — plain HTML + vanilla JS (no framework) for MVP; keeps build complexity at zero. A Svelte/React layer can be added later if the UI grows.
- **Auth** — dashboard protected by the same `MESH_AUTH_TOKEN`; pass as a Bearer token in the WebSocket upgrade or a session cookie set on first visit.

### Security panel (HMAC failure metrics)

Surface authentication anomalies in real time — the error feed alone is not enough once the cluster grows beyond two nodes.

**Coordinator-side counters (in-memory, per peer socket address):**

```rust
struct PeerSecurityStats {
    invalid_signature: u64,   // HMAC mismatch — bad key or tampering
    stale_frame: u64,          // timestamp skew > 30 s — likely clock drift
    downgrade_attempt: u64,    // plain MeshMessage JSON after auth — old binary or probe
    last_event_ts: u64,        // Unix seconds of most-recent incident
}
```

Keyed by `SocketAddr`; evicted when the connection closes. Incremented in the existing `FrameVerifyError` match arms in `coordinator/src/server.rs`.

**New wire messages (future):**

- `AdminMessage::RequestSecurityMetrics` → `SecurityMetrics(Vec<PeerReport>)` — CLI/dashboard polls on demand.
- `DashboardEvent::SecurityIncident { peer, kind, ts }` — pushed over the WebSocket on every HMAC rejection so the dashboard error feed updates without polling.

**Dashboard security panel:**

- Table: peer IP, node name (if registered), failure kind, count, last seen.
- Stale-frame rows highlighted amber with a "check NTP" tooltip.
- Downgrade-attempt rows highlighted red — indicates a node running an old binary or an active probe.
- Row clears automatically when the peer's connection closes cleanly (expected reconnect) or stays for the session if the connection was forcibly dropped (anomaly).

**CLI:**

```
mesh security-report        # one-shot snapshot of current failure counts
```

### Deferred / stretch

- Alert rules (e.g., node offline > 60s → webhook / email).
- Historical inference latency per model.
- Live intent log (query, routed-to node, latency, response preview).
- Mobile-friendly layout.

---

## Phase 12 — Distributed Execution

- Multi-node inference
- Pipeline parallelism
- Tensor parallelism

---

## Phase 13 — Auto-scaling

- Dynamic node joining
- Cloud integration
