# ai-mesh Roadmap

---

## Infra note — the mesh router network migration (2026-06-25)

Home network migrated from the ISP router to a mesh router; subnet `192.168.1.x` → `10.0.0.x` (pi1 `10.0.0.10`, beelink1 `10.0.0.11`, SLZB-06 `10.0.0.12`). `nodes/*.env`, `justfile`, `README.md`, and `handover.md` updated. Follow-ups: set the mesh router DHCP reservations (leases still dynamic); re-verify beelink BIOS Pluton/fTPM golden state (crash storm regressed during the move — `docs/windows-node-setup.md`). beelink also needed Smart App Control disabled to run the self-built agent after its earlier Windows reinstall.

**Follow-up (2026-06-29) — z2m crash-loop from a missed stale IP; all lights dead.** The migration updated runtime config but **not** zigbee2mqtt's `configuration.yaml` on pi1: its `serial.port` still pointed at the SLZB-06's old `tcp://192.168.1.16:6638`. Z2M failed with `connect EHOSTUNREACH` → `Failed to start EZSP layer (HOST_FATAL_ERROR)` and crash-looped every ~6 s, so the Zigbee bridge was down and every light command published into MQTT and vanished — with **no error on the dashboard** (the coordinator's lighting capability was happily connected to Mosquitto on `127.0.0.1`; the break was downstream at z2m↔radio). *Fixed:* repointed `serial.port` → `tcp://10.0.0.12:6638` (backup saved alongside) and restarted z2m — it reconnected (`Coordinator firmware … EmberZNet 8.0.2`, `9 devices joined`, `Connected to MQTT`); a test command round-tripped (`test_bulb` off/on → 204). **Root-cause prevention: set the mesh router DHCP reservation for the SLZB-06** (`10.0.0.12`) — z2m hard-codes it, so a lease change re-breaks this. Repo-side, the stale `192.168.1.x` sweep found no runtime config affected (`nodes/*.env`/`justfile` were already correct); only **docs** lagged — `docs/pi1-lighting-setup.md` (the z2m config source), `docs/{commands,mesh,reaper}.md` updated inline, `docs/windows-node-setup.md` got the historical-IP disclaimer. Plan docs + in-code test fixtures left as historical.

---

## Code Audit — Findings to Action (2026-06-02)

Whole-codebase adversarial bug audit (all Rust crates + frontend JS, partitioned
into 15 review units; each finding independently verified by a skeptic pass).
**15 confirmed findings: 2 critical, 3 high, 8 medium, 2 low — all fixed (2026-06-02).**
(The original tally said 16/4-high; reconciled to the 15 items actually listed below.)
`shared`, `cli`, `capabilities`, `coord-effects-b`, and `js-misc` came back clean.

### 🔴 Critical — unauthenticated DoS (do first)

> Both fixed via a single shared `shared::frame::read_bounded_frame()` (length
> checked against `MAX_FRAME_LEN` before allocation) — the one framed-read path
> for coordinator + agent (incl. test helpers), so the bound can't be forgotten.
> Covered by unit tests in `shared/src/frame.rs`.
> _Deferred follow-up (Bing review):_ log the peer address on an oversized frame
> for forensics — needs threading `SocketAddr` through `handle_connection` (the
> accept site currently drops it); low value, not done.

- [x] **Unbounded allocation from untrusted length prefix — coordinator** (`coordinator/src/server.rs:133-134` auth phase, `:198-199` main loop). A 4-byte length prefix is read and `vec![0u8; msg_len]` allocated with no bound — `0xFFFFFFFF` → 4 GB alloc → OOM crash. The auth-phase one runs **before** token validation (unauthenticated). *Fix:* add `MAX_MSG_SIZE` (e.g. 64–100 MB), reject oversized lengths before allocating. Apply to all length reads (incl. test helpers at `:752-753`, `:1004-1005`).
- [x] **Same unbounded allocation — agent** (`agent/src/main.rs:151-152`). A malicious/compromised coordinator can OOM-crash the agent the same way (length read before HMAC verify). *Fix:* same `MAX_MSG_SIZE` guard. **(Single shared fix: factor a bounded frame-read helper used by both sides.)**

### 🟠 High

- [x] **TOCTOU in `rename_device`** (`coordinator/src/http/api.rs:880-882`). Lock acquired/released separately for `set_device_name`, then `get_all_device_names`, then `rooms_from_registry` — a concurrent handler can interleave, returning state that never existed atomically. *Fix:* one lock scope spanning the write + both reads.
- [x] **Commit error silently swallowed in `reorder_room_devices`** (`coordinator/src/registry.rs:1105-1116`). `let _ = tx.commit()` and per-row `let _ =` drop all errors; HTTP handler always returns 204. UI shows success while DB keeps old order. *Fix:* check/log commit + update errors (sibling fns already `warn!`).
- [x] **No transaction in `set_room_positions`** (`coordinator/src/registry.rs:1128-1135`). Loop of UPDATEs in autocommit; a crash mid-loop leaves gaps/dupes in room ordering. *Fix:* wrap in a transaction (as `reorder_room_devices`/`set_active_effect` already do).

### 🟡 Medium

- [x] **Windows CPU cores = threads** (`agent/src/hardware.rs:34-36`). `cpu_cores = cpu_threads` over-reports physical cores on hyperthreaded Windows (4c/8t → reports 8 cores), inflating scheduling capacity. *Fix:* use `sysinfo::System::physical_core_count()`.
- [x] **Partial scene apply returns success** (`coordinator/src/http/api.rs:776,790,805` vs `:817`). color_xy/color_temp/brightness send failures are `let _`-discarded; only the final on/off sets `any_unavailable`, so a partially-applied scene still returns 204. *Fix:* track all four sends.
- [x] **Optimistic snapshot not rolled back on send failure** (`coordinator/src/http/api.rs:217,223-227`). Snapshot updated before send; on send-fail returns 503 but leaves mutated state, which a later broadcast pushes to clients. *Fix:* roll back on failure, or update only after send succeeds.
- [x] **WebSocket recv-error busy loop** (`coordinator/src/http/ws.rs:158-162`). `Some(Err(_))` falls into the `_ => {}` arm; a persistent socket error spins `recv()` at 100% CPU. *Fix:* break on `Some(Err(_))` like the send path does.
- [x] **Effect `tick()`/`on_handoff()` run while holding runner-state lock** (`coordinator/src/effects/runner.rs`). A panicking/slow effect poisoned/held the lock and froze the runner + HTTP handlers. *Fixed (2026-06-02):* `tick_one` now `Option::take`s the effect out of the instance under the lock, runs `tick()`/`on_handoff()`/`serialize_internal_state()` **outside** the lock, then re-locks to apply schedule + drift bookkeeping and put the effect back — discarding the output if the instance was cleared or its effect replaced mid-tick. `ActiveEffectInstance` gained a cached `effect_id` so concurrent lock-holders never touch the (transiently absent) boxed effect. All effect calls are now off the lock; 104 effects + 14 runner tests green.
- [x] **Aurora seed can be ≥ 1.0** (`coordinator/src/effects/aurora.rs:64`). `raw / u32::MAX` in f32 can round to ≥ 1.0 near `u32::MAX`, breaking the documented `[0,1)` phase (~1 in 14M). *Fix:* divide by `4294967296.0`.
- [x] **color_temp divide-by-zero → inf cast** (`coordinator/src/intent.rs:307`). Untrusted `value:0` → `1e6/0 = inf` cast to u16 = 65535 (nonsensical command; not UB but bad input accepted). *Fix:* clamp/validate Kelvin range before dividing.
- [x] **Document pointerdown listener leak in light popover** (`coordinator/src/http/static/rooms.js:847`). If a card is re-rendered (`render()` → `innerHTML=''`) while a temp/colour section is open, `_lcOpenDismiss`/`lcOutside` is never disarmed; listeners accumulate. *Fix:* guard `render()` against open popovers, or disarm on detach.

### 🟢 Low

- [x] **`updateOpeningCone(id)` ignores its arg** (`coordinator/src/http/static/layout.js:3060`). Callers pass an id; definition takes none (redraws all). Harmless but misleading API. *Fix:* drop the param or use it.
- [x] **Popover scroll-offset inconsistency** (`coordinator/src/http/static/layout.js`). The audit flagged the *bulb* popover for omitting `window.scrollX/scrollY` — but `.layout-popover` is `position: fixed`, so viewport coords (getScreenCTM) are correct and the bulb popover was right. The real (inverted) bug was the *opening* popover (`:3620-3621`) **adding** scroll offsets to a fixed element, mis-positioning it when scrolled. *Fixed:* removed the scroll offsets from the opening popover to match the bulb one.

> _Operational finding (2026-06-02, not part of the 16-bug code audit):_
- [x] **Live pi1 DB retains the purged `rooms.solar_enabled` column** — F-Effects-2 dropped `rooms.solar_enabled` (and `light_states.solar_enabled`) from the schema, but pi1's live `/var/lib/ai-mesh/ai_mesh.db` still has `rooms.solar_enabled` (verified 2026-06-02). *Fixed (2026-06-05):* Surgically dropped `solar_enabled` from both `rooms` and `light_states` via Python/SQLite on pi1; coordinator restarted and healthy.
- [x] **beelink1 (SER8) stability — fTPM DPC_WATCHDOG storm regressed (2026-06-02)** *(Resolved 2026-06-11: BIOS work completed — fTPM re-disabled, AC power-loss recovery set, WoL disabled; node stable.)* — the `0x00000133`/`param2=0x1e00` fTPM crash signature that was fixed on 2026-05-28 has returned (5 minidumps since 2026-06-01 22:17; full writeup in `docs/windows-node-setup.md` → 2026-06-02 entry). fTPM appears to have re-enabled itself when BIOS defaults were restored (power event / suspected weak CMOS battery). Plus a household power cut from which beelink did **not** auto-recover (down ~56 min until physical reset). Node is up and serving `qwen2.5:7b` again but will storm again until fTPM is genuinely off. **Actions:** (1) re-disable fTPM in BIOS — Advanced → SOC Misc Control → Trusted Platform Modules → dTPM Level 3 without Pluton Security Processor (and Pluton Security Processor → Disabled); (2) set BIOS "Restore on AC Power Loss = Power On" + check CMOS battery; (3) longer-term, rebuild beelink on a leaner OS (Win IoT Enterprise LTSC) or move it to Linux to escape the Windows/AMD-PSP class entirely. Pi1/coordinator/lighting unaffected throughout.

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
- `--flash-attn auto` (llama.cpp picks per model — forcing `on` hangs Gemma-3 on Vulkan); `LLAMA_GPU_LAYERS=99` offloads all layers to GPU where available
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

- **Device availability tracking** ✓ (now working end-to-end, see **F-Lighting-UX**) — per-device `online` flag flows through to the dashboard (offline cards disabled; placeholders start offline) and a bridge-level `ZigbeeStatus` drives the offline banner. *2026-06-29:* this had been silently inert — z2m's `availability` feature was disabled (so offline was never published), and the agent's `<device>/get` poll re-stamped offline bulbs as `online: true` from z2m's cached state on every reconnect. Fixed by enabling `availability.enabled: true` in z2m and stamping state reports from a per-device availability map in `capabilities/zigbee`. **Still deferred (LLM side):** suppressing/marking offline devices in the LLM system prompt, and a distinct `device 'x' is currently offline` validation error instead of the generic unknown-target error.
- **`bridge/event` subscription** — Subscribe to `zigbee2mqtt/bridge/event` for real-time device join/leave announcements. Currently `bridge/devices` (retained) covers discovery on connect, but live pairing events require this topic. Low priority until live-pair UX is needed.
- **ZigbeeClient lifecycle hardening** — `connect()` spawns one event loop task internally and is safe via `OnceCell`. If multiple lighting nodes or dynamic reconnect logic is added, consider an explicit `shutdown()` signal and replacing `OnceCell<Arc<ZigbeeClient>>` with `ArcSwap<ZigbeeClient>` to allow reconnect without structural changes.
- ~~**Bounded Manual State Cache**~~ — obsolete: `last_manual_states` was deleted entirely in the F-Effects-2 legacy purge; nothing to bound.
- **Solar Dashboard Widget** — Add a compass and elevation widget to the Topology view to visualize the real-time solar vector.
- **Debounce map pruning** — completed debounce tasks leave a dead `AbortHandle` in the map until the same device fires again. Low impact (map stays small), but a periodic sweep or a completion callback could keep it clean in long-running deployments.
- **Distributed lighting capability** — the `capability-lighting` crate connects to an MQTT broker by address (`MQTT_HOST`/`MQTT_PORT`). Any node (e.g. beelink1) could run the lighting capability pointed at pi1's Mosquitto broker, saving the coordinator→pi1 LightCommand mesh hop when beelink1 is already serving the intent. Estimated saving: ~1–2ms LAN RTT. Not worth building until the Zigbee RF round-trip (~300–500ms) is no longer the dominant latency.
- **Room temperature slider — match the light-card style** — the room-level temperature slider (in the room card's colour/temp panel) should use the same warm→cool gradient temp-bar widget as the individual device cards (`buildTempBar` in `lightcontrols.js`), rather than the generic `buildSlider`. Aligns the room and device controls visually and in behaviour, consistent with how the room brightness slider already mirrors the device brightness slider.

- **Bulb power-cycle detection + UI indication** — when a bulb is physically switched off and back on at the wall, it reverts to its factory/last-power-on state (typically warm white full brightness), overriding any active effect or scene. The coordinator needs to detect this transition (a bulb reporting `on:true` with default state after having been `on:false` or `online:false`) and: (1) surface it visually on the device card (e.g. a "reverted" indicator or distinct icon state); (2) optionally re-apply the active effect/scene to that bulb automatically. Detection signal is already available via the `LightingUpdate` WS path — needs a heuristic to distinguish a genuine power-cycle revert from a normal user-on command.

- **Zigbee latency ceiling** — the observed ~300–500ms bulb switch time is inherent to the Zigbee protocol (RF transmission + bulb processing + optional ACK). The SLZB-06 is network-attached (Ethernet, 192.168.1.16), so moving Z2M ownership to a different machine would not reduce this — the RF path is identical. Meaningful latency improvements would require: Zigbee direct binding (bypass Z2M for switch→bulb, not applicable to software commands), or migrating to Matter/Thread hardware (typically ~50–100ms).

- **Remove the dead device-card view in `lighting.js`** — `dashboard.js` calls `setRoomsActive()` unconditionally at startup with no reset, so `roomsActive` is permanently true and `lighting.js`'s device-card `render()` (and its sibling slider-patch path) always bails: it's unreachable. The lighting tab is exclusively the rooms view (`rooms.js`). Cleanup: delete the dead `render()`/device-card builders and the `roomsActive` flag from `lighting.js`, keeping only what the rooms view still imports (e.g. `buildLightControls`, `formatDeviceName`), and drop the now-unused `.light-card`-only grid styling. Surfaced while fixing the maximised rooms-view layout (the `#lighting-list` card grid only ever applied to this dead path).

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
7. **Light Games** — the 4×2 colour spot grid is a natural game board. Simon Says, Whack-a-Mole (yellow flash on a random bulb, tap before it goes out — speeds up each hit), Colour Match, Hot/Cold (motion-sensor treasure hunt), Disco Party, Strobe/Rave mode. Full game catalogue in F9.

**Future — Effects Engine (F9+):**
- **Solar / Sunset Engine** — coordinator uses device location (lat/lon) + the `sunrise` or `spa` Rust crate to derive exact sunrise/sunset times and solar elevation; drives a smooth colour-temperature curve matching the real sky throughout the day. Configurable per-group.
- **Hue Entertainment Fast Path** — MQTT round-trip (~100ms) is too slow for fluid effects. Philips' Entertainment API uses UDP streaming at ~50 Hz. A future `capability_effects` crate would open a UDP stream directly to the Zigbee coordinator (or a Hue bridge if one is ever added) for music sync, game sync, and reactive effects that feel instantaneous.
- **Screen / Monitor Sync** — screen capture (`scrap` or `xcap` Rust crate) → divide frame into regions → compute average colour per region → stream to the corresponding light in the grid. An Ambilight-style experience without the proprietary hardware. Requires the Hue Entertainment fast path (MQTT is too slow for 50 Hz updates).

---

- **F1 ✓** — Live state feed. `DashboardEvent::LightingUpdate { devices }` + `light_snapshot` in `DashboardState`; `push_lighting_update()` stores per-device and broadcasts; snapshot pushed to new WS clients on connect; `server.rs` `LightState` handler wired to dashboard; new `lighting.js` renders per-device cards (on/off badge, brightness bar %, colour temp in K, XY→RGB colour swatch, drag-to-reorder); `/static/lighting.js` route + test; `dashboard.js` wired for `LightingUpdate`. Three bugs fixed during live testing: Z2M publishes device state to base topic `zigbee2mqtt/<device>` not `/state` suffix (subscription corrected); Z2M bridge status filtered out; action events (`{"action":"toggle"}`) filtered to prevent ghost entries. 342 tests.
- **F2 ✓** — Individual device controls. `POST /api/lights/{device}/command` endpoint in `coordinator/src/http/api.rs`: `LightCommandBody { action, value?, x?, y? }` + `build_light_action()` dispatches on/off/toggle/brightness/color_temp/color_xy; `get_node_for_device()` added to `DashboardState` to resolve device→node routing; 404 for unknown device, 400 for malformed action, 503 if node not connected. `lighting.js` rewritten with interactive controls: toggle button (optimistic flip + re-render), brightness range slider (live label, send on release), colour temp slider (154–500 mireds range). Auth token read from `localStorage` and appended to all command requests. Slider drag bug fixed: `pointerdown` on input/button temporarily sets `draggable="false"` on the card. Error toast: non-2xx responses show a red pill notification above the tab bar for 4 s. 12 new tests (get_node_for_device ×2, light_command ×6, build_light_action_maps_all_variants, helpers). 354 tests.
- **F3 ✓** — Display polish. `formatDeviceName()` in `lighting.js` converts raw Z2M device IDs to title case (`test_bulb` → `Test Bulb`, `kitchen-light` → `Kitchen Light`). Node ID shown as a small muted badge beneath the device name so it's clear which physical node owns each device. Pure JS/CSS change — no Rust changes, 354 tests.
- **F4 ✓** — Colour picker. Swatch button in card header (shown for any bulb reporting `color_xy` or `color_temp`) toggles an inline picker: 12 px rainbow hue strip + saturation slider; `rgbToHsl()` initialises sliders from device's current XY state; `hslToXy()` converts back via Philips Wide Gamut D65 matrix (L fixed at 50% — brightness stays independent); CSS transition animates open/close. Also fixed startup visibility: on `ConnAck`, agent publishes to `zigbee2mqtt/bridge/request/devices`; on `bridge/devices` receipt, spawns GET requests to `zigbee2mqtt/{device}/get` for every discovered device — Z2M responds with current state immediately on connect. **Z2M config note:** set `mqtt.retain: true` in Z2M `configuration.yaml` for belt-and-braces (broker then holds last state per device topic; agent gets it on subscribe even before GET responses land). 354 tests (new logic is frontend JS + async MQTT wiring — not unit-testable without a mock broker).
- **F5 ✓** — Z2M group routing foundation. `LightTarget::Group(u16)` → `LightTarget::Group(String)` (friendly name), fixing the Z2M MQTT topic from `zigbee2mqtt/group_1/set` → `zigbee2mqtt/all/set`. `group_snapshot` added to `DashboardState`; `push_group_update` + `get_node_for_group` wired; `LightingUpdate` extended with `groups: Vec<String>`. `POST /api/lights/group/{name}/command` endpoint active for intent routing. Group cards shown in dashboard (to be superseded by F-Rooms-3 UI). 363 tests.
- **F-Rooms** — First-class user-managed rooms. Full spec: `plans/phase11f-rooms.md`. Rooms are stored in the coordinator's own SQLite — independent of Z2M groups, portable across device types. Drag-and-drop UX: devices live in an "Unassigned" strip until dragged into a room card. Room cards have on/off, brightness, CT controls that fan out to all member devices. This is the UI Hue should have built.
  - **F-Rooms-1 ✓** — SQLite schema (`rooms`, `room_devices` tables with `ON DELETE CASCADE`), `PRAGMA foreign_keys = ON`, `RoomRecord` + 8 registry CRUD methods (`list_rooms`, `create_room`, `room_exists`, `delete_room`, `rename_room`, `add_device_to_room`, `remove_device_from_room`, `get_room_for_device`), `RoomsUpdate` WS event + `RoomInfo` struct, `room_snapshot` in `DashboardState`, `push_rooms_update` / `get_room_snapshot`, snapshot-on-connect in `ws.rs`, `warm_start_rooms` populates snapshot from SQLite on coordinator start. 378 tests.
  - **F-Rooms-2 ✓** — HTTP API: `POST /api/rooms` (201 + id), `DELETE /api/rooms/{id}` (204/404), `PATCH /api/rooms/{id}/name` (204/400/404), `PATCH /api/rooms/{id}/devices { add, remove }` (204/404), `POST /api/rooms/{id}/command` fan-out (204/400/404/503). Registry passed to router via Axum `Extension`; each mutating endpoint calls `push_rooms_update` after the DB write so WS clients see live changes. 20 new tests; 398 tests total.
  - **F-Rooms-3 ✓** — `rooms.js` ES module. `RoomsUpdate` drives full re-render; `notifyDevices` called from `dashboard.js` on every `LightingUpdate` to keep chip state current. Layout: `[ + New Room ]` inline input at top; **Unassigned** strip (draggable chips for all devices not in a room, with "All devices assigned" / "No lighting devices" placeholders); **room cards** (name, on/off buttons, brightness + CT sliders shown only when room has capable devices, member chips with `✕` remove, drop-zone highlight). Drag-and-drop: `dragstart` on chip records `{ deviceId, fromRoomId }`, drop on room card calls `addDeviceToRoom` (server evicts atomically from old room), drop on Unassigned calls `removeDeviceFromRoom`. Inline rename: click name or rename button → `<input>` in place, Enter/blur confirms, Escape cancels. `lighting.js` flat list suppressed via `setRoomsActive()` export. `/static/rooms.js` route + content-type test added to `mod.rs`. 399 tests.
  - **F-Rooms-4 ✓** — Drag-to-reorder rooms (HTML5 DnD on `.room-card`, `position` column, `POST /api/rooms/reorder`); room-level colour picker (XY → CIE xy via HSL math); full device cards (brightness + CT sliders + colour picker + toggle) inside room cards replacing compact chips; device cards draggable out of rooms (`e.stopPropagation` + pointer guard); optimistic on/off update with 503 toast; on/off disabled for empty rooms. 404 tests.
- **F6 ✓** — Scenes (basic). `scenes` SQLite table (id, name, room_id FK → ON DELETE CASCADE, created_at ms, states_json). `DeviceSnapshot` + `SceneRecord` types in registry; `SceneInfo` + `ScenesUpdate` in state.rs; snapshot-on-connect in ws.rs; `warm_start_scenes` in coordinator.rs. API: `POST /api/scenes { name, room_id? }` (201), `POST /api/scenes/{id}/recall` (204/404/503), `DELETE /api/scenes/{id}` (204/404). Recall uses live `get_node_for_device` (robust to node reconnects); fans out color/brightness/on-off per device. UI: "Save scene" inline input per room card; scene list with Recall + ✕ delete; `handleScenesUpdate` export; `ScenesUpdate` routed in dashboard.js. 433 tests.
- **F-Quality ✓** — Lighting stability, colour intents, and operational reliability.
  - **Light state persistence** — `LightStateReport` upserted to SQLite `light_states` table (INSERT OR REPLACE) on every Z2M state change; `warm_start_lighting` loads all rows on coordinator boot so dashboard indicators are never stale after a restart. 5 new tests.
  - **`BrightnessTransition` variant** — new `LightAction::BrightnessTransition { value: u8, transition_secs: f32 }` carries Z2M's native `transition` field through the stack; bulb hardware interpolates brightness smoothly. `capability-zigbee` emits `{"brightness": v, "transition": t}`.
  - **Drag-to-identify pulse** — replaces the ID button. On `dragstart`, `startPulse` sets the bulb to full brightness at candle temperature (500 mireds ≈ 2000 K) then runs a cosine ease-in/out breathing cycle (80–254 brightness, 700 ms steps, 0.6 s hardware transition each step). On `dragend`, `stopPulse` restores the exact pre-grab state (colour\_xy or colour\_temp, then brightness).
  - **Colour intents (initial)** — `light_command` tool schema gains a `"color"` action; `build_light_command` routes it to `LightAction::ColorXY`. `just intent "make test bulb green"` now sends `ColorXY` instead of the old `color_temp: 7000`. (Colour encoding later superseded — see F-Colour below.)
  - **Mesh token stability** — `run-coordinator` and `restart-coordinator` source `coordinator.state` and export `MESH_AUTH_TOKEN` **before** launching the coordinator subprocess, so the same token survives restarts without requiring `source ~/.bashrc` afterwards.
  - **449 tests** across all crates.

- **F-Spatial ✓** — Solar/Spatial Lighting Engine. Coordinator-side background task (`SpatialEngine`) that computes real solar position every 60 s via the `spa` crate (lat/lon from `MESH_LATITUDE`/`MESH_LONGITUDE` env vars, defaulting to London). Per-device `solar_enabled` flag persisted in `light_states` SQLite table. When enabled, the sweep calculates brightness and colour temperature from solar elevation (night −18°→bri 1/CT 500; noon 90°→bri 255/CT 153 mireds), then spatially modulates brightness per device using a dot-product of the device's physical XY position against the sun's azimuth vector (east-facing bulbs brighten as the sun rises east). Room-aware adjustments: `orientation_degrees` rotates the effective azimuth, `window_facing` boosts/dims based on how directly the sun hits the window, `has_window: false` halves intensity. New SQLite table `light_positions (device_id, x, y, z, room_id)` with in-memory `HashMap` mirror; rooms gain `orientation_degrees`, `has_window`, `window_facing` columns (with live ALTER TABLE migrations). REST: `GET/POST /api/lights/{device}/position`. `solar_mode` added to `build_light_action`; enabling saves the device's pre-solar state; disabling restores on/off + brightness + CT + XY to the node. `DashboardEvent::SolarUpdate { azimuth, elevation }` broadcast to WS clients each tick. `spa` + `chrono` added to coordinator deps. **449 tests.**

- **F-Colour ✓** — Native CIE xy colour intent. Replaced the CSS-name/hex lookup table with direct CIE 1931 chromaticity coordinates output by the LLM. Tool schema now exposes `cx` and `cy` number fields with saturated-colour anchors (red=0.675/0.322, blue=0.167/0.040, green=0.409/0.518…) and white-point interpolation guidance for light/pale shades. `action` field gains an explicit description disambiguating `color` (any named hue, requires cx+cy) from `color_temp` (white-light warmth only). `rgb_to_xy` and `css_color_to_xy` helper functions removed. `build_light_command` returns `Option<Vec<LightCommandRequest>>` so bad colour calls are surfaced rather than silently falling back to `On`. New schema-structure test asserts `cx`/`cy` present and old `color` string field absent. **460 tests.**

- **F-Effects ✓ (palette UI + room-level solar redesign)** — Effects/Scenes taxonomy and drag-from-palette UX. Establishes a clear design split:
  - **Scenes** (static snapshots): a saved state — specific brightness, colour, on/off per device. Recalled instantly. Already live (F6). User creates, names, and recalls them per room.
  - **Effects** (dynamic, ongoing): a coordinator-side process that continuously adjusts lighting over time. Enabled per room by dragging from a palette; the coordinator keeps running the effect until explicitly disabled.
  - **Effects palette** — a draggable chip strip above the room list. Chips are categorised (dynamic effects vs. scenes). Drag a chip onto any room card → enables that effect for all room devices. The room card gains a coloured badge (e.g., ☀ Solar in amber) that confirms the effect is active; clicking the badge disables it. Solar chip is also droppable onto collapsed room cards (card-level `dragover`/`drop` handlers handle the collapsed case; body handler fires only when expanded).
  - **Room-level solar model** — `solar_enabled` is a room flag (SQLite `rooms.solar_enabled`, persisted) rather than a per-device flag. All bulbs in the room receive solar commands; any manual brightness/CT/colour override marks that individual bulb as overridden (clears its per-device `solar_enabled` in `light_states`). Per-device override is shown as a dim ☀ button on the device card (lit = solar active, dim = manually overridden; absent = room has no solar effect). Clicking the dim ☀ restores the bulb to solar tracking via `POST /api/lights/{device}/restore-solar`, with optimistic update and explicit rollback on failure. Selecting a different effect (colour, scene) when solar is active automatically disables the bulb's per-device solar flag so solar and manual states do not fight each other.
  - **Immediate sweep on enable** — `POST /api/rooms/{id}/solar` wakes `SpatialEngine` immediately via `tokio::sync::Notify` (`DashboardState::solar_sweep_notify`) so bulbs change within the same second the effect is enabled, instead of waiting up to 60 s. The engine uses `tokio::select!` to wake on either the 60 s timer or the notify signal.
  - **Unassigned device strip** — hidden when empty; reappears automatically when a device is dragged out of any room (JS `wireDeviceDrag` dragstart reveals it; dragend hides it again if still empty).
  - **Solar** is the first effect chip. Subsequent planned effects: **Circadian** (smooth CT curve through the day driven by time only, no solar position needed — simpler than Solar, useful for rooms without windows); **Telemetry** (GPU inference running → subtle blue pulse on desk lamp, node offline → brief red flash — ties cluster health to ambient light); **Animate** (coordinator-timed breathing, colour cycle, or strobe — no fast-path needed for these slow effects).
  - **Deferred**: Solar telemetry widget in the dashboard (compass + elevation display, current azimuth/bri/CT readout per room so the user can see what the engine is sending). Currently the ☀ badge confirms the effect is *enabled*; the next step is showing what it's *doing*.
  - **Engineering note — effect handler registry** *(superseded by F-Effects-2)*: The current drop handler uses a single `if (effect === 'solar')` check. F-Effects-2 generalises this past a frontend handler map to a backend `Effect` trait + registry so adding a new effect doesn't require frontend or API changes — see `plans/phase11f-effects-2.md`.
  - **Engineering note — optimistic UI**: `setSolarMode` optimistically updates `roomsData[idx].solar_enabled` before the server confirms; restore-solar optimistically marks the device solar_enabled=true and reverts on failure. The coordinator broadcasts a `LightingUpdate` on `set_solar_enabled`, so the WS event auto-corrects within milliseconds.
  - **294 tests** across all crates.

- **F-Spatial-2** — Room Layout Canvas. Clicking a room name/header opens a full layout view for that room (replaces the card in-place, back button returns to the room list). The layout view is an SVG canvas — a top-down floor plan the user builds themselves.
  - **UX**: Click room header → layout view slides in. No extra buttons needed; the card IS the entry point. Back/close returns to rooms list.
  - **Phase A ✓ Complete — Bulb placement**: SVG canvas (`viewBox="0 0 1000 1000"`, scales to any screen). Bulb chips in sidebar; drag to canvas → snaps to 1/20 grid, drops to normalised (x, y, z) position. `fixture_type` (`ceiling_spot` | `pendant` | `table_lamp` | `floor_lamp` | `led_strip`) sets default Z and solar sensitivity. Canvas popover: height slider, fixture type picker, remove button. Undo/redo (Ctrl+Z/Y) via JS snapshot stack. Bulb icons reflect live state (colour swatch, brightness). `SpatialEngine` uses updated positions + Z on next sweep (3D dot-product when Z > 0, 2D fallback for legacy). `fixture_type` column added to `light_positions` table. **278 tests.**
  - **Phase B ✓ Complete — Windows & Doors**: `openings` SQLite table with full CRUD REST API (`GET/POST /api/rooms/{id}/openings`, `PATCH/DELETE /api/rooms/{id}/openings/{oid}`). Legacy migration converts `has_window=1` rooms to opening rows on startup. Canvas: drag window/door chip to wall edge (magnetic snap within 80 SVG units), midpoint snap, resize handles at each end, **move drag** (grab body → slide along wall or drag to different wall, `wall_edge` PATCH-able). Transmission popover with type-aware presets (Clear/Frosted/Blind for windows, Solid/Half-glazed/Full-glazed for doors). Live light cone SVG overlay (angle from solar azimuth, opacity from `transmission × elevation_factor`), redrawn on every `SolarUpdate` WS event. `SpatialEngine` replaced `has_window`/`window_facing` with per-opening geometry: `contribution += (1 − norm/90) × transmission × width_norm × clamp(elevation/45, 0, 1)`. `opening_scope`, `height_norm`, `height_span` fields stored for Phase C 3D geometry. Service worker cache bumped to `mesh-v3`. **280 tests.**
  - **Rooms UX (alongside Phase B) ✓**: Removed redundant rename button — hover pencil `✎` appears on room name hover. Room cards collapsible via `▾`/`▸` chevron, state persisted in `localStorage` per room. Fixed silent FK bug in `delete_room`: `light_positions.room_id` now NULLed before deleting the room so bulbs survive as unassigned devices with coordinates intact.
  - **Phase C — Three.js 3D view**: Replace the hand-rolled SVG canvas with a **Three.js** scene (CDN importmap — no build step). An orthographic camera reproduces the current 2D floor-plan view exactly; a "3D" toggle button switches to a perspective camera + `OrbitControls` so the room can be spun and zoomed. Room geometry is a `THREE.Shape` polygon so non-rectangular rooms (L, T, etc.) are naturally supported. Openings are coloured wall patches; fixtures are emissive spheres whose brightness/colour tracks the live WS state. Prerequisites: add `width_m`, `depth_m`, `height_m` floats and optional `shape_vertices` JSON column to the `rooms` table (ALTER TABLE migration), and a UI to set them (numeric inputs in the room settings popover). Without dimensions the view defaults to a 4 × 4 × 2.5 m box. Rendering tasks: floor + ceiling quads, four extruded wall meshes with per-face UVs, opening cutouts as CSS colour patches (full boolean geometry holes deferred — hard in Three.js without BSP), fixture spheres with `PointLight` per bulb, ambient + directional sun light that tracks `SolarUpdate`. Canvas toggle preserves all placed bulbs and openings — no data reload needed. Target: 2D view is visually identical to the current SVG canvas; 3D adds spin/zoom and approximate room shape. Phase C replaces the SVG `<g>` layers entirely; the JS module (`layout.js`) is refactored but the REST API and data model are unchanged.
  - **Phase D ✓ Complete — Live sun arc + Room Compass Orientation + Light Simulation**
    - **Sun arc overlay**: A sun icon traces an arc across the canvas perimeter showing today's solar path (sunrise azimuth → sunset azimuth mapped to compass bearing around the room boundary). A pulsing dot marks the current position, driven by the existing `SolarUpdate` WS event — no new backend needed. The arc is drawn relative to `orientation_degrees` so it immediately reflects the room's real-world facing.
    - **Time scrubber**: A slider below the canvas lets the user preview "what will my lights look like at 4pm?" — dragging computes a hypothetical solar position (client-side trig, same formula as `spa` but in JS) and animates bulb brightness/CT to their predicted state. Releasing snaps back to real-time mode. Scrubber initialises to current wall-clock time (not noon). Releasing the scrubber also sends real Zigbee commands to solar-enabled bulbs in the room (brightness + CT with 1.5 s transition) so the physical lights preview the simulated state. Makes the SpatialEngine logic tangible and child-friendly.
    - **9-model light cone system**: Canvas light-cone overlay switched to a menu of nine physically-inspired models. Default is `parallel-beam` — a parallelogram whose aperture edges run along the `wallTangent` (N/S → horizontal, E/W → vertical) so the window opening stays anchored to the wall axis regardless of sun angle. Full model list: Parallel beam, Beam + footprint, Soft beam, Cone, Gradient cone, Caustic patch, Bright patch, Wall glow, Sun arc. Model selection persists across room switches; changing model immediately redraws using the current scrubber position.
    - **Civil twilight fade**: Light cones fade gracefully through −6° to 0° elevation (civil twilight) rather than snapping off at the horizon. Beams below −6° are suppressed entirely.
    - **Compass dial (universal baseline)**: A small SVG compass rose rendered in the top-right corner of the layout canvas. A draggable handle rotates it; a fixed N marker shows where North is. As the user rotates, the sun arc rotates in real time — instant visual feedback. On `pointerup`, debounced `PATCH /api/rooms/{id}/orientation { orientation_degrees }` persists the value. Pointer capture fixed to use `svg.setPointerCapture` (not `e.target.setPointerCapture`) so fast drags never lose the needle. Tooltip: "Drag to set orientation: point N toward the real-world compass direction your top canvas wall actually faces." `SpatialEngine` picks up orientation on next sweep (no restart needed).
    - **Phone compass calibration (mobile, one-tap)**: A "📱 Use phone compass" button in the canvas toolbar. On tap: requests `DeviceOrientationEvent.requestPermission()` on iOS (required user-gesture guard); listens for `deviceorientationabsolute` event; snaps the dial to `ev.alpha` (degrees from magnetic North) and fires the PATCH. On desktop the event never fires — no error, no noise. Requires HTTPS (already enforced in production; localhost exempt).
    - **Optional — Sun calibration mode**: A secondary "☀ Calibrate from sun" button, visible only when `elevation > 5°`. Shows the four walls; user taps the wall the sun is currently hitting. Back-calculates: `orientation = solar_azimuth − wall_facing_degrees`. Fires the same PATCH as the dial. Elegant because it uses the broadcast solar data already in the client — zero extra API calls.
    - **Scene/solar badge interaction**: When a scene is recalled for a solar-enabled room, the ☀ Solar badge becomes dim + strikethrough ("paused by scene"). Clicking the paused badge resumes solar immediately (`POST /api/lights/{device}/restore-solar` for each device + clears the scene active marker). Solar badge state is not lost — the room card still shows the badge so the user can always find and re-enable it.
    - **Smooth solar enable transition**: Dragging the "Solar" effect onto a room triggers a 3-second brightness + CT transition to the current solar state (using `lastKnownSolar` from the most recent `SolarUpdate` WS event) rather than a hard jump.
    - **Source-of-truth rule**: The compass dial is always the single source of truth. Phone compass and sun calibration both set the dial value and then PATCH — they never bypass the dial. This keeps the UI consistent and prevents competing orientation sources.
    - **Backend**: `PATCH /api/rooms/{id}/orientation { orientation_degrees: f32 }` → clamp to `[0, 360)` → `UPDATE rooms SET orientation_degrees`. `SpatialEngine` unchanged — it already reads `orientation_degrees` on every sweep. No schema changes needed.
    - **Deferred — OSM building footprint auto-detect**: Geocode address → Overpass API building polygon → derive longest wall axis. Too many edge cases and external API dependency for marginal gain; revisit if users request it.
  - **Phase E ✓ Complete — Room dimensions, draggable crosshair, mobile UX polish**
    - **Room dimensions**: `rooms` table gains `width_m`, `depth_m`, `height_m` floats and `origin_x`, `origin_y` normalised origin (default 3 × 6 × 2.5 m, origin 0.5, 0.5) via idempotent ALTER TABLE migrations. `PATCH /api/rooms/{id}/origin` and `PATCH /api/rooms/{id}/dimensions` endpoints (Phase E-1, commit `1118061`). Sidebar W/D/H number inputs PATCH on change.
    - **Origin crosshair restyle (E-2)**: 16 px cyan ring + inner plus replaces the offset `⊕` glyph. Invisible 36 px-radius hit area on top — ~9× the previous tappable area. Touch input requires double-tap to start drag (first tap thickens the ring as armed-hint; second tap within 400 ms enters drag mode); mouse retains single-press-drag. **Live dimensions during drag**: two cyan labels — `↑ X.XX m` (distance to top edge, on the vertical line) and `X.XX m →` (distance to right edge, on the horizontal line above the crosshair) — with a black stroke outline so they stay readable over any room contents. Both vanish on release.
    - **Sidebar collapsibles**: `Bulbs` / `Openings` / `Room size` become collapsible sections with the same `▾`/`▸` chevron pattern as the room cards, state persisted in `localStorage` per section. Sidebar width narrowed 150 → 110 px, chip padding tightened, freeing ~40 px of canvas width.
    - **Touch-device tap-target media query**: `@media (hover: none) and (pointer: coarse)` block bumps icon/badge buttons (`.room-collapse-btn`, `.room-remove-btn`, `.room-layout-btn`, `.room-action-btn`, `.light-toggle-btn`, `.layout-toolbar-btn`) to `min-width/height: 36px`, `.color-swatch-btn` and `.solar-dot` to 32 × 32, slider thumbs 14 → 22 px, slider tracks 4 → 6 px, chip padding/font bumped. Desktop with a mouse is untouched.
    - **Slider lock during automatic control**: brightness and color-temp sliders get `disabled` + faded label/value when `dev.solar_enabled` (or any future per-device effect) is true, with tooltip "Disabled while solar is active — click ☀ to take manual control". Browsers don't fire pointer events on disabled inputs, so the `slider-active` race path is short-circuited as a side effect.
    - **Slider race fix (server + client)**: previously, releasing a brightness/CT slider could snap back to the pre-command value because the next WS snapshot (triggered by another device's report) still carried the stale value. Fixed in two places: (1) coordinator `DashboardState::apply_command_to_snapshot(device, action)` mutates the in-memory light snapshot at the moment the command is sent, so subsequent broadcasts carry the intended value; (2) client `pendingCommands` overlay in `rooms.js` reconciles incoming snapshots against pending optimistic values with a 2 s TTL, dropping the overlay on snapshot agreement. Net: the slider stays where the user released it, even if multiple devices report concurrently.
    - **Light cone shadow camera tightening (3D view)**: directional-light shadow camera frustum sized to the room diagonal × 1.5 (was `±(W+D)`, ~3× oversize) — roughly 3× more shadow-map texels per surface unit.
    - **`dashboard-mobile` recipe**: `just dashboard-mobile` adds idempotent Windows portproxy 9001 → WSL2:9001 (folded into the existing `update-portproxy` so a single UAC prompt handles both 9000 and 9001), opens the Windows firewall for TCP/9001, then prints the LAN URL with the auth token. Pre-cutover convenience — retired in Phase 11.6 when the coordinator moves to pi1.
  - **Engineering**: Room layout view is a new `layout.js` module. Canvas coordinates are always 0–1 so the SVG scales to any screen. Bulb icons on canvas reflect live state (colour swatch, brightness) from the existing `devicesMap`. DB migration in Phase B: existing `rooms.has_window` / `window_facing` rows are converted to `openings` rows on first startup with the new schema.
    - **3D solar weighting** — `SpatialEngine` upgrades from a 2D azimuth dot-product to a full 3D calculation once Z is populated. The sun direction vector is `(sin(az)·cos(el), cos(az)·cos(el), sin(el))`; each bulb's normalised position vector is `(x−0.5, y−0.5, z−0.5)`. The dot-product of these two gives solar exposure: a table lamp near a south-facing window at the same height gets high weighting; a ceiling spot directly above gets much less because the sun vector is nearly perpendicular to the vertical offset. Bulbs with no Z set (legacy) fall back to the existing 2D calculation so old behaviour is preserved until the user sets up the canvas.
    - **`fixture_type` column** added to `light_positions` table (TEXT, nullable — null treated as `ceiling_spot` for backward compat).
    - **Phase A auto-arrange** — once ≥2 anchor lights are placed, an "Auto-arrange remaining" button distributes unplaced lights of the same fixture type evenly across the canvas (ceiling spots in a uniform grid, floor/table lamps pushed toward walls). Pure JS, no backend needed.
    - **Deferred — Zigbee signal triangulation**: Z2M exposes per-link `linkquality` (LQI) in neighbour/routing tables. With ≥3 lights that can hear each other, rough relative XY positions can be inferred via trilateration. LQI is noisy (affected by obstructions and antenna orientation) so results need user confirmation, but could seed the canvas with plausible starting positions before manual refinement. Blocked on: assessing Z2M neighbour-table API stability and LQI accuracy in practice. **Note:** Philips Hue SpatialAware (Bridge Pro, shipped Apr 2026) solves this with AR camera scanning but requires the Bridge Pro hardware and bypasses Z2M — not directly usable here, though a one-time import via the local Hue API v2 entertainment_configuration endpoint is worth revisiting if users have a Bridge Pro alongside Z2M.

- **F-Scenes-2 ✓** — Scene UX polish, drag reorder, toggle/revert, and stability fixes.
  - **Scene preview colours** — `SceneRecord::preview_color()` method averages CIE xy chromaticity across all snapshot devices, falling back to a colour-temperature approximation (K = 1 000 000/CT, linear xy interpolation between warm 2700 K and cool 6500 K anchors). `SceneInfo` carries `preview_color: Option<[f32; 2]>` (omitted from JSON when absent). Scene chips display a colour gradient swatch from the saved state. Warm-start propagates preview colours from SQLite on coordinator boot.
  - **Active scene indicator** — `activeSceneByRoom: Map<roomId, sceneId>` in `rooms.js` tracks which scene is live per room. Active chips show a filled dot prefix, bold weight, and a ring shadow — industry-standard "selected" treatment so the current scene is immediately obvious at a glance.
  - **Scene toggle / revert** — clicking the active scene chip a second time reverts all room devices to the pre-scene state, snapshotted into `preSceneStateByRoom` just before the original recall. 0.8 s hardware transitions make the revert smooth. Any manual device command (on/off, slider, intent) also clears the active marker.
  - **Scene drag reorder** — `position INTEGER NOT NULL DEFAULT 0` column added to `scenes` (ALTER TABLE migration guarded by `PRAGMA table_info`; new scenes insert at `COALESCE(MAX(position)+1, 0)`). `POST /api/scenes/reorder { ids: [...] }` updates all positions in a single batch; registered before `/{id}/recall` to avoid route shadowing. Horizontal drag-and-drop in the quick-scene chip bar with live insert preview; order reflected in both the chip bar and the full scene list on every render. Stable sort key: `(a.position − b.position) || (a.created_at − b.created_at)`.
  - **Scene name save fix** — `blur → doSave` handler removed (fired on every WS re-render, truncating names to 1–2 characters). Replaced with `activeSceneEdit: { roomId, value }` module state that survives DOM rebuilds; `render()` restores the input, re-focuses, and repositions the cursor so typing is never interrupted.
  - **Colour picker click-outside** — `document.click` handler closes any open `.light-colour-picker` whose ancestor is not the event target. Escape closes both the colour picker and any in-progress scene name input.
  - **Engineering fixes (Bing/Gemini review)**:
    - `dragType` global leak — `window.addEventListener('dragend/drop', …)` in `layout.js` ensures drag state is always cleared after a drop or cancelled drag.
    - `placedOpenings` stale state — `openLayout()` resets `placedOpenings = {}` on entry, preventing stale SVG opening elements from persisting across room switches.
    - `openPickerIds` stale prune — `render()` removes IDs from `openPickerIds` when the corresponding device is no longer in the render data, preventing phantom open pickers on next paint.
    - Warm-start device names — `warm_start_rooms()` calls `push_rooms_update_with_names(room_infos, names)` so display names are available immediately after a coordinator restart, before any heartbeat arrives.
  - **Chaos test 6/7 fix** — Heartbeat handler returned `None` (no reply frame); changed to `Some(MeshMessage::Acknowledge)` so the "valid heartbeat is acknowledged" scenario passes.
  - **Canvas breathing fix** — `stopPulse` was called on every `pointerup`, killing the animation before the popover opened. Moved to fire only on confirmed drag end; `dismissPopover()` now calls `stopPulse` so breathing stops when the popover is dismissed.
  - **295 tests** across all crates.

- **F-Effects-2 ✓ Complete** (commits `ced2c03` → `58f4c26`). Effects registry + curated catalogue. Spec lives at `plans/phase11f-effects-2.md`. Replaced the placeholder `rooms.solar_enabled` bool and the inline `if (effect === 'solar')` drop-handler with an extensible registry. Adding effect #N is a single file in `coordinator/src/effects/`.
  - **`Effect` trait** (`coordinator/src/effects/mod.rs`) — `id` / `display_name` / `category` / `cadence` / `params_schema` / `default_params` / `tick(&EffectCtx) -> Vec<EffectCommand>` plus lifecycle hooks (`on_enable`, `on_handoff`, `on_disable`), `respects_overrides`, and the opt-in persistence triad (`persist_cadence`, `serialize_internal_state`, `deserialize_internal_state`) so stochastic effects survive a coordinator restart without continuous disk writes.
  - **`SpatialHelpers`** ships with `west_to_east` + `directional_offset(bulb, Direction)` returning offsets in `[-0.5, +0.5]` for the `t_bulb = clamp(t_global + offset, 0, 1)` pattern. Other geometric helpers (`angle_to_sun`, `window_proximity`, `altitude_band`, `distance_to_wall`) listed in the plan are still TODO and will land when an effect needs them. Room-orientation rotation is also deferred until an effect requires true world-direction alignment.
  - **`room_effects` SQLite table** with `snapshot_json` + `internal_state_json` + partial unique index `uid_enabled_room_effect ON (room_id) WHERE enabled = 1` enforcing one-active-effect-per-room at the DB layer + `CHECK (enabled IN (0,1))`. The original "silent migration from `rooms.solar_enabled`" was replaced by a full legacy-column purge per the project's no-backward-compat policy.
  - **`EffectRunner`** — single tokio task. Tick scheduler with per-room `next_tick_at_ms`, per-room `RoomLes` cache, delta-threshold dedup gate (brightness Δ≥2, xy Δ≥0.005, CT Δ≥4 mireds), opt-in internal-state persistence schedule (`PersistCadence::Never | OnEnableOnly | Periodic`), and cadence-drift EWMA that fires a single throttled `warn!` when an effect sustains >20% drift from its declared cadence for 30 s. Computes the solar position once per loop and pushes it as `DashboardEvent::SolarUpdate` so the compass UI keeps working without `SpatialEngine`.
  - **`effects::blend`** — hand-rolled Oklab pipeline (matrix coefficients from Ottosson's paper) + 8-point CT-mireds → CIE xy lookup table + `oklab_lerp` + `BlendPoint` + `lerp_u8`. Red→blue cross-fade passes through clean purple, not muddy grey (regression-tested). Public to the rest of the crate — same machinery is reusable for time scrubbing, parameter preview sliders, jump-to-midpoint controls, arbitrary-duration scene recall, and the 1 s effect→effect handoff (not yet wired into the runner; flagged for a future slice).
  - **Catalogue shipping** (7 effects, 4 categories): **Solar** (port of the legacy spatial-engine math), **Sunset** (7-keyframe OKLCH palette, west-biased, ceiling spots run the full curve, lamps run a truncated ramp/hold/fade and clamp colour to `t ≤ 0.6` to stay warm during fade), **Sunrise** (inverse palette, east-biased, lamps run the full curve as wake-up lights), **Breathing** (sine on brightness, optional `colour_xy`, no spatial input — the minimum-cost effect), **Candlelight** (per-bulb stateless flicker via `mix3(master_seed, fnv1a_64(device_id), step)`; `PersistCadence::OnEnableOnly` writes the seed exactly once; restart resumes the same flicker pattern), **Aurora** (10 Hz 2D-field wave through a 4-keyframe green→cyan→purple loop with persisted seed), **Telemetry** stub (registered under `Reactive` category for catalog visibility — `tick()` returns empty Vec until F8 #1 wires inference + heartbeat events).
  - **Data-driven frontend** — `renderEffectsPalette()` iterates `GET /api/effects` so adding effect #N requires no JS change; generic `<EffectParamsEditor>` reads each effect's JSON Schema and renders sliders for `integer`/`number` (with `min`/`max`/`step`), segmented buttons for `string` + `enum`, and checkboxes for `boolean`. The badge renders icon + display_name for whichever effect is in `roomEffectsMap`; click → toggle the inline editor; right-click → quick disable. Server-side `jsonschema` validation against schemas compiled once at startup (cached in the registry, not per-request) is the backstop; the editor enforces ranges at the input layer.
  - **`merge_with_defaults`** in `coordinator/src/http/api.rs` overlays partial body params on top of effect default_params so the runner never has to handle missing keys on the tick path.
  - **Legacy purge** (combined with F-Effects-2.3 per the no-backward-compat policy) — dropped `rooms.solar_enabled` + `light_states.solar_enabled` columns, deleted `POST /api/rooms/{id}/solar` + `PATCH /api/lights/{device}/solar` + `POST /api/lights/{device}/restore-solar`, removed `LightAction::SolarMode`, removed `LightStateReport.solar_enabled`, deleted `DashboardState::set_solar_enabled` / `get_solar_enabled_devices` / `save_manual_state` / `get_manual_state` / `last_manual_states`, deleted `Registry::set_room_solar` / `get_room_for_device_solar`, removed `is_solar:true` light-command flag, removed the 3 s smooth-transition kick and per-device solar dot rendering. `SpatialEngine` task deleted entirely.
  - **Tests landed**: scaffolding (29) + 2.1 runtime (11) + 2.2 API (8) + (10 + 4 SpatialHelpers + 3 merge_defaults + 8 Solar) + 2.5 Sunrise/Breathing/Candlelight (19) + 2.6 Aurora/Telemetry/drift (12). Total effects tests: 87. Workspace passes 572 across all crates.
  - **Engineering decisions locked**: single effect per room (Telemetry overlay revisited when F8 #1 ships); tick-driven, not reactive streams; JSON Schema params (no per-effect frontend code); seed + elapsed persistence preferred over continuous writes; **no backward-compat shims** (per memory `feedback_no_backward_compat`).
  - **Deferred to future slices**: 1 s OKLCH blend on effect→effect handoff (`blend` layer ready; runner doesn't wire it yet — no flicker observed in practice with hard transitions); `SpatialHelpers::angle_to_sun` / `window_proximity` / `altitude_band` / `distance_to_wall` (no effect needs them yet); room-orientation rotation in directional offsets; Telemetry effect real wiring (F8 #1).

- **F-Lighting-UX ✓** — Casual-first room cards, unified slider model, bulb deletion, and Zigbee-down indicator. Mobile-first redesign so the three everyday actions (On/Off, Brightness, Scenes) are always visible; colour/temperature behind a single 🎨 popup; floor-plan and delete tucked away.
  - **Unified slider core** — one `attachThumbSlider()` (thumb-only grab, track click passes through to the card, value bubble, `.slider-active` guard so a WS-driven `render()` can't wipe a slider mid-drag) wraps every slider via `buildSlider` / `wireDeviceSlider`. Deduplicated `lockSliderToThumb` (was copy-pasted in `rooms.js` + `layout.js`). `patchDevice()` helper replaces repeated optimistic `devicesMap.set` boilerplate.
  - **Colour-vs-temperature mode persists** — Hue bulbs always report *both* `color_xy` and `color_temp`, so the active mode can't be inferred from state. Mode is persisted per device (`mesh-mode-<id>`) and per room (`mesh-room-mode-<id>`) in `localStorage`; adjusting a slider pins its mode. *(Single-dot toggle superseded by the icon↔dot model in **F-Lighting-UX-2** below.)*
  - **Bulb deletion** — `DELETE /api/lights/{device}` → `Registry::delete_device` removes the device from `room_devices`, `light_states`, and `light_positions` (DB + in-memory). Delete button on unassigned-strip chips.
  - **Zigbee-down indicator** — `MeshMessage::ZigbeeStatus { online }` (emitted by the lighting capability from `zigbee2mqtt/bridge/state` via `parse_bridge_online`, and from MQTT `ConnectionLost`) → `DashboardEvent::ZigbeeStatus` drives an amber offline banner and disables all light controls. Placeholder devices from `push_device_discovery` now start `online: false` (only a real `LightState` report marks a bulb online). Client-side `inferZigbeeStatus` fallback covers the case where z2m crashes before ever connecting (no Last Will): rooms exist but no devices reported ⇒ offline. Per-card: offline device cards disable light controls but keep drag + delete. *(Implements the "Device availability tracking" deferred item below.)*
  - **Tests**: `parse_bridge_online` (3), `delete_device_clears_room_membership_and_position` (1), placeholder-offline assertion. **600 tests across the workspace.**

- **F-Lighting-UX-2 ✓ (2026-06-02)** — One affordance language for colour/temp and effect/scene: an **icon at rest, a marker when engaged**.
  - **Colour/temp icon↔dot** (supersedes the single-toggle dot above). Each domain shows its glyph (🎨/🌡) at rest; the domain you last *set* becomes a live dot tracking the value and **persists** — tracked in per-device `deviceDotDomain` / per-room `roomDotDomain` session maps, painted by `paintDeviceButton` / `paintRoomButton`. Brightness leaves it; setting the other domain moves it; on/off or a scene clears both back to icons. While a control is dragged it shows the live value. Kills the old state-derived flicker and the thumb-like temp dot.
  - **Consistent warm-white On** — room On *and* single-bulb On both power up to a shared `HUE_DEFAULT_ON` (brightness 200 + `color_temp` 370 ≈ 2700 K). Soft on/off remains via the brightness slider; Off is plain off.
  - **Per-light scene + effect pause** — effects already had per-device override (`room_effects.overrides_json`, `set_effect_override`, `EffectRunner::tick_one` filter, `EffectUpdate.overrides`, greyed `device-effect-btn`); scenes now match. Per-light marker (`device-scene-btn` 🎭 / the effect glyph) is **lit when participating, greyed when paused**; click greyed to resume. Scene pause is frontend session state (`pausedSceneDevices`): pause replays the device's pre-scene snapshot (or `HUE_DEFAULT_ON` if none, e.g. after a reload) via `sendDeviceCommand(…, {keepScene:true})` so the room's scene stays active for the rest; resume re-applies the scene to just that light via `POST /api/scenes/{id}/recall { device_id }` — a new optional filter on the existing recall handler (reuses the full per-device fan-out). Room scene chip dims (`.partly-paused`) when some member lights are paused, greys (`.all-paused`) when all are; the effect ghost badge already resumes on click. The participating marker is now full-opacity (was a dim 0.85 that read as greyed).
  - **Effect re-activation resets overrides** — `set_active_effect` now sets `overrides_json = '[]'` in its `ON CONFLICT DO UPDATE` (it previously left it untouched). Dragging an effect onto a room is a fresh start with every light in; stale exclusions from a prior session can't silently persist (the "freshly-dropped solar showed 7/8 bulbs greyed" bug). Test `overrides_cleared_per_activation` flipped to assert the reset (it had enshrined the old buggy contract).
  - **Mobile scene row** — scene chips get `touch-action: pan-x` so a horizontal swipe scrolls the chip row instead of being grabbed; the **`+ New Room` row moved above the effects palette** so an over-swipe no longer lands in that input.
  - **'all' group hidden** — the zigbee catch-all `all` group is no longer rendered as a bulb-like, undeletable card in the Lighting tab (`lighting.js`).
  - **Overflow fix** — restored `min-width:0` on the room header flex chain (`.room-card-header`, `.room-header-name-row`) and added `flex:1; min-width:0` to `.light-name-group`, so long hex device names ellipsis instead of forcing `#lighting-list` past the viewport (which had stopped the effects palette scrolling). Regression of `e6da0f6`, undone by the casual-first refactor.
  - **Tests**: `recall_scene_with_device_id_targets_only_that_device` (single-device recall). Frontend has no JS test harness. (Routing-invariant tests + `docs/coordinator.md` lighting-routing note landed in `f19b503`.)

- **F7** — Switches / Input Devices. New `capability_switches` crate. Subscribes to Z2M button, remote, motion sensor, and contact sensor events. Emits `SwitchEvent { device_id, action }` to coordinator; coordinator broadcasts `DashboardEvent::SwitchEvent`. Dashboard: live switch activity feed. Foundation for the Reactive Graph.
- **F8** — Creative Features. Delivery order: (1) Telemetry Lighting effect (GPU activity → ambient pulse); (2) Photo Colour Picker (client-side Canvas API → CIE xy); (3) Intent-First Scenes (LLM resolves "cozy" to actual device states at runtime); (4) Temporal Composer (drag-and-drop scene timeline, coordinator executes transitions); (5) Reactive Graph (plain-language automation rules parsed by LLM into `{trigger, action}` pairs); (6) 2D Room Layout (SVG floor plan, builds on F-Rooms + F-Spatial).
- **F9** — Light Games. The bulb grid is a natural game board — game logic lives in the coordinator (Rust, deterministic, no browser needed). Each game is a coordinator-side state machine that sends `LightCommand` sequences; the dashboard renders a game UI overlay and relays player input via a new `POST /api/games/{game}/input` endpoint.
  - **Simon Says** (MVP) — coordinator picks a random bulb, flashes it a colour, player taps the matching card in the dashboard. Sequence grows by one each round. Lives: 3. High score persisted in SQLite.
  - **Whack-a-Mole** — coordinator lights a random bulb for 1.5 s (yellow flash). Player must tap it in the dashboard before it goes out. Miss = −1 life, hit = +1 point + speeds up by 50 ms. 3 lives, 60 s game. Bulbs turn red on miss, green on hit.
  - **Colour Match** — coordinator sets two bulbs to slightly different CIE xy values; player picks which bulb matches a reference swatch shown in the UI. Difficulty ramps by reducing the xy delta each round.
  - **Hot/Cold** — motion-sensor driven (requires F7). Coordinator picks a "treasure" room; player walks around, bulbs glow warmer (red) when closer, cooler (blue) when farther. Win = enter the correct room.
  - **Disco Party** — no player input. Intent-triggered ("start a disco!"). Coordinator runs a choreographed colour sweep loop across all bulbs using the Effects engine; stops on "stop the disco" intent or after a configurable duration.
  - **Strobe / Rave mode** — fast-path only (F10 prerequisite). Sub-100 ms colour cycling across the grid, tempo synced to a BPM set via intent ("strobe at 120 BPM").
  - **Engineering note**: each game is a `GameSession` struct spawned as a tokio task. `GameRegistry` (similar to effect handler map) stores active sessions keyed by game ID. Dashboard game overlay is a separate JS module (`games.js`) that subscribes to a new `DashboardEvent::GameState` WS event for render updates.
- **F10** — Effects Engine fast path. Prerequisite: evaluate SLZB-06 + Z2M latency floor. If viable: `capability_effects` crate with fast-path UDP for fluid effects at ~50 Hz. Screen sync via `getDisplayMedia` (browser API, no Rust needed for MVP) deferred until fast path confirmed.

### Phase G — Security panel

### Phase H — Polish, icons, desktop layout pass

### Deferred dashboard polish (raised post-C5, updated 2026-06-11)

> Full backlog audit with effort estimates and recommended order: `plans/backlog-2026-06.md` (2026-06-11). Items below are kept in place for context; the plan file is the working list.

- **Multi-turn chat context** ✓ (2026-06-08) — `chat.js` accumulates `conversationContext` pairs; `POST /api/chat` carries `context: Vec<IntentTurn>`; `handle_intent` prepends the turns before the current user message. New-conversation button shipped alongside. **Token-budget truncation** ✓ (2026-06-08, `c1e45d0`) — client trims to `MAX_CONTEXT_TURNS = 20` oldest-first; turn counter shown in UI.

These are non-blocking UX improvements for after C6 ships:

- **Dashboard preferences persistence** ✓ (2026-06-13, updated 2026-06-14) — `dashboard_preferences (user_id, key, value)` SQLite table; hybrid optimistic `localStorage` write + async server sync. `loadPrefs()` hydrates localStorage from the server on page load so panel order and collapse state look the same from any device or browser. Synced keys: `meshNodeOrder`, `meshHealthOrder`, `meshModelOrder`, `meshLightOrder`, `mesh-health-collapsed-*`, `mesh-room-collapsed-*`. New `prefs.js` module with `setPref` (instant) and `setPrefDebounced` (200 ms, used by all drag-end handlers to avoid write storms). `PUT /api/preferences/{key}` returns the full updated map (no re-fetch needed). `DELETE /api/preferences/{key}` added (returns 200 + updated map, 404 if key absent). 7 preference tests (445 coordinator total, 567 workspace).

  **Deferred preferences improvements:**
  - Schema / key allow-list — any string accepted today; enforce at write boundary when key space stabilises.
  - `user_id` wired to auth token — currently hardcoded `"default"`; safe for single-user, must change before multi-user auth.
  - Multi-device live sync — `loadPrefs()` on page load is sufficient for single-user; real-time cross-device push needs a WS-pushed `PrefsUpdated` event.
  - `updated_at` / `version` columns — deferred until migration tooling exists.
  - Consistent key naming (`dashboard.mesh.order`, `dashboard.health.collapsed.<id>` etc.) — rename sweep is a separate commit with localStorage migration.

- **Metric colour thresholds** ✓ (2026-06-11) — `metricClass(pct, metric)` with per-metric bands (`METRIC_THRESHOLDS`: CPU/RAM warn 75 / crit 90, GPU 90/98, VRAM 95/99 — VRAM normally sits near full when a model is loaded) colours the value text amber/red on CPU%, RAM%, GPU%, and VRAM%.
- **Sparkline tooltip with exact timestamp** ✓ (2026-06-11) — per-point transparent `<rect>` hit regions (Voronoi midpoint splits, no gaps) each carry a `<title>` showing `CPU: 87.3%  at 14:23:01`; falls back to `(sample N)` labels when timestamps are absent or mismatched. Mini sparklines on the Nodes tab unchanged.
- **Sparkline fill area** ✓ (2026-06-11) — `<polygon>` at 0.15 fill-opacity closes the area under each line in the stroke colour.
- **Per-node collapse/expand in Health panel** ✓ (2026-06-12, `4f25eee`) — each health card has a `▾ / ▸` toggle; collapsed cards show only the last value, not the full sparkline; `localStorage` persistence.
- **Dashboard housekeeping** ✓ (2026-06-18):
  - **Models leaderboard links** — two pills in the Models tab header (`panel-head`): "Leaderboard" → HF Open LLM Leaderboard (benchmark quality) and "Popularity" → OpenRouter rankings (usage), both `target="_blank" rel="noopener noreferrer"`.
  - **Model picker locked while unloading** — clicking Unload disables that node's select + Load button (amber "Unloading — freeing VRAM…" note) until the model has left the snapshot **and** VRAM has actually dropped (≥0.3 GB, or no VRAM telemetry, or 30 s fallback). Tracked client-side in `models.js` (`unloadingNodes`/`nodeBusy`/`clearUnloadIfDone`) since there is no backend `Unloading` state.
  - **Desktop tab bar / double-scrollbar fix** — dropped the desktop `body`+`.panels` dual-scroll overrides; the app-shell flex layout (single scrolling `.panels`, flex-anchored tab bar) now applies at all widths, so the tabs stay visible when maximised. SW cache bumped to `mesh-v5`.

#### Layout view — known issues / deferred fixes (2026-06-05)

- **Dragging windows/doors from the popover unreliable** ✓ (2026-06-13, `3feadde`) — fixed by capturing pointer on `document.body` instead of the chip element, so `closeSidebarSheet()` hiding the chip cannot fire `pointercancel` mid-drag; move/up/cancel handlers promoted to named functions registered on `document` so `cleanup()` can remove them by reference.

- **Room brightness/colour/temp controls (action bar)** — popover is correctly positioned above the action bar on desktop; mobile behaviour needs verification after the `position:fixed` + `getBoundingClientRect` approach was adopted. The centred-modal approach used for the ＋Add palette may be worth applying here too if offset issues surface on mobile.

- **＋Add palette (mobile)** — switched to a centred modal (`position:fixed; top/left:50%; translate(-50%,-50%)`) to escape the dynamic-viewport toolbar issue. The `collapsed` class was found to hide all content when the sidebar had been collapsed on desktop and then opened on mobile — fixed by removing `collapsed` on open. Monitor for any remaining edge cases.

### Deferred chaos / QA scenarios (raised post-Phase B)

✓ All three scenarios covered (2026-06-13, `97eb9f7`):

- **WS auth edge cases** ✓ — `ws_handler_rejects_wrong_token`, `ws_handler_accepts_both_tokens_during_rotation`, `ws_handler_rejects_expired_token_during_rotation` in `ws.rs` tests; real TCP server, real HTTP upgrade requests.
- **Lagged broadcast receiver** ✓ — `broadcast_receiver_gets_lagged_when_slow` in `state.rs` tests; 200 events overflow the 128-slot channel, receiver gets `RecvError::Lagged`.
- **Channel closed arm** ✓ — `broadcast_receiver_gets_closed_when_state_dropped` in `state.rs` tests; dropping the last `Arc<DashboardState>` gives `RecvError::Closed`.

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
- ~~**Beelink BIOS — locate Wake on LAN setting**~~ — done 2026-06-11: WoL located and disabled alongside the fTPM re-disable; node stable.

---

## Phase 11.6 — Always-On Coordinator + Remote Access ✓ Complete

Full design spec: `plans/coordinator-on-pi1.md`

Completed 2026-05-31. Coordinator successfully migrated from WSL2 laptop (`OmniLink1`) to always-on Pi 5 (`pi1`, `192.168.1.11`). Dashboard now runs 24/7 independent of laptop power state. Tailscale tunnel enables remote phone access (cellular, public WiFi, anywhere) without port forwarding or Let's Encrypt.

**Achieved**

- ✓ **pi1 hosts the coordinator** — co-located with zigbee2mqtt + Mosquitto on localhost. Beelink freed for heavy compute. Pi 5 fanless, low-power, zero crashes (vs. Beelink's fTPM/GPU driver instability).
- ✓ **Tailscale remote access** — phone authenticated to 100.100.100.100; dashboard URL bookmarked; MagicDNS pending propagation (IP URL works immediately).
- ✓ **Coordinator state migration** — cert/key/token + `ai_mesh.db` all in `/var/lib/ai-mesh/`; token reused across restarts so bookmarks remain valid.
- ✓ **Systemd service** — `Restart=always` + `RestartSec=5s` for automatic recovery from transient network/DB issues; `ProtectSystem=full` + `ProtectHome=true` + `NoNewPrivileges=true` for baseline hardening.
- ✓ **Agent repointing** — Beelink + OmniLink1 now point to pi1:9001; OmniLink1 reclassified as Compute (was Controller); all agents verified connected and healthy.

**Deployment recipes**

- `just deploy-coordinator pi1` — idempotent cross-build aarch64 → scp binary/state/DB → install systemd → enable+start. Wraps scp wildcard with directory check to avoid shell expansion on first deploy.
- `just verify-coordinator pi1` — Phase-2 health check (5 tests: service active, HTTP responds, startup logs, fingerprint extraction, DB exists).
- `just rollback-coordinator` — emergency revert: kill remote, restart on laptop.

**Code quality**

- Bing review: port hardcoding fixed ({{coordinator_port}} variable); AGENT_ROLE semantic change verified safe (display-only, not logic-critical).
- Gemini review: scp wildcard safety added (directory non-empty check); systemd resilience tuned (Restart=always for transient recovery).
- All pre-commit checks + unit tests passed (572 tests across workspace).

**Testing**

- ✓ Laptop closed; dashboard accessible on phone via Tailscale IP (100.100.100.100:9001)
- ✓ On/off, scenes, effects responsive over Tailscale tunnel
- ✓ Cellular tested (WiFi off); stable remote access confirmed
- ✓ Beelink heartbeats flowing; agent connectivity healthy
- ✓ MagicDNS propagating (http://pi1:9001 will resolve in <5 minutes)

**Future hardening (deferred — not blocking)**

- ✓ `ProtectHome=true` on systemd unit — `MESH_STATE_DIR=/var/lib/ai-mesh` redirects cert/key/state out of home; `StateDirectory=ai-mesh` ensures the dir exists with correct ownership.
- SQLite `journal_mode=WAL` + `synchronous=NORMAL` for SD-card wear mitigation on pi1.
- `LimitNOFILE=4096` if cluster grows past ~100 agents.
- Per-node TLS certs instead of single self-signed cert across all agents.

---

## Phase 11.7 — REAPER DAW Integration ✓ Complete (2026-06-14)

Full details: `docs/reaper.md`

LLM control of the REAPER digital audio workstation via the coordinator intent pipeline. REAPER runs on Windows (OmniLink1); the agent runs in WSL2 and reaches the REAPER web server over loopback (WSL2 mirrored networking).

**Achieved**

- ✓ `capability-reaper` crate — polls `/_/TRANSPORT` every 2 s; parses both JSON and tab-delimited responses; reports `ReaperStatusReport` (online, play state, position, tempo, time sig) to coordinator.
- ✓ Named transport actions — `play`, `stop`, `pause`, `record`, `rewind` mapped to REAPER command IDs; numeric IDs and SWS string actions (e.g. `_SWS_ABOUT`) passed through directly.
- ✓ `reaper_transport` + `reaper_action` tool schemas — LLM can trigger transport and arbitrary REAPER actions by name or numeric ID.
- ✓ `reaper.js` dashboard panel — live online/offline badge, play state, position, tempo, time sig, command log; all elements null-guarded.
- ✓ `scripts/install-reaper-windows.ps1` — downloads and silently installs REAPER, writes web server config (`reaper-webbrd.ini`, port 8080, `0.0.0.0`), registers the Web Browser Control surface in `reaper.ini`, opens firewall rule.
- ✓ OmniLink1 registered as a `controller` node with `--features reaper`; REAPER env vars in systemd drop-in (`REAPER_HOST=127.0.0.1`, `REAPER_PORT=8080`).
- ✓ `just set-fingerprint` and `just restart-node` handle local nodes (`NODE_HOST=127.0.0.1`) without SSH — write drop-ins and restart directly.

**Follow-up (2026-06-16):**

- ✓ Structured track tools `reaper_add_track` / `reaper_remove_track` — coordinator
  generates correct Lua so small models can't produce blank-named tracks. Auto-suffixes
  duplicate names (`Vocals`, `Vocals 2`, …), exclusively arms the new track, deletes by name.
- ✓ Daemon relays the script's `return` value as the result message, so tools report what
  they did (`Added 'Vocals 2' as track 5 (armed)`) instead of a blind `ok`.
- ✓ `try_parse_tool_calls` handles multiple fenced JSON blocks / consecutive objects from
  small models (previously only the first block ran), and lifts args a model nested under a
  stray schema-mirrored `properties` key (e.g. gemma3 `args: {properties: {name: …}}`).

**Follow-up (2026-06-17):**

- ✓ Tempo / time-sig control via intent — `reaper_set_tempo` tool. Tempo-only changes use
  `SetCurrentBPM` (no marker); a time-signature change writes/reuses the tempo-time-sig marker
  at the project start, filling any unspecified field from the project's current value.
- ✓ Track list / project state queries — `reaper_get_project` returns a multi-line summary
  (name, tempo, time sig, transport, per-track name + armed status).
- ✓ Multi-line daemon result messages — agent now parses the whole `ai_mesh_result.txt`
  (`<id>\t<ok|err>\t<message>`, message may span lines) instead of the first line; the daemon
  writes the result via a temp file + rename so a multi-line result is never read half-written.
- ✓ REAPER auto-launch (v1) — when a `reaper_*` tool is requested while REAPER is offline,
  the coordinator asks the node to spawn REAPER (`ReaperCommand` action `launch`; exe via
  `REAPER_EXE`, default `/mnt/c/Program Files/REAPER (x64)/reaper.exe`, launched through WSL
  interop) and replies "started it, give it ~15s, ask again" instead of timing out. A 30 s
  cooldown on the node prevents a retry burst opening several instances. Deliberately does **not**
  force a new project (REAPER opens its own default). Deferred follow-ups:
  - **Auto-retry after ready** — poll `/_/TRANSPORT` until online (cap ~30–45 s) and then run the
    *original* queued command, so the user doesn't have to re-ask. Skipped in v1 because cold
    start + plugin scan far exceeds the intent timeout; needs an async "resume the intent" path.
  - **Explicit "start a new project" intent** — expose `new_project` (action 40023, already mapped
    in capability-reaper) as a tool, keeping auto-launch non-opinionated about the session.
  - **macOS launch path** — `REAPER_EXE` defaults to the app-bundle binary but is untested; gated
    on the same Mac mini provisioning as the items below.
- ◐ REAPER on macOS — code is macOS-ready (`default_scripts_dir()` resolves
  `~/Library/Application Support/REAPER/Scripts` on macOS, `/mnt/c/...` on WSL2); full
  provisioning (install script + node setup + testing) still pending the Mac mini (~end Jul 2026).
- ◐ Multi-REAPER instances — route intents to a specific REAPER node once more than one
  exists (e.g. OmniLink1 + Mac mini). Registry side is ready (`nodes_with_feature` returns all
  matching nodes); intent routing currently grabs the **first connected** REAPER node (the
  `extend this to a policy` comment in `intent.rs` is the only code concession). Needs: a target
  (explicit `node` arg + an active/default instance, since small models can't reliably infer it
  from the utterance), a **per-tool** selection policy (most intents hit one instance, a few like
  "stop all" fan out), disambiguation when none is specified, and per-instance status keyed by
  node ID in the poller + dashboard panel. Parked until a second REAPER box exists — gated on the
  same Mac mini as the macOS work. Full design notes: `docs/reaper.md` (Deferred).
- ◐ Plugin stack + FX automation — curated free-first third-party plugin list (vocal + guitar
  tracking weighted: Analog Obsession, TDR Nova, Valhalla Supermassive, Youlean, …) plus a generic
  FX-control tool layer. No plugin exposes its own API — control is uniformly via REAPER's
  `TrackFX_*` ReaScript functions, so it extends the existing structured-tool pattern. Must guard
  against FX index drift (resolve by name in the coordinator, never trust raw LLM indices),
  `AddByName` format-prefix instability across OS, and lazy param-map init. Melodyne is **not**
  automatable by us (offline/ARA). Plugin install is manual (Windows-side, not automated).
  Execution is **incremental, free-first, one plugin at a time**, and every slice must be
  verifiable **without recording audio** (insert + read-back) since the studio isn't built yet.
  First target: **Valhalla Supermassive** (free reverb — famous named presets, the cleanest demo).
  - Slice 1 — `reaper_add_fx` (◐ code + unit tests done; pending live verification): inserts an
    FX by name on a named track, matching the bare product name (no `VST:`/`VST3:` prefix) and
    reporting unresolved plugins instead of a silent no-op. Verify with **Valhalla Supermassive**
    (a VST that resolves by bare name). **Note:** stock **ReaVerbate** is *not* a safe control —
    it's a JSFX and does **not** resolve via `TrackFX_AddByName` by bare name (JS plugins need a
    different name form), so don't use it as the "always installed" fallback.
  - Slice 2 — `reaper_list_fx` + `reaper_list_fx_params` (◐ code + unit tests done; pending live
    verification): discovery. `reaper_list_fx` walks a track's chain returning name + 1-based slot
    (+ bypass flag); `reaper_list_fx_params` resolves the FX by name match (never a raw index) and
    lists each param's name, formatted value, and raw 0–1 value. Running `reaper_list_fx_params` on
    Supermassive answers the open question of whether its modes are REAPER presets (→ `SetPreset`)
    or internal params — if a "mode" param appears, they're params; if not, they're presets.
  - Slice 3 — `reaper_set_fx_preset` + a small curated preset/mode catalog: the "make it
    spacious" payoff.
  - Later — `reaper_set_fx_param` / `reaper_get_fx_param`, `reaper_bypass_fx`, then the next free
    plugin (TDR Nova / Analog Obsession).

  Full list, pricing, install steps, and per-plugin automation: `docs/reaper-plugins.md`.

---

## Phase 11.8 — Multi-Device Home (Blinds, HVAC) + Room-Centric Control (Design)

Captured 2026-06-29 from a design discussion. Nothing built yet except the first
piece (the Zigbee bridge health card, below). The home is about to grow well past
lights — **~7 blinds and aircon/HVAC** are coming, each as its own Zigbee device
class — so this records the navigation/architecture model before the device count
forces an ad-hoc answer.

### Guiding principle (from how the competitors do it)

The serious platforms all converge on one split: **room/area is the primary axis
for *control*; device-type is the *data model* and a *secondary* (management)
view.** Nobody makes you visit a "Lights" page, then a "Blinds" page, to operate
one room.

- **Home Assistant** — the closest model to ours. It rigorously separates the
  **registry** (entities → devices → **areas**) from the **dashboard**
  (presentation). Areas are first-class; device *type* is just the entity domain
  (`light`, `cover` = blinds, `climate` = HVAC) which decides the *widget*, not
  the navigation. Domain views exist but are for bulk/management.
- **Apple Home** — aggressively room-centric; accessory *type* only chooses the
  control widget (blind = slider, thermostat = dial). No "all lights" daily driver.
- **SmartThings / Google Home** — rooms primary, device tiles within, scenes/
  routines on top. **Homey** — hierarchical "Zones"; **Hubitat** — more
  device/admin-centric (power-user skew) but still room-grouped.

Takeaway: **crate-per-device-type is the right *backend* boundary and is
independent of the UI axis.** Keep it. Make rooms primary for control; demote
type-tabs to setup/management.

### Architecture decisions

- **Keep crate-per-capability** as the backend/data-model boundary — `lighting`,
  and future `blinds` (Z2M `cover`) + `hvac` (Z2M `climate`) — each owns its MQTT
  topics and command schema. This is honest 1:1 with the UI's per-type management
  surfaces.
- **Decouple room membership from lighting (the enabling refactor).** Rooms are
  currently lights-specific (`all_light_device_names`, `room_devices` assume light
  devices, room positions live on the lighting map). Generalise to a
  device-type-agnostic **room/device registry**: a room holds devices of *any*
  type; each device knows its crate/type. Do this **before blinds lands** so we
  don't deepen the lighting coupling and then have to unpick it.
- **Rooms/House tab becomes the *primary* control surface** (HA/Apple model): open
  "Living Room" → see its lights + blinds + climate, each rendered by its type's
  widget. **Per-type tabs (Lights, Blinds, HVAC) demote to setup/bulk**: pairing,
  naming, calibration, firmware, "all blinds down".
- **One widget per type, shared across both surfaces.** The room view and the
  type-tab must reuse the *same* per-type control component (one "blind control"
  used in both places). Building control logic twice is the thing to avoid.
- **Infrastructure ≠ control.** The Zigbee bridge/dongle, MQTT broker, and z2m are
  plumbing every device crate depends on — they have a *health state*, not buttons.
  They belong on **Health/Nodes**, never as a control tab and never as a fake peer
  in the node list.
- **Tab-sprawl rule.** Flat tabs (one per control domain) while control tabs ≤ ~5.
  When they exceed that, fold them under a single **Devices** hub with a left
  sub-nav (Lights / Blinds / Climate / …). Build the hub *then*, not now (YAGNI).
  Projected near-term set (~8, still readable): Nodes · Health · Chat · Online AI ·
  Lighting · Blinds · HVAC · REAPER.

### Concrete steps

- **Zigbee bridge health card** ✓ (2026-06-29) — first instance of the infra-vs-
  control split. A "Zigbee bridge" card on the Health tab shows Online / Offline /
  Unknown, driven by the existing `ZigbeeStatus` signal. Two backend fixes make it
  honest: (1) stub mode (`MQTT_HOST` unset) now *explicitly reports the bridge
  offline* (`capability-lighting` start); (2) the coordinator's stored status is
  now **tri-state** — `DashboardState::zigbee_status: Option<bool>` defaulting to
  `None` (Unknown) instead of the old `AtomicBool` default-`true`, so the card
  reads amber "Unknown" until a lighting node actually reports, rather than a
  misleading green "Online" when no node is connected at all. `ZigbeeStatus`
  (dashboard event) carries `Option<bool>` → serialises `null`; `rooms.js` is
  unaffected (its `inferZigbeeStatus` re-derives from device state each render).
  (3) On the lighting node's **disconnect**, the coordinator resets the status to
  `None` (`server.rs` cleanup → `reset_zigbee_status`, gated on
  `Registry::node_has_feature(id, "lighting")`), so a node that dies *silently*
  (TCP drop without first sending `offline`) no longer leaves a stale "Online".
  Together these close the blind spot where light commands vanished with nothing
  on the dashboard to explain why.
- **When blinds land** (`capability-blinds`, Z2M `cover`): join the *generalised*
  room model, not the lighting-specific path. New `cover` widget (position + tilt).
  Good moment to replace raw feature strings (`"lighting"`, `"reaper"`, soon
  `"cover"`/`"climate"`) with a feature **enum** — there'll finally be enough
  variants (`nodes_with_feature` / `node_has_feature` / capability registration)
  to justify the type safety. Premature while only lighting exists.
- **Defer the full room-centric view** until a second device type exists to justify
  it; until then don't over-invest in per-domain navigation a room view will
  replace.

### Open question (resolved)

Room-centric vs device-type-centric was the tension. **Decision: room-centric
primary for control, device-type for the data model + management** — matching HA
and Apple. The crate boundaries are unaffected by that choice.

---

## OpenAI-Compatible Inbound API ✓ Complete (2026-07-02)

The coordinator now speaks the OpenAI API inbound (`docs/openai-api.md`) — the
first productization step from `an internal productization plan` (mesh as a
drop-in private AI gateway).

- **`POST /v1/chat/completions`** (non-streaming) + **`GET /v1/models`** on the
  dashboard port; `Authorization: Bearer <mesh token>` (SDK-style) with
  `?token=` fallback; OpenAI error envelope with per-case codes
  (`model_not_found`, `stream_not_supported`, `no_model_ready`, …)
- **Pure chat semantics** — caller messages verbatim, no device-schema
  injection or tool execution (that stays on `/api/chat`); qwen `/no_think` /
  DeepSeek-R1 prefill quirks stay on the agent and respect caller turns
- **Model routing**: Ready local model → scheduler dispatch; gateway's
  selected model (enabled + configured) → cloud, no silent fallback; omitted
  model → largest ready local, else gateway
- **Wire v3**: `InferenceRequest` carries `messages: Vec<ChatTurn>` (roles
  serialize as OpenAI strings) replacing `system_prompt`+`prompt`, so
  llama-server applies real chat templates to multi-turn history;
  `InferenceResult` gains `prompt_tokens` for honest `usage` accounting;
  intent path + CLI build 2-turn arrays; `OpenAiCompatProvider::complete`
  takes the turns array; local dispatch extracted to
  `coordinator/src/inference.rs` (shared by intent + openai handlers)
- `Registry::ready_llm_models()`; `just openai <text> [model]` recipe;
  21 new tests (725 total)
- **Deploy note**: v2 agents fail fast on v3 frames — ship coordinator and all
  agents in one pass
- Next phase (not started): per-user API keys with usage attribution, and
  coordinator-side rate limiting alongside them (a limit needs a key identity
  to attach to)
- Deferred until a third model family lands: replace the hard-coded
  qwen/deepseek checks in `llama::build_messages` with a model-quirk registry
  (same premature-abstraction call as the feature enum in Phase 11.8)

---

## OpenAI API — SSE Streaming ✓ Complete (2026-07-02)

`stream: true` on `/v1/chat/completions` now returns OpenAI-spec SSE on both
local and cloud routes (`docs/openai-api.md` → Streaming).

- **Wire v4**: required `stream: bool` on `InferenceRequest`; new
  `ModelInferenceChunk` message (delta batches), terminated by the usual
  `ModelInferenceResult` carrying totals/error
- **Agent**: `llama::generate_stream` — dedicated no-total-timeout reqwest
  client (the shared 90s client kills long streams), incremental SSE parse via
  new pure `shared::sse` module, per-chunk idle timeout
  (`LLAMA_STREAM_IDLE_TIMEOUT_SECS`, default 300s to cover 14b prefill);
  forwarder drains before the terminal result so chunks never trail it
- **Coordinator**: `PendingStreams` map (cap 256, `try_send` — the agent read
  loop never blocks behind a slow SSE client; overflow kills that stream);
  disconnect teardown fails in-flight streams exactly like pending oneshots
- **HTTP**: spawned-emitter SSE (`tokio-stream` ReceiverStream + axum `Sse`),
  role-first chunk, shared `id`/`created`, `stream_options.include_usage`
  usage chunk, `[DONE]` sentinel; first-chunk 300s / inter-chunk 60s
  deadlines; **failure semantics**: node death / stall / overflow → one SSE
  error event + `[DONE]`, never a hang; client hang-up cancels generation on
  the node via `CancelInference` (found in live verification: without it the
  agent generated to completion for nobody, holding the inference slot — the
  demux now replies to any orphan chunk with a cancel and the agent aborts
  the task, freeing the slot within ~one token)
- **Cloud passthrough**: `OpenAiCompatProvider::complete_stream` (1h cap) +
  the same `shared::sse` parser; gateway stats recorded
- **Graceful degradation**: a terminal-only reply (pre-v4 agent) is emitted
  as a single-delta stream, so coordinator-first deploys stay safe
- **Deploy ordering: coordinator FIRST**, then agents (a v4 agent can't parse
  v3 requests; a v3 agent under a v4 coordinator degrades cleanly)
- 20 new tests (745 total); `just openai-stream` recipe
- **Post-deploy fix (verified live)**: client hang-up now cancels generation
  on the node via `CancelInference` — measured 54s → 924ms follow-up recovery;
  node-death mid-stream → error event + `[DONE]` in 9ms

---

## OpenAI API — Per-Key Auth, Usage Attribution & Rate Limiting (Next)

The third productization step (`an internal productization plan` item 3): one
shared mesh token is fine at home, but a client site needs per-user/team API
keys, per-key usage accounting, and per-key rate limits. This is the feature
that produces the monthly "here's what each team used, and what it saved you
versus cloud pricing" report — the ongoing-value justification.

Design sketch:

- **`api_keys` SQLite table** (registry DB): `key_hash` (SHA-256 — never store
  the raw key), `label` ("finance-team"), `created_ms`, `revoked` flag,
  optional `rate_limit_per_min`. Keys minted/revoked via new dashboard
  Security-tab UI + `/api/keys` CRUD (mesh-token-auth only, like every
  existing `/api/*` route).
- **/v1 auth resolution order**: mesh token (admin, as today) → `api_keys`
  lookup by hash. The resolved key label rides the request for attribution.
- **`usage_log` table**: `ts_ms`, `key_label`, `model`, `prompt_tokens`,
  `completion_tokens`, `duration_ms`, `served_by` (node / cloud), `stream`.
  Written once per completed request (terminal result / finish chunk) —
  both /v1 paths already have every field in hand.
- **Rate limiting**: fixed-window per key (requests/min) checked at the top of
  `chat_completions`; over-limit → the existing 429 `rate_limit_error`
  envelope. No token-bucket sophistication until a client needs it. Optional
  per-key token ceiling per calendar month alongside it (same check site,
  reads `usage_log`) — the "no runaway spend" guarantee in the pitch.
- **Reporting**: `GET /api/usage?from=&to=` aggregates per key/model/day;
  dashboard panel later — CSV/JSON export first (the monthly PDF is generated
  off-mesh). Include a cloud-equivalent-cost column (tokens × configurable
  per-model cloud price) for the money-saved line.
- **Non-goals for this phase**: multi-tenancy isolation (all keys see all
  models), per-key model allowlists, OAuth — all deferred until a real client
  asks.

---

## Phase 12 — Distributed Execution

- Multi-node inference
- Pipeline parallelism
- Tensor parallelism

---

## Phase 13 — Auto-scaling

- Dynamic node joining
- Cloud integration
