# Modular Capability Architecture — Implementation Plan

**Branch:** `feat/modular-capability-architecture`  
**Date:** 2026-05-21  
**Status:** Planning — no code written yet

---

## Goal

Extend ai-mesh agents from "LLM-only" to a pluggable capability system where each node runs a different set of features selected at build time via Cargo feature flags. The first two capabilities are:

- `llm` — current llama-server integration (all compute nodes)
- `lighting` — Zigbee2MQTT-based smart lighting control (pi1 only)

**Hard constraint:** The OmniBook7 runs coordinator only — no agent, no capabilities.

---

## Current Architecture

```
agent/src/main.rs         — monolithic: connect loop + message dispatch + inline LLM handling
agent/src/llama.rs        — llama-server HTTP client, model pull, inference, process management
agent/src/agent.rs        — heartbeat loop
agent/src/capabilities.rs — hardware → NodeCapabilities (unrelated to this plan)
shared/src/messages.rs    — MeshMessage enum (all wire types)
```

`main.rs` currently has inline `match` arms for `ModelLoad`, `ModelUnload`, `RequestModelInference` that call directly into `llama::*`. The goal is to move this logic into a `capabilities/llm/` crate and dispatch via a trait object.

---

## Proposed Workspace Structure

```
Cargo.toml                       — workspace (add capabilities/core, capabilities/llm, capabilities/lighting)
shared/                          — wire types, hardware specs
capabilities/
  core/                          — Capability trait (depends on shared)
  llm/                           — LLM capability (depends on capabilities/core, shared)
  lighting/                      — Lighting capability (depends on capabilities/core, shared)
agent/                           — thin shell: connect loop + capability dispatch
  Cargo.toml                     — [features]: llm, lighting; optional deps on capability crates
coordinator/                     — no change until Phase 0 (intent routing)
cli/                             — no change until Phase 0 (mesh intent command)
nodes/
  pi1.env                        — NODE_FEATURES=llm,lighting
  beelink1.env                   — NODE_FEATURES=llm
```

---

## The Capability Trait

Native Rust 1.75 `async fn in trait` is not object-safe with `dyn Trait`, so we use the `async-trait` crate to box futures and enable `dyn Capability`.

```rust
// capabilities/core/src/lib.rs
use async_trait::async_trait;
use shared::MeshMessage;
use tokio::sync::mpsc::Sender;

#[async_trait]
pub trait Capability: Send + Sync {
    /// Short identifier shown in logs (e.g. "llm", "lighting").
    fn name(&self) -> &'static str;

    /// Returns true if this capability wants to handle this inbound message.
    /// Called on every inbound message — must be cheap (no I/O).
    fn handles(&self, msg: &MeshMessage) -> bool;

    /// Long-running background task (event loops, polling).
    /// Spawned once at startup outside the reconnect loop.
    /// Returns Err if the capability cannot start (logged; agent continues without it).
    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String>;

    /// Handle one inbound MeshMessage routed to this capability.
    /// Called after `handles()` returns true.
    async fn handle(&self, msg: MeshMessage, tx: Sender<MeshMessage>);

    /// Tool schemas exposed to the intent router. Default: empty.
    /// Each entry describes one callable action in JSON Schema format.
    fn tools(&self) -> Vec<ToolSchema> { vec![] }
}

pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,  // JSON Schema object
}
```

**Why `handles()` + `handle()` rather than a single returning bool:** `handles()` is a cheap sync check; separating it means the routing loop never pays async overhead for messages that don't match, and routing can be tested independently of execution.

**Why `start` returns `Result`:** A capability that can't initialise (MQTT broker down, llama-server binary missing) should surface that clearly so the agent logs it rather than silently doing nothing.

**Why `start` is separate from `handle`:** The lighting capability needs a persistent MQTT event loop that fires on Zigbee device events and sends `MeshMessage::LightState(...)` to the coordinator unprompted. `start` runs that loop. `handle` handles coordinator-initiated commands. LLM has no background loop — its `start` returns `Ok(())` immediately.

---

## New Message Types (shared crate, Phase 1)

### Lighting wire types

