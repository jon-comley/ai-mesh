# AI Mesh

This document describes the architecture and operation of the AI Mesh system,
including support for heterogeneous compute nodes across Linux (ARM64, x86_64)
and Windows.

## Compute Nodes

The mesh supports heterogeneous nodes. Any machine can be added as a compute
node by creating a `nodes/<name>.env` file and running `just deploy-node <name>`.

### Workflow

1. **Create a node config file**
   ```bash
   # nodes/mynode.env
   NODE_HOST=10.0.0.x
   NODE_USER=youruser
   NODE_OS=linux      # or windows
   NODE_ROLE=compute
   ```

2. **Provision the node**
   ```
   just deploy-node mynode
   ```
   This cross-compiles the correct binary, uploads it, installs llama-server, and
   registers a persistent service.

3. **Verify the node is registered**
   ```
   just sanity-node mynode
   ```

The node will appear in the node table with role `Compute`.

## Coordinator

The coordinator is the central hub of the mesh. It:

- Accepts **TLS-encrypted** TCP connections (self-signed cert, SHA-256 fingerprint TOFU)
- Validates `AuthToken` first-frame and HMAC-signed `SignedFrame` messages
- Maintains a **SQLite-backed** registry of nodes (survives restarts)
- Tracks heartbeats and last-seen timestamps
- Routes model load, inference, lighting, and intent commands to the correct agent
- Writes `/var/lib/ai-mesh/coordinator.state` on startup

The coordinator binds to `0.0.0.0:9000` and runs on **pi1** (`pi1.local`) as an always-on systemd service; nodes connect there directly. (Historically it ran in WSL2 behind a Windows portproxy — that portproxy is now vestigial.)

### Message Handling

| Message | Action |
|---------|--------|
| `Heartbeat` | Updates last-seen; validates per-heartbeat auth token; registers agent tx |
| `HardwareReport` | Stores hardware specification |
| `Capabilities` | Stores capability information |
| `RequestNodes` | Returns `NodeList(Vec<NodeRecordLite>)` |
| `RequestNodeInfo(id)` | Returns `NodeInfo(NodeRecordFull)` including model allocations |
| `ModelLoad` | Forwards to target agent via connections map |
| `ModelStatus` | Updates model allocation state in registry |
| `ModelUnload` | Forwards to target agent; agent kills llama-server |
| `RequestModelInference` | Selects ready node via scheduler; forwards; awaits result (up to 300 s) |
| `LightCommand` | Forwards to lighting node's tx channel |
| `LightDeviceList` | Persists device/group names to SQLite |
| `IntentRequest` | LLM routing + tool-call dispatch (lighting, inference) |
| `Admin(ResetRegistry)` | Clears all nodes from registry |

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

The agent binary is cross-compiled for each target platform from WSL and
deployed via `just deploy-node`.

### Node Roles

Roles are set via the `AGENT_ROLE` environment variable (written into the
service definition by the install scripts).

| Role | Behaviour |
|------|-----------|
| `compute` (default) | Full hardware + capability reporting; eligible for inference |
| `controller` | Sends heartbeats only; manages mesh; never used for inference |

### Agent Binary Locations

| OS | Path |
|----|------|
| Linux | `/home/<user>/agent` |
| Windows | `C:\Users\<user>\ai-mesh\agent.exe` |

## Controller Agent

The controller agent runs on the developer machine and is responsible for
orchestrating tasks, dispatching work, and interacting with the coordinator.

## Justfile Targets

| Command | Description |
|---------|-------------|
| `just deploy-node <node>` | Provision or re-provision a node |
| `just update-node <node>` | OTA binary update |
| `just uninstall-node <node>` | Remove agent service |
| `just sanity-node <node>` | Service check + node table |
| `just sanity-full` | Full cluster validation |
| `just dev` | Dev loop: start cluster, watch |

Per-node configuration (host, user, OS, role) lives in `nodes/<name>.env`.
The justfile contains only coordinator-level variables (`coordinator_ip`, `coordinator_port`).
