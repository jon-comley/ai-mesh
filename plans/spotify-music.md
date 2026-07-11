# Spotify Music Capability ("play xyz" via chat & puck)

## Context

Add music to the mesh: say "play Blackbird by the Beatles", "pause", "skip", "go back 30 seconds", "what's playing?" in dashboard chat or to the voice puck, and music plays on the house speakers. The user has never used Spotify, so the plan includes a click-by-click account/setup phase.

**Key decisions:**
- **Spotify Premium** — required (free tier cannot be API-controlled at all).
- **One room today** (pi2's paired Bluetooth amp), **multi-room later** — a Spotify account streams to only ONE Connect device at a time, so multi-room must be one player fanned out to synced clients (Snapcast), not one player per room. We build single-room now but choose the plumbing (librespot `pipe` backend) so multi-room is a swap of the output stage only, not a redesign.

**Architecture (two planes):**
- **Playback engine**: `librespot` (open-source Rust Spotify Connect client) runs on pi2 as a child process supervised by a new `capability-music`, with the **`pipe` backend** (raw PCM on stdout, zero C deps → cross-compiles with the existing aarch64 toolchain). PCM is piped into `pacat --device=<paired BT sink>` (PipeWire, same sink TTS uses). Later: pipe → snapserver FIFO instead of pacat = multi-room.
- **Control plane**: Spotify Web API called from `capability-music` on pi2 (plain reqwest + rustls, matching house style — not rspotify) using a refresh token: search → play on the librespot device, pause/resume/next/previous/relative-seek/volume/shuffle/status.
- **Routing**: new `music_control` LLM tool in the coordinator, dispatched to pi2 over the mesh exactly like `reaper_transport` (`dispatch_reaper_command` at `coordinator/src/intent.rs:860` is the copied skeleton: `connected_feature_node` + `PendingIntents` oneshot + timeout). Chat and puck converge at `handle_intent`, so both work automatically.

**Verified constraints that shape the code:**
- **No second LLM turn**: the tool's returned string goes verbatim into `ToolCallRecord.result`; voice speaks only `IntentResponse.text` (currently only `offline_skip_summary`, intent.rs:360). So every `MusicCommandResult.message` must be a finished human sentence, and "what's playing?" needs a `music_reply_summary()` to populate `text` or the puck stays silent.
- **`nodes/*.env` is committed to git** — Spotify secrets must NOT go there. Precedent: `MESH_AUTH_TOKEN` via systemd drop-in pushed by `_push-node-env` (justfile:629). Mirror it with a `spotify.conf` drop-in; drop-ins survive installer re-runs.
- `paired_bluetooth_sink()` (`capabilities/audio/src/lib.rs:201`) is private → make it `pub` and have capability-music depend on capability-audio (precedent: `configured_backends()` is already pub for the agent, "one parser, no drift").
- Shared wire-type change ⇒ **deploy coordinator AND node together** (WIRE_VERSION bump 10 → 11).

---

## Phase 0 — Spotify setup for a first-timer (user does this, we guide; goes in `docs/music.md`)

1. **Account + Premium**: spotify.com → sign up → Premium → Individual plan (~£12/mo, usually a free first month). Premium is mandatory: every player API endpoint returns 403 on free accounts.
2. **Developer app**: developer.spotify.com → log in with the same account → Dashboard → accept dev terms → Create app: name `ai-mesh`, Redirect URI **exactly** `http://127.0.0.1:8888/callback` (Spotify rejects `http://localhost` now), tick "Web API" → Save. Copy **Client ID** and **Client Secret** from Settings.
3. **Get refresh token** (WSL2 has no browser, so paste-the-URL flow): `SPOTIFY_CLIENT_ID=... SPOTIFY_CLIENT_SECRET=... just spotify-auth` → open printed URL in the Windows browser → approve → browser lands on a dead `127.0.0.1:8888/callback?code=...` page (expected) → copy full address-bar URL, paste into the terminal. Helper writes `~/.config/ai-mesh/spotify.env` (chmod 600).
4. `just spotify-push-creds pi2` (systemd drop-in).
5. `just build-librespot && just deploy-librespot pi2 && just spotify-login pi2` — librespot's own one-time OAuth (its `--enable-oauth` flow, redirect tunneled back through `ssh -L`); credentials cache to `~/.ai-mesh/spotify-cache/credentials.json` on pi2.

Note for `docs/music.md`: steps 3 and 5 are **two independent credential stores** — step 3 authorizes the control plane (Web API refresh token, lives in the systemd drop-in), step 5 authenticates the playback device (librespot's `credentials.json` on pi2). Redoing one never fixes the other.

## Phase 1 — Wire types + capability skeleton

- `shared/src/hardware.rs:96`: add `Music` to `enum Feature`.
- `shared/src/messages.rs`: add `MusicCommandRequest { request_id, action: String, params: serde_json::Value }` (mirrors `ReaperCommandRequest`) and `MusicCommandResult { request_id, ok, message }`; add `MeshMessage::MusicCommand/MusicCommandResult` variants; bump `WIRE_VERSION` (messages.rs:6) 10 → 11. Serde round-trip test like the existing ones.
- New crate `capabilities/music/` (workspace member): `Capability` impl (`name`=`"music"`, `handles` matches `MusicCommand`), stub `execute()` for now. `start()` uses the `Once`-guarded background-spawn idiom from `capabilities/audio/src/lib.rs:299`.
- Agent wiring: `agent/Cargo.toml` feature `music = ["dep:capability-music"]`; `agent/src/dispatch.rs:11` build entry; `agent/src/capabilities.rs:26` advertise entry.

**Verify**: `cargo test` green (pre-commit hook runs clippy -D warnings + full test anyway).

## Phase 2 — Coordinator routing

All in `coordinator/src/intent.rs` unless noted:
- Add `Feature::Music` to the feature loop in `collect_tool_schemas` (:23).
- `tool_schemas_for_feature` (:1886): one `music_control` tool, house style (single tool, `action` enum): actions `play|pause|resume|next|previous|seek|volume|shuffle|status`; params `query` (what to play), `entity_type` (track/album/artist/playlist, default track), `seconds` (relative seek, negative = rewind), `percent` (volume), `on` (shuffle).
- `dispatch_music_command` next to `dispatch_reaper_command` (:860), same skeleton but: **10 s timeout** (play = up to 4 upstream HTTP calls), always relay `r.message` (it's already human-readable), tolerate model arg drift (`query` | `target` | `song` — precedent: get_climate at :534).
- `dispatch_tool` (:922): `"music_control"` arm.
- `coordinator/src/server.rs` (~:1496, next to `ReaperCommandResult`): route `MusicCommandResult` back through `pending_intents`.
- Voice audibility: `text = offline_skip_summary(...).or_else(|| music_reply_summary(&records))` at :360 — returns the result string when the call was `action=status` (questions get spoken; commands stay silent like lights). Unit-test both branches.
- `build_system_prompt` (:2146): when a `music_control` schema is present, append rule: music questions ("what's playing?") are `music_control action=status`, never free text.

**Verify**: unit tests (schema present, prompt rule conditional, summary branches); `cargo test`.

## Phase 3 — Control plane (Web API) + credentials

- `capabilities/music/src/web_api.rs`: token refresh (`POST accounts.spotify.com/api/token`, Basic auth, cache until expiry−60 s) + thin wrappers: search, devices, play, pause, next, previous, seek, volume, shuffle, currently-playing. Env: `SPOTIFY_CLIENT_ID/SECRET/REFRESH_TOKEN`. If Spotify rotates the refresh token in a refresh response, persist it to `~/.ai-mesh/spotify_refresh_token` (chmod 600) and prefer that file over the env var at load — otherwise creds silently die weeks later on a headless node. 429s: surface immediately as a spoken sentence in v1 (single-household use makes them effectively theoretical); if ever observed, a `Retry-After` retry is only worth adding when the wait fits inside the coordinator's 10 s dispatch timeout — otherwise surfacing beats retrying into a guaranteed timeout.
- Real `execute()`: resolve librespot device_id from `/me/player/devices` by `SPOTIFY_DEVICE_NAME` (cache, re-resolve once on miss); handle `NO_ACTIVE_DEVICE` by retrying with explicit device_id; every outcome a finished sentence ("Now playing 'Hey Jude' by The Beatles", "Nothing is playing", "the Spotify player on this node isn't registered yet — is librespot running?").
- `capabilities/music/src/bin/spotify_auth.rs` (`[[bin]]`): prints authorize URL (scopes `user-modify-playback-state user-read-playback-state user-read-currently-playing`), reads pasted redirect URL, exchanges code, writes `~/.config/ai-mesh/spotify.env`.
- justfile: `spotify-auth` (cargo run the bin), `spotify-push-creds node` (drop-in `/etc/systemd/system/ai-mesh-agent.service.d/spotify.conf`, cloned from `_push-node-env` linux branch justfile:646 incl. daemon-reload + stop/kill/start).
- `nodes/pi2.env` (non-secrets only): `NODE_FEATURES=art,audio,music`, `SPOTIFY_DEVICE_NAME=AI Mesh`.
- `scripts/install-node-linux.sh`: positional arg 13 `SPOTIFY_DEVICE_NAME`; `MUSIC_ENV_BLOCK` (sets `SPOTIFY_LIBRESPOT_BIN`, device name, and `XDG_RUNTIME_DIR` if audio feature absent); add block to unit heredoc. justfile `deploy-node` passes arg 13.

**Verify** (user does Phase 0 steps 1–4 first): deploy **both** `just deploy-coordinator pi1` + `just deploy-node pi2` (wire bump); chat "what's playing" → "Nothing is playing" — the control plane works against any Spotify device (e.g. the user's phone playing) before librespot even exists, a clean intermediate checkpoint.

## Phase 4 — Playback engine (librespot on pi2)

- `capabilities/music/src/player.rs` supervisor loop: if no `credentials.json` → warn + sleep 60 s; resolve `paired_bluetooth_sink()` fresh each spawn; spawn `librespot --name <SPOTIFY_DEVICE_NAME> --backend pipe --cache ~/.ai-mesh/spotify-cache --format S16 --bitrate 160 --initial-volume 60` (stdout piped) → `pacat --device=<sink> --raw --format=s16le --rate=44100 --channels=2` (stdin piped) → `tokio::io::copy` between them; on either exit, kill both, backoff 2 s→60 s (reset after 5 min healthy). First spawn: reap orphans from `systemctl kill` deploys (precedent: stt.rs port-based stray-kill) — match on the cache dir path (`pkill -f "librespot.*--cache <SPOTIFY_CACHE_DIR>"`), not just the device name, so a manually-launched debug librespot with its own cache is never killed.
- `capabilities/audio/src/lib.rs:201`: `pub fn paired_bluetooth_sink()` with the "one parser, no drift" doc note.
- justfile: `build-librespot` (`RUSTFLAGS="" cargo install librespot --version 0.6.0 --locked --no-default-features --target aarch64-unknown-linux-gnu --root target/librespot-aarch64` — RUSTFLAGS="" neutralizes workspace `-Dwarnings`; the `.cargo/config.toml` aarch64 linker still applies), `deploy-librespot node` (scp + chmod), `spotify-login node` (`ssh -t -L 5588:127.0.0.1:5588 ... "~/librespot --enable-oauth ..."` with echo-banner instructions).

**Verify** (user does Phase 0 step 5): `journalctl -u ai-mesh-agent` on pi2 shows librespot up; device "AI Mesh" appears in the Spotify app's Connect device list; chat "play blackbird by the beatles" → audio from the BT amp; kill librespot manually → supervisor restarts it; puck: say "what's playing" → spoken answer.

## Phase 5 — Polish

- `just test-music` smoke recipe cloned from `test-reaper` (justfile:2175): "pause the music" → tool==music_control; "what's playing?" → status result non-empty AND `text` set; "play blackbird by the beatles" → result starts "Now playing".
- `docs/music.md`: Phase 0 walkthrough + troubleshooting (no device = librespot down; 403 = not Premium; silent audio = stale BT sink).

## Phase 6 — Multi-room transport (built 2026-07-11; rooms param deferred)

**Built**: the pacat stage is gone. librespot writes PCM into a FIFO
(`~/.ai-mesh/spotify-fifo`); snapserver (own unit `ai-mesh-snapserver`,
runs as the agent user, installed/configured by `install-node-linux.sh`)
fans it out sample-synced; the agent supervises a local snapclient
(`--hostID ai-mesh`, `PULSE_SINK` = paired Bluetooth sink, re-resolved per
restart) playing the stream. librespot and snapclient are supervised
independently — a session drop doesn't tear down the audio path and vice
versa. Adding a room = install snapclient on that node, point it at pi2.

**Deferred until a second speaker exists**: the `music_control` `rooms`
param (per-room on/off via snapserver's JSON-RPC on 1705, mapped through
the `room-audio-sink:<room>` prefs) — untestable with one room, and the
room-mapping convention should be decided against real hardware.

---

## Deploy recipes to ship this (in order)

1. `just deploy-coordinator pi1` **and** `just deploy-node pi2` — same session (WIRE_VERSION bump; old coordinator silently drops unknown variants)
2. `just spotify-push-creds pi2` (after `just spotify-auth`)
3. `just build-librespot && just deploy-librespot pi2 && just spotify-login pi2`
4. Other nodes (pi1 agent, beelink1, omnilink1) redeploy at next convenient window for the wire bump.

## Risks / verify during implementation

1. **librespot 0.6.0 aarch64 build purity** — pipe backend is featureless-clean in principle (`--no-default-features` avoids alsa-sys), but check `cargo tree` for native-tls/openssl leakage. Fallbacks: newer version with rustls feature → cross libssl → native build on pi2 once → extract binary from Raspotify's arm64 .deb.
2. **librespot OAuth flags** — confirm `--enable-oauth` port (5588 assumed) and that cached `credentials.json` suffices on later runs without the flag.
3. **pacat behavior** — confirm s16le/44.1k/2ch matches `--format S16` output; add `--latency-msec` only if underruns.
4. **Stale BT sink landmine** (documented at audio lib.rs:626): pacat to a vanished sink silently plays to default. v1 accepts this; future: restart pipeline on `bluetooth::is_connected` change.
5. **Refresh-token rotation** — handled in v1: rotated tokens persist to `~/.ai-mesh/spotify_refresh_token`, preferred over env at load (see Phase 3).
6. **Small-model arg drift** — watch live intent logs during Phase 3; extend the query/target/song fallback as observed. Same watch covers action misclassification (e.g. a "what's playing?" question emitted as `action=play`): deferred — no coordinator-side override heuristic unless the logs actually show it; the conditional system-prompt rule is the v1 defense.
