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

### Layer 2 — Shared auth token in Heartbeat

Each agent includes an `auth_token` in its `Heartbeat` message. The coordinator rejects the connection if the first `Heartbeat` carries an absent or wrong token. A single shared secret covers all nodes for now; per-node tokens (allowing individual revocation) are a future improvement.

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

**Modified file:** `shared/src/messages.rs`
- Add to `Heartbeat`:
  ```rust
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub auth_token: Option<String>,
  ```

**Modified file:** `coordinator/src/server.rs`
- After receiving the first `Heartbeat`, validate `auth_token` against the coordinator's accepted token list
- If the list is non-empty and the message token is absent or not in the list: send an error response and close the connection
- If the accepted list is empty and `MESH_INSECURE=1` is set: skip validation (dev mode); otherwise treat as misconfiguration and refuse connection

**Token list for rotation:** the coordinator accepts a `MESH_AUTH_TOKEN` (current) and optionally `MESH_AUTH_TOKEN_NEXT` (next). This allows a rolling rotation — add the new token to the coordinator first, then update nodes one by one, then remove the old token — without a full cluster stop. Blast radius note: a compromised shared token grants access to the whole mesh; rotation procedure is: generate new token, deploy to coordinator, update each node via `just update-node`, remove old token.

**Modified file:** `agent/src/agent.rs`
- Read `MESH_AUTH_TOKEN` from env; include in every `Heartbeat`

**Token format:** 32-byte random hex string, e.g. `openssl rand -hex 32`

**Justfile:**
- `gen-token` recipe — runs `openssl rand -hex 32` and prints the result
- `deploy-node` — already propagates env vars from `nodes/<name>.env`; just add `MESH_AUTH_TOKEN` to the template

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

## Rollout Order

1. Deploy Step 1 (coordinator TLS) + Step 2 (agent TLS) together — all nodes must update at the same time since the protocol changes from plain TCP to TLS
2. `just run-coordinator` prints the fingerprint on startup
3. Add `MESH_TLS_FINGERPRINT` to each `nodes/<name>.env`
4. `just update-node <name>` for each node
5. Generate token: `just gen-token` → add `MESH_AUTH_TOKEN` to coordinator env and each `nodes/<name>.env`
6. `just update-node <name>` again (or include in same deployment as Step 1)
7. Deploy Step 4 (HMAC) independently — optional field, backwards compatible

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
- Existing 182 tests continue to pass (tests use in-memory registries and do not go through TLS)

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

- **Token rotation — dual token, graduated rollout.** Coordinator accepts `MESH_AUTH_TOKEN` (current) + `MESH_AUTH_TOKEN_NEXT` (optional next). Rotation procedure: (1) set `MESH_AUTH_TOKEN_NEXT` on coordinator and restart; (2) update nodes one by one via `just update-node`, each connecting with the new token; (3) once all nodes migrated, promote next → current and remove next. No flag day required.

- **mTLS as a future upgrade.** The TLS infrastructure from Steps 1/2 is fully compatible with adding client cert verification later. Per-node certs would enable individual revocation without rotating the shared secret. Deferred until the cluster grows to warrant the cert management overhead.
