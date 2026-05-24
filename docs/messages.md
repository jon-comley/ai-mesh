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

`HeartbeatPayload` wraps `NodeIdentity` (flattened in JSON) plus a per-message auth token and optional health metrics:

| Field | Type | Description |
|-------|------|-------------|
| `id` | String | Persistent node UUID |
| `hostname` | String | OS hostname |
| `ip` | String | LAN IP address |
| `role` | NodeRole | `Controller` or `Compute` |
| `auth_token` | String | `MESH_AUTH_TOKEN` value; empty string when not configured |
| `cpu_usage_pct` | Option\<f32\> | All-core average CPU utilisation 0.0–100.0; omitted by agents < Phase C |
| `ram_used_gb` | Option\<f32\> | RAM currently in use, gibibytes; omitted by agents < Phase C |
| `ram_total_gb` | Option\<f32\> | Total physical RAM, gibibytes; omitted by agents < Phase C |

The three health fields use `#[serde(default, skip_serializing_if = "Option::is_none")]` so older agents that don't send them are still accepted (backward-compatible).

The coordinator validates `auth_token` on every heartbeat (defence-in-depth on top of the connection-level `AuthToken` check). An empty or wrong token is rejected when auth is configured.

Coordinator updates last-seen timestamp on successful validation and, from Phase C onwards, appends a `HealthSample` (coordinator-stamped timestamp + metrics) to a per-node ring buffer.

---

## SetHeartbeatInterval { secs: u64 }
Sent **coordinator → agent** to dynamically change how often the agent sends heartbeats.

| Field | Type | Description |
|-------|------|-------------|
| `secs` | u64 | New interval in seconds (e.g. 5, 30, 60) |

Pushed by the coordinator when the operator calls `POST /api/nodes/{id}/heartbeat-interval` or `mesh set-heartbeat <node> <secs>`. The agent applies the new interval immediately without restarting.

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
pub const WIRE_VERSION: u32 = 1;
#[serde(default = "default_wire_version")]
```

This ensures **cross-platform compatibility** between:
- macOS agents (Mac mini)
- Linux agents (Pi 5, Beelink)
- Linux coordinator (OmniBook)

Older agents that do not send `wire_version` will still deserialize safely.

### `RequestModelInference`
CLI → Coordinator  
Requests inference for a named model. The coordinator uses the scheduler to select a ready node.

Fields:
- `request_id: String`
- `node_id: Option<String>` — caller-supplied pin; `null`/absent means "let the scheduler decide" (`#[serde(default)]`)
- `model_name: String`
- `prompt: String`
- `max_tokens: u32`
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
- `target: LightTarget` — `Device(name)` or `Group(id)`
- `command: LightAction` — `On`, `Off`, `SetBrightness(u8)`, `SetColorTemp(u16)`, `SetColorXY(f32,f32)`

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

---
## Versioning and Compatibility

All Phase 6 messages use explicit version tagging.  
This prevents protocol mismatches when nodes run different builds.

Rules:
- Missing `wire_version` defaults to `1`
- Nodes must accept messages with equal or lower `wire_version`
- Higher versions must be rejected with a safe error (Phase 7)

This ensures ai-mesh remains **safe, deterministic, and cross-platform** as the cluster grows.
