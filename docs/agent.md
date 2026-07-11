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
- **TCP keepalive** — after connecting, `socket2::SockRef` sets `SO_KEEPALIVE` with a 10 s idle probe and 5 s retry interval. Helps with NIC power management or network idle timeouts, but note it only fires when the connection is *idle* — the 5 s heartbeat traffic keeps it busy, so keepalive cannot by itself catch a dead peer.
- **Read-half-driven reconnect** — the reader and writer halves are coupled via a `tokio::sync::Notify` (`conn_dead`). The read half is the reliable liveness signal: a vanished coordinator (restart, or a WSL2 suspend/resume that leaves a half-open socket) surfaces as EOF on read, whereas the writer can buffer small heartbeats into a half-open socket for minutes without erroring. On EOF/error the reader signals `conn_dead`; the writer loop `select!`s on it and breaks to reconnect, and the stale reader is `abort()`ed on teardown so it can't linger across reconnects. Capabilities re-`start()` with the new connection's sender on each reconnect; long-lived pollers (e.g. REAPER) are bound to *their* connection's sender so they exit when it drops rather than accumulating — a variant that only needs a per-connection sender swap (no guard) is `capability-audio`'s `AudioShared`, which instead spawns its background status loop exactly once per process (`std::sync::Once`) since that loop isn't tied to any one mesh connection.
- **Exponential reconnect backoff** — a connection-*establishment* failure (TCP connect exhausted, TLS handshake, auth-token write, or the startup burst) sleeps a backoff that doubles each time, capped at 60s, instead of the flat 5s retry every prior version used. Resets to the 5s base the moment a connection actually gets established (the startup burst succeeds), so a brief, otherwise-healthy disconnect still recovers in 5s — only repeated failure to connect at all slows down. The coordinator address is re-resolved via mDNS on every reconnect attempt (not just at startup), so a transient mDNS blip can't wedge the agent onto a stale/`127.0.0.1` address forever.
- **Process-wide inference semaphore** — `static INFER_SEM: OnceLock<Semaphore>` with capacity 1. A second inference request queues here rather than launching a concurrent llama-server call that would double GPU memory usage and risk an OOM freeze.
- **Cancellation on disconnect** — inference is wrapped in `tokio::select! { _ = tx2.closed() => cancel, res = llama::generate() => ... }`. If the TCP connection drops mid-inference, the HTTP request to llama-server is cancelled immediately, freeing the GPU without waiting for the 120 s timeout.

---

## 4. Internal llama-server Management

The agent manages `llama-server` (llama.cpp) as a child process. It does not use the system PATH; it expects the binary at `%LOCALAPPDATA%\Programs\llama.cpp\llama-server.exe` (Windows) or `/opt/llama.cpp/llama-server` (Linux).

### CLI Flags
When the agent starts a model, it executes `llama-server` with the following hardcoded flags:
- `--model <path>`: Path to the first GGUF shard.
- `--host 127.0.0.1`: Binds to localhost for security (agent acts as the proxy).
- `--port 8080`: Default port for the llama.cpp HTTP API.
- `--ctx-size 4096`: Default context window.
- `--n-gpu-layers <N>`: Offloads $N$ layers to GPU (set via `LLAMA_GPU_LAYERS` env).
- `--flash-attn <on|off|auto>`: Flash-attention mode (set via `LLAMA_FLASH_ATTN`, default `auto`).

### Environment Variables
The agent's behavior can be tuned via these environment variables:
- `LLAMA_GPU_LAYERS`: (Default: `0`) Number of layers to offload. Set to `99` for full offload on SER8.
- `LLAMA_FLASH_ATTN`: (Default: `auto`) Flash-attention mode `on`/`off`/`auto`. `auto` lets llama.cpp enable it where supported (≈2× prefill on Qwen/Llama/Mistral) and skip it where it isn't — forcing `on` hangs Gemma-3 on the Vulkan backend.
- `LLAMA_HEALTH_TIMEOUT_SECS`: (Default: `180`) How long to wait for the `/health` endpoint after starting the process.
- `LLAMA_HOST`: (Default: `http://127.0.0.1:8080`) The URL the agent uses to talk to its local child process.

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

---

## 6. Testing Strategy

- Unit tests for each module
- Mocked tests for agent loop
- Round-trip tests for messages
- Hardware detection tests use controlled fixtures
- `llama.rs` tests for GGUF resolution and chat response parsing

---

## 7. Future Extensions

- Update system (OTA manifest distribution)
- GPU benchmarking (inference tokens/sec reporting)
- mDNS or broadcast discovery

