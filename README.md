# ai-mesh

ai-mesh is a lightweight distributed system for connecting multiple machines
into a hardware-aware inference mesh. Each node runs an agent that reports:

- Hardware specs  
- Inference capabilities  
- Heartbeats  

The coordinator maintains a live registry of all nodes, and the CLI provides
introspection tools.

This README provides a **quick-start reference** for day-to-day development.
For full documentation, see the `docs/` directory.

---

## Quick Start

### 1. One-time laptop setup

```
just setup-controller
```

Installs Rust, cross-compilation toolchains, git hooks, SSH keys, and cross-compilation targets (aarch64-unknown-linux-gnu for pi1, x86_64-pc-windows-gnu for Windows nodes).

### 2. Deploy coordinator to pi1 (one-time)

```
just deploy-coordinator pi1
```

Cross-builds coordinator for ARM64, copies state/DB from laptop, installs as systemd service on pi1 (10.0.0.10:9001). The coordinator now runs 24/7 independent of laptop power. Verify with `just verify-coordinator pi1`.

For remote phone access, install Tailscale on pi1 + phone; dashboard is then accessible at `http://100.100.100.100:9001/?token=...` or `http://pi1:9001/?token=...` (once the built-in DNS propagates).

### 3. Provision compute nodes

```
just deploy-node pi1      # (agent only — coordinator already running)
just deploy-node beelink1
```

Builds the correct binary for the node's OS, uploads it, installs llama-server, and
registers a persistent service (systemd on Linux, NSSM on Windows). Also configures
passwordless sudo on Linux nodes so fingerprint pushes work non-interactively. If the
coordinator is already running, credentials (TLS fingerprint + auth token) are pushed to
the node automatically at the end of provisioning — no separate `set-fingerprint` step needed.

### 4. Start the cluster

```
just start-cluster
```

Starts your laptop's local agent (**controller role**, pointed at the coordinator on pi1), pushes TLS fingerprint + auth token to all remote nodes, and loads the best-fit model on each compute node. Model loads are **retried automatically until every compute node reports `Ready`** — a load issued while an agent is still reconnecting after the credential-push restart can otherwise be dropped on the torn connection. The coordinator on pi1 was already started by `deploy-coordinator` and keeps running whether your laptop is on or off.

Credentials are written to `~/.bashrc` automatically.

### 5. Check mesh state

```
just nodes          # current node table
just validate-routing   # confirm inference routes to correct nodes
```

### 6. Day-to-day

```
just restart-coordinator        # after waking laptop or any coordinator restart
just intent "turn the lights off"
just rotate-token               # zero-downtime auth token rotation (no inference drop)
```

---

## OpenAI-Compatible API

The coordinator exposes the mesh as a standard OpenAI endpoint — point any
OpenAI-SDK client at `http://pi1:9001/v1` with the mesh auth token as its API
key and requests are served by local nodes (or the cloud gateway):

```
just openai "why is the sky blue?"          # curl convenience wrapper
curl http://pi1:9001/v1/models -H "Authorization: Bearer $MESH_AUTH_TOKEN"
```

Full reference (routing rules, parameters, errors, SDK examples): `docs/openai-api.md`.

---

## Dashboard

The coordinator on **pi1** serves a web dashboard on **port 9001** (override with `MESH_HTTP_PORT`):

**Local access (home LAN):**
```
http://10.0.0.10:9001/
```

**Remote access (Tailscale, anywhere):**
```
http://100.100.100.100:9001/
http://pi1:9001/              (once built-in DNS propagates)
```

