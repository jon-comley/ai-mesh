# Phase 11D — Model Management Panel

**Goal:** A live Models tab in the dashboard showing per-node model state with load/unload buttons and capacity bars.

Reviewed by Bing + Gemini. All design decisions finalised before coding.

---

## Architecture decisions

| Question | Decision | Rationale |
|---|---|---|
| How does model data reach the dashboard? | New `DashboardEvent::ModelUpdate` WS event | Targeted — only fires on model state change; TopologyUpdate stays lightweight |
| Where is `NodeModelInfo` defined? | `coordinator/src/http/state.rs` (coordinator-internal) | Only coordinator + JS need it; no reason to put it in `shared` |
| Where is model snapshot stored? | `Mutex<Vec<NodeModelInfo>>` in `DashboardState` | Same pattern as HealthStore; enables snapshot-on-connect |
| HTTP verbs for load/unload? | `POST /api/models/load` and `POST /api/models/unload` | Consistent with heartbeat-interval; avoids DELETE routing complexity |
| How does load reach the agent? | `send_to_node(node_id, MeshMessage::ModelLoad)` via NodeConnections | Agent responds with `ModelStatus` via TCP → coordinator updates registry normally |
| Capacity bar data source? | Static ceiling: `HardwareSpec.ram_gb` + VRAM total from latest `HealthSample`; live usage from latest `HealthSample` | VRAM total comes from driver (heartbeat payload), not HardwareSpec |
| Cross-module health data access? | Export `getLatestSample(nodeId)` from `health.js` | models.js reads live usage without duplicating samplesMap |
| Model dropdown contents? | Hardcoded list of 5 known models | Stable set; free-text input adds no value for this cluster |
| Unload safety? | `window.confirm()` before sending | Prevents accidental unloads |

---

## Wire protocol

### New `DashboardEvent::ModelUpdate`

```json
{
  "type": "ModelUpdate",
  "nodes": [
    {
      "node_id": "550e8400-...",
      "hostname": "beelink1",
      "role": "Compute",
      "ram_gb": 32.0,
      "gpu_vram_total_gb": 4.0,
      "models": [
        { "name": "qwen2.5:7b", "size_mb": 4000, "state": "Ready" }
      ]
    },
    {
      "node_id": "661f9511-...",
      "hostname": "pi1",
      "role": "Compute",
      "ram_gb": 8.0,
      "gpu_vram_total_gb": null,
      "models": [
        { "name": "qwen2.5:1.5b", "size_mb": 1000, "state": "Loading" }
      ]
    }
  ]
}
```

`state` values: `"Ready"`, `"Loading"`, `"Failed"` (with optional `reason` field), `"Unloaded"` (omitted from list).

### Rust types (coordinator/src/http/state.rs)

```rust
#[derive(Clone, Serialize)]
pub struct NodeModelInfo {
    pub node_id:          String,
    pub hostname:         String,
    pub role:             String,
    pub ram_gb:           f32,
    pub gpu_vram_total_gb: Option<f32>,
    pub models:           Vec<ModelEntry>,
}

#[derive(Clone, Serialize)]
pub struct ModelEntry {
    pub name:     String,
    pub size_mb:  u64,
    pub state:    String,   // serialised from ModelLifecycleState
    pub reason:   Option<String>,  // set when state == "Failed"
}
```

`DashboardEvent` gains a third variant:

```rust
ModelUpdate {
    nodes: Vec<NodeModelInfo>,
}
```

---

## HTTP API

### POST /api/models/load

```json
{ "node_id": "550e8400-...", "model_name": "qwen2.5:7b", "size_mb": 4000 }
```

Responses: `200 OK` · `400 Bad Request` (missing/invalid fields) · `401 Unauthorized` · `404` (node not connected)

### POST /api/models/unload

```json
{ "node_id": "550e8400-...", "model_name": "qwen2.5:7b" }
```

Responses: `200 OK` · `400` · `401` · `404`

Both endpoints authenticate via `?token=` query param (same pattern as heartbeat-interval).

---

## Sub-phases

### D1 — Data pipeline: ModelUpdate event + snapshot

**Goal:** Coordinator broadcasts `ModelUpdate` when model state changes; new WS clients receive a snapshot on connect.

Files to change:

1. **`coordinator/src/http/state.rs`**
   - Add `NodeModelInfo` and `ModelEntry` structs
   - Add `DashboardEvent::ModelUpdate { nodes: Vec<NodeModelInfo> }`
   - Add `model_snapshot: Mutex<Vec<NodeModelInfo>>` field to `DashboardState`
   - Add `push_model_update(nodes: Vec<NodeModelInfo>)` — stores snapshot + broadcasts event
   - Add `get_model_snapshot() -> Vec<NodeModelInfo>` — returns point-in-time copy

