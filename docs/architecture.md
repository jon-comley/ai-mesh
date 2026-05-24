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
  coordinator/               # Central orchestrator: TLS server, registry, scheduler
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

The system currently supports the following outbound flow from agent to coordinator:

```
Agent Startup
├── Heartbeat(NodeIdentity)
├── HardwareReport(HardwareSpec)
├── Capabilities(NodeCapabilities)
└── Heartbeat Loop (every N seconds)
```

Coordinator behaviour will be implemented in Phase 4.

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
