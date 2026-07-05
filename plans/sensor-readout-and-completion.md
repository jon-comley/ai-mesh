# Sensor readout + sensors-arc completion

Written 2026-07-04. Companion to `plans/multi-domain-home.md`. Phase B is
software-complete and pairing/removal works from the app (commits `b0cf895`,
`3906936`, `a3ff8ac`, `20e526b` — wire v7).

**Part 1 shipped (2026-07-04, commit `9749451`, wire v8):** the Lighting
panel now renders read-only sensor cards — the one visible gap (no
`SensorUpdate` WS handler existed) is closed. Hardware also arrived: SONOFF
SNZB-02P ×4 (temp/humidity) + SNZB-03P R2 ×3 (motion). Checking their exact
z2m `exposes` before pairing turned up a real gap beyond the UI: the R2
motion sensor reports a *numeric lux* `illuminance` that `SensorReport` had
no field for — added as part of Part 1 (see the addendum below). Everything
that remains (Part 2) needs the live hardware and is unblocked.

---

## Part 1 — Minimal sensor readout (build this)

Read-only sensor cards on the **Lighting panel**, rendered after the light
cards. Deliberately interim: Phase D's Home tab (room cards with mixed-domain
widgets) supersedes this; keep it small and don't build abstractions for it.

### Wire format (already shipping — do not change)

WS event, also replayed on every connect from the coordinator's
`sensor_snapshot` (ws.rs already does this — the frontend handler is the only
missing piece):

```json
{ "type": "SensorUpdate", "sensors": [ {
    "node_id": "pi1", "device_id": "office_climate",
    "temperature": 21.4, "humidity": 47.0, "battery": 98,
    "occupancy": true, "contact": false, "online": true } ] }
```

All measurement fields optional (absent = never reported — devices carry
different subsets). `online: false` means z2m marked it unavailable; keep
showing the last readings, dimmed, per the merge semantics in
`DashboardState::push_sensor_update`.

### Changes (3 files, frontend only — no Rust)

1. **`coordinator/src/http/static/dashboard.js`** — one line in the
   `handlers` map, next to `LightingUpdate`:
   `SensorUpdate: evt => lighting.handleSensorUpdate(evt),`

2. **`coordinator/src/http/static/lighting.js`** — mirror the existing
   lighting pattern exactly (`devicesMap` / `handleLightingUpdate` /
   `render()` at the top of the file):
   - module-level `sensorsMap = new Map()`;
   - `export function handleSensorUpdate(evt)` — repopulate `sensorsMap`
     from `evt.sensors`, then re-render the sensor section only (don't
     re-render light cards — their `render()` bails during drag; give
     sensors their own container so the two never interfere);
   - render one card per sensor into a `#sensor-list` div: name via the
     existing `formatDeviceName()`, node badge (`light-node-badge` class),
     then a compact readout line showing only the fields present:
     `21.4°C · 47% RH · 🔋98%`, plus `Motion` / `Clear` when `occupancy`
     is present and `Open` / `Closed` when `contact` is present (use the
     existing `.badge badge-green` / `.badge badge-muted` classes);
   - `online: false` → add a muted/dimmed class + "offline" badge, keep
     last readings visible (do NOT blank them);
   - no controls, no drag, no delete on these cards (management stays in
     the existing flows; Phase D owns the rest).

3. **`coordinator/src/http/static/index.html`** — add
   `<div id="sensor-list" hidden></div>` between `#pair-feed` and
   `#lighting-list`; unhide when `sensorsMap` is non-empty. A small
   `<h3>Sensors</h3>` header inside it is fine.

   **`style.css`** — reuse `.light-card`; add only a `.sensor-card`
   modifier (no hover affordances) and a `.sensor-readout` line style.
   Follow the `.pair-*` block added at the same spot for conventions.

### Addendum — illuminance (folded into Part 1, wire v8)

Real hardware exposed a gap the original wire shape didn't cover. The
**SNZB-03P R2** (the model actually purchased) reports ambient light as a
numeric `illuminance` (lux) — added to `SensorReport` as
`illuminance: Option<f32>` and to the readout as `💡{lx} lx`. **Do not
confuse with the base SNZB-03P** (no "R2"): it instead exposes a `dim`/
`bright` enum on a differently-named property (`illumination`), which is
not parsed — if that model ever gets paired, its light-level reading is
silently dropped (same "unhandled sensor field" behaviour as any other
unmapped property; not a crash, just missing data).

Three things forget-easily-caused bugs live here — check all three when
touching `SensorReport` again:
1. The zigbee parser (`parse_sensor_report` in `capabilities/zigbee/src/client.rs`).
2. The coordinator's field-wise merge (`push_sensor_update` in
   `coordinator/src/http/state.rs`) — the easy one to miss, since a
   compile error won't catch a merge arm that quietly drops a field the
   struct literal already has.
