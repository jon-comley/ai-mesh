# AI Mesh

This document describes the architecture and operation of the AI Mesh system.
It has been updated to include support for Raspberry Pi compute nodes and the
minimal `just` targets required for day‑to‑day development.

## Raspberry Pi Compute Node

The mesh supports heterogeneous nodes, including ARM64 devices such as the
Raspberry Pi. A Pi runs the same agent binary as other nodes, compiled for
`aarch64-unknown-linux-gnu`.

### Workflow

1. **Cross‑compile the agent for ARM64**
   ```
   just build-pi
   ```

2. **Deploy the agent to the Pi**
   ```
   just deploy-pi
   ```

3. **Run the agent on the Pi**
   ```
   just run-pi
   ```

4. **Verify the Pi is registered**
   ```
   just sanity-pi
   ```

The Pi will appear in the node table with role `Compute`.

## Coordinator

The coordinator is the central hub of the mesh. It:

- Accepts TCP connections from agents and CLI clients
- Maintains an in-memory registry of nodes
- Tracks heartbeats and last-seen timestamps
- Stores hardware and capability reports
- Responds to CLI queries

The coordinator binds to `0.0.0.0:9000` to accept connections from all
network interfaces, including remote nodes such as Raspberry Pi compute nodes.

### Message Handling

| Message | Action |
|---------|--------|
| `Heartbeat` | Updates last-seen timestamp |
| `HardwareReport` | Stores hardware specification |
| `Capabilities` | Stores capability information |
| `RequestNodes` | Returns `NodeList(Vec<NodeRecordLite>)` |
| `RequestNodeInfo(id)` | Returns `NodeInfo(NodeRecordFull)` |

### Concurrency Model

The registry is protected by a `Mutex`. Locks are never held across `.await`.
All message processing happens inside a single async loop per connection.

## Agent

Each node in the mesh runs the agent binary. On startup it:

1. Detects hardware specifications.
2. Determines inference capabilities.
3. Identifies itself (ID, hostname, IP).
4. Sends a heartbeat and hardware/capability reports to the coordinator.
5. Enters a periodic heartbeat loop.

The agent binary is compiled for the target platform. For ARM64 devices it is
cross-compiled on the developer machine and deployed via `just deploy-pi`.

### Node Roles

Roles are set via the `AGENT_ROLE` environment variable.

| Role | Behaviour |
|------|-----------|
| `compute` (default) | Full hardware + capability reporting; eligible for inference |
| `controller` | Sends heartbeats only; manages mesh; never used for inference |

## Controller Agent

The controller agent runs on the developer machine and is responsible for
orchestrating tasks, dispatching work, and interacting with the coordinator.

## Minimal Justfile Targets

The project includes a minimal set of `just` targets to support development
without unnecessary complexity. These targets provide the essential workflow
for building, deploying, and running agents.

```
build-pi:
    cargo build --release --target aarch64-unknown-linux-gnu -p agent

deploy-pi: build-pi
    scp target/aarch64-unknown-linux-gnu/release/agent pi@192.168.1.11:/home/pi/agent

run-pi:
    ssh pi@192.168.1.11 "COORDINATOR_IP=192.168.1.12 AGENT_ROLE=compute /home/pi/agent"

sanity-pi:
    cargo run -p cli -- nodes

run-coordinator:
    cargo run -p coordinator

run-controller:
    AGENT_ROLE=controller cargo run -p agent
```

These targets intentionally avoid orchestration or automation beyond what is
required for a fast development loop.
