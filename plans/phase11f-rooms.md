# Phase 11F — Rooms

> Rooms are the first thing every home lighting system gets wrong. They're either a side-effect of device discovery, a Z2M group, or a metadata tag. We're treating them as first-class spatial objects the user directly controls — create, name, populate, and rearrange without touching a config file.

---

## Vision

The Lighting panel becomes a spatial controller, not a device list. A user opens it and sees:

1. An **Unassigned** strip — any bulb not yet placed in a room lives here.
2. **Room cards** — user-created, each containing its member devices inline.
3. A **drag-and-drop** interaction: pick up a bulb and slide it into a room (or between rooms, or back to Unassigned). One gesture. No modals. No save button.
4. Room-level controls — on/off, brightness, colour temp — that fan out to every device inside.

This is the UI Hue should have built. It's spatial, immediate, and fully local.

---

## What this is NOT

- **Not Z2M groups.** Rooms are stored in the coordinator's own SQLite database. They are independent of any Zigbee concept. A room works for Zigbee bulbs today, WiFi bulbs tomorrow, and Matter devices next year.
- **Not a static preset.** Rooms are live — a device added to a room starts responding to room commands immediately.
- **Not a mirror.** Z2M's `all` group and similar constructs remain wired for intent routing (`LightTarget::Group`) but are **not shown in the Rooms UI**. Rooms are the UX layer; Z2M groups are the routing layer.

---

## Data Model

### SQLite schema (additions to `coordinator/src/registry.rs`)

```sql
CREATE TABLE IF NOT EXISTS rooms (
    id       TEXT PRIMARY KEY,   -- UUID v4
    name     TEXT NOT NULL,
    position INTEGER NOT NULL DEFAULT 0  -- display order in the panel
);

CREATE TABLE IF NOT EXISTS room_devices (
    room_id   TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,     -- Z2M friendly name, e.g. "test_bulb"
    PRIMARY KEY (room_id, device_id)
);
```

`ON DELETE CASCADE` means deleting a room automatically cleans up all its device memberships.

### Rust types (new, in `coordinator/src/registry.rs`)

```rust
#[derive(Debug, Clone)]
pub struct RoomRecord {
    pub id: String,
    pub name: String,
    pub position: i64,
    pub device_ids: Vec<String>,
}
```

### Registry methods (new)

```rust
fn list_rooms(&self) -> Vec<RoomRecord>
fn create_room(&mut self, name: &str) -> RoomRecord          // generates UUID
fn delete_room(&mut self, id: &str)
fn rename_room(&mut self, id: &str, name: &str)
fn set_room_devices(&mut self, id: &str, device_ids: &[&str])
fn add_device_to_room(&mut self, room_id: &str, device_id: &str)
fn remove_device_from_room(&mut self, room_id: &str, device_id: &str)
fn get_room_for_device(&self, device_id: &str) -> Option<String>  // returns room_id
```

---

## WebSocket Event

```rust
DashboardEvent::RoomsUpdate {
    rooms: Vec<RoomInfo>,
}
```

```rust
#[derive(Clone, Serialize)]
pub struct RoomInfo {
    pub id: String,
    pub name: String,
    pub position: i64,
    pub device_ids: Vec<String>,
}
```

Pushed:
- On WS connect (snapshot-on-connect, same pattern as `LightingUpdate`)
- On room create / delete / rename
- On device add / remove

`DashboardState` gets:
- `room_snapshot: Mutex<Vec<RoomInfo>>`
- `push_rooms_update(rooms: Vec<RoomInfo>)`
- `get_room_snapshot() -> Vec<RoomInfo>`

---

## HTTP API

All endpoints share the existing `?token=` auth pattern.

### Create room
```
POST /api/rooms
Body: { "name": "Living Room" }
Response 201: { "id": "uuid" }
Response 400: name empty
```

### Delete room
```
DELETE /api/rooms/{id}
Response 204
Response 404: room not found
```

### Rename room
```
PATCH /api/rooms/{id}/name
Body: { "name": "Dining Room" }
Response 204
Response 400: name empty
Response 404: room not found
```

### Modify membership (add or remove devices)
```
PATCH /api/rooms/{id}/devices
Body: { "add": ["device_id_1"], "remove": ["device_id_2"] }
Response 204
Response 404: room not found
```

