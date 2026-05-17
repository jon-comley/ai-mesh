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

### 2. Run the Controller Agent (OmniBook)

```
just run-controller
```

This registers the OmniBook as a **Controller** node.  
Controllers never run inference.

### 3. Build & Deploy the Agent to Raspberry Pi

Cross-compile for ARM64:

```
just build-pi
```

Deploy the binary to the Pi:

```
just deploy-pi
```

### 4. Run the Pi Agent

```
just run-pi
```

This registers the Pi as a **Compute** node.

### 5. Check Mesh State

```
just sanity-pi
```

This prints the current node table via the CLI.

### Full Cluster Validation

```
just sanity-all
```

This starts:
- Coordinator
- Controller agent
- Pi compute agent

And validates the mesh state.

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
| `just sanity-all` | Full local cluster validation (recommended) |
| `just build-pi` | Cross-compile agent for ARM64 |
| `just deploy-pi` | Deploy binary to Pi |
| `just run-pi` | Start compute agent on Pi via SSH |
| `just sanity-pi` | Check node table |

See `docs/commands.md` for full reference.

---

## Documentation

See the `docs/` directory for detailed architecture, message protocol,
testing strategy, and crate-level documentation.