```rust
// New MeshMessage variants:
MeshMessage::LightCommand(LightCommandRequest)  // coordinator → lighting node
MeshMessage::LightState(LightStateReport)       // lighting node → coordinator
MeshMessage::SceneLoad(SceneLoadRequest)        // coordinator → lighting node
MeshMessage::SceneLoaded(SceneLoadedReport)     // lighting node → coordinator

pub struct LightCommandRequest {
    pub request_id: String,
    pub target: LightTarget,
    pub command: LightAction,
}

pub struct LightStateReport {
    pub node_id: String,
    pub device_id: String,
    pub on: bool,
    pub brightness: Option<u8>,
    pub color_xy: Option<(f32, f32)>,
    pub color_temp: Option<u16>,
}

pub struct SceneLoadRequest {
    pub request_id: String,
    pub scene_name: String,
    pub transition_ms: u32,
}

pub struct SceneLoadedReport {
    pub request_id: String,
    pub scene_name: String,
    pub success: bool,
    pub error: Option<String>,
}

pub enum LightTarget {
    Group(u16),
    Device(String),  // Zigbee IEEE address
}

pub enum LightAction {
    On,
    Off,
    Toggle,
    Brightness(u8),
    ColorXY { x: f32, y: f32 },
    ColorTemp(u16),  // mireds
}
```

### Intent wire types

```rust
// New MeshMessage variants:
MeshMessage::IntentRequest(IntentRequest)
MeshMessage::IntentResponse(IntentResponse)

pub struct IntentRequest {
    pub request_id: String,
    pub text: String,
    pub model_name: Option<String>,     // None = coordinator picks best available
    pub context: Vec<IntentTurn>,       // prior turns for multi-turn conversation
}

pub struct IntentTurn {
    pub role: IntentRole,
    pub content: String,
}

pub enum IntentRole { User, Assistant }

pub struct IntentResponse {
    pub request_id: String,
    pub node_id: String,
    pub text: Option<String>,           // free-text answer or post-tool summary
    pub tool_calls: Vec<ToolCall>,
    pub error: Option<String>,
}

pub struct ToolCall {
    pub tool: String,                   // e.g. "light_command", "scene_load"
    pub args: serde_json::Value,
    pub result: Option<String>,         // "ok" or error from the capability
}
```

---

## Phase 0: Intent Routing

**Scope:** A general-purpose natural language entry point. The LLM stays fully usable for general queries — intent routing is additive, triggered only by `mesh intent`.

### The Problem

Without this layer, the user must know the exact API:
```
mesh infer qwen2.5:7b "What is the capital of France?"
```

With intent routing:
```
mesh intent "dim the living room lights to something warm"
mesh intent "explain how TCP keepalive works"
mesh intent "turn everything off and make it bright in the kitchen"
```

The LLM decides whether a tool call is needed or whether to answer in free text.

### Architecture

The intent router lives in the **coordinator** so any client (CLI, future web UI, voice) gets the same routing without duplicating logic.

```
User text
    │
    ▼  mesh intent "<text>"
    │
    ▼  MeshMessage::IntentRequest
Coordinator
    │  assembles tool schema from registered capability nodes
    │  routes to best available LLM node with injected system prompt
    ▼  MeshMessage::RequestModelInference
LLM node (pi1 / beelink1)
    │
    ▼  MeshMessage::ModelInferenceResult  (free text OR tool-call JSON)
Coordinator
    │
    ├─ free text ──────────────────────────────► IntentResponse.text → caller
    │
    └─ tool call JSON ─────────────────────────► dispatch LightCommand / SceneLoad
           │
           ▼
       Capability node (e.g. pi1 lighting)
           │
           ▼  LightState / SceneLoaded
       Coordinator
           │
           ▼  IntentResponse (tool_calls filled, optional LLM summary)
       Caller
```

### Tool Schema

Each capability self-describes its tools via `fn tools()` on the `Capability` trait (defined in the trait above). The coordinator assembles tool schemas from all nodes at intent time — adding a new capability automatically extends what the LLM knows it can do.

**LLM capability** — `tools()` returns empty (it is the reasoning engine, not a tool target).

**Lighting capability** — `tools()` returns:
```json
[
  {
    "name": "light_command",
    "description": "Turn lights on/off, set brightness or colour temperature",
    "parameters": {
      "type": "object",
      "properties": {
        "target": { "type": "string", "description": "Room or device name, e.g. 'living_room'" },
        "action": { "type": "string", "enum": ["on", "off", "toggle", "brightness", "color_temp"] },
        "value":  { "type": "number", "description": "Brightness 0–255 or colour temp in Kelvin" }
      },
      "required": ["target", "action"]
    }
  },
  {
    "name": "scene_load",
    "description": "Load a named lighting scene (e.g. 'cozy', 'bright', 'movie')",
    "parameters": {
      "type": "object",
      "properties": {
        "scene": { "type": "string" },
        "room":  { "type": "string", "description": "Optional — omit to apply everywhere" },
        "transition_ms": { "type": "integer" }
      },
      "required": ["scene"]
    }
  }
]
```

