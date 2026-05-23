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

### 1. One-time controller setup

```
just setup-controller
```

Installs Rust, cross-compilation toolchains, git hooks, SSH keys, and Windows portproxy.

### 2. Provision compute nodes

```
just deploy-node pi1
just deploy-node beelink1
```

Builds the correct binary for the node's OS, uploads it, installs llama-server, and
registers a persistent service (systemd on Linux, NSSM on Windows). Also configures
passwordless sudo on Linux nodes so fingerprint pushes work non-interactively.

### 3. Start the cluster

```
just restart-coordinator
```

This starts the coordinator, **automatically generates a TLS certificate**, pushes the
fingerprint to all compute nodes, starts the local controller, and loads the best model
on each compute node. The TLS fingerprint is written to `~/.bashrc` automatically — no
manual configuration needed.

### 4. Check mesh state

```
just nodes          # current node table
just validate-routing   # confirm inference routes to correct nodes
```

### 5. Day-to-day

```
just restart-coordinator   # after waking laptop or any coordinator restart
just intent "turn the lights off"
```

---

---

## Security (TLS)

All coordinator ↔ agent and coordinator ↔ CLI traffic runs over TLS. The coordinator
generates a self-signed certificate on first start and persists it at
`~/.config/ai-mesh/coordinator.crt`. Agents and the CLI verify the cert via its
SHA-256 fingerprint (TOFU model — trust on first contact, reject any change).

**`just restart-coordinator` handles everything automatically:**
- Reads the fingerprint from the coordinator log
- Writes `export MESH_TLS_FINGERPRINT=...` to `~/.bashrc` on the controller machine
- Pushes the fingerprint to every compute node via `just set-fingerprint <node>`

You never need to copy or paste a fingerprint manually.

**To skip TLS verification** (dev/test only):
```bash
MESH_INSECURE=1 cargo run -p cli -- nodes
```

---

## Cluster Nodes

| Node | OS | Hardware | Model | Capabilities |
|------|----|----------|-------|--------------|
| pi1 | Linux (ARM64) | Raspberry Pi 5, 8 GB RAM | `qwen2.5:1.5b` | llm, lighting |
| Beelink SER8 | Windows 11 | AMD Radeon 780M, 8 GB VRAM | `qwen2.5:7b` | llm |
| Mac mini M4 | macOS (ARM64) | 48 GB unified, 16 CPU / 20 GPU cores | `qwen2.5:32b` | llm |

The coordinator schedules inference requests to whichever node has the requested model loaded and ready.

**Lighting**: pi1 runs Mosquitto + Zigbee2MQTT with an SLZB-06 Zigbee coordinator. Natural language intents like `just intent "turn all lights off"` are routed through the LLM and executed as MQTT commands to Zigbee devices. The coordinator receives the live device/group list from pi1 on connect (persisted across restarts), injects it into the LLM system prompt, and validates targets before dispatch — unknown device names return a clear error rather than a silent no-op. See `docs/pi1-lighting-setup.md`.

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
| `just load-model <node> <model>` | Load a specific model on a node (e.g. `qwen2.5:7b`); coordinator also accepts `mesh load <model> <size_mb>` with no node — picks best-fit automatically |
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
| `just load <model>` | Auto-place a model via coordinator (no SSH needed, e.g. `just load qwen2.5:7b`) |
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
