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
2. **Phone compass calibration** — a "📱 Use phone compass" button that reads the
   device magnetometer on mobile and drives the dial live. User locks the heading when
   it settles; a rolling average smooths sensor noise.
3. **Sun arc overlay** — an arc drawn around the canvas perimeter tracing today's
   solar path (sunrise → sunset), with a pulsing dot at the current position.
   Driven by the existing `SolarUpdate` WS event; no new backend needed.
4. **Time scrubber** — a slider below the canvas that lets the user preview predicted
   light levels at any hour. Client-side JS trig; releasing snaps back to live mode.
   A "Simulation Mode" visual tint signals the canvas is not live.
5. **Optional sun calibration** — "☀ Calibrate from sun": user taps the wall the sun
   is currently hitting; system back-calculates orientation. Only shown when
   `elevation > 5°`.

**Source-of-truth rule:** every calibration mode (drag, phone, sun) writes through the
dial and fires the same `PATCH /api/rooms/{id}/orientation` — no competing sources.

**Canvas coordinate convention:** the top of the layout canvas is the structural North
of the building (i.e. the wall the user decided to draw at the top). `orientation_degrees`
maps that structural North to real-world magnetic North. If the building sits at 15° off
true North, the user sets the dial to 15° — the solar sweep accounts for the rest.

---

## 1. Backend

### `GET /api/solar/config`

Preferred over HTML template injection — keeps the HTML static and the endpoint
independently testable with `curl`.

```rust
#[derive(serde::Serialize)]
pub struct SolarConfig {
    lat: f64,
    lon: f64,
}

pub async fn solar_config(
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    Json(SolarConfig { lat: state.lat, lon: state.lon })
}
```

Add `lat: f64` and `lon: f64` to `DashboardState` (parsed from env vars at startup,
same values `SpatialEngine` already reads).

Register: `.route("/api/solar/config", get(api::solar_config))` — no auth required
(lat/lon are not sensitive).

### `PATCH /api/rooms/{id}/orientation`

The `rooms` table already has `orientation_degrees REAL`. No schema changes.

**registry.rs** — new method:

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

**api.rs** — validate before clamping to prevent NaN propagating into SQLite and then
into the SpatialEngine trig (NaN from `rem_euclid(NaN)` silently poisons every sweep):

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
    if !body.orientation_degrees.is_finite() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut reg = registry.lock().unwrap();
    if !reg.room_exists(&room_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    reg.set_room_orientation(&room_id, body.orientation_degrees);
    drop(reg);
    state.push_rooms_update(rooms_from_registry(&registry));
    state.solar_sweep_notify.notify_one();
    StatusCode::NO_CONTENT.into_response()
}
```

**mod.rs:**
```rust
.route("/api/rooms/{id}/orientation", axum::routing::patch(api::set_room_orientation))
.route("/api/solar/config", get(api::solar_config))
```

**SpatialEngine:** no changes — already reads `r.orientation_degrees` on every sweep.

---

## 2. layout.js — Compass dial

### New state

```js
let compassDeg = 0;
let compassDragging = false;
let compassOrientTimer = null;
```

### `openLayout(roomId)` additions

Fetch lat/lon from the new config endpoint (once, cached in module scope):

```js
if (MESH_LAT == null) {
    const cfg = await fetch('/api/solar/config').then(r => r.json());
    MESH_LAT = cfg.lat; MESH_LON = cfg.lon;
}
compassDeg = roomsData.find(r => r.id === roomId)?.orientation_degrees ?? 0;
renderCompass();
```

### `renderCompass()` — SVG group `#lc-compass`

Fixed in the top-right corner (SVG coords, translate(925, 75), radius 52).

```svg
<g id="lc-compass" transform="translate(925, 75)">
  <circle r="52" fill="rgba(0,0,0,0.55)" stroke="#555" stroke-width="1.5"/>
  <!-- Cardinal labels — FIXED, not part of rotating dial group.
       pointer-events:none so they don't block drag on the handle behind them. -->
  <text y="-36" text-anchor="middle" class="lc-compass-label" style="pointer-events:none">N</text>
  <text y="41"  text-anchor="middle" class="lc-compass-label" style="pointer-events:none">S</text>
  <text x="38"  dy="5" text-anchor="middle" class="lc-compass-label" style="pointer-events:none">E</text>
  <text x="-38" dy="5" text-anchor="middle" class="lc-compass-label" style="pointer-events:none">W</text>
  <!-- Rotating dial -->
  <g id="lc-compass-dial" transform="rotate(0)">
    <polygon points="0,-40 5,-20 -5,-20" fill="#e8c84a"/>  <!-- N pointer amber -->
    <polygon points="0,40 5,20 -5,20"   fill="#555"/>       <!-- S pointer grey -->
    <circle r="5" fill="#fff"/>
  </g>
  <!-- Invisible full-radius drag handle on top -->
  <circle r="52" fill="transparent" id="lc-compass-handle" style="cursor:grab"/>
</g>
```

