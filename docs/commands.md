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

## Local Development

| Command | Description |
|---------|-------------|
| `just run-coordinator` | Start the coordinator on `0.0.0.0:9000` |
| `just run-controller` | Start a controller agent (`AGENT_ROLE=controller`) |
| `just run-agent` | Start a generic agent |
| `just reset` | Send `AdminMessage::ResetRegistry` — clears all nodes from the live coordinator |

---

## Sanity Tests

| Command | Description |
|---------|-------------|
| `just sanity` | Local single-machine test: coordinator + controller agent + CLI node list |
| `just sanity-all` | Full test: kills stale coordinators, starts fresh coordinator, resets registry, starts controller, prints node table |
| `just sanity-pi` | Print node table + open live watch view |

### sanity-all detail

`just sanity-all` is the recommended pre-Phase-6 validation workflow:

1. Kills any stale coordinator process holding port 9000
2. Starts a fresh coordinator
3. Resets the registry (`just reset`)
4. Starts a controller agent
5. Prints the node table — expect exactly one fresh entry
6. Cleans up all processes

---

## Raspberry Pi

| Command | Description |
|---------|-------------|
| `just build-pi` | Cross-compile agent for `aarch64-unknown-linux-gnu` |
| `just deploy-pi` | `scp` binary to Pi at `pi_host` |
| `just run-pi` | Start compute agent on Pi via SSH |

### Pi deployment workflow

```
just build-pi
just deploy-pi
just run-pi
just sanity-pi   # verify Pi appears in node table
```

---

## Configuration Variables

Defined at the top of `justfile` — change once, all targets update:

| Variable | Default | Description |
|----------|---------|-------------|
| `pi_host` | `192.168.1.11` | Pi IP address |
| `pi_user` | `jonno` | Pi SSH username |
| `coordinator_ip` | `192.168.1.12` | Windows host IP (used by Pi to reach coordinator) |
| `coordinator_port` | `9000` | Coordinator TCP port |
