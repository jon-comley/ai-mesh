# Coordinator Documentation

## Responsibilities
- Accept TCP connections from agents and CLI clients
- Maintain an in-memory registry of nodes
- Track heartbeats and last-seen timestamps
- Store hardware and capability reports
- Respond to CLI queries

## Message Handling

### Heartbeat
Updates last-seen timestamp for a node.

### HardwareReport
Stores hardware specification for the node.

### Capabilities
Stores capability information for the node.

### RequestNodes
Returns:
```
MeshMessage::NodeList(Vec<NodeRecordLite>)
```

### RequestNodeInfo(id)
Returns:
```
MeshMessage::NodeInfo(NodeRecordFull)
```

## Registry Methods

### list_nodes()
Returns lightweight summaries for all nodes.

### get_node_full(id)
Returns full diagnostic information for a specific node.

## Concurrency Model
- Registry is protected by a Mutex
- Locks are never held across `.await`
- All message processing happens inside a single async loop per connection