**N is fixed at the top of the dial group.** Rotating the dial by `compassDeg` degrees
means "the top of the canvas is `compassDeg`° clockwise from magnetic North".

### Drag interaction

```js
function wireCompass() {
    const handle = document.getElementById('lc-compass-handle');
    const dial   = document.getElementById('lc-compass-dial');
    // SVG-space centre of the compass group
    const cx = 925, cy = 75;

    handle.addEventListener('pointerdown', e => {
        compassDragging = true;
        handle.setPointerCapture(e.pointerId);
        handle.style.cursor = 'grabbing';
    });

    handle.addEventListener('pointermove', e => {
        if (!compassDragging) return;
        const pt = svgPoint(e);
        const angle = Math.atan2(pt.y - cy, pt.x - cx) * 180 / Math.PI + 90;
        compassDeg = ((angle % 360) + 360) % 360;
        dial.setAttribute('transform', `rotate(${compassDeg})`);
        redrawSunArc();
        redrawLightCones();
    });

    handle.addEventListener('pointerup', () => {
        compassDragging = false;
        handle.style.cursor = 'grab';
        clearTimeout(compassOrientTimer);
        // Debounce: wait for user to finish dragging before PATCHing
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

**Design decision:** do not snap on the first sensor frame — magnetometer data is noisy
and the first few readings are often stale. Instead, drive the dial live so the user
can see it settle, then let them lock it with a "Save" button. A 500 ms rolling
average over the last 10 samples smooths jitter.

**iOS vs Android:** `deviceorientationabsolute` + `ev.alpha` works on Android.
On iOS Safari `deviceorientationabsolute` is often unsupported; use
`webkitCompassHeading` from a standard `deviceorientation` event instead.

```html
<button id="lc-phone-compass" title="Calibrate with phone compass">📱</button>
<button id="lc-phone-lock" style="display:none">🔒 Lock heading</button>
```

```js
let phoneOrientActive = false;
let phoneHeadingSamples = [];
const PHONE_SAMPLE_WINDOW = 10;

document.getElementById('lc-phone-compass').addEventListener('click', async () => {
    if (typeof DeviceOrientationEvent !== 'undefined' &&
        typeof DeviceOrientationEvent.requestPermission === 'function') {
        const perm = await DeviceOrientationEvent.requestPermission();
        if (perm !== 'granted') return;
    }

    phoneOrientActive = true;
    phoneHeadingSamples = [];
    document.getElementById('lc-phone-lock').style.display = '';

    function getHeading(e) {
        // iOS uses webkitCompassHeading (true north); Android uses alpha (magnetic)
        const h = (e.webkitCompassHeading != null)
            ? e.webkitCompassHeading
            : (e.alpha != null ? ((e.alpha % 360 + 360) % 360) : null);
        if (h == null || !phoneOrientActive) return;

        phoneHeadingSamples.push(h);
        if (phoneHeadingSamples.length > PHONE_SAMPLE_WINDOW)
            phoneHeadingSamples.shift();

        // Rolling average (handles 0/360 wrap with circular mean)
        const sinSum = phoneHeadingSamples.reduce((s, a) => s + Math.sin(a * Math.PI/180), 0);
        const cosSum = phoneHeadingSamples.reduce((s, a) => s + Math.cos(a * Math.PI/180), 0);
        const avg = (Math.atan2(sinSum, cosSum) * 180 / Math.PI + 360) % 360;

        compassDeg = avg;
        document.getElementById('lc-compass-dial').setAttribute('transform', `rotate(${avg})`);
        redrawSunArc();
    }

    // Try absolute first, fall back to standard (iOS)
    window.addEventListener('deviceorientationabsolute', getHeading);
    window.addEventListener('deviceorientation', getHeading);

    document.getElementById('lc-phone-lock').onclick = () => {
        phoneOrientActive = false;
        window.removeEventListener('deviceorientationabsolute', getHeading);
        window.removeEventListener('deviceorientation', getHeading);
        document.getElementById('lc-phone-lock').style.display = 'none';
        patchOrientation(currentRoomId, compassDeg);
    };
});

