# shared Crate Documentation

The `shared` crate defines all common data structures, message formats, and versioning logic used across the entire ai-mesh system. It is the foundational layer that ensures consistent communication and behaviour between the agent, coordinator, and CLI.

---

## 1. Purpose

The `shared` crate provides:

- A unified message protocol (`MeshMessage`)
- Hardware specification structures (`HardwareSpec`)
- Node identity and capability definitions
- Versioning and update metadata
- Serialization and deserialization via Serde
- Types that must remain stable across updates

This crate contains **no business logic** — only pure data structures and shared definitions.

---

## 2. Modules

### `messages.rs`
Defines the message protocol used for communication between nodes:

- Heartbeats
- Capability reports
- Hardware reports
- Update notifications
- Acknowledgements

These messages are serialized using JSON for simplicity and cross-language compatibility.

### `hardware.rs`
Defines:

- HardwareSpec (CPU, RAM, GPU, OS, architecture)
- NodeIdentity (ID, hostname, IP, role)
- NodeCapabilities (CPU/GPU/ANE inference support)
- VersionInfo (agent + model versions)
- UpdateManifest (download URL + checksum)
- UpdateChannel (Stable, Beta, Canary)

These structures form the core of the mesh's introspection and update system.

---

## 3. Serialization Format

All messages and structures use:

- `serde` for serialization
- `serde_json` for encoding/decoding

JSON is chosen because:

- It is human-readable
- It is AI-friendly
- It is cross-language compatible
- It is stable across versions

Binary formats (e.g., bincode) may be added later for performance.

---

## 4. Stability Guarantees

The `shared` crate must maintain:

- Backwards compatibility where possible
- Stable message formats
- Clear versioning rules
- Minimal breaking changes

Any breaking change must be documented in:

- `docs/decisions/`
- `docs/roadmap.md`

---

## 5. Testing Strategy

The shared crate includes:

- Unit tests for each struct
- Serialization/deserialization tests
- Round-trip tests (encode → decode → compare)
- Versioning tests

These tests ensure:

- Messages remain stable
- Structures serialize correctly
- No accidental breaking changes occur

---

## 6. Interaction With Other Crates

### Agent
- Sends heartbeats
- Sends hardware reports
- Sends capability reports
- Receives update manifests

### Coordinator
- Receives heartbeats
- Maintains node registry
- Sends update notifications
- Validates version compatibility

### CLI
- Queries coordinator for node info
- Displays hardware and capability data
- Shows version and update status

---

## 7. Future Extensions

Potential future additions:

- Cryptographic signatures for messages
- Binary serialization for performance
- Compression for large payloads
- Extended hardware metrics (thermal, power, load)
- Model capability negotiation

These will be added incrementally as needed.

---

This document will evolve as the shared crate grows.
