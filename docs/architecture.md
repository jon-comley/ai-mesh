# ai-mesh Architecture Overview

This document captures the foundational design principles, workflows, and engineering standards for the ai-mesh project. It serves as the primary reference for humans and AIs collaborating on the system.

---

## 1. Project Goals

- Build a distributed mesh of nodes capable of hardware detection, capability reporting, and coordinated model execution.
- Support auto-discovery, auto-updates, and a flexible message protocol.
- Maintain extremely high code quality, test coverage, and documentation.
- Enable multiple AIs (Copilot, , Gemini) to collaborate effectively.

---

## 2. Workspace Structure

```
ai-mesh/
  shared/                    # Shared types, messages, HMAC wire protocol
  agent/                     # Node agent: hardware detection, heartbeats, inference
  coordinator/               # Central orchestrator: TLS server, registry, scheduler, HTTP dashboard
  cli/                       # CLI + chaos security-test binary
  capabilities/
    core/                    # Capability trait and dispatch
    llm/                     # llama-server integration (GGUF download, inference)
    lighting/                # Zigbee2MQTT MQTT client (pi1)
    zigbee/                  # Zigbee device/group discovery
  docs/                      # Documentation
  nodes/                     # Per-node config files (nodes/<name>.env)
  scripts/                   # Provisioning scripts (OS-dispatched)
  plans/                     # Design docs for specific features
  Cargo.toml                 # Workspace manifest
```

---

## 3. AI-Augmented Development Workflow

The project uses a three-AI development loop:

### Copilot (Architect)
- Designs modules
- Generates code, tests, and documentation
- Ensures architectural consistency

###  CLI (Implementation Engineer)
- Creates and edits files
- Applies patches
- Shows diffs for human review

### Gemini (Debugger & Reviewer)
- Explains errors
- Reviews diffs
- Suggests improvements

### Human (Jonathan)
- Lead engineer
- Reviews diffs
- Approves changes
- Runs builds/tests
- Provides direction

This workflow ensures correctness, clarity, and rapid iteration.

---

## 4. Documentation Philosophy

Documentation is created incrementally alongside code. The docs folder will contain:

- architecture.md (this file)
- commands.md (just target reference)
- shared.md
- agent.md
- coordinator.md
- cli.md
- testing.md
- roadmap.md
- decisions/ (ADR-style design decisions)

Each module gets its own `.md` file explaining:
- Purpose
- API
- Invariants
- Interactions
- Tests
- Design rationale

This creates a living knowledge base for humans and AIs.

---

## 5. Tooling & Quality Standards

### Rust Tooling
- rustfmt (formatting)
- clippy (linting)
- cargo-watch (auto rebuild)
- cargo-llvm-cov (coverage)

### Workspace Linting
A `.cargo/config.toml` will enforce:
```
-Dwarnings
```

### Pre-commit Hook
Runs:
- cargo fmt
- cargo clippy
- cargo test

### Justfile
Provides shortcuts for:
- build
- test
- lint
- run-agent
- run-coordinator
- deploy-node
- update-node
- uninstall-node
- sanity-node
- sanity-full (full cluster validation)

---

## 6. Testing Strategy

- Every module includes unit tests.
- Integration tests live in `tests/`.
- Coverage is tracked using llvm-cov.
- Tests are required before commit.
- AIs generate tests alongside code.

---

## 7. Versioning & Update Strategy

The shared crate defines:
- VersionInfo
- UpdateManifest
- UpdateChannel (Stable, Beta, Canary)

The coordinator will manage:
- Update distribution
- Rollbacks
- Version negotiation

---

## 8. Next Steps

1. Implement the shared crate (types, messages, tests).
2. Document the shared crate in `docs/shared.md`.
3. Implement hardware detection in the agent.
4. Implement heartbeats and capability reporting.
5. Implement coordinator registry.
6. Add CLI after git init.

---

This document will evolve as the project grows.

---

## 9. Completed Components (Milestone Summary)

The following components are now fully implemented and tested:

### Shared Crate
- Message protocol (`MeshMessage`)
- HardwareSpec, NodeIdentity, NodeCapabilities
- Versioning structures
- JSON serialization
- Full test suite

### Agent Crate
- Hardware detection subsystem
- Identity detection subsystem
- Capability detection subsystem
- Async heartbeat loop
- Initial message flow
- Full test suite (sync + async)

### Tooling
- Pre‑commit hook (fmt, clippy, tests)
- Workspace‑wide `-D warnings`
- Justfile for common tasks
- Documentation mesh

This forms a stable foundation for distributed behaviour.

---

## 15. Security Architecture (Phase 10 / 10.5)

Three independent layers, each independently useful:

| Layer | Mechanism | What it stops |
|-------|-----------|---------------|
| TLS | Self-signed cert + SHA-256 fingerprint TOFU | Eavesdropping, MITM |
| AuthToken | First-frame `AuthToken(token)` + per-heartbeat field | Unauthenticated connections, rogue nodes |
| HMAC | `SignedFrame { ts, payload, sig }` — HKDF-SHA256 key | Message forgery, replay attacks (±30 s window) |

All three are active whenever `MESH_AUTH_TOKEN` is configured (the normal production case). Dev mode (`MESH_AUTH_TOKEN` unset) skips auth and HMAC but keeps TLS optional via `MESH_INSECURE=1`.

---

## 10. Agent → Coordinator Message Flow (Current)

