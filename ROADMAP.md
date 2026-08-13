# ai-mesh — Roadmap

Living list of **outstanding** work. Sections are kept in the order they were tracked in
the original `docs/roadmap.md` before the split, not re-sorted by priority — read the
whole file rather than assuming top-to-bottom urgency; entries carry their own dates.

**Finished work lives in [`HISTORY.md`](HISTORY.md)**, which keeps this file to what
still needs doing. Split out of `docs/roadmap.md` on 2026-08-14 — that file mixed roughly
2,100 lines of finished phases with open ones, which is the same content made harder to
find (see `ventures/strategy/documentation.md`, and `ventures/PRIORITIES.md` item 7).
Where a phase had both finished and open sub-parts (Phase 11 and its sub-phases, Phase
11.7, several review backlogs), only the still-open sub-sections are here — see
HISTORY.md for the same heading's finished parts. Several entries carry a long record of
what was investigated and rejected along the way; that reasoning is preserved
deliberately, not padding, so read the whole entry before picking one up.

## Hunts / eBay Bargain Finder — deployed, production keyset issued, not yet exercised with real data (2026-07-15)

`plans/ebay-bargain-finder.md` is fully implemented and deployed to pi1:
the `ebay` crate, registry persistence, coordinator HTTP API, background
per-hunt timer with startup re-arm, and the Hunts dashboard tab. Full
workspace test suite and clippy are clean; live-verified against the running
coordinator (config round-trip, hunt CRUD, `run-now`, `analyze` validation,
static assets/tab) and the eBay OAuth call path was genuinely exercised
(clean auth failure with placeholder credentials, handled gracefully).

A production Browse API keyset was issued 2026-07-15. Generating it required
satisfying eBay's mandatory **Marketplace Account Deletion/Closure
Notification** endpoint field — undocumented in `docs/ebay-hunts.md`'s
walkthrough, and a real blocker since it demands a publicly-reachable HTTPS
URL that pi1 (LAN/Tailscale-only) can't serve directly. Hunts never uses
OAuth user tokens (Browse API only needs an app-level client-credentials
token), so no eBay user PII is ever stored and the notification itself needs
no real handling — just the verification challenge response. Solved with a
standalone Cloudflare Worker (`your-worker.example.workers.dev`,
deployed via `wrangler` CLI rather than the dashboard's web editor, which
repeatedly corrupted pasted code) that computes the SHA-256 challenge
response and 200s any deletion POST. Decoupled from home infra on purpose —
it has to answer eBay's periodic re-verification checks regardless of
whether pi1/Tailscale is up.

