# Phase D — Live Sun Arc + Room Compass Orientation

## Context

Phases A and B placed bulbs and openings on the layout canvas and wired them into
the `SpatialEngine` solar sweep. The engine already reads `orientation_degrees` from
the `rooms` table to rotate the effective solar azimuth per room, but there is no UI
to set it — users have no way to tell the system which way their room faces.

Phase D closes the solar jigsaw:

1. **Compass dial** — a draggable SVG rose in the layout canvas corner that sets
   `orientation_degrees`. The sun arc rotates in real time as the user drags, giving
   instant visual feedback.
2. **Phone compass calibration** — a "📱 Use phone compass" button that reads
   `deviceorientationabsolute` on mobile and snaps the dial to the real magnetic
   heading. One-tap calibration, zero manual work.
3. **Sun arc overlay** — an arc drawn around the canvas perimeter tracing today's
   solar path (sunrise → sunset), with a pulsing dot at the current position.
   Driven by the existing `SolarUpdate` WS event; no new backend needed.
4. **Time scrubber** — a slider below the canvas that lets the user preview predicted
   light levels at any hour. Client-side JS trig; releasing snaps back to live mode.
5. **Optional sun calibration** — "☀ Calibrate from sun": user taps the wall the sun
   is currently hitting; system back-calculates orientation. Only shown when
   `elevation > 5°`.

**Source-of-truth rule:** every calibration mode (drag, phone, sun) writes through the
dial and fires the same `PATCH /api/rooms/{id}/orientation` — no competing sources.

---

## 1. Backend — `PATCH /api/rooms/{id}/orientation`

The `rooms` table already has `orientation_degrees REAL`. We only need one new endpoint.

### registry.rs

New method (alongside the existing `set_room_solar`):

```rust
pub fn set_room_orientation(&mut self, room_id: &str, degrees: f32) {
    let clamped = degrees.rem_euclid(360.0);
    if let Err(e) = self.conn.execute(
        "UPDATE rooms SET orientation_degrees = ?1 WHERE id = ?2",
        params![clamped, room_id],
    ) {
        warn!(error = %e, "set_room_orientation failed");
    }
}
```

### api.rs

New body struct and handler:

```rust
#[derive(Deserialize)]
pub struct SetOrientationBody {
    orientation_degrees: f32,
}

pub async fn set_room_orientation(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetOrientationBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut reg = registry.lock().unwrap();
    if !reg.room_exists(&room_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    reg.set_room_orientation(&room_id, body.orientation_degrees);
    drop(reg);
    // Push updated rooms so WS clients get the new orientation immediately.
    state.push_rooms_update(rooms_from_registry(&registry));
    // Wake SpatialEngine so the new orientation takes effect within seconds.
    state.solar_sweep_notify.notify_one();
    StatusCode::NO_CONTENT.into_response()
}
```

### mod.rs

```rust
.route("/api/rooms/{id}/orientation", axum::routing::patch(api::set_room_orientation))
```

### SpatialEngine

No changes — it already reads `r.orientation_degrees` on every sweep.

---

## 2. layout.js — Compass dial

### New state

```js
let compassDeg = 0;          // current orientation_degrees for this room
let compassDragging = false;
let compassOrientTimer = null;
```

### `openLayout(roomId)` additions

After fetching openings, also read `orientation_degrees` from the room data already
available in `roomsData`:

```js
compassDeg = roomsData.find(r => r.id === roomId)?.orientation_degrees ?? 0;
renderCompass();
```

### `renderCompass()` — SVG group `#lc-compass`

Place in the top-right corner of the canvas (SVG coords ~870,20 → 980,130).

```
<g id="lc-compass" transform="translate(925, 75)">
  <!-- outer ring -->
  <circle r="52" fill="rgba(0,0,0,0.55)" stroke="#555" stroke-width="1.5"/>
  <!-- tick marks at 0/90/180/270 -->
  <line ... />   <!-- N tick, longer -->
  <!-- cardinal labels, fixed (not rotated with dial) -->
  <text y="-36" text-anchor="middle" class="lc-compass-label">N</text>
  <text y="41"  text-anchor="middle" class="lc-compass-label">S</text>
  <text x="38"  text-anchor="middle" class="lc-compass-label">E</text>
  <text x="-38" text-anchor="middle" class="lc-compass-label">W</text>
  <!-- rotating dial group — transform updated on drag -->
  <g id="lc-compass-dial" transform="rotate(0)">
    <polygon points="0,-40 5,-20 -5,-20" fill="#e8c84a"/>  <!-- N pointer -->
    <polygon points="0,40 5,20 -5,20"  fill="#888"/>        <!-- S pointer -->
    <circle r="5" fill="#fff"/>
  </g>
  <!-- drag handle — invisible larger hit area -->
  <circle r="52" fill="transparent" id="lc-compass-handle" style="cursor:grab"/>
</g>
```