### System Prompt Injected by Coordinator

```
You are a smart home and general-purpose assistant.

You have access to the following tools:
<tool_schema_json>

If the user's request maps to a tool, respond with ONLY a JSON object:
{"tool": "<name>", "args": { ... }}

If the user's request is a general question or conversation, respond normally in free text.
Do not explain your reasoning. Do not use markdown for tool calls.
```

A clear system prompt is more reliable than relying on OpenAI-style tool-call tokens across all qwen2.5 model sizes.

### Coordinator Intent Handler (new: `coordinator/src/intent.rs`)

```rust
pub async fn handle_intent(
    request: IntentRequest,
    registry: &NodeRegistry,
    tool_schemas: &[ToolSchema],
) -> IntentResponse {
    // 1. Build system prompt with assembled tool schemas
    // 2. Route InferenceRequest to best LLM node
    // 3. Receive InferenceResult
    // 4. Try to parse result as JSON tool call
    // 5a. Free text  → return as IntentResponse.text
    // 5b. Tool call  → dispatch capability message to appropriate node
    //                → wait for LightState / SceneLoaded response
    //                → optional: second LLM call for human-readable summary
    //                → return IntentResponse with tool_calls filled
}
```

The second LLM summary pass is opt-in (env flag) — skip for latency-sensitive use, enable for voice where "OK" is not enough.

### New CLI Command

```
mesh intent "<text>"
```

Tool call output:
```
[scene_load] cozy → living_room (2000ms fade)
Done — living room set to cozy mode.
```

Free-text output:
```
TCP keepalive is a mechanism that...
served-by: <node-uuid> | qwen2.5:7b | 312 tokens | 4821ms
```

### Implementation Order

Phase 0 is implemented in parts across the other phases:
1. Add `IntentRequest`/`IntentResponse` wire types in Phase 1 alongside lighting types
2. Implement `coordinator/src/intent.rs` after Phase 3 (capability dispatch working)
3. Add `tools()` schemas to lighting capability in Phase 5
4. Add `mesh intent` CLI command after Phase 5

---

## Phase 1: Core Crate + Wire Types

**Scope:** No behaviour change. Wiring up the new structure.

### Steps

1. **Add `capabilities/core/` crate**
   - `Cargo.toml`: `[lib]`, depends on `shared`, `async-trait`, `tokio`, `serde_json`
   - `src/lib.rs`: `Capability` trait + `ToolSchema` as defined above
   - Add to workspace `Cargo.toml`

2. **Add `features` field to `NodeCapabilities` in `shared`**
   ```rust
   pub struct NodeCapabilities {
       pub cpu_inference: bool,
       pub gpu_inference: bool,
       pub ane_inference: bool,
       pub max_model_size_gb: f64,
       pub features: Vec<String>,   // e.g. ["llm", "lighting"]
   }
   ```
   The agent populates this from active Cargo features at startup. The coordinator uses it to identify which nodes have the `lighting` capability when routing `LightCommand` from the intent router.

3. **Add lighting + intent message types to `shared/src/messages.rs`**
   - All structs and `MeshMessage` variants as designed above
   - Round-trip serialisation tests for each new type

4. **Bump `WIRE_VERSION` from `1` to `2`**
   - All nodes are redeployed together so no compatibility concern — just bump and redeploy.

**Tests to add:**
- Round-trip `serde_json` for each new message type
- `LightAction::ColorXY` round-trips with correct float values
- `NodeCapabilities` with `features` field round-trips correctly

**Validation:** `cargo test -p shared` green. No agent or coordinator changes yet.

---

## Phase 2: Extract LLM into capabilities/llm

**Scope:** Move all LLM-specific code from `agent/src/` into `capabilities/llm/`.

### Steps

