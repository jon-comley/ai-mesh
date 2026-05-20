# Development Workflow

## Sanity Testing

The project includes a `just sanity` command which performs a minimal live
cluster validation using only the local machine as both coordinator and
controller agent.

This test verifies:
- Coordinator startup
- Agent startup
- CLI node listing
- Log output from both processes
- Clean shutdown

### Cluster Sanity Tests

| Command | What it validates |
|---------|------------------|
| `just sanity` | Local only — coordinator + controller; no remote nodes |
| `just sanity-node <node>` | Service state on a specific node + node table |
| `just sanity-full` | Full cluster — all nodes in `nodes/`; stops, resets, starts fresh |
| `just test-inference` | End-to-end inference pipeline across all compute nodes |

`just sanity-full` is the canonical full-cluster validation. It uses
`systemctl stop/start` for Linux nodes and `sc.exe stop/start` for Windows
nodes — never bare process killing — so the service supervisor stays consistent.

## Cross-Platform Agent Builds

The agent supports Windows, Linux x86_64, and Linux ARM64. Cross-compilation
is done from WSL. The `deploy-node` recipe handles this automatically based on
`NODE_OS` in the node's `.env` file, but the toolchains must be installed once:

```bash
# Windows nodes (x86_64)
sudo apt install gcc-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu

# Linux ARM64 nodes (e.g. Raspberry Pi)
rustup target add aarch64-unknown-linux-gnu
```

Build targets:
- `target/x86_64-pc-windows-gnu/release/agent.exe`
- `target/aarch64-unknown-linux-gnu/release/agent`

See `docs/windows-node-setup.md` for the full Windows provisioning guide.
