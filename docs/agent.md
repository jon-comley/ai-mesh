# Agent Crate Documentation

The `agent` crate implements the logic for a mesh node that runs on each machine. Its responsibilities include:

- Detecting hardware specifications
- Determining inference capabilities
- Identifying the node (ID, hostname, IP)
- Sending periodic heartbeats to the coordinator
- Reporting hardware and capabilities
- Receiving update notifications
- Applying updates (future phase)

---

## 1. Modules

### hardware.rs
Detects:
- CPU model
- Core/thread count
- RAM
- OS + architecture
- GPU (NVIDIA/AMD/Intel)
- ANE (stubbed on Linux)

### identity.rs
Provides:
- Node ID generation
- Hostname detection
- Local IP detection

### capabilities.rs
Determines:
- CPU inference support
- GPU inference support
- ANE inference support
- Maximum model size

### agent.rs
Implements:
- Heartbeat loop
- Hardware report
- Capability report
- Coordinator discovery (stub)
- Message sending

---

## 2. Testing Strategy

- Unit tests for each module
- Mocked tests for agent loop
- Round-trip tests for messages
- Hardware detection tests use controlled fixtures

---

## 3. Future Extensions

- Update system
- Model loading
- Local inference
- GPU benchmarking
- Secure message signing

---

This document will evolve as the agent crate grows.

---

## 4. Current Implementation Status

As of this stage of development, the agent crate includes fully implemented and tested subsystems:

### ✔ Hardware Detection
- CPU model, cores, threads
- RAM detection
- OS and architecture
- GPU detection (NVIDIA, AMD, Intel)
- ANE detection stub (Linux)
- Fully tested with round‑trip and basic invariants

### ✔ Identity Detection
- Node ID generation (UUID v4)
- Hostname detection
- Local IP detection via UDP socket trick
- Fully tested

### ✔ Capability Detection
- CPU inference support (always true)
- GPU inference support (based on hardware detection)
- ANE inference (stubbed false on Linux)
- Max model size heuristic (50% of RAM)
- Fully tested

### ✔ Agent Runtime
- Async heartbeat loop (Tokio)
- Sends:
  - Heartbeat
  - HardwareReport
  - Capabilities
- Configurable heartbeat interval
- Fully tested using mocked channels

---

## 5. Message Flow

The agent currently emits the following sequence when started:

1. `MeshMessage::Heartbeat(NodeIdentity)`
2. `MeshMessage::HardwareReport(HardwareSpec)`
3. `MeshMessage::Capabilities(NodeCapabilities)`
4. Enters heartbeat loop:
   - Periodically sends `MeshMessage::Heartbeat(NodeIdentity)`

This flow is validated by async unit tests.

---

## 6. Runtime Behaviour

- The agent runs on a Tokio async runtime.
- Heartbeat interval is configurable via `Agent::new()`.
- All outbound messages are sent through an async MPSC channel.
- Coordinator discovery is currently stubbed and will be implemented in Phase 4.

---

## 7. Next Steps

- Implement coordinator crate (registry, server, orchestration)
- Add real networking (TCP/QUIC)
- Add update manifest handling
- Add CLI integration
- Add mDNS or broadcast discovery

---

## 8. Coordinator Integration

The agent is now fully integrated with the coordinator:

- Sends heartbeats, hardware reports, and capabilities
- Coordinator receives and acknowledges messages
- End‑to‑end tests confirm full communication path

This completes the agent's core responsibilities.

---
