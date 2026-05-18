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

# Message Types

## Heartbeat(NodeIdentity)
Sent by agents periodically to indicate liveness.

Includes:
- id
- hostname
- ip
- role

Coordinator updates last-seen timestamp.

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
## Versioning and Compatibility

All Phase 6 messages use explicit version tagging.  
This prevents protocol mismatches when nodes run different builds.

Rules:
- Missing `wire_version` defaults to `1`
- Nodes must accept messages with equal or lower `wire_version`
- Higher versions must be rejected with a safe error (Phase 7)

This ensures ai-mesh remains **safe, deterministic, and cross-platform** as the cluster grows.