**N is fixed at the top of the dial group.** Rotating the dial by `compassDeg` degrees
means "North in this room is `compassDeg`° clockwise from the top of the canvas".

### Drag interaction

```js
function wireCompass() {
    const handle = document.getElementById('lc-compass-handle');
    const dial   = document.getElementById('lc-compass-dial');
    const cx = 925, cy = 75; // SVG centre of compass in canvas coords

    handle.addEventListener('pointerdown', e => {
        compassDragging = true;
        handle.setPointerCapture(e.pointerId);
        handle.style.cursor = 'grabbing';
    });

    handle.addEventListener('pointermove', e => {
        if (!compassDragging) return;
        const pt = svgPoint(e);  // existing helper that maps to SVG coords
        const angle = Math.atan2(pt.y - cy, pt.x - cx) * 180 / Math.PI + 90;
        compassDeg = ((angle % 360) + 360) % 360;
        dial.setAttribute('transform', `rotate(${compassDeg})`);
        redrawSunArc();       // sun arc rotates in real time
        redrawLightCones();   // light cones also update
    });

    handle.addEventListener('pointerup', () => {
        compassDragging = false;
        handle.style.cursor = 'grab';
        clearTimeout(compassOrientTimer);
        compassOrientTimer = setTimeout(() => patchOrientation(currentRoomId, compassDeg), 400);
    });
}

async function patchOrientation(roomId, deg) {
    await fetch(`/api/rooms/${roomId}/orientation?token=${TOKEN}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ orientation_degrees: deg }),
    });
}
```

---

## 3. layout.js — Phone compass button

Add to the canvas toolbar (above or beside the existing undo/redo buttons):

```html
<button id="lc-phone-compass" title="Use phone compass">📱</button>
```

```js
document.getElementById('lc-phone-compass').addEventListener('click', async () => {
    // iOS requires permission inside a user gesture
    if (typeof DeviceOrientationEvent !== 'undefined' &&
        typeof DeviceOrientationEvent.requestPermission === 'function') {
        const perm = await DeviceOrientationEvent.requestPermission();
        if (perm !== 'granted') return;
    }

    function onOrientation(e) {
        if (e.alpha == null) return;
        window.removeEventListener('deviceorientationabsolute', onOrientation);
        compassDeg = ((e.alpha % 360) + 360) % 360;
        document.getElementById('lc-compass-dial')
            .setAttribute('transform', `rotate(${compassDeg})`);
        redrawSunArc();
        patchOrientation(currentRoomId, compassDeg);
    }

    window.addEventListener('deviceorientationabsolute', onOrientation);
    // Fallback: hide the button on desktop where event never fires
    setTimeout(() => window.removeEventListener('deviceorientationabsolute', onOrientation), 5000);
});

// Show button only if API may be available (heuristic)
if (!('ondeviceorientationabsolute' in window)) {
    document.getElementById('lc-phone-compass').style.display = 'none';
}
```

---

## 4. layout.js — Sun arc overlay

### New SVG layer

Insert above `#lc-openings` (behind everything):

```html
<g id="lc-sun-arc"/>
```

### Solar position JS (client-side, no backend)

Approximate solar azimuth/elevation at a given UTC time using the same algorithm
family as `spa`. A compact JS implementation (~40 lines) is sufficient for the arc
preview — it does not need to be as precise as the Rust `spa` crate:

```js
function solarPosition(dateUtc, lat, lon) {
    // Julian date → hour angle → declination → azimuth/elevation
    // Returns { azimuth: 0-360, elevation: -90..90 }
    // See: NOAA solar calculator formulas (public domain)
}
```

