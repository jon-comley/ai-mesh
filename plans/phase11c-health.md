# Phase 11C — Health Timeline

**Goal:** Add per-node CPU% and RAM% sparklines to the Health panel, fed by real-time
heartbeat data. Also add dynamic per-node heartbeat interval control via the dashboard
and CLI.

Reviewed by Bing + Gemini post-Phase B. All design decisions finalised.

---

## Architecture decisions

| Question | Decision | Rationale |
|---|---|---|
| Timestamp source | Coordinator stamps `HealthSample` on arrival | Agent clocks can drift; coordinator is the timeline authority |
| RAM% derivation | Wire carries `ram_used_gb` + `ram_total_gb`; coordinator computes `ram_pct` | Frontend stays dumb; wire stays flexible |
| HealthUpdate payload | Full 60-sample window per node per heartbeat | Frontend redraws from scratch — no client-side ring buffer needed |
| HealthStore location | Inside `DashboardState` | Health samples are an ephemeral dashboard concern, never persisted |
| Heartbeat interval scope | Per-node | Different nodes may benefit from different rates |
| Interval trigger | CLI script + dashboard button | Both paths push via coordinator over existing connection |
| Coordinator → agent | `MeshMessage::SetHeartbeatInterval { secs: u64 }` | Coordinator pushes; agent applies immediately |
| GPU% | Deferred to Phase C+ | AMD + NVIDIA require platform-specific code; not blocking for MVP |
| HTTP API style | `POST /api/nodes/{id}/heartbeat-interval` | Fire-and-forget; no success feedback needed for Phase C |

---

## Wire protocol additions (`shared` crate)

### `HeartbeatPayload` — new optional fields

```rust
pub struct HeartbeatPayload {
    #[serde(flatten)]
    pub identity: NodeIdentity,
    pub auth_token: String,
    // Runtime health metrics — None for agents that haven't been updated yet.
    #[serde(default)]
    pub cpu_usage_pct: Option<f32>,   // 0.0–100.0, all-core average
    #[serde(default)]
    pub ram_used_gb: Option<f32>,
    #[serde(default)]
    pub ram_total_gb: Option<f32>,
}
```

All fields are `Option` with `#[serde(default)]` so old agents sending heartbeats
without these fields continue to work unchanged.

### New message variant

```rust
// Coordinator → agent only. Agent applies immediately.
SetHeartbeatInterval { secs: u64 },
```

Valid range enforced by coordinator before sending: 1–300 seconds.

---

## Coordinator-side structs (`coordinator` crate)

### `HealthSample` — coordinator only, never on the wire

```rust
#[derive(Clone, Serialize)]
pub struct HealthSample {
    pub ts_secs: u64,   // coordinator wall-clock seconds (not agent time)
    pub cpu_pct: f32,
    pub ram_pct: f32,   // derived: (ram_used_gb / ram_total_gb) * 100.0
}
```

### `HealthStore` inside `DashboardState`

```rust
pub struct DashboardState {
    pub tx: broadcast::Sender<DashboardEvent>,
    pub auth_tokens: Arc<Vec<String>>,
    // Per-node ring buffer. Capped at HEALTH_RING_CAPACITY samples.
    health: Mutex<HashMap<String, VecDeque<HealthSample>>>,
}

const HEALTH_RING_CAPACITY: usize = 60; // ~5 min at 5 s heartbeat
```

New methods:

```rust
impl DashboardState {
    /// Record a new sample for node_id and return the full window.
    pub fn record_health(&self, node_id: &str, sample: HealthSample) -> Vec<HealthSample>;

    /// Push a HealthUpdate event if there are receivers.
    pub fn push_health(&self, node_id: &str, name: &str, samples: Vec<HealthSample>);
}
```

### New `DashboardEvent` variant

```rust
#[derive(Clone, Serialize)]
#[serde(tag = "type")]
pub enum DashboardEvent {
    TopologyUpdate { nodes: Vec<NodeDashInfo> },
    HealthUpdate { node_id: String, name: String, samples: Vec<HealthSample> },
}
```

---

## Agent changes (`agent` crate)

### Dependency

```toml
sysinfo = "0.31"
```

### Metrics collection

On every heartbeat tick, before sending:

```rust
let mut sys = System::new();
sys.refresh_cpu_usage();
sys.refresh_memory();
let cpu_pct = sys.global_cpu_usage();
let ram_used = sys.used_memory() as f32 / 1_073_741_824.0; // bytes → GB
let ram_total = sys.total_memory() as f32 / 1_073_741_824.0;
```

The `System` instance should be kept alive across heartbeats (not re-created each time)
so sysinfo can compute accurate CPU deltas.

### Dynamic interval

The agent heartbeat loop currently sleeps a fixed duration. After Phase C it will:

1. Start with default `HEARTBEAT_SECS` (currently 5)
2. After sending each heartbeat, check for an incoming `SetHeartbeatInterval` message
3. If received, update the local sleep duration immediately
4. Persist the new interval in memory for the lifetime of the connection