2. **`coordinator/src/server.rs`**
   - After every `registry.update_model_status(...)` call: build `Vec<NodeModelInfo>` from registry and call `dashboard.push_model_update(...)`
   - After `registry.update_hardware(...)` call: same (VRAM total may have just arrived)
   - Helper fn `build_model_snapshot(registry) -> Vec<NodeModelInfo>` — iterates `NodeRecordFull` list, maps to `NodeModelInfo`, skips `Unloaded` models

3. **`coordinator/src/http/ws.rs`**
   - On new WS client connect: send `ModelUpdate` snapshot (same pattern as health snapshot)

**Tests to add (state.rs):**
- `push_model_update_stores_snapshot`
- `push_model_update_broadcasts_event`
- `get_model_snapshot_is_point_in_time_copy`
- `push_model_update_with_no_receivers_is_noop`
- `model_update_event_wire_format` (asserts JSON shape)

---

### D2 — HTTP API: load and unload

**Goal:** `POST /api/models/load` and `POST /api/models/unload` send the appropriate `MeshMessage` to the target node.

Files to change:

1. **`coordinator/src/http/api.rs`**
   - Add `LoadModelBody { node_id: String, model_name: String, size_mb: u64 }`
   - Add `UnloadModelBody { node_id: String, model_name: String }`
   - Add `load_model(...)` handler → `send_to_node(node_id, MeshMessage::ModelLoad { model_name, size_mb })`
   - Add `unload_model(...)` handler → `send_to_node(node_id, MeshMessage::ModelUnload { model_name })`
   - Auth + 400/404 handling identical to `set_heartbeat_interval`

2. **`coordinator/src/http/mod.rs`**
   - Add routes: `POST /api/models/load` and `POST /api/models/unload`

**Tests to add (api.rs):**
- `load_model_ok_queues_message`
- `load_model_returns_404_for_unknown_node`
- `load_model_returns_401_for_wrong_token`
- `load_model_returns_400_for_missing_fields`
- `unload_model_ok_queues_message`
- `unload_model_returns_404_for_unknown_node`

---

### D3 — Dashboard: models.js

**Goal:** Models tab renders per-node cards with live capacity bars, model state badges, Unload buttons, and a Load form.

Files to change:

1. **`coordinator/src/http/static/health.js`**
   - Export `getLatestSample(nodeId)` — returns the last `HealthSample` for a node or `null`

2. **`coordinator/src/http/static/models.js`** (new file)
   - `handleModelUpdate(evt)` — stores `nodesMap: Map<nodeId, NodeModelInfo>`, calls `renderModelsPanel()`
   - `renderModelsPanel()` — renders one card per compute node (skips Controller nodes)
   - Each card:
     - Node name + role badge
     - RAM capacity bar: `ram_used_gb / ram_total_gb` from latest HealthSample; ceiling from `node.ram_gb`
     - VRAM capacity bar (hidden when `gpu_vram_total_gb == null`): `gpu_vram_used_gb / gpu_vram_total_gb` from latest HealthSample
     - Model list: name, size badge, state badge (green=Ready, amber=Loading, red=Failed), Unload button
     - Load form: model dropdown (5 known models) + Load button
   - `promptUnload(nodeId, modelName)` — `window.confirm()` then `POST /api/models/unload`
   - `doLoad(nodeId, modelName, sizeMb)` — `POST /api/models/load`; button disabled while in-flight
   - On `HealthUpdate`: call `renderModelsPanel()` to refresh capacity bars

3. **`coordinator/src/http/static/dashboard.js`**
   - Import `* as models from '/static/models.js'`
   - Add `ModelUpdate` handler: `models.handleModelUpdate(evt)`
   - Add to `HealthUpdate` handler: `models.onHealthUpdate()`

4. **`coordinator/src/http/mod.rs`**
   - Add `MODELS_JS: &str = include_str!(...)` and serve at `/static/models.js`

**No new Rust tests for JS behaviour** — content-type test for `models.js` follows same pattern as `health_js_returns_correct_content_type`.

---

## Test plan

### Rust unit tests (target: ~12 new tests, total ~331)
- D1: 5 tests in `state.rs`
- D2: 6 tests in `api.rs`
- D1 (mod.rs): 1 content-type test for `models.js`

### Live validation
1. `just restart-coordinator` → open dashboard → Models tab
2. Confirm each compute node card appears with RAM bar
3. beelink1: confirm VRAM bar visible; pi1: confirm VRAM bar hidden
4. Load `qwen2.5:1.5b` on pi1 via dashboard → badge shows Loading → Ready
5. Unload it → confirm dialog → model disappears from card
6. Attempt load on disconnected node → `Failed: HTTP 404`
