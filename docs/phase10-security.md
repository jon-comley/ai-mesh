# Phase 10 — Security & Auth: Planning Document

---

## Threat Model

**In scope:**
- **Rogue nodes** — an untrusted machine on the LAN registers itself as a compute node, receives inference requests, or manipulates registry state
- **Eavesdropping** — someone on the LAN captures prompts, responses, or model names from the plain TCP stream
- **Message injection / replay** — a crafted or replayed message triggers actions (registry reset, model load, inference)

**Out of scope for this phase:**
- Internet-facing coordinator (no NAT/firewall hardening)
- Compromise of a legitimate node's stored credentials
- Side-channel attacks on inference output

---

## Approach

Three layers, implemented in order. Each layer is independently useful and deployable.

### Layer 1 — TLS on the coordinator TCP listener

Encrypts the wire, prevents eavesdropping. The coordinator generates a self-signed certificate on first run and writes it to disk. Agents verify the coordinator cert by SHA-256 fingerprint (stored in each node's `.env` file during provisioning).

**Why self-signed, not a CA:** The mesh is a closed system with a known, small set of nodes. A full PKI adds complexity without benefit. Fingerprint-based trust-on-first-use (TOFU) is the same model SSH uses and is well understood.

### Layer 2 — Shared auth token (connection + per-heartbeat)

**Two sub-layers:**
1. **Connection-level `AuthToken` first frame** — on every new TCP connection, the first message must be `AuthToken(token)` carrying the correct shared secret. Wrong or absent → connection dropped before any registry interaction.
2. **Per-heartbeat `auth_token` field** — `HeartbeatPayload` carries `auth_token: String` in every `Heartbeat`. The coordinator rejects heartbeats with an empty or wrong token when auth is configured. This is defence-in-depth: even if the connection-level check were bypassed, individual messages can't manipulate the registry.

`auth_token` is a required `String` field (always serialised). Agents without a token configured send `""`. A single shared secret covers all nodes; per-node tokens (allowing individual revocation) are a future improvement.

### Layer 3 — HMAC message integrity

HMAC-SHA256 on each message body, using the same shared secret as the auth token. Prevents injection of crafted messages even if TLS is somehow stripped. Added as an optional envelope field so the wire protocol stays backwards-compatible during rollout.

---

## Implementation Steps

### Step 1: Coordinator TLS listener

**New crate dependencies** (`coordinator/Cargo.toml`):
- `tokio-rustls` — async TLS on top of Tokio TCP
- `rcgen` — self-signed cert generation in pure Rust
- `rustls-pemfile` — PEM load/save

**New file:** `coordinator/src/tls.rs`
- `load_or_generate_cert(path) -> (CertificateDer, PrivateKeyDer)` — generates with `rcgen` on first run, writes PEM to disk, loads on subsequent runs
- `fingerprint(cert: &CertificateDer) -> String` — SHA-256 of DER bytes, colon-separated hex
- `make_acceptor(cert, key) -> TlsAcceptor`

**Modified file:** `coordinator/src/server.rs`
- Replace `TcpListener::accept()` with `TlsAcceptor::accept()` wrapping each incoming stream
- On startup, print: `coordinator TLS fingerprint: AA:BB:CC:...`

**Cert storage path:** `~/.config/ai-mesh/coordinator.crt` + `coordinator.key`, or overridden via env vars `MESH_TLS_CERT` / `MESH_TLS_KEY`.

---

### Step 2: Agent TLS connection

**New crate dependencies** (`agent/Cargo.toml`):
- `tokio-rustls`
- `rustls-pemfile`

**Modified file:** `agent/src/agent.rs` (or wherever TCP connect lives)
- Wrap TCP `TcpStream::connect()` with a `TlsConnector`
- Implement a custom `rustls::client::ServerCertVerifier` that:
  - Extracts the coordinator's DER cert SHA-256
  - Compares against `MESH_TLS_FINGERPRINT` from env
  - Returns `Ok` on match, error on mismatch

**Node env files** (`nodes/<name>.env`):
- Add `MESH_TLS_FINGERPRINT=AA:BB:CC:...` (populated during provisioning, printed by coordinator on startup)

**Insecure mode:** if `MESH_INSECURE=1` is set, log a loud warning on every startup and skip fingerprint verification. Insecure mode must be explicitly opted into — an unset fingerprint is an error in production, not a silent bypass. This makes insecure mode visible in logs and `grep`-able.

**Fingerprint UX:** on startup the coordinator always prints:
```
coordinator TLS fingerprint: AA:BB:CC:...  (first run — share this with all nodes)
```
On subsequent runs it prints the same line without the parenthetical. If a connecting agent presents a cert with a different fingerprint, the coordinator logs:
```
TLS fingerprint mismatch — expected AA:BB:CC: got DD:EE:FF: (possible MITM or coordinator rekey)
```
This makes "first connect" clearly distinguishable from "key changed".

**Justfile:** add a `show-fingerprint` recipe that prints the coordinator's current fingerprint without starting the full server, for use during node provisioning.

---

### Step 3: Auth token in Heartbeat

**Modified file:** `shared/src/hardware.rs`
- `HeartbeatPayload` struct (replaces bare `NodeIdentity` in `Heartbeat`):
  ```rust
  pub struct HeartbeatPayload {
      #[serde(flatten)]
      pub identity: NodeIdentity,
      pub auth_token: String,   // always serialised; empty string when not configured
  }
  ```

**Modified file:** `coordinator/src/server.rs`
- After receiving the first `Heartbeat`, validate `auth_token` against the coordinator's accepted token list
- If the list is non-empty and the message token is absent or not in the list: send an error response and close the connection
- If the accepted list is empty and `MESH_INSECURE=1` is set: skip validation (dev mode); otherwise treat as misconfiguration and refuse connection

**Token list for rotation:** the coordinator accepts a `MESH_AUTH_TOKEN` (current) and optionally `MESH_AUTH_TOKEN_NEXT` (next). This allows a rolling rotation without a full cluster stop. Use `just rotate-token` for automated zero-downtime rotation — it opens the dual-token window, distributes the new token to all compute nodes, waits for reconnection, then revokes the old token. Blast radius note: a compromised shared token grants access to the whole mesh; rotate immediately with `just rotate-token`.

**Modified file:** `agent/src/agent.rs`
- Read `MESH_AUTH_TOKEN` from env; include in every `Heartbeat`

**Token format:** 32-byte random hex string, e.g. `openssl rand -hex 32`

**Justfile:**
- `gen-token` recipe — runs `openssl rand -hex 32` and prints the result
- `deploy-node` — pushes credentials (fingerprint + auth token) automatically at the end of provisioning when the coordinator is already running; prints a reminder to run `just set-fingerprint <node>` when the coordinator is not yet started

---

### Step 4: HMAC message integrity

**New crate dependencies** (`shared/Cargo.toml`):
- `hmac`
- `sha2`

**Modified file:** `shared/src/messages.rs`
- Add optional `hmac` field to the wire envelope (either a wrapper struct or a top-level field alongside the message discriminant):
  ```rust
  pub struct Envelope {
      #[serde(flatten)]
      pub message: MeshMessage,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub hmac: Option<String>,
  }
  ```

**Modified file:** `shared/src/wire.rs` (or wherever framing/serialisation lives)
- `sign(msg: &MeshMessage, key: &[u8]) -> String` — HMAC-SHA256 of the serialised message bytes
- `verify(envelope: &Envelope, key: &[u8]) -> bool` — recompute and compare
- On send: if key is configured, attach `hmac`
- On receive: if `hmac` is present and key is configured, verify; reject if mismatch; pass through if `hmac` absent and key not enforced (rollout mode)

The HMAC key is the same `MESH_AUTH_TOKEN` value used for Heartbeat validation.

---

## Rollout Order (as implemented)

1. Deploy coordinator TLS + agent TLS together — protocol changes from plain TCP to TLS
2. On first `just start-cluster` or `just restart-coordinator`: coordinator auto-generates `MESH_AUTH_TOKEN` if unset, writes to `coordinator.state`, and pushes fingerprint + token to all compute nodes via `set-fingerprint` — no manual steps
3. `just deploy-node <name>` for new nodes: credentials pushed automatically at end of provisioning if coordinator is running
4. `just rotate-token` for zero-downtime key rotation at any time
5. Step 4 (HMAC) — **implemented as Phase 10.5**; see `shared/src/frame.rs`. The actual implementation differs from the draft above: `SignedFrame { ts, payload, sig }` (no optional field on `MeshMessage`), HKDF-SHA256 key derivation from `MESH_AUTH_TOKEN`, ±30s timestamp window. All paths (coordinator, agent, CLI) are covered. `just chaos` validates the implementation against the live coordinator.

### Operational notes

**Clock skew** — the ±30s timestamp window means nodes whose clocks differ by more than 30 seconds will be silently disconnected with a "stale frame" warning. If you see this in coordinator logs, check NTP on the offending node: `sudo systemctl restart systemd-timesyncd` (Linux) or `w32tm /resync` (Windows). The coordinator log message for stale frames now includes "check that node clock is NTP-synced".

**CLI HMAC** — the CLI signs messages with the same `MESH_AUTH_TOKEN` as agents. When `MESH_AUTH_TOKEN` is unset the CLI sends plain frames and expects a plain coordinator (dev mode). There is no "debug CLI against a secured coordinator without a token" path — this is intentional; use `MESH_INSECURE=1` only for TLS bypass (not auth bypass).

---

## Testing Plan

**Unit tests:**
- `tls.rs`: cert generates successfully, fingerprint is stable across load/save, wrong fingerprint rejected
- `server.rs`: missing auth token rejected, wrong token rejected, correct token accepted, token validation skipped when `MESH_AUTH_TOKEN` unset
- `wire.rs`: HMAC signs and verifies correctly, tampered payload fails verification, missing HMAC passes when not enforced

**Integration tests:**
- Agent with wrong TLS fingerprint fails to connect
- Agent with correct fingerprint but wrong auth token is disconnected after first Heartbeat
- Legitimate agent (correct fingerprint + correct token) connects and registers successfully
- Existing tests continue to pass (tests use in-memory registries and do not go through TLS)

---

## Crates Affected

| Crate | Changes |
|-------|---------|
| `shared` | `Heartbeat.auth_token`, optional HMAC envelope field, sign/verify helpers |
| `coordinator` | TLS acceptor, token validation on first Heartbeat |
| `agent` | TLS client, fingerprint verifier, auth token in Heartbeat |
| `cli` | TLS client (same as agent — CLI connects to coordinator directly) |

---

## Resolved Design Decisions

- **TLS termination in-Rust, not stunnel/proxy.** `tokio-rustls` + `rcgen` is ~50 lines on top of the existing Tokio accept loop. Both crates are widely used and well-audited. A reverse proxy or stunnel adds an external process to manage on WSL, extra config files, and another thing to restart after suspend recovery. The "single binary, `just run-coordinator`" operational model is preserved.

- **CLI cert trust — SSH known_hosts TOFU.** On first connect to an unknown coordinator, the CLI prints the fingerprint and prompts `Trust this coordinator? [y/N]`. If accepted, the fingerprint is written to `~/.config/ai-mesh/known-coordinators`. Subsequent connections verify silently. `--insecure-skip-tls-verify` and `MESH_FINGERPRINT` env var override for CI/scripts. This is the SSH mental model — familiar and explicit.

- **Token rotation — dual token, automated.** Coordinator accepts `MESH_AUTH_TOKEN` (current) + `MESH_AUTH_TOKEN_NEXT` (optional next). `just rotate-token` automates the full procedure: open the dual-token window, distribute new token to all compute nodes and the local controller, wait for all nodes to reconnect, close the window (revoke old token), clear stale SQLite model state, then reload models. No flag day, no inference drop. A regression test (`clear_all_removes_stale_ready_state_after_coordinator_restart`) guards against the stale-Ready race where SQLite-persisted Ready state causes `wait-ready` to return a false positive before llama-server is actually running.

- **mTLS as a future upgrade.** The TLS infrastructure from Steps 1/2 is fully compatible with adding client cert verification later. Per-node certs would enable individual revocation without rotating the shared secret. Deferred until the cluster grows to warrant the cert management overhead.
