# CLI Crate Documentation

The CLI crate provides a user-facing interface to the ai-mesh system. It communicates with the coordinator over TCP and displays mesh state in a human-friendly format.

---

## 1. Responsibilities

- Query coordinator for mesh status
- List nodes and their properties
- Watch live updates
- Manage update channels (future)
- Provide debugging and introspection tools

---

## 2. Commands

### mesh status
Check if the coordinator is reachable and responding.

### mesh nodes
List all nodes with:
- ID
- Hostname
- IP
- Role
- Last heartbeat (ms)
- Models (live allocation state)

### mesh info <node-id>
Show detailed information about a specific node, including hardware spec, capabilities, and loaded models.

### mesh watch
Full-screen TUI (ratatui, alternate screen). Redraws every second. Exit with `q`, `Esc`, or `Ctrl+C`. Below the node table an Events panel shows timestamped join/leave/model-state events (up to 20).

### mesh load <node-id> <model-name> <size-mb>
Send a `ModelLoad` request to the coordinator targeting the specified node. The coordinator forwards the command to the agent, which responds with `ModelStatus(Loading)` then `ModelStatus(Ready)`.

### mesh infer <model-name> <prompt>
Send a `RequestModelInference` to the coordinator. The coordinator uses `select_node_for_inference` to find a Ready node and forwards the request. Output format:
```
<response text>
served-by: <hostname> | <model> | <tokens> tokens | <ms>ms
```
If hostname lookup fails, the first 8 characters of the node ID are shown instead.

### mesh wait-ready [<ip>...] --timeout <secs>
Poll the coordinator until all specified node IPs have at least one model in `Ready` state, then exit 0. Exits 1 on timeout or user abort (`q`/`Esc`/`Ctrl+C`).

When stdout is a TTY, displays a live ratatui table with per-node status, a colour-coded status bar, and a 5-second linger countdown on success. When stdout is not a TTY (e.g. called from a shell script), falls back to plain-text progress lines — `wait-ready: N/M Ready | Xs elapsed` — so `just start-cluster` doesn't panic with `ENXIO`.

### mesh find-node <host-or-ip>
Return the node ID of the registered node matching the given hostname or IP. Used internally by `just reload-node` and `just load-model` to resolve a node name to its UUID without requiring the user to look it up manually.

### mesh reset-registry
Send `AdminMessage::ResetRegistry` to the coordinator. Clears all registered nodes without restarting.

---

## 3. Communication Protocol

The CLI communicates with the coordinator using:
- **TLS** — coordinator cert verified by SHA-256 fingerprint (`MESH_TLS_FINGERPRINT`); `MESH_INSECURE=1` for dev bypass
- **Auth token** — `AuthToken(token)` sent as the first (unsigned) frame when `MESH_AUTH_TOKEN` is set
- **HMAC-signed frames** — all subsequent messages wrapped in `SignedFrame` (HMAC-SHA256, HKDF key from auth token)
- 4-byte little-endian length prefix + JSON body

All connection setup is handled by `cli/src/connection.rs`; individual commands call `send_recv(stream, msg)`.

---

## 4. Testing Strategy

- Unit tests for command parsing
- Integration tests with a live coordinator instance
- Snapshot tests for table output (future)

---

## 5. Additional Binaries

### chaos (security validation)
`cli/src/bin/chaos.rs` — a standalone binary that fires 6 adversarial scenarios at the live coordinator to verify the HMAC security stack. Run via `just chaos`. Results are exit-code 0 on full pass, 1 on any failure. Automatically invoked by `just validate-routing` as a prerequisite gate.

---

## mesh info <id>

Displays a full diagnostic snapshot of a specific node in the mesh.

### Example
```
mesh info 71bb63d0-ee96-4e54-b750-096cdcc599fb
```

### Output
- Node ID
- Hostname
- IP
- Role
- Last heartbeat age
- Hardware specification
- Capabilities

This command uses:
```
MeshMessage::RequestNodeInfo(id)
MeshMessage::NodeInfo(NodeRecordFull)
```

---

## mesh watch

Full-screen TUI built with `ratatui` (crossterm alternate screen). Redraws every second. Exit with `q`, `Esc`, or `Ctrl+C`.

### Example
```
mesh watch
```

### Layout
- **Status bar** (top) — timestamp, node count, exit hint; turns red on coordinator error
- **Nodes table** — ID, Hostname, IP, Role, Last Seen (ms), Models
- **Events panel** (bottom, appears once first event fires) — timestamped `[+] joined`, `[-] left`, `[M] model state change / removed`; up to 20 entries

### Implementation notes
- Uses `EnterAlternateScreen` on start and `LeaveAlternateScreen` on exit — the preceding terminal output (e.g. `just dev` preamble) is preserved in the scrollback and restored on exit
- Raw mode disables SIGINT, so exit is handled via crossterm key-event polling in a `spawn_blocking` task
- Fetches `MeshMessage::RequestNodes` then `MeshMessage::RequestNodeInfo` per node each tick; diffs successive snapshots to generate events
