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
- Signed wire messages (HMAC) — deferred, optional defence-in-depth

---

## Phase 11 — Web Dashboard

- Live mesh view
- Node health monitoring
- Model deployment UI

---

## Phase 12 — Distributed Execution

- Multi-node inference
- Pipeline parallelism
- Tensor parallelism

---

## Phase 13 — Auto-scaling

- Dynamic node joining
- Cloud integration