// Hide button entirely on desktop (API not available)
if (!('ondeviceorientation' in window)) {
    document.getElementById('lc-phone-compass').style.display = 'none';
}
```

---

## 4. layout.js — Sun arc overlay

### New SVG layer

```html
<g id="lc-sun-arc"/>   <!-- inserted first, behind all other layers -->
```

### `GET /api/solar/config` + client-side solar trig

```js
let MESH_LAT = null, MESH_LON = null;  // loaded once in openLayout()

function solarPosition(dateUtc, lat, lon) {
    // NOAA simplified solar calculator (public domain, ~40 lines)
    // Returns { azimuth: 0–360, elevation: -90..90 }
    // ±1–2° accuracy is sufficient — the live dot comes from the Rust backend.
}
```

### `redrawSunArc(azimuth, elevation)`

Called on every `SolarUpdate` WS event and on every compass drag.

```js
function redrawSunArc(azimuth, elevation) {
    const g = document.getElementById('lc-sun-arc');
    g.innerHTML = '';

    // Compute today's sunrise and sunset by sweeping solarPosition() over the day.
    // Polar edge case: if the sun never crosses 0° elevation today (midnight sun or
    // polar night), draw either a full circle (always up) or nothing (always down).
    const { sunriseAz, sunsetAz, polarDay, polarNight } = todaySunriseSunset();
    if (polarNight) return;

    // Map azimuth → SVG perimeter point, rotated by -compassDeg
    // r=540 places the arc just outside the 1000×1000 canvas
    const pts = arcPoints(polarDay ? 0 : sunriseAz, polarDay ? 360 : sunsetAz, compassDeg);

    // Arc path
    const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
    path.setAttribute('d', svgArcPath(pts));
    path.setAttribute('stroke', 'rgba(255,200,50,0.6)');
    path.setAttribute('stroke-width', '4');
    path.setAttribute('fill', 'none');
    path.setAttribute('stroke-dasharray', elevation <= 0 ? '8 6' : '');
    g.appendChild(path);

    // Current position dot
    if (elevation > -18) {
        const p = azimuthToPerimeterPoint(azimuth, compassDeg);
        const dot = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
        dot.setAttribute('cx', p.x); dot.setAttribute('cy', p.y);
        dot.setAttribute('r', '10');
        dot.setAttribute('fill', '#FFD700');
        dot.classList.add('lc-sun-dot');
        g.appendChild(dot);
    }
}

function azimuthToPerimeterPoint(az, orientDeg) {
    const adjusted = ((az - orientDeg) % 360 + 360) % 360;
    const rad = (adjusted - 90) * Math.PI / 180;
    return { x: 500 + 540 * Math.cos(rad), y: 500 + 540 * Math.sin(rad) };
}

function todaySunriseSunset() {
    // Sweep every 5 mins of today; find first/last crossing of elevation=0.
    // If elevation is always > 0 → polarDay. Always < 0 → polarNight.
    let riseAz = null, setAz = null, wasUp = null;
    for (let m = 0; m <= 1440; m += 5) {
        const d = new Date(); d.setHours(0, m, 0, 0);
        const { azimuth, elevation } = solarPosition(d, MESH_LAT, MESH_LON);
        const up = elevation > 0;
        if (wasUp === false && up)  riseAz = azimuth;
        if (wasUp === true  && !up) setAz  = azimuth;
        wasUp = up;
    }
    const polarDay   = riseAz == null && wasUp === true;
    const polarNight = riseAz == null && wasUp === false;
    return { sunriseAz: riseAz, sunsetAz: setAz, polarDay, polarNight };
}
```

---

## 5. layout.js — Time scrubber

```html
<div id="lc-scrubber-bar">
  <span id="lc-scrubber-time">Now</span>
  <input type="range" id="lc-scrubber" min="0" max="1440" step="5"/>
  <button id="lc-scrubber-live">↺ Live</button>
</div>
```

- No value attribute — live mode is detected by a module flag `scrubberLive = true`
- `previewSolarState(az, el)` animates bulb brightness/CT on canvas icons using the
  same piecewise linear formula as the Rust `calculate_solar_state` — translated to JS:
  ```js
  function calculateSolarState(elevation) {
      if (elevation <= 0) {
          const t = Math.max(0, Math.min(1, (elevation + 18) / 18));
          return { bri: Math.round(1 + t * 29), ct: 500 };
      }
      const t = Math.min(1, elevation / 90);
      return { bri: Math.round(30 + t * 225), ct: Math.round(454 - t * 301) };
  }
  ```
- **Simulation Mode indicator:** when scrubber is not live, the canvas SVG gets a
  coloured border and a floating "Simulation" chip so the user never mistakes the
  preview for real device state.
- **Throttle to ~30fps:** use `requestAnimationFrame` to cap preview redraws during
  slider drag — avoids layout thrash on low-end mobile.

```js
let scrubberLive = true;
let scrubberRafPending = false;

