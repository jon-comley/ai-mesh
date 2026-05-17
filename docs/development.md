# Development Workflow

## Development Workflow

### Sanity Testing

The project includes a `just sanity` command which performs a minimal live
cluster validation using only the local machine (OmniBook) as both:
- Coordinator
- Controller agent

This test verifies:
- Coordinator startup
- Agent startup
- CLI node listing
- Log output from both processes
- Clean shutdown

Phase 6 messages are **not** injected during this test because the ai-mesh
wire protocol uses a 4-byte little-endian length prefix. Raw TCP tools such
as `nc` cannot be used without a framing helper. A framed test client will be
added during Phase 6.

### Future Sanity Tests

As additional compute nodes come online (Pi 5, Beelink SER8, Mac mini M4),
device-specific sanity tests will be added:
- `just sanity-pi`
- `just sanity-beelink`
- `just sanity-macmini`
- `just sanity-full` (entire cluster)

These will validate:
- HardwareReport correctness
- Capabilities reporting
- Model lifecycle reporting
- Scheduler placement decisions (Phase 6+)

The universal `just sanity` test will remain the baseline for verifying that
the core coordinator/agent/CLI stack is healthy regardless of cluster size.
