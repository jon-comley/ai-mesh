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
| `just dev` | **Start here.** Kills stale local processes, starts coordinator + controller agent, verifies portproxy connectivity, bounces Pi + Beelink services, drops into live `mesh watch`. Ctrl+C stops local services only — remote nodes keep running |
| `just run-coordinator` | Start coordinator on `0.0.0.0:9000` (foreground) |
| `just run-controller` | Start controller agent in `controller` role (foreground) |
| `just reset` | Send `AdminMessage::ResetRegistry` — clears all nodes from the live coordinator |

---

## Logs

| Command | Description |
|---------|-------------|
| `just logs` | Tail coordinator + controller + Pi + Beelink log streams simultaneously with prefixes. Ctrl+C cleans up all SSH sessions |
| `just logs-pi` | Live tail of Pi agent systemd journal (`journalctl -u ai-mesh-agent -f`) |
| `just logs-beelink` | Live tail of Beelink agent log file via SSH |

---

## Sanity Tests

| Command | Description |
|---------|-------------|
| `just sanity` | Local single-machine test: coordinator + controller + CLI node list |
| `just sanity-all` | Local test: kills stale processes, starts fresh, resets, runs controller, prints node table |
| `just sanity-full` | Full cluster: coordinator + controller + Pi + Beelink; stops all remote agents first |
| `just sanity-pi` | Verify Pi appears in node table (requires coordinator running) |
| `just sanity-beelink` | Coordinator + controller locally, restart Beelink service, check node table |

### sanity-all detail

`just sanity-all` is the recommended local validation workflow:

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
| `just deploy-pi` | Stop Pi service, ship updated binary, reinstall + restart service |
| `just run-pi` | Install/update `ai-mesh-agent` systemd service on Pi and start it |
| `just logs-pi` | Live tail of Pi agent journal log |
| `just sanity-pi` | Verify Pi appears in node table (requires coordinator running) |

### Pi deployment workflow

```
just build-pi
just deploy-pi    # stops service, uploads binary, reinstalls service
just sanity-pi    # verify Pi appears in node table
```

The Pi agent runs as a persistent systemd service (`ai-mesh-agent`) with `Restart=always`. It reconnects automatically when the coordinator restarts — no manual intervention needed.

---

## Beelink (Windows)

| Command | Description |
|---------|-------------|
| `just build-beelink-exe` | Cross-compile agent for `x86_64-pc-windows-gnu` |
| `just update-beelink` | Rebuild, upload via temp file, restart NSSM service on Beelink |
| `just sanity-beelink` | Coordinator + controller locally, restart Beelink service, check node table |
| `just logs-beelink` | Live tail of Beelink agent log file |

### Beelink deployment workflow

```
just build-beelink-exe
just update-beelink    # uploads as agent_next.exe, moves to agent.exe, restarts service
```

The Beelink agent runs as an NSSM service (`ai-mesh-agent`). Never SCP directly to `agent.exe` while the service is running — it's file-locked.

---

## Inference

| Command | Description |
|---------|-------------|
| `just test-inference` | End-to-end inference test: loads `qwen2.5:0.5b` on all compute nodes, fires 4 requests |

---

## Configuration Variables

Defined at the top of `justfile` — change once, all targets update:

| Variable | Default | Description |
|----------|---------|-------------|
| `pi_host` | `192.168.1.11` | Pi IP address |
| `pi_user` | `jonno` | Pi SSH username |
| `beelink_host` | `192.168.1.14` | Beelink IP address |
| `beelink_user` | `jonno` | Beelink SSH username |
| `coordinator_ip` | `192.168.1.12` | Windows host IP (used by remote nodes to reach coordinator via portproxy) |
| `coordinator_port` | `9000` | Coordinator TCP port |
