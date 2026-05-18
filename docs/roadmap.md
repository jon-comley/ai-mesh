# ai-mesh Roadmap

This document outlines the phases of development.

---

# Phase 6 — Model Scheduling ✓ Complete

## Delivered

- **Model registry** — `ModelAllocation` + `update_model_status` in coordinator registry
- **Wire protocol messages** — `ModelLoad`, `ModelUnload`, `ModelStatus`, `RequestModelInference`, `ModelInferenceResult` with `wire_version` compatibility
- **Allocation-aware scheduler** — `Scheduler::select_node_for_model(mb)`: filters by role, remaining capacity; `Ready` + `Loading` states count against capacity, `Unloaded` / `Failed` do not
- **Connection routing map** — per-connection `mpsc::Sender` registered on `Heartbeat`, purged on disconnect; enables coordinator → agent command forwarding
- **ModelLoad forwarding** — coordinator looks up target agent tx and forwards; warns if not connected
- **Agent command handling** — bidirectional TCP: reader task handles `ModelLoad` from coordinator; replies `ModelStatus(Loading)` immediately, `ModelStatus(Ready)` after 2s background task (simulated)
- **CLI `mesh load`** — `mesh load <node-id> <model-name> <size-mb>` sends `ModelLoad` to coordinator
- **Models column** — `mesh nodes` fetches `NodeRecordFull` per node and renders live model lifecycle state
- **54 tests** across all four crates; integration test validates end-to-end `ModelLoad` forwarding

## Remaining / not yet wired

- `ModelUnload` forwarding (server logs it; not yet forwarded to agent)
- `RequestModelInference` dispatch (server logs "pending"; scheduler not yet invoked)
- `mesh infer` CLI command
- Real model integration (ollama / llama.cpp)

---

# Phase 7 — Inference Routing (Next)

## Goals
- Route `RequestModelInference` through the scheduler to the selected agent
- Forward `InferenceRequest` down the agent connection; agent returns `InferenceResult`
- Add `mesh infer <model-name> <prompt>` CLI command
- Wire up `ModelUnload` forwarding and agent-side `Unloaded` reporting

---

# Phase 8 — Security & Auth
- TLS
- Node authentication
- Signed messages

---

# Phase 8 — Web Dashboard
- Live mesh view
- Node health
- Model deployment UI

---

# Phase 9 — Distributed Execution
- Multi-node inference
- Pipeline parallelism
- Tensor parallelism

---

# Phase 10 — Auto-scaling
- Dynamic node joining
- Cloud integration