Lat/lon are read from the existing `MESH_LATITUDE`/`MESH_LONGITUDE` env vars.
Expose them as JS constants injected by the coordinator into the HTML template
(a single `<script>` block: `const MESH_LAT = {{lat}}; const MESH_LON = {{lon}};`).

### `redrawSunArc(azimuth, elevation)`

Called on every `SolarUpdate` WS event and on compass drag.

```
- Compute sunrise and sunset azimuth for today using solarPosition() at midnight + sweep
- Map each azimuth angle to a point on the canvas perimeter (outside the 1000×1000 area,
  at radius ~540 from centre 500,500)
- Draw a <path> arc segment between sunrise and sunset points
- Place a pulsing ☀ circle at the current azimuth point
- Rotate all points by -compassDeg so the arc is relative to room orientation
```

Arc coordinate mapping (azimuth → canvas perimeter):
```js
function azimuthToPerimeterPoint(az, orientDeg) {
    const adjusted = ((az - orientDeg) % 360 + 360) % 360;
    const r = 540; // outside 1000×1000 canvas, so arc hugs the edge
    const rad = (adjusted - 90) * Math.PI / 180;
    return { x: 500 + r * Math.cos(rad), y: 500 + r * Math.sin(rad) };
}
```

Arc styling:
- Path stroke: `rgba(255, 200, 50, 0.6)`, stroke-width 4, no fill
- Current-position dot: 14px radius, `#FFD700`, CSS `@keyframes pulse` (scale 1→1.3→1)
- Night segment (elevation ≤ 0): dashed stroke, reduced opacity

---

## 5. layout.js — Time scrubber

```html
<div id="lc-scrubber-bar">
  <span id="lc-scrubber-time">Now</span>
  <input type="range" id="lc-scrubber" min="0" max="1440" step="5" value="-1"/>
  <button id="lc-scrubber-live">↺ Live</button>
</div>
```

- Value −1 (or sentinel) = live mode; display shows "Now"
- Dragging: compute `solarPosition` at the selected minute of today → call
  `previewSolarState(azimuth, elevation)` which animates bulb brightness/CT on
  the canvas icons (does **not** send any light commands to nodes)
- `previewSolarState` reuses the existing `calculate_solar_state` logic,
  translated to JS (same piecewise linear formula)
- Releasing / clicking ↺: snap back to live mode, re-apply last real `SolarUpdate`
- Scrubber hidden when layout view is closed

```js
const scrubber = document.getElementById('lc-scrubber');
scrubber.addEventListener('input', () => {
    const mins = parseInt(scrubber.value);
    if (mins < 0) return;
    const date = new Date();
    date.setHours(0, mins, 0, 0);
    const { azimuth, elevation } = solarPosition(date, MESH_LAT, MESH_LON);
    document.getElementById('lc-scrubber-time').textContent =
        `${String(Math.floor(mins/60)).padStart(2,'0')}:${String(mins%60).padStart(2,'0')}`;
    previewSolarState(azimuth, elevation);
});
document.getElementById('lc-scrubber-live').addEventListener('click', () => {
    scrubber.value = -1;
    document.getElementById('lc-scrubber-time').textContent = 'Now';
    redrawSunArc(lastSolar.azimuth, lastSolar.elevation);
    previewSolarState(lastSolar.azimuth, lastSolar.elevation);
});
```

---

## 6. layout.js — Optional sun calibration

Show a "☀ Calibrate from sun" button only when `lastSolar.elevation > 5`:

```js
function updateSunCalibButton() {
    document.getElementById('lc-sun-calib').style.display =
        lastSolar.elevation > 5 ? '' : 'none';
}
```

On click, enter calibration mode:
- Highlight the four wall edges (N/S/E/W labels appear on the canvas walls)
- User clicks a wall → `orientation = (lastSolar.azimuth - wallFacing + 360) % 360`
  where `wallFacing` is 0/90/180/270 for N/E/S/W
- Apply to dial + PATCH; exit calibration mode

---

## 7. rooms.js — Forward orientation from WS

When `RoomUpdate` arrives via WS, update `compassDeg` if the layout view is open for
that room:

```js
// in the existing RoomUpdate handler:
if (layoutOpenRoomId && msg.type === 'RoomsUpdate') {
    const room = msg.rooms.find(r => r.id === layoutOpenRoomId);
    if (room) layout.notifyOrientationUpdate(room.orientation_degrees);
}
```

