# ai-mesh Roadmap

---

## Phase 6 — Model Scheduling ✓ Complete

- Model registry (`ModelAllocation` + `update_model_status`)
- Wire protocol messages (`ModelLoad`, `ModelUnload`, `ModelStatus`, `RequestModelInference`, `ModelInferenceResult`) with `wire_version` compatibility
- Allocation-aware scheduler: `select_node_for_model(mb)` (capacity) + `select_node_for_inference(name)` (Ready model)
- Connection routing map — per-connection `mpsc::Sender` registered on `Heartbeat`, purged on disconnect
- ModelLoad forwarding and agent-side `ModelStatus` replies
- CLI `mesh load`, `mesh nodes` Models column
- 54 tests across all four crates

---

## Phase 7 — Inference Routing ✓ Complete

- `RequestModelInference` routed through scheduler to selected agent
- Agent calls llama-server `POST /v1/chat/completions` (`stream: false`); returns real output, token count, duration
- `mesh infer <model-name> <prompt>` CLI command
- `ModelUnload` forwarding; agent reports `Unloaded`
- Oneshot channel per inference request; coordinator waits up to 300s for result
- `model_is_loading` registry query; coordinator polls for up to 300s before dispatching inference

---

## Phase 8 — Production Hardening ✓ Complete

- **Inference timeout tuning** — split into 300s pull-wait (Phase 1) + 120s generate (Phase 2); distinct error strings per phase
- **SQLite persistent registry** — `rusqlite` (bundled); `Registry::open(path)` for prod, `Registry::new()` (in-memory) for tests; state survives coordinator restarts
- **Pi (ARM64) compute node** — cross-compiled with `rustls-tls`; `just deploy-node pi1` fully self-provisioning
- **Beelink SER8 (Windows 11) compute node** — cross-compiled (`x86_64-pc-windows-gnu`); NSSM service; `sysinfo` crate for hardware detection (no child-process spawning); `just update-node beelink1` for OTA updates
- **Generic node provisioning** — `nodes/<name>.env` inventory; `deploy-node`, `update-node`, `uninstall-node` work for any Linux or Windows node without justfile changes
- **Agent reconnect loop** — graceful channel handling; retries TCP connection every 5s on disconnect
- **Cross-platform agent** — conditional compilation for hardware detection (Windows: `sysinfo`; Linux/macOS: `/proc`); hostname detection (Windows: `COMPUTERNAME`; Linux: `/etc/hostname`)

---

## Phase 8.5 — llama-server Migration ✓ Complete

- Replaced Ollama with llama-server (llama.cpp) across all nodes
- Agent downloads GGUF shards from Hugging Face on `ModelLoad`; no pre-caching during provisioning
- Inference switched to `POST /v1/chat/completions` with system + user message format
- `--flash-attn on` enabled; `LLAMA_GPU_LAYERS=99` offloads all layers to GPU where available
- Windows: Vulkan-enabled llama.cpp ZIP; AMD Radeon 780M at 29/29 GPU layers, 17.6 t/s (qwen2.5:7b)
- Linux: architecture-aware tarball download (x86_64 or ARM64)
- `just load-model <node> <model>` replaces `change-model`; `just update-llama <node>` for llama-server updates

---

## Phase 9 — Remaining Cluster Nodes (In Progress)

- **Mac mini M4** ⚠️ _hardware not available until ~end of July 2026_ — cross-compile for `aarch64-apple-darwin`, provision as compute node, add `just deploy-node mac1`
- **Multi-node load balancing validation** — run `test-inference` with 2+ compute nodes, verify scheduler distributes load correctly across Pi and Beelink
- **`just start-cluster` recipe** — start coordinator + controller + all remote agents + load each node's preferred model, leaving the mesh in a ready-to-use inference state. Discuss model assignment strategy (per-node preferred model in `nodes/<name>.env`? explicit args? automatic by RAM?) before implementing
- **Automatic model placement** — wire `select_node_for_model(mb)` into the coordinator's `ModelLoad` handler so the coordinator picks the best-fit node automatically instead of requiring the CLI caller to name one. The scheduler building block already exists; what's missing is the handler logic that calls it when `node_id` is absent from the `PullModel` request.

---

## Phase 10 — Security & Auth

- TLS on coordinator TCP listener
- Node authentication (shared secret or certificates)
- Signed wire messages

---

## Phase 11 — Web Dashboard

- Live mesh view
- Node health monitoring
- Model deployment UI

---

## Phase 12 — Distributed Execution

- Multi-node inference
- Pipeline parallelism
- Tensor parallelism

---

## Phase 13 — Auto-scaling

- Dynamic node joining
- Cloud integration
