# Coordinator Documentation

## Responsibilities
- Accept **TLS-encrypted** TCP connections from agents and CLI clients
- Validate connection auth token (first-frame `AuthToken`) and HMAC-signed messages (Phase 10 / 10.5)
- Maintain a SQLite-backed registry of nodes (survives coordinator restarts)
- Track heartbeats and last-seen timestamps
- Store hardware and capability reports
- Route model load, inference, and lighting commands to the correct agent
- Respond to CLI queries
- Write `~/.config/ai-mesh/coordinator.state` on startup (TLS fingerprint + auth token, shell-sourceable)

## Architecture

### Connection Routing Map
Each agent connection registers its outbound `mpsc::Sender<MeshMessage>` in a shared `Arc<Mutex<HashMap<String, mpsc::Sender>>>` keyed by node ID. This enables the coordinator to push commands (e.g. `ModelLoad`) down to a specific agent's live TCP connection. Entries are removed when the socket drops.

### Scheduler
`coordinator::Scheduler` has two selection methods:

**`select_node_for_model(size_mb: u64)`** — finds a Compute node with enough remaining capacity to load a new model:
- Role must be `Compute`
- Remaining capacity: `max_model_size_gb * 1024 - allocated_mb >= size_mb`
- `Ready` and `Loading` allocations count against capacity; `Unloaded` and `Failed` do not

**`select_node_for_inference(model_name: &str)`** — finds a Compute node that already has the named model in `Ready` state:
- Role must be `Compute`
- `models` must contain an entry for `model_name` with `state == Ready`
- `Loading`, `Unloaded`, and `Failed` states are excluded
- Used by the `RequestModelInference` handler in `server.rs`

## Security Layers

Each inbound connection passes through three layers before any message is processed:

1. **TLS** — all connections are TLS-wrapped (`tokio-rustls`). Agents verify the coordinator cert by SHA-256 fingerprint.
2. **AuthToken first-frame** — when `MESH_AUTH_TOKEN` is configured, the first frame must be a plain (unsigned) `AuthToken(token)`. Wrong or missing token → connection closed.
3. **HMAC SignedFrame** — all subsequent frames must be `SignedFrame { ts, payload, sig }`. The coordinator verifies HMAC-SHA256 (key derived via HKDF from the auth token) and rejects frames with invalid signatures or timestamps older than ±30s. Stale-frame log messages include a "check NTP" hint.

## Message Handling

| Message | Action |
|---------|--------|
| `Heartbeat` | Updates last-seen; validates per-heartbeat `auth_token`; registers agent tx in connections map |
| `HardwareReport` | Stores hardware specification for the node |
| `Capabilities` | Stores capability information for the node |
| `RequestNodes` | Returns `NodeList(Vec<NodeRecordLite>)` |
| `RequestNodeInfo(id)` | Returns `NodeInfo(NodeRecordFull)` including model allocations |
| `ModelLoad` | Looks up target agent tx in connections map; forwards command |
| `ModelStatus` | Calls `registry.update_model_status()`; updates allocation state |
| `ModelUnload` | Forwards to target agent via connections map; agent kills llama-server |
| `RequestModelInference` | Runs `select_node_for_inference`; forwards to agent; awaits result via oneshot channel (up to 300 s) |
| `LightCommand` | Forwards to the registered lighting node's tx channel |
| `LightState` | Stores device state in registry |
| `LightDeviceList` | Persists device/group names to SQLite; injected into LLM system prompt |
| `SceneLoad` | Forwards to lighting node |
| `SceneLoaded` | Stored; forwarded to pending intent response if applicable |
| `IntentRequest` | Selects largest ready LLM node; sends `RequestModelInference`; handles tool-call dispatch (lighting, etc.) |
| `IntentResponse` | Resolved from pending oneshot; returned to CLI |
| `Admin(ResetRegistry)` | Clears all nodes from the registry |

## Registry Methods

### list_nodes()
Returns lightweight summaries (`NodeRecordLite`) for all nodes.

### get_node_full(id)
Returns full diagnostic information including hardware, capabilities, and model allocations.

### update_model_status(id, model_name, size_mb, state)
Inserts or updates a `ModelAllocation` entry for the given node. Used to track which models are loaded and in what lifecycle state.

### eligible_compute_nodes()
Returns all `NodeRecordFull` records for nodes with role `Compute`.

## Pending Inference Map

When the coordinator forwards a `RequestModelInference` to an agent it inserts a `oneshot::Sender` into `PendingInferences`:

```rust
Arc<Mutex<HashMap<String, (oneshot::Sender<MeshMessage>, String)>>>
//                                                         ^node_id
```

The node_id is stored alongside the sender so that when an agent disconnects the coordinator can fast-fail any outstanding requests routed to that agent. Without this, the CLI would wait the full `GENERATE_TIMEOUT_SECS` (300 s) before seeing an error.

**Fast-fail on disconnect** — when an agent's TCP socket closes, the connection-handler task locks the pending map, finds all entries whose node_id matches the disconnected agent, removes them, and sends an `Error("agent disconnected")` on each `oneshot`. The CLI receives the error immediately.

## Concurrency Model
- Registry is protected by a `Mutex`
- Connections map is protected by a separate `Mutex`
- Pending inferences map is protected by a separate `Mutex`
- Locks are never held across `.await`
- Each TCP connection runs its own reader loop and a dedicated writer task draining an `mpsc` channel