document.getElementById('lc-scrubber').addEventListener('input', () => {
    scrubberLive = false;
    setScrubberSimMode(true);
    if (scrubberRafPending) return;
    scrubberRafPending = true;
    requestAnimationFrame(() => {
        scrubberRafPending = false;
        const mins = parseInt(document.getElementById('lc-scrubber').value);
        const d = new Date(); d.setHours(0, mins, 0, 0);
        const { azimuth, elevation } = solarPosition(d, MESH_LAT, MESH_LON);
        document.getElementById('lc-scrubber-time').textContent =
            `${String(Math.floor(mins/60)).padStart(2,'0')}:${String(mins%60).padStart(2,'0')}`;
        redrawSunArc(azimuth, elevation);
        previewSolarState(azimuth, elevation);
    });
});

document.getElementById('lc-scrubber-live').addEventListener('click', () => {
    scrubberLive = true;
    setScrubberSimMode(false);
    document.getElementById('lc-scrubber-time').textContent = 'Now';
    redrawSunArc(lastSolar.azimuth, lastSolar.elevation);
    previewSolarState(lastSolar.azimuth, lastSolar.elevation);
});

function setScrubberSimMode(active) {
    document.getElementById('lc-canvas-svg').classList.toggle('lc-sim-mode', active);
    document.getElementById('lc-sim-chip').style.display = active ? '' : 'none';
}
```

---

## 6. layout.js — Optional sun calibration

Shown only when `lastSolar.elevation > 5°`. On click, dims the canvas and overlays
clickable wall targets so the active calibration state is visually distinct.

```js
function enterSunCalibMode() {
    document.getElementById('lc-canvas-svg').classList.add('lc-calib-mode');
    // Render four wall highlight strips as clickable <rect> elements in #lc-preview
    const walls = [
        { label: 'N', facing: 0,   x: 0,   y: 0,   w: 1000, h: 30 },
        { label: 'S', facing: 180, x: 0,   y: 970,  w: 1000, h: 30 },
        { label: 'E', facing: 90,  x: 970, y: 0,   w: 30,   h: 1000 },
        { label: 'W', facing: 270, x: 0,   y: 0,   w: 30,   h: 1000 },
    ];
    walls.forEach(wall => {
        const rect = makeSvgEl('rect', { ...wall, class: 'lc-wall-calib-target' });
        rect.addEventListener('click', () => {
            const orientation = ((lastSolar.azimuth - wall.facing) % 360 + 360) % 360;
            exitSunCalibMode();
            applyOrientation(orientation); // sets dial + PATCHes
        });
        document.getElementById('lc-preview').appendChild(rect);
    });
}

function exitSunCalibMode() {
    document.getElementById('lc-canvas-svg').classList.remove('lc-calib-mode');
    document.getElementById('lc-preview').innerHTML = '';
}
```

---

## 7. rooms.js — Forward orientation from WS

When `RoomsUpdate` arrives, update the dial if the layout view is open for that room.
This closes the loop: PATCH → DB → SpatialEngine → WS → UI stays in sync.

```js
// in existing RoomsUpdate WS handler:
if (layoutOpenRoomId) {
    const room = msg.rooms?.find(r => r.id === layoutOpenRoomId);
    if (room != null) layout.notifyOrientationUpdate(room.orientation_degrees);
}
```

Export from `layout.js`:

```js
export function notifyOrientationUpdate(deg) {
    compassDeg = deg;
    document.getElementById('lc-compass-dial')
        ?.setAttribute('transform', `rotate(${deg})`);
    if (scrubberLive) redrawSunArc(lastSolar.azimuth, lastSolar.elevation);
}
```

Note: if the scrubber is in preview mode, we deliberately skip re-drawing the arc
so the preview position is not disrupted by the WS event.

---

## 8. style.css additions

```css
/* Compass */
#lc-compass { user-select: none; }
.lc-compass-label { fill: #ccc; font-size: 28px; font-weight: 600; pointer-events: none; }
#lc-compass-handle { cursor: grab; }
#lc-compass-handle:active { cursor: grabbing; }

/* Sun arc */
#lc-sun-arc { pointer-events: none; }
.lc-sun-dot { animation: lc-sun-pulse 1.8s ease-in-out infinite; }
@keyframes lc-sun-pulse { 0%,100% { r: 10; opacity: 1; } 50% { r: 14; opacity: 0.6; } }

/* Scrubber bar */
#lc-scrubber-bar {
    display: flex; align-items: center; gap: 8px;
    padding: 6px 12px; background: var(--surface2);
    border-top: 1px solid var(--border);
}
#lc-scrubber { flex: 1; accent-color: var(--amber); }
#lc-scrubber-live { font-size: 12px; padding: 2px 8px; }

