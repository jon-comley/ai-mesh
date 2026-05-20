# Just Command Reference

All commands are run via `just <target>` from the workspace root.

---

## Build & Test

| Command | Description |
|---------|-------------|
| `just build` | Build all crates |
| `just test` | Run all tests |
| `just lint` | Run `cargo fmt` + `cargo clippy` |

---

## Dev Workflow

| Command | Description |
|---------|-------------|
| `just dev` | **Start here.** Kills stale local processes, starts coordinator + controller, verifies portproxy, bounces all remote node services, drops into live `mesh watch`. Ctrl+C stops local services only — remote nodes keep running |
| `just run-coordinator` | Start coordinator on `0.0.0.0:9000` (foreground) |
| `just run-controller` | Start controller agent (foreground) |
| `just reset` | Send `AdminMessage::ResetRegistry` — spins up a temporary coordinator, clears all nodes, then shuts it down |

---

## Generic Node Management

Node config lives in `nodes/<name>.env`. Each file defines four variables:

```bash
NODE_HOST=192.168.1.x
NODE_USER=youruser
NODE_OS=linux        # or windows
NODE_ROLE=compute    # or controller
```

| Command | Description |
|---------|-------------|
| `just deploy-node <node>` | First-time provision or full re-provision. Builds the correct binary, uploads it, installs llama-server, and registers the agent service |
| `just update-node <node>` | OTA binary update only — rebuild, upload, restart. No reprovisioning |
| `just load-model <node> <model>` | Load a specific model on a live node (e.g. `just load-model pi1 qwen2.5:1.5b`). Prints hardware-filtered fallback options if the model fails to load |
| `just auto-load-model <node>` | Detect node hardware and automatically load the best-fit model |
| `just start-cluster` | Bring the full cluster up: coordinator, controller, all remote agents, and hardware-selected models on every compute node. Leaves everything running after exit |
| `just uninstall-node <node>` | Remove the `ai-mesh-agent` service from the node |
| `just logs-node <node>` | Live tail of the agent log on the node |
| `just sanity-node <node>` | Check service state on the node and print the node table |

### Supported models

| Model | Size on disk | Minimum memory | Default node |
|-------|-------------|----------------|--------------|
| `qwen2.5:0.5b` | ~0.5 GB | 1 GB | fallback only |
| `qwen2.5:1.5b` | ~1 GB | 2 GB | Pi (ARM64) |
| `qwen2.5:7b` | ~4 GB | 4 GB VRAM / 10 GB CPU RAM | Beelink SER8 (AMD Radeon 780M, ~4 GB VRAM) |
| `qwen2.5:14b` | ~8 GB | 9 GB | — |
| `qwen2.5:32b` | ~20 GB | 22 GB | Mac mini M4 (48 GB unified) |

Models are downloaded as GGUF shards from Hugging Face on first `load-model`. Nothing is pre-cached during provisioning.

`just auto-load-model <node>` detects available GPU VRAM (or system RAM for CPU-only nodes) and selects the largest model that fits. The same thresholds are baked into the install scripts, which log the selected model and the `auto-load-model` command to run after provisioning.

**Windows AMD GPU nodes:** GPU acceleration requires AMD Adrenalin driver 26.5.2+. The install script installs llama-server with the Vulkan backend automatically. The driver must be installed manually before running `just deploy-node`. See `docs/windows-node-setup.md` for details and measured performance figures.

### Adding a new node

1. Create `nodes/<name>.env` with the four variables above
2. `just deploy-node <name>` — no other files need changing

---

## Logs

| Command | Description |
|---------|-------------|
| `just logs` | Tail coordinator + controller + all remote node logs simultaneously with prefixes. Ctrl+C cleans up all SSH sessions |
| `just logs-node <node>` | Live tail of a single node's agent log |

---

## Diagnostics

| Command | Description |
|---------|-------------|
| `just hardware-report` | Print hardware + capability summary for every registered node. Starts coordinator in the background if not running, starts remote agents, then streams each node's block as it registers over a 20 s scan window |
| `just start-agents` | Start `ai-mesh-agent` on all remote nodes without touching the local coordinator. Safe to call when agents are already running (`systemctl start` is idempotent) |

---

## Sanity Tests

| Command | Description |
|---------|-------------|
| `just sanity` | Local single-machine test: coordinator + controller + CLI node list |
| `just sanity-node <node>` | Check service state on a specific node and show the node table |
| `just sanity-full` | Full cluster: coordinator + controller + all nodes in `nodes/`; stops all remote agents first, resets registry, starts fresh |

### sanity-full detail

`just sanity-full` is the recommended full-cluster validation workflow:

1. Refreshes the portproxy (WSL2 IP may have changed)
2. Stops all remote agent services (SSH with `ConnectTimeout=5` — offline nodes warn and continue)
3. Kills any stale local coordinator / agent processes
4. Starts a fresh coordinator
5. Resets the registry
6. Starts a controller agent
7. Starts all remote node services (via `nodes/*.env`)
8. Waits 12 s for registration
9. Prints the node table — expect one entry per node

---

## Provisioning Scripts

| Script | Purpose |
|--------|---------|
| `scripts/install-node-windows.ps1` | Windows node installer. Params: `-CoordinatorIp`, `-Role`. Installs llama-server (Vulkan) via ZIP and registers the NSSM agent service |
| `scripts/uninstall-node-windows.ps1` | Removes NSSM service. Flag: `-RemoveBinary` |
| `scripts/install-node-linux.sh` | Linux node installer. Args: `<coordinator_ip> <role> <user>`. Installs llama-server via tarball and registers a systemd service |
| `scripts/uninstall-node-linux.sh` | Removes systemd service |

These are uploaded and run remotely by the justfile recipes — you do not need to run them manually.

---

## Inference

| Command | Description |
|---------|-------------|
| `just test-inference` | End-to-end inference test: loads `qwen2.5:1.5b` on all compute nodes, fires 4 requests |

---

## Configuration Variables

Defined at the top of `justfile`:

| Variable | Default | Description |
|----------|---------|-------------|
| `coordinator_ip` | `192.168.1.12` | Windows host IP used by remote nodes to reach the coordinator via portproxy |
| `coordinator_port` | `9000` | Coordinator TCP port |

Per-node config (host, user, OS, role) lives in `nodes/<name>.env`, not in the justfile.
