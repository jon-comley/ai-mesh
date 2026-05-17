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
