# Home Tab Redesign + In-Room Groups — Unified Implementation Plan

## Status (2026-07-06)

- **Phase 1 — shipped, deployed, live-verified.** Tile grid, colour wash,
  power toggle, notable-only badges, whole-house summary all landed in
  `7e6ef30` and confirmed looking good on a live phone check against pi1.
- **Phase 2 backend — shipped** in the same commit: `room_groups` schema/
  registry/CRUD, `dispatch_light_command` fan-out helper shared by
  `room_command`/`group_command`, all 5 group API routes, and voice/intent
  targeting (group-name resolution + the room-name multi-device fan-out
  fix). Two independent code reviews (Bing, Gemini) of this commit were
  checked against the actual code: one real gap found and fixed (missing
  device-level warn logging on fan-out failure); the rest were already
  handled by existing code or not real regressions.
- **Phase 2 frontend — in progress.** Group cluster UI in the expanded
  panel (per-group control cluster, `buildGroupSelect` dropdown, device
  list partitioned into ungrouped + per-group sub-lists, group create/
  rename/delete UI) not yet built. This is the immediate next step.
- Phases 2b, 3, 4 — not started.

## Context

Current room cards try to show everything at once for every room, all the
time: on/off, brightness slider, colour/temp popup triggers, effects row,
sensor strip, scene chips. This got worse the moment "groups within a room"
(Kitchen's 8 spots vs. 3 counter pendants, each wanting independent
control) was added to the requirements — another control cluster on an
already-cluttered card.

Decision, after reviewing 6 candidate directions and two independent
appraisals (Bing, Gemini) of them: **build one solution combining the
practical daily-driver (glanceable tiles + whole-house summary) with the
genuine differentiator (spatial floorplan), plus groups, plus a small
camera-assist for room setup accuracy** — rather than picking only one
direction. The four pieces are complementary, not competing: the tile
view is the safe, always-available fallback; the floorplan is an
alternate view mode layered on top once the structural declutter is
proven; groups slot into both views' expanded controls; the camera
backdrop improves the data both spatial views are drawn from.

**Rejected as infeasible in this project's actual architecture:** full
camera-based room scanning (à la Apple RoomPlan / Google ARCore Scene
Semantics). Both are native-app-only APIs (RoomPlan needs LiDAR + native
iOS/Swift; ARCore's semantic scanning is native Android SDK) — ai-mesh is
a browser PWA with no native shell, and WebXR (the browser AR API) doesn't
expose that level of room-geometry understanding. Building monocular
room-layout inference from a plain camera feed ourselves is a real,
still-active computer-vision research problem, not a scoped feature.
What's actually needed instead (Phase 4, below) is much smaller.

---

## Phase 1 — Structural declutter: tile grid + whole-house summary

**Goal:** default view = status, not controls. This is the highest-value,
lowest-risk piece and should land first — everything else builds on it.

- Replace the always-expanded room card grid on the Home tab with a grid
  of compact tiles. Each tile: room name, a colour-wash background
  reflecting aggregate light state (warm glow when on-and-warm, cool
  white-blue when on-and-cool, dark/muted when off), and a minimal icon
  row shown *only* when notable (motion dot while occupied, temp badge
  only if unusually off from the house average — e.g. ±2°C from the
  house median, low-battery warning only if real — e.g. <20%).
  **Tap = expand the control panel; a small dedicated power icon on the
  tile does the quick light toggle** (decided with Jon: avoids Apple
  Home's widely-complained-about accidental tap-to-toggle while
  scrolling, while keeping the most common action one visible tap away).
- **Colour-wash implementation anchor**: reuse the existing colour
  pipeline, don't invent one — `xyToRgb` already exists client-side
  (used for scene-chip preview colours in `rooms.js`) and
  `SceneRecord::preview_color` already demonstrates the xy-averaging
  approach server-side. Tile wash = average the on-lights' xy (falling
  back to CT-derived xy, same fallback `preview_color` uses), map through
  `xyToRgb`, apply as a background gradient; dark/muted when all off.
- A room with no lights (e.g. a sensor-only room, or the walk-in larder
  before its spots pair) renders a neutral tile with just its sensor
  badges — the wash is a lights concept, absence of lights isn't "off".
- An active effect shows a small icon on the tile (reuse the existing
  `EFFECT_ICONS` mapping from `rooms.js`) — the wash still reflects the
  actual current colour, which under an animating effect is itself the
  honest live state.
- **Pinned sensors map directly onto this design**: "pinned" (the
  just-shipped `mesh-pinned-sensors` feature) becomes "shows on the
  collapsed tile" — same localStorage key, same toggle UI in the expanded
  panel, new render target. No migration, elegant continuity.