/* Simulation mode indicator */
#lc-canvas-svg.lc-sim-mode { outline: 2px solid var(--amber); border-radius: 2px; }
#lc-sim-chip {
    display: none; position: absolute; top: 6px; left: 50%;
    transform: translateX(-50%);
    background: var(--amber); color: #000; font-size: 11px;
    padding: 2px 8px; border-radius: 10px; pointer-events: none;
}

/* Phone compass */
#lc-phone-compass { font-size: 18px; background: none; border: none; cursor: pointer; padding: 4px; }
#lc-phone-lock { font-size: 12px; padding: 2px 8px; }

/* Sun calibration */
#lc-sun-calib { font-size: 12px; padding: 2px 8px; }
#lc-canvas-svg.lc-calib-mode { filter: brightness(0.65); }
#lc-canvas-svg.lc-calib-mode .lc-wall-calib-target { filter: brightness(1.8); }
.lc-wall-calib-target {
    fill: rgba(255,200,50,0.15); stroke: var(--amber);
    stroke-width: 4; cursor: pointer; pointer-events: all;
}
.lc-wall-calib-target:hover { fill: rgba(255,200,50,0.35); }
```

---

## 9. SVG layer order (updated)

```html
<g id="lc-sun-arc"/>     <!-- arc + pulsing dot — behind everything -->
<g id="lc-openings"/>    <!-- window/door rects + handles -->
<g id="lc-shadow"/>      <!-- light cone polygons -->
<g id="lc-bulbs"/>       <!-- bulb icons -->
<g id="lc-preview"/>     <!-- drag ghost + calibration wall overlays -->
<g id="lc-compass"/>     <!-- compass rose — on top, non-blocking -->
```

---

## Critical path

```
registry.rs: set_room_orientation()
        │
        ├──► api.rs: PATCH /api/rooms/{id}/orientation (with is_finite guard)
        │    api.rs: GET  /api/solar/config
        │           │
        │           └──► mod.rs: register both routes
        │
        └──► layout.js
              ├── Solar config fetch (MESH_LAT/LON loaded once)
              ├── Compass dial (renderCompass, wireCompass, patchOrientation)
              ├── Phone compass button (rolling average, iOS webkitCompassHeading)
              ├── Sun arc overlay (redrawSunArc, todaySunriseSunset, polar edge case)
              ├── Time scrubber (rAF throttle, sim mode tint, calculateSolarState JS)
              └── Sun calibration (elevation-gated, canvas dim, wall tap hitboxes)
                          │
                          └──► rooms.js: notifyOrientationUpdate on RoomsUpdate WS
```

---

## Verification

1. `cargo test -p coordinator` — all existing tests pass; add:
   - `set_room_orientation_persists_flag` (registry)
   - `patch_orientation_returns_401_for_wrong_token` (api)
   - `patch_orientation_returns_404_for_unknown_room` (api)
   - `patch_orientation_clamps_to_360` (send 400°, expect 40° stored)
   - `patch_orientation_rejects_nan` (send NaN, expect 400)
   - `patch_orientation_rejects_infinity` (send Inf, expect 400)
   - `solar_config_returns_lat_lon` (GET /api/solar/config returns valid JSON)
   - `patch_orientation_pushes_rooms_update` (WS propagation: PATCH → rooms broadcast received)
2. Open layout view → compass dial visible; sun arc drawn around perimeter
3. Drag dial → arc and light cones rotate in real time; release → PATCH fires; reload → angle persists
4. On mobile (HTTPS): tap 📱 → dial follows phone live; tap 🔒 → heading locked → PATCH fires
5. iOS: verify `webkitCompassHeading` path fires (use Safari dev tools on device)
6. Polar-night edge case: set lat to 89°N in December → arc absent, no JS error
7. Midnight-sun edge case: set lat to 89°N in June → full-circle arc drawn
8. Scrubber: drag to 12:00 → sim mode tint + "Simulation" chip appear; bulb icons animate; release ↺ → snaps live
9. Sun calibration: visible when elevation > 5°; canvas dims; tap a wall → orientation correct; canvas un-dims
10. Enable solar → change orientation → `SpatialEngine` sweep log shows updated effective azimuth
