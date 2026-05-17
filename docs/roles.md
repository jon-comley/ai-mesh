# Node Roles

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