1. **Create `capabilities/llm/` crate**
   - `Cargo.toml`: depends on `capabilities-core`, `shared`, `reqwest`, `futures-util`, `tokio`, `tracing`, `dirs`, `thiserror`
   - Move `agent/src/llama.rs` → `capabilities/llm/src/llama.rs` (unchanged)
   - Create `capabilities/llm/src/lib.rs` with `LlmCapability` implementing `Capability`:
     ```rust
     pub struct LlmCapability;

     #[async_trait]
     impl Capability for LlmCapability {
         fn name(&self) -> &'static str { "llm" }

         fn handles(&self, msg: &MeshMessage) -> bool {
             matches!(msg,
                 MeshMessage::ModelLoad(_)
                 | MeshMessage::ModelUnload(_)
                 | MeshMessage::RequestModelInference(_)
             )
         }

         async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> {
             Ok(())  // llama-server is launched on ModelLoad, no background loop needed
         }

         async fn handle(&self, msg: MeshMessage, tx: Sender<MeshMessage>) {
             // Move ModelLoad / ModelUnload / RequestModelInference match arms from main.rs here
         }
     }
     ```
   - `INFER_SEM` (process-wide inference semaphore) moves to a module-level static in `capabilities/llm/src/llama.rs`

2. **Add `capabilities/llm` to workspace `Cargo.toml`**

3. **Wire into `agent` as an optional dep**
   ```toml
   # agent/Cargo.toml
   [features]
   llm     = ["dep:capability-llm"]
   lighting = ["dep:capability-lighting"]  # wired in Phase 5

   [dependencies]
   capability-llm = { path = "../capabilities/llm", optional = true }
   ```
   `#[cfg(feature = "llm")]` guard around `LlmCapability` construction in `main.rs`.

4. **Remove `agent/src/llama.rs`** and all direct `llama::` calls from `agent/src/main.rs`

**Tests to add:**
- Llama unit tests stay in `capabilities/llm/src/llama.rs` (already there, move with the file)
- `LlmCapability::handles()`: assert true for `ModelLoad`, false for `Heartbeat`

**Validation:** `cargo build -p agent --features llm` compiles. Behaviour identical to before.

---

## Phase 3: Refactor Agent Shell

**Scope:** Replace the hardcoded match block in `main.rs` with a capability dispatch loop.

### Steps

1. **Build capability registry before the reconnect loop**
   ```rust
   let caps: Vec<Arc<dyn Capability + Send + Sync>> = {
       let mut v: Vec<Arc<dyn Capability + Send + Sync>> = vec![];
       #[cfg(feature = "llm")]
       v.push(Arc::new(capability_llm::LlmCapability::new()));
       #[cfg(feature = "lighting")]
       v.push(Arc::new(capability_lighting::LightingCapability::new()));
       v
   };
   ```
   Using `Arc` rather than `Box` so the same instance can be shared between the startup spawn and the reconnect loop without reconstructing state.

2. **Spawn `start()` for each capability once, outside the reconnect loop**
   ```rust
   for cap in &caps {
       let cap = Arc::clone(cap);
       let tx = tx.clone();
       tokio::spawn(async move {
           if let Err(e) = cap.start(tx).await {
               warn!("capability '{}' failed to start: {}", cap.name(), e);
           }
       });
   }
   ```

3. **Replace inline match with dispatch in the reader task**
   ```rust
   let mut handled = false;
   for cap in &caps {
       if cap.handles(&msg) {
           cap.handle(msg.clone(), tx_in.clone()).await;
           handled = true;
           break;
       }
   }
   if !handled {
       warn!("no capability handles: {:?}", msg);
   }
   ```

4. **Populate `NodeCapabilities.features` at startup**
   ```rust
   let mut features = vec![];
   #[cfg(feature = "llm")]      features.push("llm".to_string());
   #[cfg(feature = "lighting")] features.push("lighting".to_string());
   // include in the Capabilities message sent during startup
   ```

**Tests to add:**
- **`TestCapability` mock** — records handled messages, used to test dispatch without real infrastructure:
  ```rust
  struct TestCapability {
      handled: Mutex<Vec<MeshMessage>>,
      handles_fn: fn(&MeshMessage) -> bool,
  }
  #[async_trait]
  impl Capability for TestCapability {
      fn name(&self) -> &'static str { "test" }
      fn handles(&self, msg: &MeshMessage) -> bool { (self.handles_fn)(msg) }
      async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> { Ok(()) }
      async fn handle(&self, msg: MeshMessage, _tx: Sender<MeshMessage>) {
          self.handled.lock().await.push(msg);
      }
  }
  ```
- Dispatch test: two `TestCapability` instances with non-overlapping `handles()`, send one message of each type, assert each landed in the right one
- Integration test: build with `--features llm`, send `ModelLoad` → assert `ModelStatus(Loading)` response

**Validation:** `cargo test -p agent --features llm` green. `just validate-routing` still passes.