When a device is added to a room via this endpoint, the coordinator also removes it from any other room it was in — a device can only be in one room at a time.

### Room-level command
```
POST /api/rooms/{id}/command
Body: { "action": "on" }  (same LightCommandBody as device commands)
Response 204
Response 404: room not found
Response 503: one or more nodes not connected (best-effort; still sends to reachable nodes)
```

Fan-out: coordinator iterates `room.device_ids`, calls `get_node_for_device()` for each, groups by node, sends one `LightCommand` per device. CT-only bulbs silently ignore colour commands (Z2M handles this).

---

## Dashboard UI — `rooms.js`

New ES module: `coordinator/src/http/static/rooms.js`

`lighting.js` becomes the **device renderer** (cards, sliders, colour picker). `rooms.js` becomes the **layout and grouping layer** that wraps device cards.

### Panel structure

```
[ + New Room ]

┌─ Unassigned ─────────────────────────────────────────────────────────┐
│  [Test Bulb ▦]  [Desk Lamp ▦]  (draggable device chips)              │
└──────────────────────────────────────────────────────────────────────┘

┌─ Living Room ──────────────────────────────────── [rename] [delete] ─┐
│  [On] [Off]  ───── Brightness ─────  ───── Colour Temp ─────         │
│                                                                       │
│  [Test Bulb ✕]  [Ceiling Spot ✕]   ← member device cards            │
│                                                                       │
│  ┄ drop bulbs here ┄                                                  │
└──────────────────────────────────────────────────────────────────────┘

┌─ Bedroom ──────────────────────────────────────── [rename] [delete] ─┐
│  ...                                                                  │
└──────────────────────────────────────────────────────────────────────┘
```

### Interactions

| Gesture | Action |
|---|---|
| Drag device chip from Unassigned → room | `PATCH /api/rooms/{id}/devices { add: [device_id] }` |
| Drag device chip between rooms | Remove from old room, add to new room (single PATCH call or two) |
| Drag device chip out of room → Unassigned | `PATCH /api/rooms/{id}/devices { remove: [device_id] }` |
| Click room On/Off | `POST /api/rooms/{id}/command { action: "on"/"off" }` |
| Room brightness slider change | `POST /api/rooms/{id}/command { action: "brightness", value: N }` |
| Room colour temp slider change | `POST /api/rooms/{id}/command { action: "color_temp", value: N }` |
| Click `+` / type name / confirm | `POST /api/rooms { name }` |
| Click rename, edit, confirm | `PATCH /api/rooms/{id}/name { name }` |
| Click delete | `DELETE /api/rooms/{id}` (devices move back to Unassigned) |
| Click `✕` on device in room | `PATCH /api/rooms/{id}/devices { remove: [device_id] }` |

### Drag-and-drop algorithm

Reuse the same HTML5 DnD engine already in `lighting.js` / `models.js` — no new library needed.

```
dragstart (device chip):
  dragSrc = { deviceId, fromRoomId | "unassigned" }
  e.dataTransfer.effectAllowed = 'move'

dragover (room card or Unassigned strip):
  e.preventDefault()
  highlight drop target

dragleave:
  remove highlight

drop (on room card):
  if dragSrc.fromRoomId != targetRoomId:
    if dragSrc.fromRoomId != "unassigned":
      PATCH /api/rooms/{dragSrc.fromRoomId}/devices { remove: [deviceId] }
    PATCH /api/rooms/{targetRoomId}/devices { add: [deviceId] }

drop (on Unassigned):
  if dragSrc.fromRoomId != "unassigned":
    PATCH /api/rooms/{dragSrc.fromRoomId}/devices { remove: [deviceId] }
```

The coordinator responds to each PATCH by broadcasting `RoomsUpdate`, which triggers a re-render. Optimistic UI is not needed — the round-trip is fast enough on LAN.

### Inline rename UX

Click the room name → it becomes a focused `<input>` in place. Enter or blur confirms. Escape cancels.

```javascript
nameEl.addEventListener('click', () => {
  const input = document.createElement('input');
  input.value = nameEl.textContent;
  nameEl.replaceWith(input);
  input.focus();
  input.addEventListener('keydown', e => {
    if (e.key === 'Enter') confirmRename(input.value);
    if (e.key === 'Escape') render();
  });
  input.addEventListener('blur', () => confirmRename(input.value));
});
```

