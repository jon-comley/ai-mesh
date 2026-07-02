# Mesh Message Protocol

This document defines all message types used across the ai-mesh system.
All messages are serialized using JSON via Serde.

---

## Overview

Messages flow between:
- Agent → Coordinator
- CLI → Coordinator
- Coordinator → CLI

Messages are defined in `shared::MeshMessage`.

---

## Wire Format

When `MESH_AUTH_TOKEN` is configured, every message after the initial `AuthToken` handshake is wrapped in a `SignedFrame`:

```json
{ "ts": 1716543000, "payload": [<MeshMessage JSON bytes>], "sig": [<32 HMAC-SHA256 bytes>] }
```

- `ts` — Unix timestamp (seconds). Receiver rejects frames with `|ts − now| > 30`.
- `payload` — JSON-encoded `MeshMessage` bytes.
- `sig` — HMAC-SHA256 over `ts_le_bytes || payload`, using a key derived from `MESH_AUTH_TOKEN` via HKDF-SHA256 (`label = "ai-mesh-hmac-v1"`).

Without auth configured, raw `MeshMessage` JSON is sent directly (dev/test mode).

All frames use a 4-byte little-endian length prefix: `[len: u32 LE][frame JSON bytes]`.

---

# Message Types

## AuthToken(String)
**First frame** sent by every agent/CLI on connect, before any other message.
Always sent **unsigned** (plain `MeshMessage` JSON, not a `SignedFrame`) — this frame IS the key establishment step.

When `MESH_AUTH_TOKEN` is set on the coordinator, the first frame must be `AuthToken` carrying the correct token. Wrong or missing token → connection closed immediately.

---

## Heartbeat(HeartbeatPayload)
Sent by agents periodically to indicate liveness.

`HeartbeatPayload` wraps `NodeIdentity` (flattened in JSON) plus a per-message auth token and health metrics:

| Field | Type | Description |
|-------|------|-------------|
| `id` | String | Persistent node UUID |
| `hostname` | String | OS hostname |
| `ip` | String | LAN IP address |
| `role` | NodeRole | `Controller` or `Compute` |
| `auth_token` | String | `MESH_AUTH_TOKEN` value; empty string when not configured |
| `cpu_usage_pct` | f32 | All-core average CPU utilisation 0.0–100.0 (required since Phase C2) |
| `ram_used_gb` | f32 | RAM currently in use, gibibytes (required since Phase C2) |
| `ram_total_gb` | f32 | Total physical RAM, gibibytes (required since Phase C2) |
| `gpu_usage_pct` | Option\<f32\> | GPU utilisation 0.0–100.0; omitted on CPU-only nodes (added Phase C7) |
| `gpu_vram_used_gb` | Option\<f32\> | GPU VRAM in use, gibibytes; omitted on CPU-only nodes (added Phase C7) |
| `gpu_vram_total_gb` | Option\<f32\> | Total GPU VRAM, gibibytes; omitted on CPU-only nodes (added Phase C7) |

The three CPU/RAM fields are required. The three GPU fields are optional (`serde(default)` → `None` when absent); pre-C7 agents that omit them remain compatible. Pre-C2 agents that omit the CPU/RAM fields fail deserialization and are rejected.

The coordinator validates `auth_token` on every heartbeat (defence-in-depth on top of the connection-level `AuthToken` check). An empty or wrong token is rejected when auth is configured.

On a valid heartbeat the coordinator updates last-seen timestamp, appends a coordinator-stamped `HealthSample` to a per-node ring buffer (capped at 60 entries), and broadcasts `DashboardEvent::HealthUpdate` over WebSocket to connected dashboard clients.

---

## SetHeartbeatInterval { secs: u64 }
Sent **coordinator → agent** to dynamically change how often the agent sends heartbeats.

| Field | Type | Description |
|-------|------|-------------|
| `secs` | u64 | New interval in seconds (e.g. 5, 30, 60) |

Pushed by the coordinator when the operator calls `POST /api/nodes/{id}/heartbeat-interval` or `mesh set-heartbeat <node> <secs>`. The agent applies the new interval immediately without restarting.

