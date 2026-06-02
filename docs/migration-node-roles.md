# Migration Guide: Node Roles and Cluster Setup

## OmniBook Laptop (Controller Only)

- Purpose: monitoring, CLI, orchestration
- Must never be used for inference

```
AGENT_ROLE=controller ./agent
```

Run `mesh nodes` — you should see Role = Controller, no hardware/capability data used for scheduling.

## Raspberry Pi 5 (Compute)

```
AGENT_ROLE=compute ./agent
```

Suitable for: tiny models, edge experiments.

## Beelink SER8 (Compute)

```
AGENT_ROLE=compute ./agent
```

Suitable for: medium CPU-bound models.

## Mac mini M4 (Compute — planned, not yet configured)

> Future node (targeted ~end of July 2026). No `nodes/mac1.env` exists yet; the config below is the intended setup once it joins.

```
AGENT_ROLE=compute ./agent
```

Preferred for: large models, GPU/ANE-accelerated workloads.

## Verifying Scheduling Behaviour

1. Start agents on all machines with the configs above.
2. Run `mesh nodes` and confirm OmniBook shows `Controller`, others show `Compute`.
3. Issue a test inference request (`mesh infer ...`).
4. Confirm no inference is ever scheduled on the OmniBook.
