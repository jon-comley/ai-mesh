# Coordinator Documentation

## Responsibilities
- Accept TCP connections from agents and CLI clients
- Maintain an in-memory registry of nodes
- Track heartbeats and last-seen timestamps
- Store hardware and capability reports
- Route model load commands to the correct agent
- Respond to CLI queries

## Architecture

### Connection Routing Map
Each agent connection registers its outbound `mpsc::Sender<MeshMessage>` in a shared `Arc<Mutex<HashMap<String, mpsc::Sender>>>` keyed by node ID. This enables the coordinator to push commands (e.g. `ModelLoad`) down to a specific agent's live TCP connection. Entries are removed when the socket drops.

### Scheduler
`coordinator::Scheduler` has two selection methods:

**`select_node_for_model(size_mb: u64)`** — finds a Compute node with enough remaining capacity to load a new model:
- Role must be `Compute`
- Remaining capacity: `max_model_size_gb * 1024 - allocated_mb >= size_mb`
- `Ready` and `Loading` allocations count against capacity; `Unloaded` and `Failed` do not

**`select_node_for_inference(model_name: &str)`** — finds a Compute node that already has the named model in `Ready` state:
- Role must be `Compute`
- `models` must contain an entry for `model_name` with `state == Ready`
- `Loading`, `Unloaded`, and `Failed` states are excluded
- Used by the `RequestModelInference` handler in `server.rs`

## Message Handling

| Message | Action |
|---------|--------|
| `Heartbeat` | Updates last-seen timestamp; registers agent tx in connections map |
| `HardwareReport` | Stores hardware specification for the node |
| `Capabilities` | Stores capability information for the node |
| `RequestNodes` | Returns `NodeList(Vec<NodeRecordLite>)` |
| `RequestNodeInfo(id)` | Returns `NodeInfo(NodeRecordFull)` including model allocations |
| `ModelLoad` | Looks up target agent tx in connections map; forwards command |
| `ModelStatus` | Calls `registry.update_model_status()`; updates allocation state |
| `ModelUnload` | Logged; forwarding to agent not yet implemented |
| `RequestModelInference` | Calls `select_node_for_inference`; logs selected node or returns `Error` if none ready |
| `Admin(ResetRegistry)` | Clears all nodes from the registry |

## Registry Methods

### list_nodes()
Returns lightweight summaries (`NodeRecordLite`) for all nodes.

### get_node_full(id)
Returns full diagnostic information including hardware, capabilities, and model allocations.

### update_model_status(id, model_name, size_mb, state)
Inserts or updates a `ModelAllocation` entry for the given node. Used to track which models are loaded and in what lifecycle state.

### eligible_compute_nodes()
Returns all `NodeRecordFull` records for nodes with role `Compute`.

## Concurrency Model
- Registry is protected by a `Mutex`
- Connections map is protected by a separate `Mutex`
- Locks are never held across `.await`
- Each TCP connection runs its own reader loop and a dedicated writer task draining an `mpsc` channel
