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
- GPU (NVIDIA via nvidia-smi; VGA via lspci on Linux; AMD iGPU on Windows not yet detected)

Platform implementations are gated with `#[cfg]`:
- **Windows** — uses the `sysinfo` crate (no child-process spawning; avoids NSSM STOP_PENDING deadlock)
- **Linux / macOS** — reads `/proc/cpuinfo` and `/proc/meminfo` directly

### identity.rs
Provides:
- Node ID generation — UUID v4 persisted to `~/.ai-mesh/node-id`; the same ID is returned on every agent restart, so the coordinator recognises the node without stale registry entries
- Hostname detection — Windows reads `COMPUTERNAME` env var; Linux reads `/etc/hostname`
- Local IP detection via UDP socket trick (cross-platform)

### capabilities.rs
Determines:
- CPU inference support
- GPU inference support
- ANE inference support
- Maximum model size

### agent.rs
Implements:
- Heartbeat loop (configurable interval, default 5 s)
- Hardware report and capability report sent once on startup via `start_once()`
- Message sending through an async MPSC channel

### main.rs (runtime)
- **TLS connection** — connects via `tokio-rustls`; verifies coordinator cert against `MESH_TLS_FINGERPRINT` (SHA-256 TOFU). Falls back to permissive mode when `MESH_INSECURE=1` is set (logged loudly).
- **Auth + HMAC** — sends `AuthToken(token)` as the first (unsigned) frame when `MESH_AUTH_TOKEN` is set, then wraps all subsequent outgoing messages in `SignedFrame` (HMAC-SHA256, HKDF-derived key). Verifies and unwraps `SignedFrame` on all inbound messages; drops frames with bad signatures or stale timestamps.
- **TCP keepalive** — after connecting, `socket2::SockRef` sets `SO_KEEPALIVE` with a 10 s idle probe and 5 s retry interval. Prevents NIC power management or network idle timeouts from silently dropping long-running inference connections.
- **Process-wide inference semaphore** — `static INFER_SEM: OnceLock<Semaphore>` with capacity 1. A second inference request queues here rather than launching a concurrent llama-server call that would double GPU memory usage and risk an OOM freeze.
- **Cancellation on disconnect** — inference is wrapped in `tokio::select! { _ = tx2.closed() => cancel, res = llama::generate() => ... }`. If the TCP connection drops mid-inference, the HTTP request to llama-server is cancelled immediately, freeing the GPU without waiting for the 120 s timeout.

---

## 2. Testing Strategy

- Unit tests for each module
- Mocked tests for agent loop
- Round-trip tests for messages
- Hardware detection tests use controlled fixtures

---

## 3. Future Extensions

- Update system (OTA manifest distribution)
- GPU benchmarking (inference tokens/sec reporting)

---

This document will evolve as the agent crate grows.

---

## 4. Current Implementation Status

As of this stage of development, the agent crate includes fully implemented and tested subsystems:

### ✔ Hardware Detection
- CPU model, cores, threads
- RAM detection
- OS and architecture
- GPU detection (NVIDIA via nvidia-smi; VGA via lspci on Linux; AMD iGPU on Windows not yet detected)
- Cross-platform: Windows uses `sysinfo` crate (brand strings are trimmed); Linux/macOS use `/proc`
- Fully tested with round‑trip and basic invariants

### ✔ Identity Detection
- Node ID generation — UUID v4 persisted to `~/.ai-mesh/node-id`; stable across restarts
- Hostname detection — Windows reads `COMPUTERNAME`; Linux reads `/etc/hostname`
- Local IP detection via UDP socket trick (cross-platform)
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

The agent emits the following sequence when started:

1. (TLS handshake — cert fingerprint verified)
2. `MeshMessage::AuthToken(token)` — unsigned, sent as the first plain frame when `MESH_AUTH_TOKEN` is set
3. `SignedFrame(MeshMessage::Heartbeat(HeartbeatPayload))` — includes `auth_token` field for per-message defence-in-depth
4. `SignedFrame(MeshMessage::HardwareReport(HardwareSpec))`
5. `SignedFrame(MeshMessage::Capabilities(NodeCapabilities))`
6. Enters heartbeat loop — periodic `SignedFrame(MeshMessage::Heartbeat(...))`

When `MESH_AUTH_TOKEN` is unset (dev mode), plain `MeshMessage` JSON is sent without a `SignedFrame` wrapper.

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
