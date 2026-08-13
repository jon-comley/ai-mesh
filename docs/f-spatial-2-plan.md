# F-Spatial-2 Implementation Plan — Room Layout Canvas

## Overview

Three phases, each independently shippable. Phase A ships as a single PR covering both Rust backend and the new `layout.js` canvas frontend.

---

## Delivery phases

### Phase A — Bulb placement canvas
### Phase B — Windows & Doors
### Phase C — Live sun arc

See the F-Spatial-2 entries in [`../HISTORY.md`](../HISTORY.md) (Phases A, B, Rooms UX,
D, E — shipped) and [`../ROADMAP.md`](../ROADMAP.md) (Phase C — Three.js 3D view, still
open) for the full feature spec.

---

## Phase A — Detail

### 1. Rust backend

#### `registry.rs`
- `ALTER TABLE light_positions ADD COLUMN fixture_type TEXT` migration (alongside existing column checks, ~line 191)
- Update HashMap value type from `(f32, f32, f32, Option<String>)` to include `fixture_type: Option<String>`
- Update `set_light_position` to accept and persist `fixture_type`
- Update `get_all_light_positions` load query to include the column

#### `server.rs`
- `POST /api/lights/{device}/position` — add optional `fixture_type` to request body (backward compat)
- New `GET /api/rooms/{id}/positions` — returns all placed bulbs for a room (`device_id, x, y, z, fixture_type`); needed so `layout.js` can reconstruct canvas state on load

#### `spatial.rs`
- Upgrade from 2D azimuth dot-product to full 3D:
  - Sun vector: `(sin(az)·cos(el), cos(az)·cos(el), sin(el))`
  - Bulb vector: `(x−0.5, y−0.5, z−0.5).normalise()`
  - Exposure: `dot(sun, bulb).max(0.0)`
- Fallback: if `z` is null/0 (legacy), keep existing 2D path — old behaviour preserved until user sets up canvas
- Solar sensitivity multiplier per fixture type (applied after dot-product):

| Type | Sensitivity | Rationale |
|---|---|---|
| ceiling_spot | 0.4 | Above window path |
| pendant | 0.7 | Mid-height |
| table_lamp | 1.0 | At window sill height |
| floor_lamp | 0.8 | Low but shielded |
| led_strip | 0.9 | User-defined mount |

---

### 2. Frontend — `layout.js` (new module, ~400–500 lines)

#### State
```js
let layoutRoom = null;        // room object currently in layout view
let placedBulbs = {};         // device_id → {x, y, z, fixture_type, svgEl}
let undoStack = [];           // snapshots of placedBulbs (session only)
let redoStack = [];
let snapGrid = 20;            // divisions (1/20 of canvas width)
```

#### Entry / exit
- `openLayout(room)` — fetches `GET /api/rooms/{id}/positions`, renders canvas, hides room list
- `closeLayout()` — restores room list, clears canvas state

#### SVG canvas structure
```
<svg id="layout-canvas">
  <g id="lc-openings"/>       <!-- Phase B placeholder -->
  <g id="lc-bulbs"/>          <!-- placed bulb icons -->
  <g id="lc-preview"/>        <!-- drag preview glow -->
  <g id="lc-sun-arc"/>        <!-- Phase C placeholder -->
</svg>
```

#### Drag-to-place flow
1. Sidebar chip `dragstart` → capture `device_id`; trigger Z2M pulse via existing pulse-on-grab
2. Canvas `dragover` → compute snapped XY (see snap grid below), render preview glow in `#lc-preview`
3. Canvas `drop` → `POST /api/lights/{device}/position`, push undo snapshot, render bulb icon
4. Bulb icon click → show popover (fixture type picker + height slider Z)
5. Popover change → `POST /api/lights/{device}/position`, update icon, push undo snapshot

#### Fixture icons (inline SVG)

| Type | Icon |
|---|---|
| ceiling_spot | filled circle |
| pendant | circle + vertical drop line (length ∝ `1−z`) |
| table_lamp | circle + short base rect |
| floor_lamp | circle + tall base line |
| led_strip | rounded rect |

#### Snap grid
On `dragover`: `snapX = Math.round(normX * snapGrid) / snapGrid`