```
Agent Startup
├── AuthToken(token)          — first frame, connection-level auth
├── Heartbeat(HeartbeatPayload)
│   ├── NodeIdentity (flattened)
│   ├── auth_token            — per-heartbeat defence-in-depth
│   ├── cpu_usage_pct         — all-core CPU % (required since C2; pre-C2 agents rejected)
│   ├── ram_used_gb           — RAM in use, GiB (required since C2)
│   └── ram_total_gb          — total RAM, GiB (required since C2)
├── HardwareReport(HardwareSpec)
├── Capabilities(NodeCapabilities)
└── Heartbeat Loop (every N seconds, interval adjustable via SetHeartbeatInterval)

Coordinator → Agent
├── ModelLoad / ModelUnload / RequestModelInference
└── SetHeartbeatInterval { secs }   — Phase C+: dynamically change heartbeat period
```

All frames after `AuthToken` are wrapped in `SignedFrame { ts, payload, sig }` when `MESH_AUTH_TOKEN` is set (HMAC-SHA256, HKDF key derivation).

---

## 16. Health Timeline Architecture (Phase 11C)

Health metrics flow from agent → coordinator → dashboard:

```
Agent (sysinfo crate + gpu.rs)                             ← C2 ✓ / C7 ✓
└── HeartbeatPayload { cpu_usage_pct, ram_used_gb, ram_total_gb,
                       gpu_usage_pct?, gpu_vram_used_gb?, gpu_vram_total_gb? }
    Linux GPU: amdgpu sysfs (/sys/class/drm/card0/device/gpu_busy_percent etc.)
    Windows GPU: PowerShell perf counter query (no extra crates)
    CPU-only nodes: GPU fields omitted (serde(default) → None on coordinator)

Coordinator (HealthStore in DashboardState)               ← C3 ✓ / C7 ✓
├── On each Heartbeat: stamp coordinator ts_ms, append HealthSample
├── HealthSample: { ts_ms, cpu_pct, ram_used_gb, ram_total_gb,
│                   gpu_pct?, gpu_vram_used_gb?, gpu_vram_total_gb? }
├── Ring buffer per node: Mutex<HashMap<node_id, VecDeque<HealthSample>>> capped at 60
└── Broadcast DashboardEvent::HealthUpdate { node_id, samples } over WebSocket

WebSocket connect                                           ← C5 ✓
└── coordinator calls get_all_health_snapshots() and pushes HealthUpdate per node
    so sparklines populate immediately without waiting for the next heartbeat

Dashboard (health.js)                                      ← C5 ✓ / C7 ✓
├── Health panel: SVG sparklines (CPU %, RAM %) per node + current values
├── Health panel: GPU% + VRAM sparklines shown only when node reports GPU data
├── Nodes panel: mini CPU sparkline in each node card
├── repaintAll() refills mini sparklines after every TopologyUpdate
└── Interval control button → POST /api/nodes/{id}/heartbeat-interval  ← C4 ✓
                            → coordinator pushes SetHeartbeatInterval to agent
```

**Design decisions:**
- Timestamps are stamped by the coordinator on receipt to avoid clock-skew across nodes.
- `HealthSample` is a coordinator-only struct — not part of the `shared` wire protocol.
- RAM% and VRAM% are derived by the dashboard JS, not the coordinator.
- GPU fields are `Option<f32>` with `serde(default)` — CPU-only nodes omit them; pre-C7 agents remain compatible.
- Windows GPU reads via PowerShell subprocess (~1 s overhead per heartbeat); acceptable at 30 s cadence and avoids the `wmi` crate.
- `SetHeartbeatInterval` is pushed over the agent's existing open TCP connection — no extra endpoint needed on the agent.

---

## 11. Next Major Phase: Coordinator Crate

The next stage of development focuses on the coordinator, which will include:

- Node registry (identity, hardware, capabilities, last‑seen)
- Async TCP server for receiving messages
- Message routing and state updates
- Update manifest distribution
- CLI query support (future)
- End‑to‑end tests with agent

This phase will complete the core mesh architecture.

---

## 12. Coordinator Crate Completed (Milestone Summary)

The coordinator crate now includes:

- In‑memory node registry
- Async TCP server
- Message routing
- Coordinator orchestrator
- Full async test suite

This completes the backend of the mesh.

---

## 13. End‑to‑End Message Flow (Agent → Coordinator)

```
Agent Startup
├── Heartbeat(NodeIdentity)
├── HardwareReport(HardwareSpec)
├── Capabilities(NodeCapabilities)
└── Heartbeat Loop (every N seconds)

Coordinator
├── TCP server receives messages
├── JSON decoding
├── Registry updates
├── Acknowledge responses
└── State exposed to CLI (future)
```

---

## 14. Next Major Phase: CLI Crate

The CLI will provide:

- `mesh status` — show coordinator health
- `mesh nodes` — list all nodes
- `mesh watch` — live updates
- `mesh updates` — manage update channels

This will complete the user‑facing interface of the system.

---

## New Message Types (Phase 5)

### RequestNodeInfo
Sent by the CLI to request a full diagnostic record for a specific node.

### NodeInfo
Returned by the coordinator. Contains:
- Identity
- Hostname
- IP
- Role
- Last heartbeat age
- HardwareSpec (optional)
- NodeCapabilities (optional)

### Updated Message Flow

CLI → Coordinator:
- RequestNodes
- RequestNodeInfo(id)

Coordinator → CLI:
- NodeList(Vec<NodeRecordLite>)
- NodeInfo(NodeRecordFull)

### Registry Additions

The coordinator registry now supports:
- `list_nodes()` — returns lightweight node summaries
- `get_node_full(id)` — returns full node diagnostics

### CLI Capabilities (Updated)

The CLI now supports:
- `mesh nodes` — list all nodes
- `mesh info <id>` — full node diagnostics
- `mesh watch` — live-updating node table

---