Export from `layout.js`:

```js
export function notifyOrientationUpdate(deg) {
    compassDeg = deg;
    document.getElementById('lc-compass-dial')
        ?.setAttribute('transform', `rotate(${deg})`);
    redrawSunArc(lastSolar.azimuth, lastSolar.elevation);
}
```

---

## 8. Lat/lon injection into HTML

In `coordinator/src/http/mod.rs` (or the HTML template), inject lat/lon into the
served page so the JS solar calculator can use them without an extra API call:

```html
<script>
  const MESH_LAT = {{ lat }};
  const MESH_LON = {{ lon }};
</script>
```

The coordinator already reads `MESH_LATITUDE`/`MESH_LONGITUDE` env vars in
`SpatialEngine::new()`. Extract the parsed values into `DashboardState` (two new
`f64` fields) and use them in the HTML template.

Alternatively: a trivial `GET /api/solar/config` endpoint returning
`{ lat, lon }` — avoids template injection complexity and is easier to test.

---

## 9. style.css additions

```css
/* Compass */
#lc-compass { user-select: none; }
.lc-compass-label { fill: #ccc; font-size: 28px; font-weight: 600; }
#lc-compass-handle:active { cursor: grabbing; }

/* Sun arc */
#lc-sun-arc path { pointer-events: none; }
.lc-sun-dot { animation: lc-pulse 1.8s ease-in-out infinite; }
@keyframes lc-pulse { 0%,100% { r: 7; opacity: 1; } 50% { r: 11; opacity: 0.7; } }

/* Scrubber bar */
#lc-scrubber-bar {
    display: flex; align-items: center; gap: 8px;
    padding: 6px 12px; background: var(--surface2);
    border-top: 1px solid var(--border);
}
#lc-scrubber { flex: 1; accent-color: var(--amber); }
#lc-scrubber-live { font-size: 12px; padding: 2px 8px; }

/* Phone compass button */
#lc-phone-compass { font-size: 18px; background: none; border: none; cursor: pointer; }

/* Sun calibration */
#lc-sun-calib { font-size: 12px; padding: 2px 8px; }
.lc-wall-calib-target { stroke: var(--amber); stroke-width: 6; cursor: pointer; opacity: 0.6; }
.lc-wall-calib-target:hover { opacity: 1; }
```

---

## 10. SVG layer order (updated)

```html
<g id="lc-sun-arc"/>     <!-- arc + pulsing dot — behind everything -->
<g id="lc-openings"/>    <!-- window/door rects + handles -->
<g id="lc-shadow"/>      <!-- light cone polygons -->
<g id="lc-bulbs"/>       <!-- bulb icons -->
<g id="lc-preview"/>     <!-- drag ghost -->
<g id="lc-compass"/>     <!-- compass rose — on top, non-blocking -->
```

---

## Critical path

```
registry.rs: set_room_orientation()
        │
        ├──► api.rs: PATCH /api/rooms/{id}/orientation handler
        │           │
        │           └──► mod.rs: register route
        │
        └──► layout.js
              ├── Compass dial (renderCompass, wireCompass, patchOrientation)
              ├── Phone compass button
              ├── Sun arc overlay (redrawSunArc, solarPosition JS)
              ├── Time scrubber
              └── Sun calibration (optional, gated on elevation)
                          │
                          └──► rooms.js: notifyOrientationUpdate on RoomsUpdate WS
```

---

## Verification

1. `cargo test -p coordinator` — all existing tests pass; add:
   - `set_room_orientation_persists_flag` (registry)
   - `patch_orientation_returns_401_for_wrong_token` (api)
   - `patch_orientation_returns_404_for_unknown_room` (api)
   - `patch_orientation_clamps_to_360` (api — send 400°, expect 40° stored)
2. Open layout view → compass dial visible in top-right corner
3. Drag dial → sun arc and light cones rotate in real time; release → PATCH fires; reload → angle persists
4. On mobile (HTTPS): tap 📱 button → permission dialog → dial snaps to real heading → arc updates
5. Scrubber: drag to 12:00 → bulb icons show midday brightness/CT preview; release → snap to live
6. Sun calibration: visible when elevation > 5°; tap a wall → orientation back-calculated correctly
7. Enable solar on a room → change orientation → SpatialEngine sweep log shows updated effective azimuth
