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
- **Phase 2 frontend — shipped.** Group cluster UI in the expanded panel
  (per-group on/off + brightness, inline rename/delete, `+ New group`,
  `buildGroupSelect` dropdown per light card, device list partitioned into
  Ungrouped + per-group sub-lists).
- **Phase 2b — shipped.** Group-scoped scenes: `scenes.group_id` (cascades
  on group delete, unlike `room_devices.group_id` which is only nulled —
  a group-scoped scene has nowhere else to be deleted from once its group
  is gone), `save_scene` narrows to the group's own members and never
  captures an effect for a group scope. `recall_scene` needed zero
  changes (confirmed by reading it — it only ever iterates `scene.states`).
  Frontend: quick-scene chips for group scenes share the room's existing
  bar with a `"GroupName: "` prefix; a compact `+ Save scene` row lives in
  each group cluster (factored out of `buildScenesSection` into a shared
  `buildSceneSaveRow`). The trickiest part was `scenes.js`'s per-room state
  Maps (`activeSceneByRoom`/`preSceneStateByRoom`/`pausedSceneDevices`),
  previously keyed only by room id — generalized to "scope id" (a room id
  or a group id, safe to mix since both come from the same UUID
  generator) so a room-wide scene and any of its groups' own scenes can be
  independently active/paused at once, without a second set of Maps.
- **Phase 4 — shipped, then removed (2026-07-06).** Wall-photo backdrop in
  the layout editor: one photo per wall (N/S/E/W), client-downscaled before
  upload, shown first as a 2D canvas backdrop, then (after that was pointed
  out as geometrically wrong — a wall photo is a front-on/elevation shot, the
  2D view is a straight-down plan, no rotation reconciles those two
  projections) as a texture on the matching wall in the 3D view instead.
  Removed entirely after live testing: a real phone photo of a wall in a
  normal-sized room inevitably includes floor/ceiling/perspective distortion,
  and stretching it onto a flat plane made that worse, not better. All of
  `room_wall_photos`, its REST endpoints, and the layout.js/layout3d.js/
  style.css UI for it were deleted. Superseded by a native iPad LiDAR
  RoomPlan scan, deferred until the Mac Studio needed to build it is set up
  — see `plans/roomplan-ios-scan.md`.
- **Phase 3 — shipped, rescoped.** Before implementing, found the plan's
  premise was wrong: `RoomRecord.origin_x`/`origin_y` are a *within-room*
  crosshair reference point (bulb-placement snapping, 3D centering), not a
  whole-house world position — there was no data to assemble a true house
  layout from, and adding one (new position fields + a drag-to-arrange UI)
  would have been a real feature in itself before the view even rendered.
  Given the choice, went with a **schematic proportional view** instead of
  building that: a "Tiles / Floorplan" toggle on the Home tab (persisted in
  localStorage), reusing 100% of `renderRoomCard`'s existing internals
  (group clusters, scenes, device list, click-to-expand) unchanged. In
  floorplan mode each collapsed tile's height is shaped by its own
  `depth_m`/`width_m` ratio (schematic — a fixed reference height scaled by
  the ratio, not real pixels-per-metre, since this is a single-column
  phone-width list, not a 2D house collage), a subtle graph-paper texture
  distinguishes the mode, and a tiny compass glyph reuses
  `orientation_degrees` (already captured for the solar effect) rotated the
  same way the layout editor's own compass dial rotates its N label. No
  backend changes at all — pure frontend, no wire/schema impact.

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

## Phase 3 — Floorplan view mode (rescoped from the original plan — SHIPPED)

**Original premise was wrong, corrected before writing any code.** This
section originally assumed `RoomRecord.origin_x`/`origin_y` were world
coordinates placing each room relative to the others, making "assemble a
whole-house plan" a rendering exercise over data that already existed.
Checked before implementing: they're actually a *within-room* crosshair
reference point (bulb-placement snapping, 3D centering) — there is no
data anywhere describing one room's position relative to another, and
building that (new position fields + a one-time drag-to-arrange UI) would
have been real, unscoped engineering before the view could even render.

Put to Jon as an explicit choice (true whole-house arrangement vs. a
schematic proportional view vs. holding off entirely); **schematic** was
picked. What actually shipped:

- **Positioning: an alternate view mode, not a replacement** — a
  "▦ Tiles / ⌂ Floorplan" toggle on the Home tab (persisted in
  localStorage as `mesh-home-view-mode`), living beside the whole-house
  summary line. Tiles stays the default; if floorplan doesn't read well,
  switching back costs one tap.
- **Schematic, not scaled**: since the Home tab is a single-column
  phone-width list (confirmed — `.room-list` is `flex-direction: column`,
  there's no multi-column canvas to lay rooms out across), "floorplan"
  means each room's *collapsed tile* is shaped by its own
  `depth_m`/`width_m` ratio (clamped 0.4–2.5, height clamped 70–240px off
  a 90px reference) rather than every tile sharing one uniform height —
  not a true scaled 2D house collage. A subtle CSS graph-paper texture
  (`.room-card::before`, layered behind the existing colour-wash
  background) is the only other visual cue that the mode changed.
- **Zero new data, zero new backend** — no `origin_x`/`origin_y` reuse, no
  new registry/API work, no wire change. `renderRoomCard`'s internals
  (group clusters, scenes, device list, click-to-expand) are 100% shared
  between both modes; only the tile-face sizing and a small orientation
  compass glyph (reusing `orientation_degrees`, already captured for the
  solar effect — rotated the same way the layout editor's own compass
  dial rotates its N label) are new.
- Groups still don't get their own spatial sub-regions — a room's group
  clusters live in the same expanded panel reached by tapping the tile,
  identical in both view modes.
- **Effort ended up low, not high** — the original "highest effort/risk"
  assessment assumed a genuine new render path over real spatial data;
  once that data turned out not to exist, the schematic version reduced
  to a sizing/styling variant of code that already existed.

## Phase 4 — Camera-assisted room setup — REMOVED, see roomplan-ios-scan.md

This phase shipped as originally scoped below (a photo-backdrop tracing
aid, no CV/native app/ML), but didn't survive live testing: a real phone
photo of a wall in a normal-sized room inevitably includes floor/ceiling/
perspective distortion, which looked wrong stretched onto a flat plane —
tried behind the 2D canvas first, then as a 3D wall texture once the 2D
version was correctly identified as a geometric mismatch (elevation photo
vs. top-down plan), neither held up. Removed entirely (2026-07-06).

The actual underlying want — accurate room geometry without manually
typing dimensions — is being pursued instead via the iPad Pro's LiDAR
Scanner and Apple's RoomPlan API (native iPadOS app, deferred until a Mac
is available to build it). Full research and the planned integration
shape: **`plans/roomplan-ios-scan.md`**.

<details>
<summary>Original scope (for reference — no longer built)</summary>

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

</details>

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
