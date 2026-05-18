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
Stream live updates from the coordinator. Redraws the node table every second.

### mesh load <node-id> <model-name> <size-mb>
Send a `ModelLoad` request to the coordinator targeting the specified node. The coordinator forwards the command to the agent, which responds with `ModelStatus(Loading)` then `ModelStatus(Ready)`.

### mesh reset-registry
Send `AdminMessage::ResetRegistry` to the coordinator. Clears all registered nodes without restarting.

### mesh updates
Manage update channels (future).

---

## 3. Communication Protocol

The CLI communicates with the coordinator using:
- TCP
- Length-prefixed JSON messages
- MeshMessage protocol from the shared crate

---

## 4. Testing Strategy

- Unit tests for command parsing
- Integration tests with a live coordinator instance
- Snapshot tests for table output (future)

---

## 5. Next Steps

- `mesh infer <model-name> <prompt>` — send `RequestModelInference` and display result
- `mesh models` — list models currently loaded across all nodes
- `mesh deploy <model>` — trigger scheduler-selected placement
- Update management

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

Live-updating view of all nodes in the mesh.

Refreshes every second and redraws the table without flicker.

### Example
```
mesh watch
```

### Output
A continuously updating table of:
- Node ID
- Hostname
- IP
- Last Seen (ms)

This command repeatedly sends:
```
MeshMessage::RequestNodes
```
and displays the returned `NodeList`.