---

## Phase 4: Build Pipeline (NODE_FEATURES + justfile)

**Scope:** Wire `NODE_FEATURES` from `.env` files into `cargo build --features`.

### Steps

1. **Add `NODE_FEATURES` to `.env` files**
   ```bash
   # nodes/pi1.env
   NODE_FEATURES=llm,lighting

   # nodes/beelink1.env
   NODE_FEATURES=llm
   ```

2. **Update justfile `deploy-node` and `update-node`**
   ```bash
   source nodes/${node}.env
   cargo build --release -p agent --features ${NODE_FEATURES:-llm}
   ```

3. **Default features in `agent/Cargo.toml`**
   ```toml
   [features]
   default  = []
   llm      = ["dep:capability-llm"]
   lighting = ["dep:capability-lighting"]
   ```
   No implicit default — a bare `cargo build -p agent` produces a binary that logs "no capabilities loaded". Dev builds explicitly pass `--features llm`.

4. **Install scripts** — no change needed; they upload a pre-built binary, which already has features baked in at build time.

**Tests to add:**
- `cargo build -p agent --features llm` compiles
- `cargo build -p agent --features llm,lighting` compiles (on Linux; lighting is pi1-only)
- `cargo build -p agent` compiles and agent starts with "no capabilities loaded" log

**Validation:** `just deploy-node pi1` builds with `--features llm,lighting`; `just deploy-node beelink1` builds with `--features llm`.

---

## Phase 5: Lighting Capability Stub

**Scope:** `capabilities/lighting/` — compiles, wires into agent, stubs all methods.

### SLZB-06 / Zigbee2MQTT topology on pi1

- SLZB-06 is a network Zigbee coordinator (TCP)
- Zigbee2MQTT connects to it via `tcp://192.168.1.x:6638` (in Z2M `configuration.yaml`)
- Mosquitto runs on pi1 at `127.0.0.1:1883`
- Z2M publishes state to `zigbee2mqtt/<device_name>`
- Commands go to `zigbee2mqtt/<device_name>/set`

### Steps

1. **Create `capabilities/lighting/` crate**
   ```toml
   [package]
   name = "capability-lighting"

   [dependencies]
   capability-core = { path = "../core" }
   shared          = { path = "../../shared" }
   rumqttc         = "0.24"
   serde_json      = "1"
   tokio           = { version = "1", features = ["full"] }
   tracing         = "0.1"
   ```

2. **`LightingCapability` stub**
   ```rust
   pub struct LightingCapability {
       mqtt_host: String,
       mqtt_port: u16,
   }

   impl LightingCapability {
       pub fn new() -> Self {
           Self {
               mqtt_host: std::env::var("MQTT_HOST").unwrap_or("127.0.0.1".into()),
               mqtt_port: std::env::var("MQTT_PORT").ok()
                   .and_then(|v| v.parse().ok())
                   .unwrap_or(1883),
           }
       }
   }

   #[async_trait]
   impl Capability for LightingCapability {
       fn name(&self) -> &'static str { "lighting" }

       fn handles(&self, msg: &MeshMessage) -> bool {
           matches!(msg, MeshMessage::LightCommand(_) | MeshMessage::SceneLoad(_))
       }

       async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> {
           info!("lighting: stub — MQTT event loop not yet implemented");
           Ok(())
       }

       async fn handle(&self, msg: MeshMessage, _tx: Sender<MeshMessage>) {
           info!("lighting: stub received {:?}", msg);
       }

       fn tools(&self) -> Vec<ToolSchema> {
           // Returns the JSON Schema tool definitions for light_command and scene_load
           // (see Phase 0 — Tool Schema section for full JSON)
           vec![
               ToolSchema { name: "light_command".into(), description: "...".into(), parameters: serde_json::json!({...}) },
               ToolSchema { name: "scene_load".into(),    description: "...".into(), parameters: serde_json::json!({...}) },
           ]
       }
   }
   ```

3. **Wire into agent** — add `capability-lighting` optional dep to `agent/Cargo.toml`; `#[cfg(feature = "lighting")]` guard already templated in Phase 3.

4. **Add to `nodes/pi1.env`**
   ```bash
   MQTT_HOST=127.0.0.1
   MQTT_PORT=1883
   ```

