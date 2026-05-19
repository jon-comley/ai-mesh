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

### Per-Node Sanity Tests

Each compute node has a dedicated sanity recipe:

| Command | What it validates |
|---------|------------------|
| `just sanity-all` | Local only — coordinator + controller; no remote nodes |
| `just sanity-pi` | Check node table while coordinator is already running |
| `just sanity-beelink` | Coordinator + controller locally; restarts NSSM service on Beelink; checks node table |
| `just sanity-full` | Full cluster including Pi |
| `just test-inference` | End-to-end inference pipeline (requires Pi with Ollama) |

`just sanity-beelink` is the canonical test for Windows compute node bring-up.
It uses a force-kill stop pattern (stop → kill agent/nssm → start) to avoid
the NSSM STOP_PENDING deadlock that occurs with plain `sc.exe stop`.

### Cross-Platform Agent Builds

The agent supports Windows, Linux x86_64, and Linux ARM64. Cross-compilation
is done from WSL:

```bash
# Windows (Beelink SER8)
sudo apt install gcc-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu
just build-beelink-exe   # → target/x86_64-pc-windows-gnu/release/agent.exe

# Linux ARM64 (Raspberry Pi)
rustup target add aarch64-unknown-linux-gnu
just build-pi            # → target/aarch64-unknown-linux-gnu/release/agent
```

See `docs/windows-node-setup.md` for the full Windows provisioning guide.
