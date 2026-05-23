# ai-mesh

ai-mesh is a lightweight distributed system for connecting multiple machines
into a hardware-aware inference mesh. Each node runs an agent that reports:

- Hardware specs  
- Inference capabilities  
- Heartbeats  

The coordinator maintains a live registry of all nodes, and the CLI provides
introspection tools.

This README provides a **quick-start reference** for day-to-day development.
For full documentation, see the `docs/` directory.

---

## Quick Start

### 1. Run the Coordinator (WSL / Linux)

```
just run-coordinator
```

This starts the TCP server on port 9000 and hosts the in-memory node registry.

### 2. Run the Controller Agent (local machine)

```
just run-controller
```

This registers the local machine as a **Controller** node.  
Controllers never run inference.

### 3. Provision a Compute Node

Node config lives in `nodes/<name>.env`. Add an entry, then:

```
just deploy-node <name>
```

This builds the correct binary for the node's OS, uploads it, installs
llama-server, and registers a persistent service (systemd on Linux, NSSM on Windows).

### 4. Check Mesh State

```
just sanity-node <name>
```

Or for the full cluster:

```
just sanity-full
```

### 5. Day-to-day Development

```
just dev
```

Starts coordinator + controller locally, bounces all remote node services,
and drops into live `mesh watch`. Ctrl+C stops local processes only.

---

## Cluster Nodes

| Node | OS | Hardware | Model | Capabilities |
|------|----|----------|-------|--------------|
| pi1 | Linux (ARM64) | Raspberry Pi 5, 8 GB RAM | `qwen2.5:1.5b` | llm, lighting |
| Beelink SER8 | Windows 11 | AMD Radeon 780M, 8 GB VRAM | `qwen2.5:7b` | llm |
| Mac mini M4 | macOS (ARM64) | 48 GB unified, 16 CPU / 20 GPU cores | `qwen2.5:32b` | llm |

The coordinator schedules inference requests to whichever node has the requested model loaded and ready.

**Lighting**: pi1 runs Mosquitto + Zigbee2MQTT with an SLZB-06 Zigbee coordinator. Natural language intents like `just intent "turn all lights off"` are routed through the LLM and executed as MQTT commands to Zigbee devices. See `docs/pi1-lighting-setup.md`.

---

## Justfile Reference

| Command | Description |
|---------|-------------|
| `just build` | Build all crates |
| `just test` | Run all tests |
| `just lint` | fmt + clippy |
| `just run-coordinator` | Start coordinator on port 9000 |
| `just run-controller` | Start controller agent |
| `just reset` | Clear all nodes from the live coordinator |
| `just dev` | Start full cluster in dev mode |
| `just deploy-node <node>` | First-time provision or re-provision a node |
| `just update-node <node>` | OTA binary update only (no reprovisioning) |
| `just load-model <node> <model>` | Load a specific model on a node (e.g. `qwen2.5:7b`) |
| `just auto-load-model <node>` | Detect node hardware and load the best-fit model automatically |
| `just start-cluster` | Bring the full cluster up and load the best model on each compute node |
| `just restart-coordinator` | Post-suspend recovery — restart coordinator + controller, reload models (use after opening laptop) |
| `just stop-cluster` | Stop all remote agents and the local coordinator/controller |
| `just uninstall-node <node>` | Remove agent service from a node |
| `just sanity-node <node>` | Check service state + node table |
| `just sanity-full` | Full cluster validation (all nodes) |
| `just validate-routing` | Confirm each model routes to its correct node (run after `start-cluster` or `restart-coordinator`) |
| `just logs-node <node>` | Tail live agent logs from a node |
| `just logs` | Tail all logs simultaneously |
| `just intent "<text>"` | Send a natural-language intent to the coordinator (LLM routes to tool or answers in free text) |
| `just pair-bulb` | Open a 254-second Zigbee pairing window and stream join events |

See `docs/commands.md` for full reference.

---

## Adding a New Node

1. Create `nodes/<name>.env`:
   ```bash
   NODE_HOST=192.168.1.x
   NODE_USER=youruser
   NODE_OS=linux    # or windows
   NODE_ROLE=compute
   ```
2. `just deploy-node <name>`
3. `just sanity-node <name>`

No other files need changing.

---

## Documentation

See the `docs/` directory for detailed architecture, message protocol,
testing strategy, and crate-level documentation.