5. **Document manual pi1 pre-steps** in `docs/pi1-lighting-setup.md`:
   - Install Mosquitto: `sudo apt install -y mosquitto mosquitto-clients`
   - Install Zigbee2MQTT (follow Z2M docs), point it at SLZB-06 via `tcp://192.168.1.x:6638`
   - The ai-mesh agent just needs MQTT running — it doesn't care how Z2M got there

**Tests to add:**
- `LightingCapability::handles()` unit test
- `LightingCapability::new()` reads env vars correctly
- `LightingCapability::tools()` returns non-empty vec

**Validation:** `just deploy-node pi1` with `--features llm,lighting`; agent starts, logs the stub message, LLM inference still works normally.

---

## Phase 6: Lighting Implementation

**Scope:** Full MQTT event loop + command handling. Separate story — detail TBD at implementation time.

### Background loop (`start`)

```
connect to Mosquitto via rumqttc async client
subscribe to "zigbee2mqtt/#"
loop:
  on message "zigbee2mqtt/<device>":
    parse JSON state (on, brightness, color_xy, color_temp)
    tx.send(MeshMessage::LightState(...)).await
```

### Command handling (`handle`)

```
LightCommand { target: Group(id), command: ColorXY { x, y } }:
  publish → "zigbee2mqtt/group_<id>/set": {"color": {"x": x, "y": y}}

LightCommand { target: Device(addr), command: Brightness(b) }:
  publish → "zigbee2mqtt/<addr>/set": {"brightness": b}

SceneLoad { scene_name, transition_ms }:
  look up scene definition (embedded TOML, migrate to coordinator-pushed later)
  calculate Oklab → CIE xy via `palette` crate
  publish group command with transition time
  tx.send(MeshMessage::SceneLoaded(...)).await
```

### Color pipeline

- Scene definitions use human-readable terms (warm, cool, hue angle)
- Interpolate in Oklab (perceptually uniform)
- Convert Oklab → CIE XYZ → CIE xyY for Zigbee wire format
- `colorgrad` crate for multi-stop gradient transitions
- Zigbee group cast for synchronised scene changes (no "popcorn effect")

### Additional deps (Phase 6 only)

```toml
# capabilities/lighting/Cargo.toml
palette    = "0.7"
colorgrad  = "0.6"
```

---

## Deployment Order

1. Build and redeploy all nodes with Phase 1–4 changes (redeploy together; WIRE_VERSION bump is fine since we own all nodes)
2. Deploy Phase 5 stub to pi1 — verify LLM inference still works, stub logs appear
3. Install Mosquitto + Zigbee2MQTT on pi1 (manual, see `docs/pi1-lighting-setup.md`)
4. Deploy Phase 6 full lighting implementation to pi1

Beelink1 always builds with `--features llm` only.

---

## Risk Register

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `async-trait` boxing overhead on hot path | Low | Capability dispatch is not on the inference hot path |
| MQTT reconnects on pi1 reboot | Medium | `rumqttc` handles reconnects; Phase 6 adds explicit retry |
| Zigbee2MQTT topic format changes | Low | Pin Z2M version; topic format is stable across minor releases |
| `NODE_FEATURES` in `.env` gets out of sync with actual capability code | Medium | `NODE_FEATURES` is the single source of truth — justfile reads it, no duplication |
| Lighting state lost on agent restart | Low | Coordinator registry holds last-known state; Phase 6 design decision |

---

## Open Questions

1. **Scene storage:** embed TOML in `capabilities/lighting/` for now; migrate to coordinator-pushed config when we have a scene editor.

2. **Zigbee group IDs:** need to pair SLZB-06 and enumerate groups before Phase 6. Not a blocker for the stub.

---

## Checklist (in order)

- [ ] Phase 1: `capabilities/core/` crate + all wire types in `shared` (lighting + intent) + `features` field on `NodeCapabilities`
- [ ] Phase 2: `capabilities/llm/` — extract `llama.rs` + `LlmCapability` impl
- [ ] Phase 3: agent shell refactor — capability registry + dispatch + `features` reported at startup
- [ ] Phase 4: `NODE_FEATURES` in `.env` + justfile build wiring
- [ ] Phase 0 (coordinator): `coordinator/src/intent.rs` + `IntentRequest` routing
- [ ] Phase 5: `capabilities/lighting/` stub — `tools()` schema + MQTT stub
- [ ] Phase 0 (CLI): `mesh intent` command
- [ ] Pi1: manual Mosquitto + Zigbee2MQTT install (documented in `docs/pi1-lighting-setup.md`)
- [ ] Phase 6: full lighting — MQTT event loop + color math + scene loading
