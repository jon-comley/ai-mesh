# Phase 11F — Lighting OS

> Everyone else built a light switch you can control from your phone. We're building a lighting system that understands what's happening in your home — including inside your AI cluster — and responds to it.

---

## The Innovations

### 1. Telemetry Lighting
The room is the dashboard. Your cluster's operational state is physically visible without opening a browser. GPU busy → subtle blue pulse on the desk lamp. Node offline → brief red flash. Model loading → soft green fade. Inference complete → a single warm blink. No commercial system has ever tied infrastructure telemetry to ambient lighting because no commercial system runs AI inference in your home. This is uniquely ai-mesh.

### 2. Intent-First Adaptive Scenes
"Cozy" at 7am means something different than "cozy" at 10pm. Instead of static presets, scenes are prompts. You type "reading mode" in the panel and the LLM decides what that means *right now* based on time of day, which lights are already on, and what rooms are occupied. The scene definition lives in language, not in a saved brightness value. Hue has scenes. Nobody has intent-aware scenes.

### 3. The Temporal Scene Composer
Scenes that exist across time, not just at an instant. A morning routine that transitions over 40 minutes — warm red sunrise → cool white work light — defined visually on a drag-and-drop timeline. The coordinator executes the transitions as a background task. This is genuinely closer to theatrical lighting design than anything a consumer app offers.

### 4. The Reactive Graph
The coordinator becomes a lightweight automation state machine. Rules written in plain language: *"if no motion in the living room for 5 minutes and it's after sunset, fade to 10% over 2 minutes."* The LLM parses the rule once at save time into a structured trigger+action pair. No YAML, no Home Assistant complexity, no subscription.

### 5. Presence-Weighted Circadian
Not just "shift colour temp with the sun" — shift it *per room based on who's in it and what they're doing*. Motion sensor active in the study at midnight → keep it cool and bright. Same room empty → start the wind-down regardless of the global schedule. The switches crate feeds occupancy data into the circadian engine, making it room-aware rather than house-wide.

---

## Architecture Decisions (locked)

- **`capability_switches` is a separate crate.** Input devices (buttons, remotes, motion sensors, contact sensors) have an inverted event model — device → agent → dashboard — and mixing them into `capability_lighting` pollutes the LLM tool schema. Switches are triggers; lights are actuators.
- **Agent subscribes to `zigbee2mqtt/#` wildcard.** Filtering happens locally. No dynamic resubscription, no race conditions, new devices just work.
- **HTTP API shape: `POST /api/lights/{device}/command`.** One endpoint, one struct (`LightCommand`), one mental model. No REST-y explosion.
- **Scenes stored in coordinator state file.** No database. Scenes are small JSON blobs alongside the existing state.
- **Unified Lighting Model.** Every light is addressable by `{ on: bool, brightness: u8, color_temp: u16, color_xy: (f32, f32) }`. Bulbs that can't do colour silently ignore those fields.

---

## Sub-Phases

### F1 — Live State Feed (foundational)
The equivalent of `HealthUpdate` for lighting. Nothing else is possible without this.

- **Agent:** Subscribe to `zigbee2mqtt/#`; parse state payloads into `LightStateReport`; emit on every change.
- **Coordinator:** Maintain `LightSnapshot` (like `model_snapshot`); broadcast `DashboardEvent::LightingUpdate`; snapshot-on-connect.
- **Dashboard:** New `lighting.js`; show per-device: on/off indicator, brightness %, colour temp, colour swatch for RGB bulbs.

### F2 — Individual Device Controls
Now that state is live, commands can be sent with confidence.

- **Dashboard:** Toggle, brightness slider, colour temp slider, colour picker (RGB). Optimistic UI corrected by next `LightingUpdate`.
- **Coordinator:** `POST /api/lights/{device}/command` → `send_to_node(device.node_id, MeshMessage::LightCommand(cmd))`.
- **Agent:** Already supports `LightCommand` — no changes needed.

### F3 — Groups / Rooms
Zigbee2MQTT groups already exist. Wire them in.

- **Coordinator:** Extend registry to store groups; build `GroupSnapshot`; broadcast `DashboardEvent::LightingGroupsUpdate`.
- **Dashboard:** Collapsible room cards; group-level on/off, brightness, scene selection.
- **Agent:** No changes — Z2M handles group commands natively.

### F4 — Colour Picker
For RGB bulbs, done properly.

- **Dashboard:** HSL wheel or hue strip + saturation slider; convert HSL → XY on the client; live preview swatch.
- **Coordinator / Agent:** Already support `color_xy` — no changes needed.

### F5 — Scenes (basic)
Snapshot current state → save as named scene → one-tap recall.

- **Coordinator:** Store scenes per group `{ name, devices: [(device_id, LightState)] }`; endpoints `POST /api/scenes/save` and `POST /api/scenes/recall`.
- **Dashboard:** "Save scene" button, scene list, one-tap recall.

### F6 — Switches / Input Devices
New `capability_switches` crate.

- Subscribe to Z2M events for buttons, remotes, motion sensors, contact sensors.
- Emit `SwitchEvent { device_id, action }` to coordinator.
- Coordinator broadcasts `DashboardEvent::SwitchEvent`.
- Dashboard: switch activity feed.
- Coordinator: optional switch → scene mapping (the foundation for the Reactive Graph).

### F7 — Creative Features

#### Circadian Rhythm Engine
Coordinator background task; uses sunrise/sunset for location; smoothly transitions colour temp throughout the day; per-group "Follow sun" toggle. Presence-weighted: the switches crate feeds occupancy so the engine is room-aware, not house-wide.

#### Telemetry Lighting
Coordinator watches cluster events and maps them to lighting effects:
- GPU inference running → blue pulse
- Node offline → red flash
- Model loading → soft green fade
- Inference complete → single warm blink

Opt-in per effect, configurable target device/group.

#### Intent-First Scenes
Text box in the Lighting panel. Input routes through the existing intent handler. LLM resolves "reading mode" or "make it cozy" into a `LightCommand` sequence using time of day, current room state, and known devices as context.

#### Temporal Scene Composer
Visual timeline editor. Set device states at T=0, T+N minutes, T+M minutes. Coordinator executes transitions as a background task. Use cases: morning routine, movie night fade, bedtime sequence.

#### Reactive Graph (automation rules)
Plain-language rules saved in the coordinator. LLM parses each rule once at save time into a structured `{ trigger: Condition, action: LightCommand }` pair. Coordinator evaluates triggers on every switch/state event. No YAML, no subscriptions, no Home Assistant complexity.

#### 2D Room Layout
SVG canvas in the dashboard. Drag lights and sensors onto a simple floor plan (rooms as rectangles). Click a device on the map to control it. Layout persists in `localStorage`. Optional: show motion sensor coverage radii. This is where we beat Hue — their photo-based layout is fiddly and static.