3. Every other `SensorReport { ... }` literal in the codebase (tests,
   `capabilities/sensors/src/lib.rs`'s availability-flip constructor) —
   the compiler catches these as missing-field errors, so they're the
   safe ones.

### Verification (no browser on WSL2 — house constraint)

- `cargo build -p coordinator` (static assets are `include_str!` — JS/HTML
  changes need a rebuild to serve).
- Boot locally: `MESH_INSECURE=1 MESH_HTTP_PORT=19301 ./target/debug/coordinator`
  **from a scratch directory** (it opens `ai_mesh.db` relative to CWD and
  frees ports 9000/9001 by killing occupants — never run it from the repo
  root or with default ports on the dev box).
- `curl /static/lighting.js` → confirm new code serves; `curl /api/sensors`
  → 200. Real WS/visual check happens on the phone (pi1:9001) after deploy.
- There are no JS tests in this repo; the Rust suite must stay green:
  `cargo clippy --workspace --all-features --all-targets` +
  `cargo test --workspace --all-features` (pre-commit runs both anyway).

---

## Part 2 — Outstanding for sensors-arc completion (in order)

1. ~~This readout~~ **Done** (Part 1, commit `9749451`, wire v8). Every
   Phase B deliverable now has a visible surface.

2. **Deploy** (user runs manually; always list these after changes):
   `just deploy-coordinator pi1` **first**, then `just deploy-node pi1`.
   Order matters: the old coordinator binary hard-errors on unknown
   `MeshMessage` variants and would drop the agent connection. This is now
   two wire bumps behind (v7 pairing, v8 illuminance) — one deploy covers
   both, no need to do it twice.

3. **Hardware live gate** — hardware is in hand: SNZB-02P ×4 (temp/humidity),
   SNZB-03P R2 ×3 (motion + lux). Whole flow from the phone, no SSH:
   - ~~Pair all 7~~ **Done (2026-07-05).** All 4× SNZB-02P + 3× SNZB-03P R2
     paired and actively reporting via the Devices tab / Home tab room
     strips. Two Hue Tap Dial-style switches paired alongside them,
     confirming the newer `Switch`/`SwitchAction` work (see the roadmap's
     2026-07-05 entries) against real hardware too — presses/rotation flash
     as expected; no action bound to them yet (out of scope here).
   - `curl .../api/sensors?token=…` shows the same fields — not yet
     explicitly checked (dashboard readings are visibly correct, but the
     raw endpoint wasn't curled separately).
   - Restart the coordinator → readings survive (sensor_states table) —
     **not yet exercised.**
   - Pull a sensor's battery → after z2m's passive timeout (~25 h,
     documented in `docs/pi1-lighting-setup.md` §9) the card dims to
     offline with readings intact. Don't wait a day for this one — verify
     the offline path by temporarily lowering z2m's
     `availability.passive.timeout` instead. **Not yet exercised.**
   - Delete one from the dashboard and confirm it actually leaves the
     Zigbee network (re-pairing is required to bring it back — not just a
     vanished registry row). The unpair-on-delete path only has a mocked
     connection test so far (`delete_device_requests_network_removal`);
     this is its first exercise against a real bridge. **Not yet exercised.**
   - On pass (all sub-items above, not just pairing): mark Phase B
     **complete** in `docs/roadmap.md` (11.8 section) and update the focus
     memory.

4. **Phase C — the differentiator** — code shipped 2026-07-04
   (`coordinator/src/intent.rs`, no wire bump, 824 tests), **live-verified
   against a real LLM 2026-07-05** (see the gate below):
   - ~~`"sensors"` arm in the intent router's tool schemas~~ **Done**:
     `get_climate { room? }` answered from the coordinator's sensor
     snapshot — no node round-trip.
   - ~~`build_sensor_context`~~ **Done**, per-device (not per-room) lines —
     a room commonly has more than one sensor, so per-device lets the model
     see which reading came from which unit; room tag is inline, same shape
     as `build_device_context`.
   - ~~Multi-command chat~~ **Done via the existing array mechanism**:
     `try_parse_tool_calls` already supported JSON arrays (compound light
     commands used this); the system prompt now tells the model a mixed
     action+climate turn ("turn off the lights and tell me the bedroom
     temperature") must express the climate part as a `get_climate` call
     inside that array, since one reply can't mix free text with JSON.
   - **Bonus fix while in the area**: `lighting.js`'s sensor readout had
     `contact` backwards — z2m's `contact: true` means *closed*, the card
     showed "Open". No contact sensors were in the hardware batch, so this
     was never exercised live; caught while writing the same formatting
     logic in Rust and checked against z2m's docs.
   - **Gate** ✓ **passed (2026-07-05)**: `just intent "what temperature is
     the kitchen?"` answers from real sensor data. First run surfaced two
     real bugs, both fixed same day (commit `2c067cb`) and confirmed on
     retest: raw device ids shown instead of friendly names (sensors only —
     see `docs/roadmap.md`'s 2026-07-05 Phase C entry for why lights were
     deliberately left alone), and the model calling the tool with
     `{"target": ...}` instead of the schema's `{"room": ...}`, silently
     returning every sensor in the house — `dispatch_get_climate` now
     accepts `target` as an alias. A mixed action+climate turn in one
     reply is not yet separately confirmed live.
   - **Deferred (not this slice):** no recency/staleness timestamp on
     readings. `offline: true` covers the dead-sensor case, but a live
     sensor whose last real update was a while ago (illuminance only
     updates on motion events, so a quiet room's lux reading can be stale
     while `online` stays true) has no "as of when" signal. Real gap, but
     not small — same shape as the illuminance addition (another wire bump,
     threaded through parser/merge/registry/UI). Worth doing once the live
     gate shows it's actually confusing, not speculatively before.

5. ~~Phase D~~ **Done (2026-07-04)** — see `plans/multi-domain-home.md`'s
   Phase D entry for the full account. Part 1's interim `#sensor-section`
   flat list (and the whole `lighting.js` file it lived in) was deleted in
   the same commit, superseded by per-room sensor strips on the new Home
   tab and the Sensors group in the new Devices tab.

## House rules for the implementing session

- Commit directly to main, **after Jon approves** (he reviews diffs himself
  — summarize in words, never paste diffs). No Co-Authored-By; never mention
  AI tools in commits/docs.
- Pre-commit hook runs clippy `-D warnings` + full test suite on the whole
  tree and re-stages only already-staged files.
- After any change, list the exact `just deploy-*` recipes needed.
- Pasted external code reviews get an apply/defer/skip audit table; every
  point checked in code before closing; breadcrumb every deferral.
