# Move Coordinator to pi1 — Migration Plan

> Status: **Draft.** Reviewed by Bing and Gemini; refinements from both reviews integrated. Implementation pending user approval.
>
> Goal: Relocate the coordinator role from the WSL2 laptop (`OmniLink1`) to the always-on Raspberry Pi 5 already known as `pi1` (<pi1-old>) so the laptop can be closed without taking the smart-home dashboard offline. Add Tailscale for outside-LAN phone access.

---

## Background

Today's topology has `OmniLink1` (WSL2 laptop) tagged as Controller and `pi1` + `BEELINK1` as Compute. Closing the laptop means the dashboard and all light control die. Phone access from the LAN requires a Windows portproxy that drifts on every WSL restart, and remote access is impossible.

`pi1` already runs the Zigbee stack (zigbee2mqtt + Mosquitto) and is the natural host for the coordinator: co-locating coordinator with the Zigbee agent eliminates a network hop from every light command, frees Beelink for heavy LLM compute, and gives us a fanless, low-power, always-on box for the most stateful service.

## Scope

| In scope | Out of scope |
|----------|--------------|
| Coordinator binary running as a systemd service on pi1 | Replacing zigbee2mqtt / Mosquitto |
| Cross-build pipeline from laptop → pi1 (aarch64) | Migrating compute models off Beelink |
| Migration of `~/.config/ai-mesh/` (cert, key, state) + `ai_mesh.db` | Multi-coordinator HA / failover |
| Tailscale install on pi1 + phone for outside-LAN dashboard access | Public DNS / Let's Encrypt / port forwarding |
| Repoint Beelink + laptop agents at the new coordinator host | Per-agent custom certs (single self-signed remains) |
| `just deploy-coordinator` and `just verify-coordinator` recipes | Replacing the existing `restart-coordinator` recipe |
| Retire WSL2 portproxy from the dashboard path | Removing WSL2 entirely (laptop can remain a compute node) |

## Invariants

These hold today by construction; the move must preserve them.

1. **Zigbee co-location.** The coordinator and the Zigbee agent share a host. If they're ever split, the Zigbee agent must use the coordinator's stable LAN IP — never `localhost`.
2. **State directory atomicity.** `~/.config/ai-mesh/` holds the TLS cert/key plus the token state file. All three must move together; mismatches cause TLS handshake failures.
3. **Working-directory DB path.** `ai_mesh.db` is opened via a relative path (`coordinator/src/main.rs:11`), so it sits in the coordinator's *working directory*, not in `~/.config/ai-mesh/`. The systemd unit must set an explicit `WorkingDirectory` to keep the DB stable.
4. **WebSocket reconnect is automatic.** Agents already retry with exponential backoff. Repointing them at the new host requires nothing more than updating `MESH_AUTH_TOKEN` + the coordinator address; reconnection happens unprompted.

## Host & access decisions

### Coordinator host: pi1

The coordinator's workload (HTTP/WS server, SQLite registry, mDNS, broadcast fan-out) is trivial — CPU and RAM are non-issues on any host. The choice is driven by:

- **Zigbee locality** — co-located with the Zigbee agent → localhost MQTT roundtrip for every light command.
- **Compute separation** — Beelink stays dedicated to qwen2.5:7b inference.
- **Reliability** — pi1 is fanless, low-power, no Windows updates, already on UPS-style power.

pi1 continues to run a compute agent alongside the coordinator. Coordinator workload is light enough that this doesn't dilute compute throughput.

### Remote access: Tailscale

Tailscale beats the alternatives for this use case:

- **Port forwarding + DDNS** — exposes the dashboard to the public internet behind only a token. Rejected.
- **Cloudflare Tunnel** — works, but adds a domain and external dependency for purely-private traffic.
- **Self-hosted WireGuard** — more setup; revisit if Tailscale ever becomes annoying.

Tailscale install on pi1 + phone → phone always reaches pi1 at a stable `100.x.x.x` address, on home Wi-Fi or cellular. MagicDNS makes the bookmark `http://pi1:9001/?token=…` regardless of network. No router config, no Let's Encrypt.