**⚠ Still not exercised with real eBay data.** Next:
1. Paste real client_id/client_secret into the Hunts tab's settings block
   (no deploy step — same operational model as the Online AI tab's key).
2. `POST /api/ebay/analyze` against a real listing URL, confirm term
   suggestions look sane.
3. Create a hunt, `run-now`, confirm real listings come back and the ticker
   + (if an ntfy topic is set) phone push both fire.
4. Let a hunt run unattended through a scheduled timeslot to confirm the
   background timer's real-world timing, not just the unit-tested pure
   scheduling logic.

**Timeslot picker reworked (2026-07-15):** the original UI was a fixed
00–23 hourly grid, and a CSS specificity bug (`.ebay-editor button` was
unintentionally more specific than `.ebay-slot`/`.ebay-slot-on`) meant the
"selected" highlight never actually rendered — clicking silently did
nothing. Replaced with a `type="time"` picker + removable chip list (same
pattern as the search-terms box), live-verified working on pi1. The
backend already stored timeslots as arbitrary minutes-since-midnight
(`ebay/src/schedule.rs`), so this was purely a frontend fix/upgrade — no
API or schema change.

**⚠ No cap on timeslots-per-hunt or total daily eBay calls.** Each hunt
firing makes one Browse API call per *enabled* search term
(`run_hunt_cycle`, `coordinator/src/http/api/ebay.rs`), so daily call
volume ≈ hunts × active terms/hunt × timeslots/day — and now that
timeslots are freely user-added rather than capped at 24 (one per hour),
that product has no ceiling. eBay's Browse API typically grants ~5,000
calls/day (exact figure depends on what the specific application was
approved for; see `plans/ebay-bargain-finder.md`'s original risk note),
and realistic hand-entered usage sits nowhere near that, but there is
**no enforced guard today** beyond a log line + skip-this-cycle if eBay
actually returns a 429 (`EbayError::RateLimited` handling, same file).
Worth a soft cap or running-total warning if hunt/timeslot count grows.

## Music / Spotify — code-complete, NOT yet live-tested (2026-07-12)

All build phases of `plans/spotify-music.md` are implemented and committed
(wire types + capability skeleton → coordinator `music_control` routing →
Spotify Web API control plane + OAuth tooling → librespot playback engine →
`just test-music` smoke recipe → snapcast multi-room transport). Unit tests,
clippy, and the aarch64 cross-builds (agent + librespot 0.6.0) are green.

**⚠ Nothing has run live yet — deployment is blocked on Phase 0: awaiting
Spotify Premium membership** (playback control is Premium-only; the developer
app + both OAuth logins come after signup — walkthrough in `docs/music.md`).

Rollout once membership exists:
1. `just deploy-coordinator pi1` + `just deploy-node pi2` — together (WIRE_VERSION 10→11; pi2 also gets snapserver installed)
2. `just test-music` — routing asserts pass, playback warns
3. `just spotify-auth` → `just spotify-push-creds pi2`
4. `just deploy-librespot pi2` → `just spotify-login pi2`
5. `just test-music` — fully green, audio from the pi2 Bluetooth amp

Deploy-time verification items (assumed from docs, never exercised):
librespot's blocking open of the snapserver FIFO, snapserver `mode=create`
perms under the agent user, snapclient honoring `PULSE_SINK`. Fallback for
A/B debugging: commit `f057990` still has the pre-snapcast direct-pacat
pipeline. Deferred by design: the `music_control` `rooms` param until a
second room speaker exists (`plans/spotify-music.md` Phase 6).

## Phase 9 — Remaining Cluster Nodes (In Progress)

- **Mac mini M4** ⚠️ _hardware not available until ~end of July 2026_ — cross-compile for `aarch64-apple-darwin`, provision as compute node, add `just deploy-node mac1`
- **Multi-node routing validation** ✓ — `just validate-routing` confirms `qwen2.5:1.5b` → Pi and `qwen2.5:7b` → Beelink; `mesh infer` output now includes a `served-by:` line showing the serving node; load-balancing across identical-model nodes is a future concern when a second GPU node joins
- **`just start-cluster` recipe** ✓ — starts coordinator + controller + all remote agents, then calls `auto-load-model` on every compute node; leaves mesh in a ready-to-use inference state
- **`just auto-load-model <node>`** ✓ — SSHes into node, detects GPU VRAM or CPU RAM, selects best-fit model, loads it with hardware-filtered fallback hints
- **Automatic model placement (coordinator side)** ✓ — `ModelLoadRequest.node_id` is now `Option<String>`; when absent the coordinator calls `select_node_for_model(mb)` and picks the node with the most headroom; `mesh load <model> <size>` works without `--node-id`; `just load-model` still passes explicit node for predictable placement

## Lighting — Deferred Improvements

- **Device availability tracking** ✓ (now working end-to-end, see **F-Lighting-UX**) — per-device `online` flag flows through to the dashboard (offline cards disabled; placeholders start offline) and a bridge-level `ZigbeeStatus` drives the offline banner. *2026-06-29:* this had been silently inert — z2m's `availability` feature was disabled (so offline was never published), and the agent's `<device>/get` poll re-stamped offline bulbs as `online: true` from z2m's cached state on every reconnect. Fixed by enabling `availability.enabled: true` in z2m and stamping state reports from a per-device availability map in `capabilities/zigbee`. **Still deferred (LLM side):** suppressing/marking offline devices in the LLM system prompt, and a distinct `device 'x' is currently offline` validation error instead of the generic unknown-target error.
- **`bridge/event` subscription** — Subscribe to `zigbee2mqtt/bridge/event` for real-time device join/leave announcements. Currently `bridge/devices` (retained) covers discovery on connect, but live pairing events require this topic. Low priority until live-pair UX is needed.
- **ZigbeeClient lifecycle hardening** — `connect()` spawns one event loop task internally and is safe via `OnceCell`. If multiple lighting nodes or dynamic reconnect logic is added, consider an explicit `shutdown()` signal and replacing `OnceCell<Arc<ZigbeeClient>>` with `ArcSwap<ZigbeeClient>` to allow reconnect without structural changes.
- ~~**Bounded Manual State Cache**~~ — obsolete: `last_manual_states` was deleted entirely in the F-Effects-2 legacy purge; nothing to bound.
- **Solar Dashboard Widget** — Add a compass and elevation widget to the Topology view to visualize the real-time solar vector.
- **Debounce map pruning** — completed debounce tasks leave a dead `AbortHandle` in the map until the same device fires again. Low impact (map stays small), but a periodic sweep or a completion callback could keep it clean in long-running deployments.
- **Distributed lighting capability** — the `capability-lighting` crate connects to an MQTT broker by address (`MQTT_HOST`/`MQTT_PORT`). Any node (e.g. beelink1) could run the lighting capability pointed at pi1's Mosquitto broker, saving the coordinator→pi1 LightCommand mesh hop when beelink1 is already serving the intent. Estimated saving: ~1–2ms LAN RTT. Not worth building until the Zigbee RF round-trip (~300–500ms) is no longer the dominant latency.
- **Room temperature slider — match the light-card style** — the room-level temperature slider (in the room card's colour/temp panel) should use the same warm→cool gradient temp-bar widget as the individual device cards (`buildTempBar` in `lightcontrols.js`), rather than the generic `buildSlider`. Aligns the room and device controls visually and in behaviour, consistent with how the room brightness slider already mirrors the device brightness slider.

- **Bulb power-cycle detection + UI indication** — when a bulb is physically switched off and back on at the wall, it reverts to its factory/last-power-on state (typically warm white full brightness), overriding any active effect or scene. The coordinator needs to detect this transition (a bulb reporting `on:true` with default state after having been `on:false` or `online:false`) and: (1) surface it visually on the device card (e.g. a "reverted" indicator or distinct icon state); (2) optionally re-apply the active effect/scene to that bulb automatically. Detection signal is already available via the `LightingUpdate` WS path — needs a heuristic to distinguish a genuine power-cycle revert from a normal user-on command.

- **Zigbee latency ceiling** — the observed ~300–500ms bulb switch time is inherent to the Zigbee protocol (RF transmission + bulb processing + optional ACK). The SLZB-06 is network-attached (Ethernet, <slzb-06-old>), so moving Z2M ownership to a different machine would not reduce this — the RF path is identical. Meaningful latency improvements would require: Zigbee direct binding (bypass Z2M for switch→bulb, not applicable to software commands), or migrating to Matter/Thread hardware (typically ~50–100ms).

- **Remove the dead device-card view in `lighting.js`** — `dashboard.js` calls `setRoomsActive()` unconditionally at startup with no reset, so `roomsActive` is permanently true and `lighting.js`'s device-card `render()` (and its sibling slider-patch path) always bails: it's unreachable. The lighting tab is exclusively the rooms view (`rooms.js`). Cleanup: delete the dead `render()`/device-card builders and the `roomsActive` flag from `lighting.js`, keeping only what the rooms view still imports (e.g. `buildLightControls`, `formatDeviceName`), and drop the now-unused `.light-card`-only grid styling. Surfaced while fixing the maximised rooms-view layout (the `#lighting-list` card grid only ever applied to this dead path).

## Phase 11 — Web Dashboard & Health Reporter (In Progress)

Full design spec: `plans/phase11-dashboard.md`
### Phase E — Error feed + diagnostic panel
### Phase F — Lighting OS. Full spec in `plans/phase11f-lighting.md`.

#### The Vision

> Everyone else built a light switch you can control from your phone. We're building a lighting system that understands what's happening in your home — including inside your AI cluster — and responds to it.

**Room layout context:** 3×3 grid of lights. Bottom row: 3 white-temperature-only bulbs. Main grid: 8 colour spots (4×2 arrangement). This asymmetry shapes how scenes, effects, and games are designed — colour commands must gracefully degrade to brightness-only on the CT bulbs.

**The innovations:**
1. **Telemetry Lighting** — the room is the dashboard. GPU inference running → subtle blue pulse on the desk lamp. Node offline → brief red flash. Model loading → soft green fade. No commercial system has ever tied infrastructure telemetry to ambient lighting because no commercial system runs AI inference in your home.
2. **Intent-First Adaptive Scenes** — "cozy" at 7am means something different than at 10pm. Scenes are prompts, not presets. The LLM resolves the intent using time of day, room state, and known devices.
3. **Temporal Scene Composer** — scenes that exist across time. A morning routine defined as a drag-and-drop timeline (warm red → cool white over 40 minutes); coordinator executes transitions as a background task.
4. **Reactive Graph** — plain-language automation rules. *"If no motion for 5 minutes after sunset, fade to 10% over 2 minutes."* LLM parses each rule once into a structured `{ trigger, action }` pair; coordinator evaluates triggers on every event. No YAML, no Home Assistant complexity.
5. **Presence-Weighted Circadian** — not just "shift colour temp with the sun" but per-room based on occupancy. Motion sensor active in the study at midnight → keep it cool and bright. Room empty → start wind-down regardless of global schedule.
6. **Photo Colour Picker** — a grab-box tool in the dashboard: load a photo (or in future, a live screen capture), drag a selection box over any region, and the average colour of that region is extracted client-side via Canvas API and sent to the selected lights as CIE XY. Useful for matching artwork, a sunset photo, or the palette of whatever is on screen.
7. **Light Games** — the 4×2 colour spot grid is a natural game board. Simon Says, Whack-a-Mole (yellow flash on a random bulb, tap before it goes out — speeds up each hit), Colour Match, Hot/Cold (motion-sensor treasure hunt), Disco Party, Strobe/Rave mode. Full game catalogue in F9.

**Future — Effects Engine (F9+):**
- **Solar / Sunset Engine** — coordinator uses device location (lat/lon) + the `sunrise` or `spa` Rust crate to derive exact sunrise/sunset times and solar elevation; drives a smooth colour-temperature curve matching the real sky throughout the day. Configurable per-group.
- **Hue Entertainment Fast Path** — MQTT round-trip (~100ms) is too slow for fluid effects. Philips' Entertainment API uses UDP streaming at ~50 Hz. A future `capability_effects` crate would open a UDP stream directly to the Zigbee coordinator (or a Hue bridge if one is ever added) for music sync, game sync, and reactive effects that feel instantaneous.
- **Screen / Monitor Sync** — screen capture (`scrap` or `xcap` Rust crate) → divide frame into regions → compute average colour per region → stream to the corresponding light in the grid. An Ambilight-style experience without the proprietary hardware. Requires the Hue Entertainment fast path (MQTT is too slow for 50 Hz updates).
  - **Phase C — Three.js 3D view**: Replace the hand-rolled SVG canvas with a **Three.js** scene (CDN importmap — no build step). An orthographic camera reproduces the current 2D floor-plan view exactly; a "3D" toggle button switches to a perspective camera + `OrbitControls` so the room can be spun and zoomed. Room geometry is a `THREE.Shape` polygon so non-rectangular rooms (L, T, etc.) are naturally supported. Openings are coloured wall patches; fixtures are emissive spheres whose brightness/colour tracks the live WS state. Prerequisites: add `width_m`, `depth_m`, `height_m` floats and optional `shape_vertices` JSON column to the `rooms` table (ALTER TABLE migration), and a UI to set them (numeric inputs in the room settings popover). Without dimensions the view defaults to a 4 × 4 × 2.5 m box. Rendering tasks: floor + ceiling quads, four extruded wall meshes with per-face UVs, opening cutouts as CSS colour patches (full boolean geometry holes deferred — hard in Three.js without BSP), fixture spheres with `PointLight` per bulb, ambient + directional sun light that tracks `SolarUpdate`. Canvas toggle preserves all placed bulbs and openings — no data reload needed. Target: 2D view is visually identical to the current SVG canvas; 3D adds spin/zoom and approximate room shape. Phase C replaces the SVG `<g>` layers entirely; the JS module (`layout.js`) is refactored but the REST API and data model are unchanged.
- **F7** — Switches / Input Devices. New `capability_switches` crate. Subscribes to Z2M button, remote, motion sensor, and contact sensor events. Emits `SwitchEvent { device_id, action }` to coordinator; coordinator broadcasts `DashboardEvent::SwitchEvent`. Dashboard: live switch activity feed. Foundation for the Reactive Graph.
- **F8** — Creative Features. Delivery order: (1) Telemetry Lighting effect (GPU activity → ambient pulse); (2) Photo Colour Picker (client-side Canvas API → CIE xy); (3) Intent-First Scenes (LLM resolves "cozy" to actual device states at runtime); (4) Temporal Composer (drag-and-drop scene timeline, coordinator executes transitions); (5) Reactive Graph (plain-language automation rules parsed by LLM into `{trigger, action}` pairs); (6) 2D Room Layout (SVG floor plan, builds on F-Rooms + F-Spatial).
- **F9** — Light Games. The bulb grid is a natural game board — game logic lives in the coordinator (Rust, deterministic, no browser needed). Each game is a coordinator-side state machine that sends `LightCommand` sequences; the dashboard renders a game UI overlay and relays player input via a new `POST /api/games/{game}/input` endpoint.
  - **Simon Says** (MVP) — coordinator picks a random bulb, flashes it a colour, player taps the matching card in the dashboard. Sequence grows by one each round. Lives: 3. High score persisted in SQLite.
  - **Whack-a-Mole** — coordinator lights a random bulb for 1.5 s (yellow flash). Player must tap it in the dashboard before it goes out. Miss = −1 life, hit = +1 point + speeds up by 50 ms. 3 lives, 60 s game. Bulbs turn red on miss, green on hit.
  - **Colour Match** — coordinator sets two bulbs to slightly different CIE xy values; player picks which bulb matches a reference swatch shown in the UI. Difficulty ramps by reducing the xy delta each round.
  - **Hot/Cold** — motion-sensor driven (requires F7). Coordinator picks a "treasure" room; player walks around, bulbs glow warmer (red) when closer, cooler (blue) when farther. Win = enter the correct room.
  - **Disco Party** — no player input. Intent-triggered ("start a disco!"). Coordinator runs a choreographed colour sweep loop across all bulbs using the Effects engine; stops on "stop the disco" intent or after a configurable duration.
  - **Strobe / Rave mode** — fast-path only (F10 prerequisite). Sub-100 ms colour cycling across the grid, tempo synced to a BPM set via intent ("strobe at 120 BPM").
  - **Engineering note**: each game is a `GameSession` struct spawned as a tokio task. `GameRegistry` (similar to effect handler map) stores active sessions keyed by game ID. Dashboard game overlay is a separate JS module (`games.js`) that subscribes to a new `DashboardEvent::GameState` WS event for render updates.
- **F10** — Effects Engine fast path. Prerequisite: evaluate SLZB-06 + Z2M latency floor. If viable: `capability_effects` crate with fast-path UDP for fluid effects at ~50 Hz. Screen sync via `getDisplayMedia` (browser API, no Rust needed for MVP) deferred until fast path confirmed.
### Phase G — Security panel
### Phase H — Polish, icons, desktop layout pass
### Deferred dashboard polish (raised post-C5, updated 2026-06-11) — open items
  **Deferred preferences improvements:**
  - Schema / key allow-list — any string accepted today; enforce at write boundary when key space stabilises.
  - `user_id` wired to auth token — currently hardcoded `"default"`; safe for single-user, must change before multi-user auth.
  - Multi-device live sync — `loadPrefs()` on page load is sufficient for single-user; real-time cross-device push needs a WS-pushed `PrefsUpdated` event.
  - `updated_at` / `version` columns — deferred until migration tooling exists.
  - Consistent key naming (`dashboard.mesh.order`, `dashboard.health.collapsed.<id>` etc.) — rename sweep is a separate commit with localStorage migration.
- **Room brightness/colour/temp controls (action bar)** — popover is correctly positioned above the action bar on desktop; mobile behaviour needs verification after the `position:fixed` + `getBoundingClientRect` approach was adopted. The centred-modal approach used for the ＋Add palette may be worth applying here too if offset issues surface on mobile.

- **＋Add palette (mobile)** — switched to a centred modal (`position:fixed; top/left:50%; translate(-50%,-50%)`) to escape the dynamic-viewport toolbar issue. The `collapsed` class was found to hide all content when the sidebar had been collapsed on desktop and then opened on mobile — fixed by removing `collapsed` on open. Monitor for any remaining edge cases.

A lightweight web interface embedded in the coordinator process (no separate service). Primary goal: **observable mesh** — operators can see the state of the cluster at a glance and drill into errors without SSHing into nodes.

### Core views

- **Topology** — live graph of connected nodes, their model assignments, and heartbeat latency; nodes colour-coded (green / amber / red) by health status.
- **Health timeline** — time-series strip per node showing CPU %, GPU %, RAM %, heartbeat jitter. Data sourced from the existing heartbeat `HardwareStatus` payloads; stored in a small ring buffer in the coordinator (no extra DB writes for MVP).
- **Error feed** — structured error log; whenever a node reports an error (inference failure, model load failure, Zigbee disconnect, etc.) an entry is created with: timestamp, node, error kind, severity.
- **Diagnostic panel** — clicking an error entry triggers the coordinator to request a log snapshot from the affected node (`DiagRequest` mesh message); the node responds with its last N lines of stderr/stdout and the web UI renders them inline. This avoids the operator having to SSH just to read a log.

### Model management UI

- Load / unload models on any ready node via button; mirrors `mesh load` / `mesh unload` CLI.
- Node capacity bars (VRAM / RAM) update live so the operator can see headroom before choosing a model.

### Tech choices (tentative)

- **HTTP server** — `axum` (already familiar in the Rust ecosystem; lightweight); single binary, no Node.js build step.
- **Real-time updates** — WebSocket endpoint (`/ws`) that streams `DashboardEvent` JSON; front-end subscribes and patches the DOM.
- **Front-end** — plain HTML + vanilla JS (no framework) for MVP; keeps build complexity at zero. A Svelte/React layer can be added later if the UI grows.
- **Auth** — dashboard protected by the same `MESH_AUTH_TOKEN`; pass as a Bearer token in the WebSocket upgrade or a session cookie set on first visit.

### Security panel (HMAC failure metrics)

Surface authentication anomalies in real time — the error feed alone is not enough once the cluster grows beyond two nodes.

**Coordinator-side counters (in-memory, per peer socket address):**

```rust
struct PeerSecurityStats {
    invalid_signature: u64,   // HMAC mismatch — bad key or tampering
    stale_frame: u64,          // timestamp skew > 30 s — likely clock drift
    downgrade_attempt: u64,    // plain MeshMessage JSON after auth — old binary or probe
    last_event_ts: u64,        // Unix seconds of most-recent incident
}
```

Keyed by `SocketAddr`; evicted when the connection closes. Incremented in the existing `FrameVerifyError` match arms in `coordinator/src/server.rs`.

**New wire messages (future):**

- `AdminMessage::RequestSecurityMetrics` → `SecurityMetrics(Vec<PeerReport>)` — CLI/dashboard polls on demand.
- `DashboardEvent::SecurityIncident { peer, kind, ts }` — pushed over the WebSocket on every HMAC rejection so the dashboard error feed updates without polling.

**Dashboard security panel:**

- Table: peer IP, node name (if registered), failure kind, count, last seen.
- Stale-frame rows highlighted amber with a "check NTP" tooltip.
- Downgrade-attempt rows highlighted red — indicates a node running an old binary or an active probe.
- Row clears automatically when the peer's connection closes cleanly (expected reconnect) or stays for the session if the connection was forcibly dropped (anomaly).

**CLI:**

```
mesh security-report        # one-shot snapshot of current failure counts
```

### Deferred / stretch

- Alert rules (e.g., node offline > 60s → webhook / email).
- Historical inference latency per model.
- Live intent log (query, routed-to node, latency, response preview).
- Mobile-friendly layout.
- ~~**Beelink BIOS — locate Wake on LAN setting**~~ — done 2026-06-11: WoL located and disabled alongside the fTPM re-disable; node stable.

## Phase 11.7 — REAPER DAW Integration (2026-06-14) — remaining work
- ◐ REAPER on macOS — code is macOS-ready (`default_scripts_dir()` resolves
  `~/Library/Application Support/REAPER/Scripts` on macOS, `/mnt/c/...` on WSL2); full
  provisioning (install script + node setup + testing) still pending the Mac mini (~end Jul 2026).
- ◐ Multi-REAPER instances — route intents to a specific REAPER node once more than one
  exists (e.g. OmniLink1 + Mac mini). Registry side is ready (`nodes_with_feature` returns all
  matching nodes); intent routing currently grabs the **first connected** REAPER node (the
  `extend this to a policy` comment in `intent.rs` is the only code concession). Needs: a target
  (explicit `node` arg + an active/default instance, since small models can't reliably infer it
  from the utterance), a **per-tool** selection policy (most intents hit one instance, a few like
  "stop all" fan out), disambiguation when none is specified, and per-instance status keyed by
  node ID in the poller + dashboard panel. Parked until a second REAPER box exists — gated on the
  same Mac mini as the macOS work. Full design notes: `docs/reaper.md` (Deferred).
- ◐ Plugin stack + FX automation — curated free-first third-party plugin list (vocal + guitar
  tracking weighted: Analog Obsession, TDR Nova, Valhalla Supermassive, Youlean, …) plus a generic
  FX-control tool layer. No plugin exposes its own API — control is uniformly via REAPER's
  `TrackFX_*` ReaScript functions, so it extends the existing structured-tool pattern. Must guard
  against FX index drift (resolve by name in the coordinator, never trust raw LLM indices),
  `AddByName` format-prefix instability across OS, and lazy param-map init. Melodyne is **not**
  automatable by us (offline/ARA). Plugin install is manual (Windows-side, not automated).
  Execution is **incremental, free-first, one plugin at a time**, and every slice must be
  verifiable **without recording audio** (insert + read-back) since the studio isn't built yet.
  First target: **Valhalla Supermassive** (free reverb — famous named presets, the cleanest demo).
  - Slice 1 — `reaper_add_fx` (◐ code + unit tests done; pending live verification): inserts an
    FX by name on a named track, matching the bare product name (no `VST:`/`VST3:` prefix) and
    reporting unresolved plugins instead of a silent no-op. Verify with **Valhalla Supermassive**
    (a VST that resolves by bare name). **Note:** stock **ReaVerbate** is *not* a safe control —
    it's a JSFX and does **not** resolve via `TrackFX_AddByName` by bare name (JS plugins need a
    different name form), so don't use it as the "always installed" fallback.
  - Slice 2 — `reaper_list_fx` + `reaper_list_fx_params` (◐ code + unit tests done; pending live
    verification): discovery. `reaper_list_fx` walks a track's chain returning name + 1-based slot
    (+ bypass flag); `reaper_list_fx_params` resolves the FX by name match (never a raw index) and
    lists each param's name, formatted value, and raw 0–1 value. Running `reaper_list_fx_params` on
    Supermassive answers the open question of whether its modes are REAPER presets (→ `SetPreset`)
    or internal params — if a "mode" param appears, they're params; if not, they're presets.
  - Slice 3 — `reaper_set_fx_preset` + a small curated preset/mode catalog: the "make it
    spacious" payoff.
  - Later — `reaper_set_fx_param` / `reaper_get_fx_param`, `reaper_bypass_fx`, then the next free
    plugin (TDR Nova / Analog Obsession).

  Full list, pricing, install steps, and per-plugin automation: `docs/reaper-plugins.md`.

## Phase 11.8 — Multi-Device Home + Room-Centric Control (Plan ratified 2026-07-03 — executing)

> **Still open, needs Jon physically present (destructive/hardware-in-hand):**
> - Pull a sensor's battery (or temporarily lower z2m's
>   `availability.passive.timeout` instead of waiting ~25h) → confirm the
>   card dims to offline with readings intact.
> - Delete a sensor from the dashboard → confirm it actually leaves the
>   Zigbee network (re-pairing required to bring it back, not just a
>   vanished registry row) — the unpair-on-delete path only has a mocked
>   connection test so far.

> **Open question:** does the ±25 brightness step feel right on the actual
> dial (too coarse, too fine, or about right)? Not yet checked with Jon
> against real usage — `step_delta` is trivially adjustable per-binding via
> the same `POST /api/switch-bindings` call if it needs tuning.

> **Room-scanning research → deferred (2026-07-06)** — the actual original
> ask ("a proper scan of the room, like Apple do when they scan your
> face") needed real research before any code: full findings, what was
> tried and rejected, and the planned integration shape are all in
> `plans/roomplan-ios-scan.md`. Short version: no automatic/AR-assisted
> scanning path exists for the Samsung S22 (Android, no LiDAR) that beats
> manual measurement — confirmed even Magicplan, a leading commercial
> room-scanning app, falls back to plain tap-corners-on-a-grid for
> non-LiDAR Android devices. The iPad Pro (M5) does have a LiDAR Scanner
> (confirmed from Apple's own spec page) and could run Apple's real
> RoomPlan API — but only as a native iPadOS/Swift app, which is a hard
> platform requirement (Xcode/macOS-only, no route from this project's
> Linux dev environment) rather than something buildable/verifiable the
> way everything else in this project has been so far. **Deferred until
> Jon's new Mac Studio is unboxed and set up** — plan doc has the intended
> shape (RoomPlan capture → post wall dimensions/openings straight into
> the existing `rooms`/`openings` REST API, syncing to every device via
> the shared coordinator database same as everything else) ready to pick
> up at that point.

### Concrete steps — not yet built

- **When blinds land** (`capability-blinds`, Z2M `cover`): join the *generalised*
  room model, not the lighting-specific path. New `cover` widget (position + tilt).
  Good moment to replace raw feature strings (`"lighting"`, `"reaper"`, soon
  `"cover"`/`"climate"`) with a feature **enum** — there'll finally be enough
  variants (`nodes_with_feature` / `node_has_feature` / capability registration)
  to justify the type safety. Premature while only lighting exists.

- **Defer the full room-centric view** until a second device type exists to justify
  it; until then don't over-invest in per-domain navigation a room view will
  replace.

## OpenAI API — Per-Key Auth, Usage Attribution & Rate Limiting (Next)

The third productization step (`an internal productization plan` item 3): one
shared mesh token is fine at home, but a client site needs per-user/team API
keys, per-key usage accounting, and per-key rate limits. This is the feature
that produces the monthly "here's what each team used, and what it saved you
versus cloud pricing" report — the ongoing-value justification.

Design sketch:

- **`api_keys` SQLite table** (registry DB): `key_hash` (SHA-256 — never store
  the raw key), `label` ("finance-team"), `created_ms`, `revoked` flag,
  optional `rate_limit_per_min`. Keys minted/revoked via new dashboard
  Security-tab UI + `/api/keys` CRUD (mesh-token-auth only, like every
  existing `/api/*` route).
- **/v1 auth resolution order**: mesh token (admin, as today) → `api_keys`
  lookup by hash. The resolved key label rides the request for attribution.
- **`usage_log` table**: `ts_ms`, `key_label`, `model`, `prompt_tokens`,
  `completion_tokens`, `duration_ms`, `served_by` (node / cloud), `stream`.
  Written once per completed request (terminal result / finish chunk) —
  both /v1 paths already have every field in hand.
- **Rate limiting**: fixed-window per key (requests/min) checked at the top of
  `chat_completions`; over-limit → the existing 429 `rate_limit_error`
  envelope. No token-bucket sophistication until a client needs it. Optional
  per-key token ceiling per calendar month alongside it (same check site,
  reads `usage_log`) — the "no runaway spend" guarantee in the pitch.
- **Reporting**: `GET /api/usage?from=&to=` aggregates per key/model/day;
  dashboard panel later — CSV/JSON export first (the monthly PDF is generated
  off-mesh). Include a cloud-equivalent-cost column (tokens × configurable
  per-model cloud price) for the money-saved line.
- **Non-goals for this phase**: multi-tenancy isolation (all keys see all
  models), per-key model allowlists, OAuth — all deferred until a real client
  asks.

## Phase 11.9 — Frame TV Art Display (design ratified 2026-07-05)

Local, subscription-free replacement for Samsung's Art Store on a QE32LS03C
Frame TV: a Pi Zero 2 W hidden in an in-wall recess feeds the TV a fullscreen
HDMI slideshow sourced from public-domain art collections (WikiArt,
Rijksmuseum, The Met, Unsplash), driven by ai-mesh — the TV's own Art Mode /
SmartThings / cloud features are never engaged; the TV is a dumb HDMI panel
with a local WebSocket remote-control channel for input-switch/power only.

Full design + hardware/electrical notes: `plans/frame-tv-art-display.md`.
Hands-on provisioning guide (once the recess/socket work is done):
`docs/frame-tv-setup.md`.

New domain, same shape as every other one in this mesh: a `capability-art`
crate (mirrors `capability-reaper` — drives an external process rather than
Zigbee/MQTT) + a coordinator `api/art.rs` module, following the existing
domain-module recipe. Automation triggers (occupancy, ambient light,
time-of-day) are deliberately just new *consumers* of the sensor pipeline
already shipped in Phase 11.8 (the SNZB-03P R2 motion sensors already report
both occupancy and illuminance) rather than new infrastructure — sequenced
last, after the basic slideshow works reliably on its own.

> **2026-07-06 — v1 slice built and verified end to end, ahead of the
> electrician.** The recess isn't booked yet, so development started early
> on a spare Pi 4 with the TV on a stand instead of waiting — same aarch64
> target and mesh code either way. Shipped: `WIRE_VERSION` 9
> (`MeshMessage::ArtShow`/`ArtStatus`), the `capability-art` crate (fetches
> an image, shells out to `fbi` via `sudo -n`, reports status), the `art`
> node feature flag, and coordinator `POST /api/art/show` /
> `GET /api/art/status`. Deliberately minimal — one image, no catalogue or
> rotation yet. Viewer choice took two corrections against the real
> hardware (`pi2`, <pi2>): `feh` doesn't work on Lite (no X server);
> `mpv --vo=drm` works but Debian's package drags in a 600 MB GTK/X11 stack
> as unused dependencies, so `fbi` (a handful of small packages, writes
> straight to `/dev/fb0`) replaced it — which itself needed a
> `vc4-fkms-v3d`/`hdmi_force_hotplug=1` config change and running via
> `sudo -n` (VT control needs real root) to actually work; both are now
> automated in `scripts/install-node-linux.sh` for the next node. Live
> end-to-end test against the running coordinator: `POST /api/art/show`
> correctly spawned `fbi` on `pi2`, and a second call correctly killed and
> replaced it (exactly one `fbi` process throughout, confirmed via
> `pgrep`) — the whole coordinator → mesh → node → framebuffer chain is
> proven, just with no TV plugged into the HDMI port yet to see it.
>
> **Same day — on-the-fly slideshow shipped and deployed to pi1.** Rather
> than build the batch catalogue/ingest pipeline from plan §5 first, added
> `POST /api/art/search {query, interval_secs?}` — searches the Met
> Museum's Open Access API live (no local catalogue, no API key needed),
> keeps only public-domain results with a usable image, optionally asks
> whatever local LLM is Ready to pick and order the best subset (falls back
> to the raw Met order if no model's ready, the call errors, or its reply
> doesn't parse — a 20s-max nice-to-have, not a dependency), shows the
> first result, and auto-advances through the rest on a timer (default
> 30s/floor 5s) until superseded by a new search. `POST /api/art/next` and
> `GET /api/art/current` round it out. No new wire message needed — next/
> auto-advance just resends the existing `ArtShow`, so `ArtNext` stays
> unbuilt as originally planned. Deliberately kept off the LLM's critical
> path for anything else: curation is capped to 20s (well under
> `dispatch_local_inference`'s shared 150s ceiling) and only runs once per
> search, not on every rotation tick — the agent's existing one-inference-
> per-node semaphore means a real voice/chat command could still queue
> briefly behind it if they land on the same node at the same moment, but
> never for long. Live-tested against the real coordinator + Met API + the
> actually-Ready `llama3.2:1b` on pi1: searching "Leonardo da Vinci" found
> 8 public-domain candidates, got a real LLM-curated order, displayed
> correctly on `pi2`, and both manual `/next` and the 20s auto-advance
> timer advanced the rotation correctly (one `fbi` process maintained
> throughout every transition). Next: wire up the TV.

**Added idea (2026-07-05):** voice/chat browsing — "show me some Monet",
"show the collection in date order" — via a new `art_show` intent tool
copying Phase C's `get_climate` pattern exactly (coordinator answers/acts
from its own catalogue, no node round-trip for the query itself). Needs
per-image metadata (artist/year/movement) captured at ingest, already
planned in the art-pipeline step for this reason. Arguably the strongest
showcase yet for this project's actual differentiator — "talk to your
house, and it never leaves your house."

Blocking on: electrician-designed recess + socket (wall has
tanking/shower on the other side — flagged explicitly in the plan as
needing a qualified Part P registered electrician, not a DIY job).

> **2026-07-08 — hardware decision reversed: the Pi 4 stays permanently,
> no move to a Pi Zero 2 W.** The behind-TV node is becoming the mesh's
> audio-output workhorse too (HDMI audio to the soundbar, see
> `plans/audio-output-integration.md`), a job the Zero can't do. See the
> superseded notes in `plans/frame-tv-art-display.md` and
> `docs/frame-tv-setup.md`.

## Hardware Decoupling — Investigation Queued (2026-07-06)

Jon's question: how best to decouple this project from specific hardware —
e.g. if someone wanted a different Zigbee antenna/dongle. Held for a
dedicated conversation rather than a quick answer, since it's an
architecture question, not a bug/feature ask. Starting point for that
discussion, from a first look at the code:

- **Zigbee already looks mostly decoupled.** `capability-zigbee` only ever
  talks MQTT to zigbee2mqtt (`MQTT_HOST`/`MQTT_PORT`) — it has no idea what
  radio z2m is fronting. z2m itself is the actual hardware-abstraction
  layer here and already supports a wide range of coordinator adapters
  (the SLZB-06 currently in use, ConBee II, Sonoff dongles, TI CC253x/
  CC2652, Silicon Labs EFR32MG21, etc.). Swapping the physical dongle/
  antenna looks like it would only ever touch z2m's own
  `configuration.yaml` (adapter type + serial/network port — the exact
  thing already hand-edited once before, see
  `project_zigbee_bridge_stale_ip` memory) and never `capability-zigbee`
  or coordinator code.
- Worth checking during the real discussion: whether the same
  domain-module boundary (capability crate ↔ `MeshMessage` protocol ↔
  coordinator) holds up as cleanly for the *other* hardware-coupled
  pieces of this project — inference hardware/GPU vendor
  (`capability-llm`/llama.cpp), and REAPER (`capability-reaper`, already
  process-based rather than hardware-based) — or whether "decouple
  hardware" actually means something more specific that didn't come up
  in the first pass.

## Backlog — Lighting subsystem review (2026-07-10) — open items
Genuinely open items from this review, not acted on:

- **`EffectRunner` doesn't check device online/offline status at all**
  (`runner.rs:475-497` builds each tick's bulb list from every device_id
  in the room regardless of `LightStateReport.online`). Confirmed the gap
  is real; didn't verify what actually happens downstream when a command
  goes to an offline device (z2m may silently drop it, may queue it) —
  worth checking live before deciding whether this needs a fix or is
  already harmless in practice.
- **No other effect shares Snake's stale-cache bug** — checked every
  effect file; only `snake.rs` caches an ordering. Aurora and the rest
  recompute fresh each tick. Nothing else needed the same fix.
- **`group_light_command`** (the HTTP endpoint for a raw z2m group, not
  an individual device) still doesn't exclude group members from an
  active effect — scoped out of today's fix since resolving "which room
  a group's members belong to" needs more thought than the single-device
  case. Same bug class, smaller/rarer blast radius.
- **`dispatch_light_command`/`dispatch_tool`/`handle_intent` are growing a
  long, repeated parameter list** (`registry`, `connections`,
  `pending_intents`, `device_states`, `sensor_states`, `dashboard`, ...),
  already needing `#[allow(clippy::too_many_arguments)]`. Bundling these
  into one `IntentExecutionContext`-style struct would be reasonable, but
  premature today — nothing is actually painful yet, just long signatures.
  Trigger for actually doing it: the next time a *new* piece of
  coordinator state needs threading through this same call chain, bundle
  instead of adding parameter #7.

## Backlog — Third-party review sweep (2026-07-09)

Several AI-generated reviews (Copilot, Bing, Gemini) were run against the repo
in one session and checked claim-by-claim against the actual code — most
turned out to already be solved, based on a stale/earlier snapshot, or
architecturally misinformed (see the audit tables in that session's
conversation for the full per-claim verdicts). The items below are the
genuine, still-open findings worth keeping — not urgent enough to act on
immediately, but real. Confirmed-real bugs from the same sweep were fixed
same-day: the audio playback ack-loop (`AudioPlayResult`), `free_port()`
process-ownership check, the dead `scene_load` intent tool now wired to the
real scene system with server-side effect-cancel/reactivate, and the
Frame TV art-display fullscreen/User-Agent fixes.
- **Agent reconnect has no backoff** — fixed 5s retry on every disconnect
  (`agent/src/main.rs`), flagged repeatedly across reviews. Real, but
  changes reconnect/failure semantics on a live mesh — wants deliberate
  design + testing, not a drive-by fix.
- **No cross-field effect-parameter validation** — JSON-Schema validation
  is already centralized and works (`coordinator/src/http/api/effects.rs`),
  but nothing validates relationships between fields (e.g. Snake's `length`
  vs. the room's actual bulb count). Runtime already clamps gracefully
  (no crash), so this is a UX gap, not a reliability one.
- **`EffectRunner` is one task, ticking rooms sequentially** — a slow
  effect's tick can delay other rooms' ticks in the same pass. No
  per-room/per-effect task concurrency exists. Real, but a genuine
  architecture change to a system driving physical lights — needs testing
  against real hardware before changing.
- **Cadence-drift EWMA is observability-only by design** — logs a warning,
  never corrects. Confirmed intentional (module doc says so explicitly),
  not an oversight — revisit only if drift becomes an actual nuisance.
- **Registry is 100% raw `rusqlite` SQL, no typed query builder** — ~30+
  call sites across 5 files. Real, but a large mechanical migration
  (Diesel/sqlx) that needs a deliberate decision, not urgent.
- **`Connections` map is `Mutex<HashMap>`, not `DashMap`** — true, but no
  evidence of actual lock contention at this mesh's scale (a handful of
  nodes). Low value; skip unless real contention shows up.
- **mDNS discovery falls back to `127.0.0.1:9000` silently** when
  `COORDINATOR_IP` is unset and discovery times out. Risky for a
  multi-node cluster in theory, but pi1's own co-located agent currently
  relies on exactly this fallback — changing it needs care so it doesn't
  break that setup.
- **NSSM `STOP_PENDING` on Windows nodes** — documented manual
  `taskkill` workaround only, no watchdog/auto-recovery. Can't build or
  test this without hands-on access to a Windows box.
- **Cloud gateway has no per-provider latency/success-rate metrics** in
  the dashboard — the underlying error handling/fallback logic itself is
  already solid and well-tested (`coordinator/src/cloud.rs`); this would
  just be a nice-to-have observability panel.
- **Coordinator state file has no version field** — low urgency: it's
  parsed key-by-key (`coordinator/src/state.rs`), so unknown/missing keys
  are already forward/backward compatible without one. Would only help
  external tooling detect drift.
- **No health metrics**: queue depth per room, per-command-type latency,
  reconnect attempts per node — none of this instrumentation exists
  today. New work, not a bug fix; worth doing only if a real metrics
  dashboard is wanted.
- **Traceability gap**: light commands sent via the dashboard's direct
  click path don't carry a `request_id` through to a device-side ack,
  unlike the TTS/audio-play/scene-recall/intent paths, which all thread
  one through the `pending_intents` mechanism.
- **Windows Fast Startup cold-boot issue** — still open, separate from
  the fTPM/Pluton crash-storm fix (which resolved a different symptom).
  See `handover.md` item 29.

## Bluetooth Device Management — Per-Device Unpair, Live Status & Room Indicators — follow-ups queued, not yet built
**Follow-ups queued, not yet built:**
- **Full Bluetooth playback control** — volume up/down and mute, not just
  pair/unpair. No wire messages or `bluetooth.rs` functions for this exist
  yet (today's surface is scan/pair/unpair/clear-cache only); would need a
  `BluetoothVolumeRequest`-style message and a `pactl set-sink-volume`/
  `set-sink-mute` call against the resolved sink name.
- **Nudge room assignment right after a successful pair** — the Zigbee
  light-pairing flow already does this (`pairFeedRoomPrompt` in
  `devices.js`: "Paired: X ✓ — assign to room: [dropdown]"). Bluetooth
  pairing has no equivalent, which is exactly why a freshly-paired speaker
  shows no room-card badge and no room in its paired-status label until
  someone separately uses the AV row's "+ add to room" dropdown — not a
  bug, but a real gap in prompting for the second step.
- **Room card mic icon** — the notable-badge row now shows a 🔊 badge for
  an assigned Bluetooth speaker; the voice puck (`/api/av-devices`' `puck`
  entry, room-bound via the `av-room:puck` preference) has no equivalent
  icon yet. Same mechanism, just needs its own badge alongside the speaker
  one in `renderRoomCard`.
- **Coordinator-restart resync** — concrete fix for the documented
  blind-spot (a coordinator restart loses `bluetooth_status` until the next
  actual connect/disconnect): have the coordinator ask every connected
  audio-capable node to report its current paired-device status once, right
  after that node's connection is (re-)established, instead of waiting
  passively for the next change. Small — one new request/response message
  pair, no change to the existing push-on-change semantics.

## Per-Node Comms Peripherals View (Proposed 2026-07-12)

Today the dashboard's Bluetooth/HDMI controls only exist on the AV-device
rows for whichever node has `Feature::Audio` (pi2). The ask: every node's
comms peripherals — Bluetooth, Wi-Fi, HDMI — visible and manageable the same
way, not just the one node that happens to run audio today. Needs a design
pass before building: what's the inventory model for a peripheral that
isn't a playback sink (e.g. a node's Wi-Fi link quality, or an HDMI output
with no audio capability), whether this lives in the existing Devices tab's
AV section or becomes its own per-node panel, and how much of
`buildAvRoomAssignments`/`buildBluetoothScanControls` generalizes versus
needs a parallel per-peripheral-type implementation.

## Text-to-Speech Node Placement (Proposed 2026-07-12)

TTS currently runs wherever it's invoked rather than being steered to the
fastest available node. Move it to run on whichever node has the most
headroom (mirroring `coordinator/src/scheduler.rs`'s existing model-inference
node-selection logic) instead of a fixed/default placement. Needs the actual
current placement mechanism identified first (voice pipeline config,
`capability-voice`) before deciding whether this reuses the scheduler as-is
or needs its own lighter-weight selection.

## Static vs Portable Device Placement Toggle (Proposed 2026-07-12)

Idea: a single-click toggle per device — "static" (fixed in place, can be
positioned on the room floor plan in `layout.js`) vs "portable" (moves
around, coordinates in a room don't make sense to set). Portable devices
would be excluded from the floor-plan placement UI entirely rather than
showing a meaningless fixed position. Needs a `RoomRecord`/device-position
schema decision (new column? reuse of the existing light-position table
with a null position meaning "portable"?) before implementation.

## CI (Proposed 2026-07-12)

Flagged as wanted "when ready" — no scope decided yet (what runs: clippy +
test suite already enforced locally by the pre-commit hook; GitHub Actions
vs. something else; whether it gates pushes to `main` given this is a
solo/direct-to-main repo). Revisit and scope properly when actually picked
up rather than guessing here.

## Puck as Last-Resort Announcement Target (Proposed 2026-07-13)

Triggered by a live incident: `play_announcement` fired three times for a
reminder ("time for Sigmond and the Sea Monsters") and nothing played
anywhere. Root cause — `broadcast_announcement`
(`coordinator/src/audio.rs:464`) fans out only to `nodes_with_feature(Feature::Audio)`,
which today is just pi2; when pi2's paired Bluetooth amp is down (as it was
— `bluetooth: status update ... connected=false` right in that window) there
is no other target, so the announcement is logged as skipped and dropped.
The puck's own "fall back to my speaker" logic (`capability-voice`) only
covers *its own* spoken-reply routing when a live voice session's room-sink
delivery fails — it is not wired into `play_announcement`'s broadcast path
at all, so a coordinator-initiated alert with no originating voice session
currently has nowhere to fall back to.

**Feasibility confirmed live 2026-07-13** (not just protocol theory): ran
the repo's existing `capability-voice --example entities` diagnostic
against the puck (`<puck>:6053`) and it advertises a real `media_player`
entity, independent of any voice-assistant session:

```
ListEntitiesMediaPlayerResponse { object_id: "media_player", key: 2232357057,
  supported_formats: [
    { format: "flac", sample_rate: 48000, channels: 1, purpose: Announcement },
    { format: "flac", sample_rate: 48000, channels: 2, purpose: Default },
  ]
}
MediaPlayerStateResponse { key: 2232357057, state: Idle, volume: 1.0, muted: false }
```

The `purpose: Announcement` format is the same mechanism Home Assistant's
own Voice PE integration uses for proactive announcements/timers — stock
firmware (currently 25.5.2, update to 26.6.0 available), nothing custom to
flash. Push it with a `MediaPlayerCommandRequest { key: 2232357057,
has_media_url: true, media_url: <url>, has_announcement: true, announcement:
true }` over the same ESPHome native-API connection `capability-voice`
already holds open — no wake-word session required, ducks whatever's
playing. The Rust types (`MediaPlayerCommandRequest`,
`ListEntitiesMediaPlayerResponse`, `MediaPlayerStateResponse`) already exist
in the `esphome_client` crate for free — its `build.rs` code-generates every
message with an `option (id)` in the proto file, not a hand-picked subset,
so no crate fork/patch is needed.

Confirmed the puck's ESPHome native API accepts a second concurrent client
without disturbing the production connection (pi1's own heartbeats to the
coordinator were unaffected during the live test) — worth re-checking under
real concurrent use, but not a blocker.

Open item before building: the negotiated announcement format is mono FLAC
— check whether the puck transcodes an arbitrary media URL server-side or
whether the existing Piper TTS output (WAV, per Phase 1 of
`plans/audio-output-integration.md`) needs converting first.

This is exactly the "room speaker → puck → nothing" fallback chain
`plans/audio-output-integration.md`'s Phase 6 already named as the target
design (see that file for the full room-routing policy) — this entry
narrows it to the specific, now-proven mechanism for reaching the puck
out-of-band. Implementation: extend `broadcast_announcement`/
`handle_audio_announce` (`coordinator/src/audio.rs`) to add the puck as a
final target when every other `Feature::Audio` node fails, using this
`MediaPlayerCommandRequest` path.

## Home Network Throughput Investigation (Proposed 2026-07-15)

The mesh router measures roughly half the throughput of the ISP router that
feeds it. Chase where the loss actually is instead of guessing. Known
facts from a separate domain DNS work (2026-07-15): the mesh router's WAN sits
on `<node-old>` behind the ISP router box at `<isp-router>` — so the LAN is
**double-NATted** — and the mesh router's upstream DNS now points at 1.1.1.1
(changed from `<isp-router>` after the ISP router box served stale records; DNS
is latency-only, irrelevant to throughput).

Test plan, in order:
1. **Wired test through the mesh router** vs the same test wired to the ISP router
   box — separates routing loss from Wi-Fi limits. (Prime suspect: the
   halving was measured over Wi-Fi on a 2×2 Wi-Fi 6 client whose
   real-world ceiling is ~500–700 Mbps regardless of router.)
2. **Check the mesh router WAN link rate** and the cable between the ISP router and the mesh router
   (should negotiate ≥1 Gbps).
3. **Audit the mesh router features** that tax the routing path: QoS, traffic
   meter, Netgear Armor.
4. If double NAT turns out to be implicated, consider **the mesh router AP mode**
   (the ISP router box routes, the mesh router does Wi-Fi only) — but this re-shuffles the
   LAN, and mesh device IPs are baked into configs (pi1 `<pi1>`,
   SLZB-06 `<slzb-06>` per its z2m serial.port fix), so it needs a
   planned migration, not a casual toggle.

Related gotcha (2026-07-16): the laptop turned up with DNS from the ISP router
box (`<isp-router>`) — either it hops between the ISP router's own Wi-Fi and
the mesh router's, or the adapter's static DNS got reset. Devices on the ISP router
side get its (stale-prone) DNS cache. While investigating, set the ISP router
box's DNS forwarders to 1.1.1.1 as well, and check which SSIDs the
laptop auto-joins.

## Phase 12 — Distributed Execution

- Multi-node inference
- Pipeline parallelism
- Tensor parallelism

## Phase 13 — Auto-scaling

- Dynamic node joining
- Cloud integration
