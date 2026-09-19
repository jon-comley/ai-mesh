# Node Roles

**There is no `coordinator` role, and the name is the confusing part.** The
coordinator is a separate binary — `ai-mesh-coordinator`, its own service — and
the machine running it is "the coordinator". A *role* is what an agent tells the
mesh about the node it is on, and there are only two of them: `Controller` and
`Compute`. The box that coordinates therefore runs the coordinator service *and*
an agent whose role is `controller`.

Asked on 2026-09-19 while taking pi1 out of inference, which is exactly when
somebody goes looking for `AGENT_ROLE=coordinator` and does not find it.

## Controller
- Runs CLI
- Manages mesh
- Never used for inference
- Sends only heartbeats

## Compute
- Full hardware + capability reporting
- Eligible for model scheduling
- Runs inference workloads

## Configuration

Set via `AGENT_ROLE` environment variable:

```
AGENT_ROLE=controller ./agent
```
or
```
AGENT_ROLE=compute ./agent   # default
```

**The role is not cosmetic and the default is not neutral.** `agent.rs` only
detects hardware and sends a `HardwareReport` when the role is `Compute`, so
this is what decides whether the coordinator can schedule model work on a node.
And `read_role_from_env` matches the literal string `"controller"` and falls
through to `Compute` for everything else — a typo, a capital C or an unset
variable all make a node eligible for inference.