The page itself loads with no auth; the first WebSocket connection prompts a native browser dialog for `MESH_AUTH_TOKEN` (retrieve it with `echo $MESH_AUTH_TOKEN` on OmniLink1, or from `MESH_AUTH_TOKEN=` in pi1's `/var/lib/ai-mesh/coordinator.state`) and caches it in the browser's local storage — a `?token=` query param is not read by the client. Tap the connection dot (top of the page) to re-enter it if it's ever rejected.

> **If `pi1:9001` won't load:** the `pi1` and `100.x` addresses only resolve/route when that device is connected to the tailnet. Check that **Tailscale is on** (and the device shows online in `tailscale status` on pi1). On the home LAN you can always fall back to the direct `http://10.0.0.10:9001/`.

A Progressive Web App — open in **Safari** on iOS/iPadOS (Chrome's iOS "Add to Home Screen" doesn't install a standalone PWA) and use Share → "Add to Home Screen" to install it as an app icon. Bookmark the remote URL for one-tap access from anywhere (cellular, public WiFi, etc.). Tabs: Nodes, Health, Models, Home, Devices, Security, Errors, Chat, REAPER, Online AI. Real-time data via WebSocket; the full Home (rooms, effects, scenes, device control) and Devices (pairing, inventory) tabs are live.

---

## Security

All coordinator ↔ agent and coordinator ↔ CLI traffic runs over **TLS** with a **shared auth token**.

### TLS (transport layer)
The coordinator generates a self-signed certificate on first start, persisted at `/var/lib/ai-mesh/coordinator.crt`. Agents and the CLI verify the cert via its SHA-256 fingerprint (TOFU model — trust on first contact, reject any change).

### Auth token (application layer)
Each agent includes `MESH_AUTH_TOKEN` in its startup `AuthToken` frame (connection-level) and in every `Heartbeat` (per-message defence-in-depth). The coordinator rejects any connection or heartbeat with a missing or wrong token when auth is configured. Dual-token rotation (`MESH_AUTH_TOKEN` + `MESH_AUTH_TOKEN_NEXT`) allows zero-downtime key rotation — use `just rotate-token` to rotate automatically.

### HMAC message integrity (Phase 10.5)
Every wire message after the initial `AuthToken` handshake is wrapped in a `SignedFrame` (HMAC-SHA256). The signing key is derived from `MESH_AUTH_TOKEN` via HKDF-SHA256 — no new credentials needed. The coordinator rejects frames with a wrong signature, stale timestamp (>30s skew), or no `SignedFrame` wrapper at all after auth. This closes the residual window where a rogue process that obtained a valid token could inject arbitrary messages. Use `just chaos` to adversarially verify the full security stack.

### mDNS discovery is unauthenticated
**Before running the mesh on any shared or guest LAN:** the coordinator advertises `_ai-mesh._tcp.local.` on the local network with no auth check on who can discover and connect to it. Any device on the same LAN can find the coordinator and attempt to join — the `MESH_AUTH_TOKEN`/TLS/HMAC protections above still gate what a connecting node can *do*, but not whether it can find and reach the coordinator in the first place. Safe on a trusted home LAN; do not expose the mesh on a shared or untrusted network until a join token closes this gap (planned, Phase 10, `MESH_TOKEN` env var).

### Coordinator state file
On startup the coordinator writes `/var/lib/ai-mesh/coordinator.state` (0600, shell-sourceable KEY=VALUE) containing the current fingerprint and auth token. All justfile recipes source this file — no log-grepping, no race conditions.

**`just start-cluster`, `just restart-coordinator`, and `just deploy-node` handle everything automatically:**
- Source `/var/lib/ai-mesh/coordinator.state` for fingerprint + auth token
- Write `export MESH_TLS_FINGERPRINT=...` and `export MESH_AUTH_TOKEN=...` to `~/.bashrc` on the controller machine
- Call `just set-fingerprint <node>` for every compute node, which pushes **both** `MESH_TLS_FINGERPRINT` and `MESH_AUTH_TOKEN` in one SSH operation
- `deploy-node` additionally pushes credentials immediately after provisioning if the coordinator is already running

You never need to copy or paste credentials manually.

**To skip TLS verification** (dev/test only):
```bash
MESH_INSECURE=1 cargo run -p cli -- nodes
```

---

## Cluster Nodes

| Node | OS | Hardware | Model | Capabilities |
|------|----|----------|-------|--------------|
| pi1 | Linux (ARM64) | Raspberry Pi 5, 8 GB RAM | `qwen2.5:1.5b` | llm, lighting |
| pi2 | Linux (ARM64) | Raspberry Pi | — | art, audio, music |
| Beelink SER8 (beelink1) | Windows 11 | AMD Radeon 780M, 8 GB VRAM | `qwen2.5:7b` | llm |
| OmniLink1 (WSL2) | Linux (x86_64) | controller only | — | controller |

The coordinator schedules inference requests to whichever node has the requested model loaded and ready.

**Lighting**: pi1 runs Mosquitto + Zigbee2MQTT with an SLZB-06 Zigbee coordinator. Natural language intents like `just intent "turn all lights off"` are routed through the LLM and executed as MQTT commands to Zigbee devices. The coordinator receives the live device/group list from pi1 on connect (persisted across restarts), injects it into the LLM system prompt, and validates targets before dispatch — unknown device names return a clear error rather than a silent no-op. See `docs/pi1-lighting-setup.md`.

**Music**: "play Blackbird by the Beatles", "pause", "what's playing?" — from chat or the voice puck. pi2 drives the Spotify Web API for search and control, and supervises a librespot Spotify Connect player whose audio feeds the paired Bluetooth speaker. Requires Spotify Premium and a one-time setup — see `docs/music.md`.

---

## Justfile Reference

| Command | Description |
|---------|-------------|
| `just build` | Build all crates |
| `just test` | Run all tests |
| `just lint` | fmt + clippy |
| `just run-coordinator` | Start coordinator on port 9000 |
| `just run-controller` | Start controller agent |
| `just reset` | Clear all nodes from the live coordinator |
| `just nodes` | Show the current node table |
| `just dev` | Start full cluster in dev mode |
| `just deploy-node <node>` | First-time provision or re-provision a node |
| `just update-node <node>` | OTA binary update only (no reprovisioning) |
| `just load-model <node> <model>` | Load a specific model on a node (e.g. `qwen2.5:7b`); coordinator also accepts `mesh load <model> <size_mb>` with no node — picks best-fit automatically |
| `just auto-load-model <node>` | Detect node hardware and load the best-fit model automatically |
| `just load-models-retry` | Load each compute node's best-fit model, retrying any node that doesn't reach `Ready` (shared by `start-cluster` / `restart-coordinator`) |
| `just start-cluster` | Bring the full cluster up and load the best model on each compute node (retries each load until `Ready`) |
| `just restart-coordinator` | Post-suspend recovery — restart coordinator + controller, reload models with retry (use after opening laptop) |
| `just stop-cluster` | Stop all remote agents and the local coordinator/controller |
| `just uninstall-node <node>` | Remove agent service from a node |
| `just sanity-node <node>` | Check service state + node table |
| `just sanity-full` | Full cluster validation (all nodes) |
| `just chaos` | Fire 6 adversarial HMAC security scenarios at the live coordinator (run automatically by `just validate-routing`) |
| `just validate-routing` | Confirm each model routes to its correct node (run after `start-cluster` or `restart-coordinator`) |
| `just logs-node <node>` | Tail live agent logs from a node |
| `just logs` | Tail all logs simultaneously |
| `just load <model>` | Auto-place a model via coordinator (no SSH needed, e.g. `just load qwen2.5:7b`) |
| `just intent "<text>"` | Send a natural-language intent to the coordinator (LLM routes to tool or answers in free text) |
| `just pair-bulb` | Open a 254-second Zigbee pairing window and stream join events |
| `just rotate-token` | **Zero-downtime auth token rotation** — dual-token window, distributes new token to all nodes, revokes old token, reloads models |
| `just set-fingerprint <node>` | Push TLS fingerprint **and** auth token from coordinator state file to a single node |
| `just set-auth-token <token>` | Low-level: push a specific token to all compute nodes and update `~/.bashrc` (prefer `just rotate-token`) |

See `docs/commands.md` for full reference.

---

## Adding a New Node

1. Create `nodes/<name>.env`:
   ```bash
   NODE_HOST=192.168.1.x
   NODE_USER=youruser
   NODE_OS=linux    # or windows
   NODE_ROLE=compute
   ```
2. `just deploy-node <name>`
3. `just sanity-node <name>`

No other files need changing.

---

## Documentation

See the `docs/` directory for detailed architecture, message protocol,
testing strategy, and crate-level documentation.