```rust
// Pseudocode — actual implementation uses tokio::select!
loop {
    send_heartbeat(&mut stream, &payload).await;
    match timeout(Duration::from_secs(interval), recv_message(&mut stream)).await {
        Ok(Ok(MeshMessage::SetHeartbeatInterval { secs })) => interval = secs.clamp(1, 300),
        Ok(Ok(other_msg)) => handle(other_msg),
        Err(_timeout) => {} // normal — just time to send next heartbeat
    }
}
```

---

## Coordinator HTTP additions

### New route

```
POST /api/nodes/{id}/heartbeat-interval
Content-Type: application/json

{ "secs": 10 }
```

**Handler logic:**
1. Parse and validate `secs` (1–300)
2. Look up the node's active `mpsc::Sender<MeshMessage>` in `connections`
3. Send `MeshMessage::SetHeartbeatInterval { secs }` on it
4. Return `200 OK` if node is connected, `404` if not found, `400` if out of range

The `connections` map (`HashMap<String, mpsc::Sender<MeshMessage>>`) is already
maintained in `server.rs` — this just adds a read path from the HTTP layer.

---

## Frontend (`health.js`)

### Init and event dispatch

```js
export function init(el) { container = el; }
export function handleEvent(evt) {
  if (evt.type === 'HealthUpdate') updateNode(evt);
}
```

### Node card structure

```
┌─ BEELINK1 ────────────────────────────────────┐
│  CPU  ████████░░░░░░░░░░░░  42%  ▁▂▄▆▅▃▂▁▃▅  │
│  RAM  ████████████░░░░░░░░  61%  ▂▂▃▃▃▄▄▄▄▄  │
└───────────────────────────────────────────────┘
```

SVG sparkline: `<polyline>` of normalised sample values, last 60 points,
scaled to a fixed viewport (e.g. 120×24 px). One sparkline per metric per node.

### Interval control

Each node card gets an interval button:
```
[↻ 5s ▾]  → dropdown or prompt: 1 / 2 / 5 / 10 / 30 / 60 s
```

On change: `POST /api/nodes/{id}/heartbeat-interval` with `{ "secs": N }`.

---

## CLI additions (`cli` crate)

New subcommand:

```
mesh set-heartbeat <node-hostname-or-id> <secs>
```

Calls `POST /api/nodes/{id}/heartbeat-interval` on the coordinator HTTP port.

### Justfile recipe

```just
set-heartbeat node secs:
    #!/usr/bin/env bash
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then source "$STATE"; fi
    HTTP_PORT="${MESH_HTTP_PORT:-9001}"
    # Resolve node ID from hostname if needed
    NODE_ID=$(cargo run -q -p cli -- nodes --json | jq -r \
        ".[] | select(.name == \"{{node}}\" or .id == \"{{node}}\") | .id")
    curl -s -X POST "http://localhost:${HTTP_PORT}/api/nodes/${NODE_ID}/heartbeat-interval" \
         -H "Content-Type: application/json" \
         -d '{"secs": {{secs}}}'
```

---

## Implementation order

| Step | Crate | What |
|---|---|---|
| C1 | `shared` | Add optional fields to `HeartbeatPayload`; add `SetHeartbeatInterval` message; add `HealthSample` |
| C2 | `agent` | Add `sysinfo`; collect CPU% + RAM; populate heartbeat fields; handle `SetHeartbeatInterval` in loop |
| C3 | `coordinator` | `HealthSample` struct; `HealthStore` inside `DashboardState`; `record_health` + `push_health`; wire into `process_message` |
| C4 | `coordinator` | `DashboardEvent::HealthUpdate`; new `/api/nodes/{id}/heartbeat-interval` POST route; expose `connections` map to HTTP layer |
| C5 | frontend | `health.js`: per-node cards with SVG sparklines; interval control button; wire into `dashboard.js` dispatch table |
| C6 | `cli` + justfile | `mesh set-heartbeat` subcommand; `just set-heartbeat` recipe |

---

## Tests to add

| Layer | Test |
|---|---|
| `shared` | `HeartbeatPayload` with all new fields roundtrips; old payload without new fields deserialises with `None` defaults |
| `shared` | `SetHeartbeatInterval` serialises/deserialises correctly |
| `coordinator` | `record_health` caps at 60 samples; oldest evicted first |
| `coordinator` | `ram_pct` derived correctly; zero `ram_total_gb` doesn't panic |
| `coordinator` | `push_health` no-op with no receivers |
| `coordinator` | HTTP `POST /api/nodes/{id}/heartbeat-interval` returns 404 for unknown node, 400 for out-of-range secs, 200 for valid |

---

## Deferred (Phase C+)

- **GPU%** — AMD: `/sys/class/drm/card*/device/gpu_busy_percent`; NVIDIA: NVML via `nvml-wrapper`; Apple: IOKit. Add `gpu_usage_pct: Option<f32>` to `HeartbeatPayload` and `gpu_pct: Option<f32>` to `HealthSample` once at least one platform is supported.
- **Heartbeat jitter** — measure deviation from expected interval; surface in Health panel as a stability indicator.
- **Interval change acknowledgement** — coordinator confirms the agent applied the new interval; dashboard updates the button label only on confirmation.
- **Persistent interval** — survive agent restarts (write to agent config file).
