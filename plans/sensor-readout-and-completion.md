# Sensor readout + sensors-arc completion

Written 2026-07-04. Companion to `plans/multi-domain-home.md`. Phase B is
software-complete and pairing/removal works from the app (commits `b0cf895`,
`3906936`, `a3ff8ac`, `20e526b` — wire v7). **The one visible gap:** nothing
in the UI renders sensor readings. The backend pipeline is finished — pair a
temp sensor today and it joins, classifies, reports, persists, and serves at
`GET /api/sensors` + `SensorUpdate` WS events — but `dashboard.js` has no
`SensorUpdate` handler, so readings appear nowhere on screen.

This plan: (1) the minimal interim readout to close that gap, (2) everything
outstanding to call the sensors arc done.

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

1. **This readout** (Part 1). After it: every Phase B deliverable has a
   visible surface.

2. **Deploy** (user runs manually; always list these after changes):
   `just deploy-coordinator pi1` **first**, then `just deploy-node pi1`.
   Order matters: the old coordinator binary hard-errors on unknown
   `MeshMessage` variants (wire v7) and would drop the agent connection.

3. **Hardware live gate** (blocks on sensor delivery — Aqara temp/humidity
   + motion were "to be ordered", check with Jon). Whole flow from the
   phone, no SSH:
   - Lighting tab → **Pair device** → hold the sensor's pair button →
     watch the join feed ("Paired: WSDCGQ11LM ✓").
   - Readings appear in the new sensor cards within one report interval.
   - `curl .../api/sensors?token=…` shows temperature/humidity/battery.
   - Restart the coordinator → readings survive (sensor_states table).
   - Pull the sensor's battery → after z2m's passive timeout (~25 h,
     documented in `docs/pi1-lighting-setup.md` §9) the card dims to
     offline with readings intact. Don't wait a day for this one — verify
     the offline path by temporarily lowering z2m's
     `availability.passive.timeout` instead.
   - On pass: mark Phase B **complete** in `docs/roadmap.md` (11.8 section)
     and update the focus memory.

4. **Phase C — the differentiator** (software-only, can start before
   hardware arrives; see `plans/multi-domain-home.md` Phase C):
   - `"sensors"` arm in the intent router's tool schemas: `get_climate
     { room? }` answered **from the coordinator's sensor snapshot** — no
     node round-trip (sensors are read-only push).
   - `build_sensor_context` injecting per-room lines ("Office: 21.4°C,
     47% RH, motion 3m ago") beside the existing device context in the
     intent system prompt.
   - Multi-command chat (old chat-roadmap item 7): "turn off the kitchen
     lights and tell me the bedroom temperature" — multiple tool calls per
     turn.
   - Gate: `just intent "what temperature is the office?"` answers from
     real sensor data.

5. **Phase D — not sensors work, but supersedes Part 1's interim UI**:
   Lighting tab → Home; room cards render mixed-domain members (lights get
   controls, sensors get the readout strip); single Devices tab absorbs
   pairing + inventory. When D lands, delete Part 1's sensor section from
   the Lighting panel in the same commit (house rule: no legacy mirrors).

## House rules for the implementing session

- Commit directly to main, **after Jon approves** (he reviews diffs himself
  — summarize in words, never paste diffs). No Co-Authored-By; never mention
  AI tools in commits/docs.
- Pre-commit hook runs clippy `-D warnings` + full test suite on the whole
  tree and re-stages only already-staged files.
- After any change, list the exact `just deploy-*` recipes needed.
- Pasted external code reviews get an apply/defer/skip audit table; every
  point checked in code before closing; breadcrumb every deferral.
