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

## Minimal Justfile Reference

These are the only commands required for normal development:

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

---

## Documentation

See the `docs/` directory for detailed architecture, message protocol,
testing strategy, and crate-level documentation.