No grid lines rendered — placement just feels tidy.

#### Bulb name labels
Each placed bulb renders a `<text>` element below its icon showing the device's friendly name (truncated to ~12 chars). Toggleable via a "Show labels" checkbox in the layout view header. Default on.

#### Auto-arrange
- Button visible when ≥2 bulbs are placed and unplaced bulbs remain
- Distributes unplaced bulbs in an even grid; floor/table lamps pushed toward walls
- Posts all positions in parallel; pushes single undo snapshot

#### Undo / redo
- `Ctrl+Z` / `Ctrl+Shift+Z`
- Each action pushes a shallow clone of `placedBulbs` positions
- Restore via batch `POST /api/lights/{device}/position` calls
- Session only — wiped on reload; server positions are source of truth on reload

#### Live state transitions
Bulb icons reflect live colour/brightness from the existing `devicesMap`. When `SolarUpdate` fires and repaints a bulb's fill colour, apply `transition: fill 0.8s ease` via CSS on the SVG bulb elements so the solar sweep feels gradual rather than a hard jump. Applied in the dashboard stylesheet, scoped to `#lc-bulbs circle`.

---

### 3. `rooms.js` changes
- Room header `click` → call `openLayout(room)` (currently no-op)
- Add back-button element inside layout view header
- Hide `.room-list` and unassigned strip while layout open; restore on close

---

### 4. File targets

| File | Change |
|---|---|
| `coordinator/src/registry.rs` | `fixture_type` column migration, struct update, load/save |
| `coordinator/src/server.rs` | `fixture_type` in position POST body; new GET positions-by-room endpoint |
| `coordinator/src/spatial.rs` | 3D dot-product, fixture type sensitivity multipliers, Z fallback |
| `coordinator/src/http/static/rooms.js` | Header click → `openLayout`; back button wiring |
| `coordinator/src/http/static/layout.js` | New file — full canvas module |
| `coordinator/src/http/static/dashboard.js` | Register and init `layout.js` module |

---

### Critical path within Phase A

```
registry.rs migration + struct update
        │
        ├──► server.rs POST update + new GET endpoint
        │           │
        │           ▼
        │       layout.js (canvas, drag, place, popover)
        │           │
        ▼           ▼
    spatial.rs 3D upgrade  ←── parallel with layout.js
        │
        └──► rooms.js wiring  ←── last (needs layout.js done)
```

---

## Phase B — Windows & Doors (outline)

- New `openings` SQLite table: `(room_id, type TEXT, wall_edge NSEW, x_norm, width_norm, transmission)`
- Migration: convert existing `rooms.has_window` / `window_facing` rows to `openings` rows on first startup
- REST: `GET/POST/DELETE /api/rooms/{id}/openings`
- Canvas: window/door chips in sidebar; magnetic wall snap within ~60px; resize drag handles
- Auto-shadow preview: client-side SVG light cone at current solar azimuth, opacity ∝ `transmission`
- `SpatialEngine`: iterate all openings, weighted `transmission` sum (replaces `has_window` flag)

---

## Phase C — Live sun arc (outline)

- Sun path arc across canvas top, driven by existing `SolarUpdate` WS event — no new backend
- Pulsing dot marks current azimuth
- Scrub slider for preview mode: drag moves sun, bulb icons animate to preview state; release returns to real-time

---

## Deferred / future

- **Zigbee signal triangulation** — Z2M exposes per-link `linkquality` (LQI) in neighbour tables. With ≥3 lights hearing each other, rough XY positions can be inferred via trilateration. LQI is noisy; needs accuracy assessment before building. Would seed canvas with plausible starting positions for user to refine.
- **`rotation_deg` on fixture** — directional fixtures like LED strips could carry a `rotation_deg: Option<f32>` column in `light_positions` for Phase B, when wall-mounted strips need to know which way they face. Not needed for Phase A.
- **Hue Bridge Pro import** — Hue SpatialAware (shipped Apr 2026, Bridge Pro only) stores XY positions set via AR scanning. If user has a Bridge Pro alongside Z2M, a one-time import via the local Hue API v2 `entertainment_configuration` endpoint is feasible, but requires Z2M ↔ Hue device ID mapping.