## Migration manifest

These files move from `OmniLink1` to `pi1`:

| Source on OmniLink1 (WSL2) | Destination on pi1 |
|----------------------------|--------------------|
| `~/.config/ai-mesh/coordinator.crt` | `~/.config/ai-mesh/coordinator.crt` |
| `~/.config/ai-mesh/coordinator.key` | `~/.config/ai-mesh/coordinator.key` |
| `~/.config/ai-mesh/coordinator.state` | `~/.config/ai-mesh/coordinator.state` |
| `<repo>/ai_mesh.db` | `/var/lib/ai-mesh/ai_mesh.db` |
| (none) | `/etc/systemd/system/ai-mesh-coordinator.service` (new) |

Copy order: state files first → cert/key first → DB → install unit → enable & start. Existing token survives the move (Phase E auto-load reads `coordinator.state` at startup), so the phone bookmark keeps working.

## Phases

### Phase 1 — Build & install coordinator on pi1

- Cross-compile coordinator from laptop with `cargo zigbuild --target aarch64-unknown-linux-gnu --release -p coordinator`, or native build on pi1.
- Install binary at `/usr/local/bin/ai-mesh-coordinator`.
- Create `/var/lib/ai-mesh/`, `chown pi:pi /var/lib/ai-mesh`, `chmod 700 /var/lib/ai-mesh` (DB contains registry data — service-user only).
- Copy `ai_mesh.db` into `/var/lib/ai-mesh/`.
- Copy `~/.config/ai-mesh/` (cert + key + state) to the service user's home on pi1.
- Drop `/etc/systemd/system/ai-mesh-coordinator.service`:
  - `User=` set to the pi user (or a dedicated `ai-mesh` user).
  - `WorkingDirectory=/var/lib/ai-mesh`.
  - `Environment=MDNS_ADVERTISE_IP=<pi1-old>` (pi1's LAN IP).
  - `Environment=MESH_HTTP_PORT=9001` (optional — default works).
  - `Environment=RUST_LOG=info` (default tracing level is WARN; without this you lose visibility into normal operation).
  - `Restart=on-failure`, `RestartSec=3s`.
  - `After=network-online.target`, `Wants=network-online.target` (prevents mDNS / Tailscale races).
  - `ProtectSystem=full` (mounts `/usr`, `/etc`, `/boot` read-only — cheap hardening).
  - *Not* `ProtectHome=true` — would block reads from `/home/pi/.config/ai-mesh/` where the cert + state live. See "Future hardening" below.
- `systemctl daemon-reload && systemctl enable --now ai-mesh-coordinator`.

### Phase 2 — Repoint agents and validate

- BEELINK1 (Windows) + OmniLink1 (WSL2): update `MESH_AUTH_TOKEN` and coordinator address. The existing `set-auth-token` / `start-agents` recipes already handle the cross-OS plumbing — they only need the new controller host.
- Repoint any justfile vars that hardcode the laptop as controller.
- **Health check** (must all pass before declaring cutover done):
  1. `pi1` coordinator log shows all expected agents connected.
  2. Each agent reports the correct TLS fingerprint (no handshake retries in agent logs).
  3. Zigbee agent on pi1 publishes to Mosquitto on `localhost`.
  4. Dashboard at `http://<pi1-old>:9001/?token=…` loads and shows live device state.
  5. A test light command from the dashboard reaches the bulb.

### Phase 3 — Tailscale

- On pi1: `curl -fsSL https://tailscale.com/install.sh | sh && sudo tailscale up --qr --ssh`.
  - `--qr` prints a scannable QR code in the terminal — easier than copy-pasting the auth URL off a headless Pi.
  - `--ssh` enables Tailscale SSH so the Pi stays reachable for admin even if the home LAN is sick (router rebooting, ISP flake) — phone or laptop with Tailscale can SSH in over the tunnel.
- Enable MagicDNS in the Tailscale admin panel.
- Install Tailscale app on phone, sign in to the same account.
- Verify phone bookmark `http://pi1:9001/?token=…` works on home Wi-Fi AND on cellular (Wi-Fi off).
- Optional later: ACL tags so pi1 has a stable identity if a second Pi joins the tailnet.

### Phase 4 — Retire laptop-controller path

- Remove WSL2 portproxy entries (no longer needed: dashboard reaches pi1 directly, no traffic flows via the laptop).
- Update `dashboard-mobile` justfile recipe — either delete it or repurpose it to print the new pi1 URL.
- Update memory file `project_coordinator_topology.md` to reflect pi1-as-controller.
- Keep OmniLink1 as an optional compute node; remove it entirely if not used.

## Mixed-OS considerations

- Coordinator binary runs only on pi1 (Linux/ARM). No Windows build of the coordinator needed.
- Agent binary unchanged on Linux (systemd via existing recipes) and Windows (NSSM service via existing recipes). Both already work with `MESH_AUTH_TOKEN` + a coordinator address.
- State paths are handled correctly across OSes by the `dirs` crate (`~/.config/ai-mesh/` on Linux, `%APPDATA%\ai-mesh\` on Windows).
- If we ever ran the coordinator on Windows: Windows Firewall would need an inbound allow rule for TCP/9001. Not applicable to this migration.

## Operational details

- **SQLite write durability.** `registry.rs:163` only sets `PRAGMA foreign_keys = ON;` — journal mode stays at SQLite's default `DELETE` with `synchronous=FULL`, so every commit fsyncs. Write volume is low enough that SD-card wear is not a near-term concern. If we ever switch to `journal_mode=WAL`, we should also set `synchronous=NORMAL` (the WAL-friendly pairing) — but that's an independent decision, not part of this migration.
- **Backup**: a nightly cron on pi1 that copies `/var/lib/ai-mesh/ai_mesh.db` and `~/.config/ai-mesh/` to a second host (the laptop, or rsync to a USB drive). Cheap insurance.
- **Upgrades**: `just deploy-coordinator pi1` should be idempotent — scp the new binary, `systemctl restart`, run the Phase-2 health check. No manual intervention.

## Concrete artefacts to produce next

1. `systemd/ai-mesh-coordinator.service` — the unit file, in the repo for source control.
2. `just deploy-coordinator <host>` — cross-build → scp binary + state + DB → install unit → enable + start → run health check.
3. `just verify-coordinator <host>` — the Phase-2 health check, standalone so it can be re-run any time.
4. `just rollback-coordinator` — emergency revert: stop pi1 service, restart on laptop using the local state files (which we keep as a backup until cutover is proven stable).

## Open risks / edge cases

- **mDNS on pi1 + WSL2 still on the same network**: if the WSL2 coordinator is accidentally left running, two services will advertise on 9000. Mitigation: explicitly stop and disable the laptop-side run path before the cutover. The `rollback-coordinator` recipe handles the reverse.
- **First-boot Tailscale auth**: `tailscale up` opens an auth URL. On a headless pi1, the operator copies the URL to a browser. One-time only.
- **Tailscale clock skew**: Tailscale requires accurate NTP. Pi 5 should be NTP-synced already; verify before deploying.
- **Cert fingerprint change** is *not* a risk for this migration (we copy the existing cert), but is a risk if we ever rotate certs after the move — agents would need their pinned fingerprints updated. Existing `set-fingerprint` recipe handles this.

## Future hardening (deferred — not for this migration)

- **`ProtectHome=true`** would block the coordinator from reading `/home/pi/.config/ai-mesh/`, where `dirs::config_dir()` resolves the cert + state files (`coordinator/src/tls.rs:11` and `coordinator/src/state.rs:4`). To enable full systemd sandboxing later, either override `dirs::config_dir()` with an explicit `MESH_CONFIG_DIR` env var that points at `/var/lib/ai-mesh/`, or split state into a separate `/etc/ai-mesh/` path. Either is a code change, not a config tweak.
- **`LimitNOFILE=4096`** in the systemd unit — not needed at <10 agents; revisit if the cluster grows past 100.
- **SQLite WAL switch** as noted above — small win for SD-card longevity, separate task.
- **Per-node TLS certs** instead of one self-signed cert pinned across all agents — proper PKI, larger change.