### Device chip (compact card in Unassigned strip)

Not the full device card — a compact chip showing just the friendly name and current on/off colour. Clicking it opens the full device card inline (stretch goal for F-Rooms-3; MVP just shows the name).

---

## Relationship with existing `lighting.js`

`lighting.js` currently renders all devices as full cards in a flat list. After Rooms:

- **Assigned devices** are rendered as compact chips inside their room card. Clicking a chip expands the full controls inline (or in a drawer — TBD in F-Rooms-3).
- **Unassigned devices** are rendered as compact chips in the Unassigned strip.
- The flat device card list is **removed** from the default view. It can be toggled back via a "Show all devices" link for diagnostics.

The existing device control functions (`wireControls`, `sendCommand`, colour picker logic) remain in `lighting.js` and are reused inside room cards.

---

## Z2M groups and intent routing

The current F5 Z2M group card (`all`) is **hidden from the Rooms UI** once rooms.js is loaded. The API endpoint (`POST /api/lights/group/{name}/command`) remains active — it is still used by the LLM intent handler for "turn all bulbs off" commands. The `all` group is not deleted; it just has no UI representation.

When the intent handler routes `Group("all")`, it uses the existing Z2M group mechanism. When it routes to a named room (future), it will use the Rooms API fan-out. These are parallel mechanisms and do not conflict.

---

## Implementation sub-phases

### F-Rooms-1 — Coordinator storage + WS event
- Add `rooms` and `room_devices` tables to `init_schema()`
- Implement `list_rooms`, `create_room`, `delete_room`, `rename_room`, `add_device_to_room`, `remove_device_from_room`, `get_room_for_device` on `Registry`
- Add `RoomsUpdate` to `DashboardEvent`
- Add `RoomInfo` struct, `room_snapshot`, `push_rooms_update`, `get_room_snapshot` to `DashboardState`
- Wire snapshot-on-connect in `ws.rs`
- Tests: CRUD operations, cascade delete, device uniqueness across rooms

### F-Rooms-2 — HTTP API
- Implement all five endpoints in `api.rs`
- Each mutating endpoint calls `push_rooms_update` after the DB write
- `POST /api/rooms/{id}/command` fans out to each device using `get_node_for_device`
- Tests: create/delete/rename, membership add/remove, command fan-out, 404s, 401s

### F-Rooms-3 — Dashboard UI
- New `rooms.js` ES module
- `RoomsUpdate` handler, `render()` function
- Unassigned strip + room cards with drag-drop
- Inline rename
- On/off + brightness + CT sliders per room
- `✕` remove button on device chips
- `+ New Room` button + inline name input
- Update `dashboard.js` to import `rooms.js` and route `RoomsUpdate`
- Update `lighting.js` to suppress flat device list when rooms module is active
- Update `mod.rs` to serve `rooms.js`
- CSS: room card, device chip, Unassigned strip, drop-target highlight, drag-over state

---

## What does NOT change

- `LightTarget::Group(String)` and the `POST /api/lights/group/{name}/command` endpoint — kept for intent routing
- `LightingUpdate` WS event and `lighting.js` device controls — kept, reused inside room cards
- The agent, shared crate, and wire protocol — no changes needed; room fan-out is pure coordinator logic

---

## Open questions (deferred to implementation)

1. **Multi-room membership:** MVP enforces one device → one room. Future: allow a device in multiple rooms (e.g. a hallway light shared between "Hall" and "Ground Floor"). Deferred to F-Rooms-5.
2. **Room ordering:** `position` column exists. MVP renders rooms alphabetically. Drag-to-reorder rooms (not just devices) deferred to F-Rooms-4.
3. **Compact chip vs inline full card:** MVP shows compact chips in the Unassigned strip. Clicking to expand the full card is F-Rooms-4.
4. **Room colour picker:** Room-level colour (XY) command is included in the fan-out path but no colour picker UI in the room card for MVP. Deferred to F-Rooms-4.
5. **Persist unassigned order:** Device ordering within the Unassigned strip uses `localStorage`, same pattern as existing drag-to-reorder in the flat list.
