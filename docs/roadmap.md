# ai-mesh Roadmap

This document outlines the next phases of development.

---

# Phase 6 — Model Scheduling (Next)

## Goals
- Allow agents to advertise supported model sizes and capabilities
- Allow coordinator to select the best node for a given inference request
- Add CLI commands for model deployment and inference

---

## Planned Features

### 1. Model Registry
- List of available models
- Metadata: size, quantization, architecture
- Stored in coordinator

### 2. Model Placement Rules
- Nodes declare max model size
- Nodes declare inference backends (CPU/GPU/ANE)
- Coordinator selects best node based on:
  - model size
  - hardware acceleration
  - current load (future)
  - node availability

### 3. New Messages
- `RequestModelInference`
- `ModelInferenceResult`
- `ModelLoad`
- `ModelUnload`
- `ModelStatus`

### 4. CLI Commands
- `mesh models`
- `mesh deploy <model>`
- `mesh infer <model> <input>`
- `mesh node-models <id>`

### 5. Future Extensions
- Distributed inference
- Multi-node sharding
- Model caching
- Load balancing
- GPU/ANE-aware scheduling

---

# Phase 7 — Security & Auth
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