- **Flows that must survive the tile conversion**: room drag-reorder
  (existing `meshLightOrder`-family prefs — tiles stay draggable);
  the unassigned-devices strip (tiles become drop targets for the
  existing drag-from-strip flow, reusing `wireDropZone` — the Devices
  tab dropdown remains the alternative path either way).
- Add a single whole-house summary line above the grid ("12 lights on ·
  3 occupied · 21°C avg · 2 devices need attention") — answers "is
  everything fine?" without reading any individual tile. All derivable
  client-side from `devicesMap` (offline lights + low-battery sensors =
  "need attention"); no backend work. Include an **"All off"** quick
  action here — it reuses the existing global-light command path
  (`sendRoomCommand`'s `isGlobal` flag) and is a genuine daily-driver
  win that currently requires per-room taps.
- The expanded control panel carries today's full capability set
  (on/off/brightness/colour, effects, scenes, sensor strip) but
  reorganized with clear hierarchy: whole-room cluster at top, then group
  sub-cards (Phase 2), then scenes/effects, then the sensor strip at the
  bottom.
- **Files**: this is a substantial rework of `coordinator/src/http/static/
  rooms.js`'s render path (introduces a collapsed/expanded state machine
  where today's render function builds one always-expanded card) — the
  existing control-building logic (`buildSlider`, the colour/temp popup,
  `sendRoomCommand`) is relocated into the expanded panel, not rewritten.
  `rooms.js` is already ~1,700 lines; extract the expanded-panel builder
  into a new module (e.g. `roompanel.js`) rather than growing it further —
  matches the codebase's existing module splits (`devicewidgets.js`,
  `indicators.js`, `controls.js`).
  Needs its own closer implementation pass (current DOM structure,
  exact collapse/expand transition, where state persists — likely
  `localStorage` per room, matching the existing `mesh-room-collapsed-*`
  convention) before coding starts; this plan sets direction, not
  line-level detail, for this phase.
- **Verification**: no browser on WSL2 (house constraint) — build,
  `curl` the REST/WS contract, then a live phone check on pi1:9001 for
  the actual visual/interaction feel, since "does this reduce clutter on
  a real phone screen" is the entire point and can't be judged from code
  alone.

## Phase 2 — In-room groups (fully designed, ready to build)

Already researched and refined in detail — this section is complete
enough to implement directly, independent of Phase 1's exact tile
mechanics (it targets the *expanded panel*, which exists in both the
current layout and Phase 1's redesign).

**Naming**: new table is `room_groups`, not `device_groups` — avoids
colliding with the existing unrelated `light_groups` (real Zigbee/z2m
groups, `LightTarget::Group`, `get_node_for_group` in `lights.rs`). This
feature never touches z2m groups; group commands are ai-mesh-side fan-out
to each member device individually.

- **Schema**: `room_groups(id, room_id, name, position)`,
  `ON DELETE CASCADE` on `room_id` (same as `openings`/`room_effects`);
  nullable `group_id` column on `room_devices` via the same `ALTER TABLE
  ... ADD COLUMN` idiom already used for `position`
  (`coordinator/src/registry/mod.rs` ~381-399).
- **Registry** (`registry/mod.rs`): `RoomGroupRecord { id, room_id, name,
  position, device_ids }`; `RoomRecord` gains `groups:
  Vec<RoomGroupRecord>`. Methods: `create_room_group`,
  `rename_room_group`, `delete_room_group` (clears members' `group_id`
  back to `NULL`, mirrors how `delete_room` already clears
  `light_positions.room_id`), `room_group_exists`, `get_room_group`,
  `set_device_group(device_id, group_id: Option<&str>)` — exclusive
  membership, one `UPDATE`.
- **Wire**: `RoomInfo` gains `groups: Vec<GroupInfo>`, rides the
  *existing* `RoomsUpdate` WS event (every room-membership mutation
  already ends with `push_rooms_update(rooms_from_registry(...))` — the
  new group handlers just call the same thing). No new WS event, no new
  fetch, no `WIRE_VERSION` bump — **coordinator + browser JS only,
  `just deploy-coordinator pi1` covers it, no `deploy-node`.**
- **API routes**, all in `rooms.rs` (a group is a partition of room
  membership, not a new device domain — same reasoning that already
  keeps `room_command` in `rooms.rs`):
  - `POST /api/rooms/{id}/groups` `{name}` → `201 {id}`
  - `PATCH /api/rooms/{id}/groups/{gid}/name` `{name}` → `204`
  - `DELETE /api/rooms/{id}/groups/{gid}` → `204`
  - `PATCH /api/rooms/{id}/devices/{did}/group` `{group_id: Option<String>}` → `204` (validates device/group both belong to this room)
  - `POST /api/rooms/{id}/groups/{gid}/command` (same body as `room_command`) → `204`/`400`/`404`/`503`
- **Command dispatch, one owner two callers**: extract `room_command`'s
  inline fan-out loop (`rooms.rs` ~177-223) into `dispatch_light_command
  (state, device_ids, action) -> bool`; `room_command` and the new
  `group_command` both call it. Same split client-side: extract
  `sendRoomCommand`'s optimistic-update-loop-plus-POST into
  `sendFanOutCommand(endpoint, deviceIds, body)`; `sendRoomCommand` and a
  new `sendGroupCommand` both call it.
- **Frontend rendering** (`rooms.js`): the group cluster renders between
  the room header and the quick-scenes bar — outside the collapsible
  body/expanded-panel's less-immediate sections, at the same tier as the
  room's own on/off/brightness/colour controls, so a group stays
  actionable without opening every sub-section. Reuses existing building
  blocks: `buildSlider` (already id-agnostic), the colour/temp popup
  (refactor `buildRoomControlsPanel` into a shared
  `buildControlsPopupPanel` + thin room/group wrappers), and a new
  `buildGroupSelect` in `devicewidgets.js` (mirrors the existing
  `buildRoomSelect`) for the per-light "assign to group" dropdown — MVP
  is dropdown-based, not drag-and-drop (existing drag machinery is scoped
  to moving devices *between rooms*, not sub-buckets within one room).
  The device list splits into an "Ungrouped" bucket plus one labelled
  sub-list per group.
- **Effects stay room-wide only** — no group-scoped effect concept.
- **Voice/intent targeting ships the same day as the widgets** — this is
  the product's own anchor principle (`plans/multi-domain-home.md`:
  "every new domain lands with its LLM tools and context the same day it
  lands with widgets"), and the original plan missed it. "Turn on the
  counter lights" must work as soon as the Counter group exists:
  - `intent.rs`'s `dispatch_light_command` target-resolution fallback
    (currently: known device → known z2m group → room name) gains a
    group-name match, fanning the command out to the group's member
    devices.
  - **Fix the adjacent pre-existing quirk in the same pass**: the
    existing room-name fallback resolves to only the *first* device in
    the room (`.and_then(|r| r.device_ids.into_iter().next())`) — "turn
    on the kitchen" via intent lights one bulb. The group resolution
    needs proper multi-device fan-out anyway; give the room fallback the
    same treatment.
  - `build_device_context` gains group names (e.g. a `[Kitchen/Counter]`
    tag or a "Known groups within rooms" line) so the model knows the
    targets exist.
- **Forward pointer**: groups become the natural targets for the
  upcoming switch→action binding work (the larder use-case that started
  this: two Hue dials bound to just the larder group's lights). No
  binding code in this phase — just don't design anything that assumes a
  binding target must be a whole room.

### Phase 2b — group-scoped scenes (small, follows immediately after)

- `SaveSceneBody` gains optional `group_id`; `save_scene` narrows the
  device set to the group's `device_ids` when set (same filter shape,
  smaller input). Always a flat per-device snapshot for a group scope —
  no effect reference (effects are room-wide only, called out explicitly
  rather than half-supported).
- `scenes` table gains nullable `group_id` (same `ALTER TABLE` idiom
  already used three times on this table); `SceneRecord`/`SceneInfo`
  thread it through like the existing `room_id`.
- **`recall_scene` needs zero changes** — confirmed by reading it: it
  iterates `scene.states: Vec<DeviceSnapshot>` directly, no room/group
  dependency. `group_id` is purely a save-time scoping/display tag.
- UI: a compact "+ Save scene" affordance under each group cluster;
  quick-scene chips get a group-label prefix ("Counter: Bright") so
  they read distinctly from room-wide chips in the same bar.

## Phase 3 — Spatial floorplan view (the differentiator)

**Positioning: an alternate view mode, not a replacement.** A toggle
between "Tiles" (Phase 1) and "Floorplan" for the Home tab, not a
wholesale bet on the riskiest option for daily use. If the floorplan
doesn't read well on a phone screen, Phase 1 remains the reliable
default — this directly addresses the "must be fast and legible or it
becomes a gimmick" risk both external reviews flagged, without giving up
the differentiator.

- Builds on the *existing* spatial data already captured for the layout
  editor (`coordinator/src/http/static/layout.js` — room dimensions,
  orientation, openings) — not a new geometry system. Crucially,
  `RoomRecord` already carries `origin_x`/`origin_y` world coordinates
  alongside `width_m`/`depth_m`, so assembling rooms into a whole-house
  plan is real, not speculative. **Multi-floor is explicitly out of scope
  for v1** (no floor/z concept exists in the room model; a floor selector
  is a later addition if ever needed).
- New **read-only, simplified rendering mode**: real room shapes/relative
  positions (not the full bulb-placement editing canvas — no drag
  handles, no popovers for editing openings), colour-washed by aggregate
  state using the same visual language as Phase 1's tiles. Tapping a room
  opens the same expanded control panel Phase 1/2 already built — the
  floorplan is a different way to *reach* a room's controls, not a
  parallel control surface.
- Groups do not get their own spatial sub-regions in v1 (both external
  appraisals flagged this as awkward) — a room's group clusters still
  live in the expanded panel reached by tapping the room shape.
- **Effort/risk**: highest of the three UI phases. The geometry exists,
  but was built for an editing tool, not a fast-glance daily dashboard —
  adapting it needs its own focused implementation pass (a fresh
  read-only render path, likely reusing `layout.js`'s SVG primitives
  rather than its full interactive canvas) before coding starts. Land
  Phase 1 first and use it for a while before committing engineering time
  here, per the phasing rationale above.

## Phase 4 — Camera-assisted room setup (small, scoped)

Not room scanning — a manual-tracing aid for the *existing* dimensions/
orientation/opening-placement flow in `layout.js`, which already covers
everything the spatial engine actually needs (width, depth, height,
compass orientation, opening positions — no furniture, no photorealism).

- Let the user attach one photo per wall, shown as a semi-transparent
  backdrop image layer behind the existing 2D SVG layout canvas, purely
  so they can eyeball wall/window/door proportions while dragging the
  existing markers into place.
- No computer vision, no native app, no ML model — a photo `<img>`
  element positioned under the SVG layer, with an opacity control.
  Small, well-scoped addition to an existing editor, not a new subsystem.
- Improves the accuracy of the data feeding *both* Phase 3's floorplan
  shapes and the existing solar/sun-position engine's window-facing
  calculations — the actual value driver, not the photo itself.

---

## Recommended build order

1. **Phase 1** (tile grid + summary) — biggest clutter win, lowest risk,
   everything else depends on it existing first.
2. **Phase 2 + 2b** (groups + group scenes) — fully spec'd, can start in
   parallel with Phase 1's frontend work since the backend pieces are
   independent; frontend group-cluster rendering targets whichever panel
   structure Phase 1 lands with.
3. **Phase 4** (camera backdrop) — small and independent, can slot in
   whenever convenient; worth doing before Phase 3 since it improves the
   data Phase 3 renders.
4. **Phase 3** (spatial floorplan) — the differentiator, deliberately
   last: built as an additive view mode once the declutter win (Phase 1)
   is live and proven comfortable day-to-day, not as a risky big-bang
   replacement.

## Verification (applies across phases)

- Rust: unit tests mirroring existing style for every new registry
  method/route (group CRUD, cascade-on-room-delete, exclusive membership,
  group-command fan-out, group-scoped scene save) — same pattern already
  used throughout `registry/mod.rs`/`http/api/*.rs`'s test modules.
  `cargo clippy --workspace --all-features --all-targets` + `cargo test
  --workspace --all-features` (pre-commit runs both anyway).
- No browser on WSL2 (house constraint): `cargo build -p coordinator`
  (JS is `include_str!`-embedded, won't update without a rebuild), boot
  locally from a scratch directory (`MESH_INSECURE=1 MESH_HTTP_PORT=19301
  ./target/debug/coordinator` — never the repo root or default ports),
  `curl` the REST/WS contract.
- Live phone check on pi1:9001 after each phase's deploy — required for
  every frontend-visible phase (1, 2, 3, 4), since "does this actually
  feel uncluttered/glanceable" can only be judged on the real device.
- Phase 1/2 deploys are coordinator-only (`just deploy-coordinator pi1`,
  no `deploy-node`) — neither touches `MeshMessage`/agent-side code.

### Critical files
- `coordinator/src/registry/mod.rs`, `coordinator/src/registry/scenes.rs`
- `coordinator/src/http/api/rooms.rs`, `coordinator/src/http/api/scenes.rs`
- `coordinator/src/http/state.rs`, `coordinator/src/http/mod.rs`
- `coordinator/src/intent.rs` (group voice targeting + room fan-out fix)
- `coordinator/src/http/static/rooms.js` (+ new `roompanel.js`), `devicewidgets.js`, `scenes.js`, `actions.js`, `indicators.js`, `layout.js`