---

## ZigbeeStatus { online: bool }
Sent **lighting node → coordinator** when the Zigbee bridge (zigbee2mqtt) goes up or down.

| Field | Type | Description |
|-------|------|-------------|
| `online` | bool | `true` when zigbee2mqtt is connected, `false` when it disconnects/crashes |

The lighting capability emits this from two signals: the `zigbee2mqtt/bridge/state` MQTT topic (z2m's own online/offline Last Will — see `parse_bridge_online`), and an MQTT-broker connection loss (`ConnectionLost`). The coordinator forwards it to dashboard clients as `DashboardEvent::ZigbeeStatus`, which drives the offline banner and disables all light controls. Note: when zigbee2mqtt crashes *before* ever connecting to the broker, no bridge/state Last Will is published, so the dashboard additionally infers "offline" client-side when rooms exist but no devices have reported (`inferZigbeeStatus` in `rooms.js`).

---

## HardwareReport(HardwareSpec)
Sent once at agent startup.

Includes:
- CPU model
- cores / threads
- RAM
- OS / arch
- GPU (optional)

Coordinator stores hardware in registry.

---

## Capabilities(NodeCapabilities)
Sent once at agent startup.

Includes:
- CPU inference support
- GPU inference support
- ANE inference support
- Max model size

Coordinator stores capabilities in registry.

---

## RequestNodes
Sent by CLI.

Coordinator responds with:
```
NodeList(Vec<NodeRecordLite>)
```

---

## NodeList(Vec<NodeRecordLite>)
Lightweight node summaries:
- id
- hostname
- ip
- last_heartbeat_ms

Used by:
- `mesh nodes`
- `mesh watch`

---

## RequestNodeInfo(String)
Sent by CLI to request full diagnostics for a specific node.

Coordinator responds with:
```
NodeInfo(NodeRecordFull)
```

---
## Phase 6 — Model Scheduling Messages

Phase 6 introduces a set of **model-aware wire-protocol messages** used for:
- model loading
- model unloading
- inference requests
- inference results
- lifecycle reporting

All Phase 6 messages include a `wire_version: u32` field with:

```rust
pub const WIRE_VERSION: u32 = 4;
#[serde(default = "default_wire_version")]
```

This ensures **cross-platform compatibility** between:
- macOS agents (Mac mini)
- Linux agents (Pi 5, Beelink)
- Linux coordinator (OmniBook)

Older agents that do not send `wire_version` will still deserialize safely.

> **Wire v3 (OpenAI-compatible API):** `InferenceRequest` carries a full
> `messages: Vec<ChatTurn>` conversation instead of the former
> `system_prompt` + `prompt` string pair, so llama-server applies the model's
> chat template per role. `ChatTurn { role, content }` serializes its role as
> the OpenAI strings (`"system"` / `"user"` / `"assistant"`).
> `InferenceResult` gains `prompt_tokens`. v2 agents fail fast on v3 frames
> (missing `prompt` field) — deploy coordinator and agents together.

> **Wire v4 (SSE streaming):** `InferenceRequest` gains a **required**
> `stream: bool`; new `ModelInferenceChunk(InferenceChunk)` message carries
> incremental deltas while a streaming inference runs, terminated by the
> usual `ModelInferenceResult`. Deploy the **coordinator first**: a v3 agent
> ignores the unknown `stream` field and replies non-streamed (the API
> degrades to a single-delta stream), but a v4 agent cannot parse a v3
> coordinator's requests (missing `stream`).

### `ModelInferenceChunk`
Compute node → Coordinator
One streamed token batch for an in-flight streaming inference.

Fields:
- `request_id: String`
- `node_id: String`
- `delta: String` — incremental output text
- `wire_version: u32`

### `RequestModelInference`
CLI → Coordinator  
Requests inference for a named model. The coordinator uses the scheduler to select a ready node.

Fields:
- `request_id: String`
- `node_id: Option<String>` — caller-supplied pin; `null`/absent means "let the scheduler decide" (`#[serde(default)]`)
- `model_name: String`
- `messages: Vec<ChatTurn>` — full conversation, forwarded to the model verbatim
- `stream: bool` — stream `ModelInferenceChunk`s before the terminal result
- `max_tokens: u32`
- `temperature: Option<f32>` — `None` = agent default (0.8)
- `wire_version: u32`

### `ModelInferenceResult`
Compute node → Coordinator  
Returns the output of an inference request.

Fields:
- `request_id: String`
- `node_id: String`
- `model_name: String`
- `output: String`
- `tokens_generated: u32`
- `prompt_tokens: u32` — from the backend's `usage.prompt_tokens` (`#[serde(default)]`)
- `duration_ms: u64`
- `error: Option<String>`
- `wire_version: u32`

### `ModelLoad`
Coordinator → Compute node  
Instructs a node to load a model into memory.

Fields:
- `request_id: String`
- `node_id: String`
- `model_name: String`
- `model_size_mb: u64`
- `wire_version: u32`

### `ModelUnload`
Coordinator → Compute node  
Instructs a node to unload a model.

Fields:
- `request_id: String`
- `node_id: String`
- `model_name: String`
- `wire_version: u32`

### `ModelStatus`
Compute node → Coordinator  
Reports the lifecycle state of a model.

Fields:
- `node_id: String`
- `model_name: String`
- `state: ModelLifecycleState`  
  (`Unloaded`, `Loading`, `Ready`, `Failed { reason }`)
- `wire_version: u32`

### `Error`
Coordinator → caller  
Generic error response when a request cannot be fulfilled.

Fields:
- `0: String` — human-readable reason (e.g. `"no node has model 'llama3' in Ready state"`)

---
## Admin Messages

### `Admin(AdminMessage)`
CLI → Coordinator  
Wraps administrative commands. Currently one variant:

- `ResetRegistry` — clears all nodes from the live registry. Used by `just reset`.

---
## Lighting Messages (Lighting MVP)

### `LightCommand(LightCommandRequest)`
Coordinator → Lighting node  
Instructs the lighting capability to change the state of a Zigbee device or group.

Fields:
- `request_id: String`
- `target: LightTarget` — `Device(name)` or `Group(name)`
- `command: LightAction` — one of:
  - `On` / `Off` / `Toggle`
  - `Brightness(u8)` — 0–254 (Zigbee spec reserves 255)
  - `BrightnessTransition { value: u8, transition_secs: f32 }` — brightness with hardware-interpolated fade; emits `{"brightness": v, "transition": t}` to Z2M so the bulb itself interpolates
  - `ColorTemp(u16)` — mireds (154–500; lower = cooler)
  - `ColorXY { x: f32, y: f32 }` — CIE 1931 xy chromaticity; wide-gamut D65 colour space

Intent routing converts Kelvin to mireds for `ColorTemp`, and CSS colour names / hex strings to `ColorXY` via `css_color_to_xy` + `rgb_to_xy` (inverse sRGB gamma + wide-gamut D65 matrix).

### `LightState(LightStateReport)`
Lighting node → Coordinator  
Reports a device's current state (triggered by Z2M MQTT state changes).

Fields:
- `node_id: String`
- `device_id: String` — Z2M friendly name
- `on: bool`
- `brightness: Option<u8>`
- `color_xy: Option<(f32, f32)>`
- `color_temp: Option<u16>`

### `LightDeviceList(LightDeviceListReport)`
Lighting node → Coordinator  
Reports the full list of known Z2M devices and groups. Sent on every MQTT connect; coordinator persists to SQLite so the LLM has valid targets immediately after restart.

Fields:
- `node_id: String`
- `devices: Vec<String>` — friendly names of individual Zigbee devices
- `groups: Vec<String>` — friendly names of Z2M groups (e.g. `"all"`)

### `SceneLoad(SceneLoadRequest)`
Coordinator → Lighting node  
Instructs the lighting node to activate a named scene.

Fields:
- `request_id: String`
- `scene_name: String`
- `transition_ms: u32`

### `SceneLoaded(SceneLoadedReport)`
Lighting node → Coordinator  
Reports the result of a scene load attempt.

Fields:
- `request_id: String`
- `scene_name: String`
- `success: bool`
- `error: Option<String>`

---
## Intent Routing Messages

### `IntentRequest(IntentRequest)`
CLI → Coordinator  
Submits a natural-language intent for LLM routing.

Fields:
- `request_id: String`
- `text: String` — the user's natural-language input
- `model_name: Option<String>` — pin to a specific model; `None` → coordinator picks largest ready LLM
- `context: Vec<IntentTurn>` — prior conversation turns for multi-turn intents

### `IntentResponse(IntentResponse)`
Coordinator → CLI  
Returns the LLM's response to an intent request.

Fields:
- `request_id: String`
- `node_id: String` — which node served the request
- `text: Option<String>` — free-text response (when no tool was called)
- `tool_calls: Vec<ToolCallRecord>` — tool invocations with args and results
- `error: Option<String>`
- `duration_ms: u64` — node-reported token-generation (decode) time
- `tokens_generated: u32` — tokens produced
- `prompt_eval_ms: u64` — node-reported prompt-prefill time
- `total_ms: u64` — coordinator-measured end-to-end wall time (inference dispatch + generation + tool execution + parsing); the latency a client actually waited for. Surfaced in the chat UI as the `… server` figure.

---
---

## Dashboard WebSocket Events

These are **not** `MeshMessage` types. They are sent by the coordinator over the dashboard WebSocket (port 9001 `/ws`) to connected browser clients. Defined in `coordinator::http::state::DashboardEvent`.

All events are tagged with a `"type"` field (`#[serde(tag = "type")]`).

### TopologyUpdate

```json
{ "type": "TopologyUpdate", "nodes": [ { "id": "...", "name": "...", "role": "Compute", "ip": "...", "last_seen_secs": 3, "health": "green" } ] }
```

Broadcast on every heartbeat. Replaces the full node list on the client.

`health` is `"green"` (< 10 s), `"amber"` (10–29 s), or `"red"` (≥ 30 s).

### HealthUpdate

```json
{ "type": "HealthUpdate", "node_id": "abc123", "samples": [ { "ts_ms": 1716543000000, "cpu_pct": 42.5, "ram_used_gb": 6.1, "ram_total_gb": 15.9, "gpu_pct": 12.0, "gpu_vram_used_gb": 3.7, "gpu_vram_total_gb": 4.0 }, ... ] }
```

Broadcast on every heartbeat, immediately after `TopologyUpdate`. Also sent once per node to each new WebSocket client on connect (warm-start), so sparklines populate immediately without waiting for the next heartbeat. Contains the full rolling window (up to 60 entries) of health samples for the named node.

| Field | Type | Description |
|-------|------|-------------|
| `node_id` | String | Node UUID |
| `samples[].ts_ms` | u64 | Coordinator-stamped Unix timestamp in milliseconds |
| `samples[].cpu_pct` | f32 | All-core average CPU utilisation 0.0–100.0 |
| `samples[].ram_used_gb` | f32 | RAM in use, gibibytes |
| `samples[].ram_total_gb` | f32 | Total RAM, gibibytes |
| `samples[].gpu_pct` | f32? | GPU utilisation 0.0–100.0; absent on CPU-only nodes |
| `samples[].gpu_vram_used_gb` | f32? | GPU VRAM in use, gibibytes; absent on CPU-only nodes |
| `samples[].gpu_vram_total_gb` | f32? | Total GPU VRAM, gibibytes; absent on CPU-only nodes |

Timestamps are set by the coordinator on receipt to avoid clock-skew across nodes. GPU fields are omitted from JSON when `None` (`skip_serializing_if`); the dashboard hides the GPU row when all samples lack GPU data.

---

## Versioning and Compatibility

All Phase 6 messages use explicit version tagging.  
This prevents protocol mismatches when nodes run different builds.

Rules:
- Missing `wire_version` defaults to `1`
- Nodes must accept messages with equal or lower `wire_version`
- Higher versions must be rejected with a safe error (Phase 7)

This ensures ai-mesh remains **safe, deterministic, and cross-platform** as the cluster grows.
