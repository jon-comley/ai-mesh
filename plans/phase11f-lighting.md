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

### 6. Photo Colour Picker
A grab-box tool in the dashboard: load a photo (or a live screenshot), drag a selection rectangle over any region, and the average colour of that region is extracted entirely client-side via the Canvas API and sent to the selected lights as CIE XY. No server round-trip for the image — only the resulting XY values are transmitted. Useful for matching the palette of a painting, a sunset photo, or whatever's on screen. Future: live screen region capture (see Effects Engine below).

### 7. Light Games
The 4×2 colour spot grid is a natural game board. Designed for younger players.
- **Simon Says** — coordinator generates a random colour sequence, flashes each spot in sequence; player must repeat it by tapping the corresponding card in the dashboard (or pressing physical switch buttons once F7 ships). Difficulty scales with sequence length.
- **Colour Match** — LLM picks a "mystery" colour, splits it across two lights; player guesses which bulb matches a reference swatch shown in the UI.
- **Disco Party** — "dance!" intent triggers a choreographed random colour sweep across all spots; stops on "stop the disco".
- **Hot/Cold** — one light is secretly designated "it" by the coordinator; as the player moves (tracked by motion sensors from F7), lights shift warm (hot) or cool (cold) based on proximity. Room-scale treasure hunt.

**Room layout note:** the existing installation is a 3×3 grid — bottom row of 3 white-temperature-only bulbs, plus 4×2 colour spots. Colour commands must degrade gracefully: the CT bulbs ignore XY/hue fields and Z2M handles this transparently, but game logic should account for the asymmetry (e.g. Simon Says only plays on the 8 colour spots).

---

## Future: Effects Engine (F10)

The MQTT path through Z2M has ~100ms round-trip latency — acceptable for manual control, too slow for fluid effects at ≥10 Hz.

### Solar / Sunset Engine
Coordinator background task uses device location (lat/lon) + the `sunrise` or `spa` Rust crate to compute exact sunrise/sunset times and solar elevation angle throughout the day. Drives a smooth colour-temperature + brightness curve that tracks the real sky. Configurable per-group; presence-weighted when F7 switches are available.

*Status (2026-05-29):* Solar shipped in **F-Spatial** (full 3D dot-product weighting, room orientation, opening transmission). Sunset is formally planned in **F-Effects-2** as a curated effect alongside Sunrise, Candlelight, Aurora, and Breathing — see `plans/phase11f-effects-2.md`. Presence-weighting still awaits F7.

### Hue Entertainment Fast Path
Philips' Entertainment API sends colour commands over UDP at ~50 Hz, bypassing MQTT entirely. A future `capability_effects` crate would open a UDP stream to the Zigbee coordinator (SLZB-06, if it exposes such an interface) or a Hue Bridge (if one is added) for music sync, game sync, and reactive effects that feel instantaneous. Prerequisite: verify whether SLZB-06 + Z2M supports a lower-latency path at all before investing in the crate.

### Screen / Monitor Sync
Client-side screen capture via the browser's `getDisplayMedia` API (no Rust crate needed for MVP) → divide the captured frame into regions matching the physical light grid → compute average colour per region → stream XY commands to the corresponding spots. An Ambilight-style experience without proprietary hardware. Requires the fast path — MQTT at ~10 Hz is perceptible as lag on a moving image.

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

### F5 — Groups / Rooms
Wire Z2M groups as collapsible room cards.

- **Dashboard:** Group cards above individual device cards; group-level on/off, brightness, scene selection. Collapse/expand toggle.
- **Coordinator:** `POST /api/lights/group/{name}/command` routes to `zigbee2mqtt/{group}/set`; no new Rust struct needed — reuse `LightCommandBody`.
- **Agent:** No changes — Z2M handles group commands natively.
- **Graceful degradation:** CT-only bulbs in a mixed group silently ignore XY/hue fields.

### F6 — Scenes (basic)
Snapshot current state → save as named scene → one-tap recall.

- **Coordinator:** Store scenes as JSON in coordinator state file `{ name, devices: [(device_id, LightState)] }`; endpoints `POST /api/scenes/save` and `POST /api/scenes/recall`.
- **Dashboard:** "Save scene" button per group card, named scene list, one-tap recall, delete.

### F7 — Switches / Input Devices
New `capability_switches` crate.

- Subscribe to Z2M events for buttons, remotes, motion sensors, contact sensors.
- Emit `SwitchEvent { device_id, action }` to coordinator.
- Coordinator broadcasts `DashboardEvent::SwitchEvent`.
- Dashboard: live switch activity feed.
- Foundation for the Reactive Graph: switch event → trigger evaluation.

### F8 — Creative Features

#### Telemetry Lighting
Coordinator watches cluster events and maps them to lighting effects:
- GPU inference running → blue pulse
- Node offline → red flash
- Model loading → soft green fade
- Inference complete → single warm blink

Opt-in per effect, configurable target device/group.

#### Photo Colour Picker
Canvas API in the dashboard: load a photo, drag a grab-box selection, extract average colour of the selected region, send as CIE XY to the chosen light or group. No server-side image processing — only the XY result is transmitted. Future: live screen region via `getDisplayMedia`.

#### Intent-First Scenes
Text box in the Lighting panel. Input routes through the existing intent handler. LLM resolves "reading mode" or "make it cozy" into a `LightCommand` sequence using time of day, current room state, and known devices as context.

#### Circadian Rhythm Engine
Coordinator background task; `sunrise` or `spa` Rust crate computes solar elevation from lat/lon; smoothly transitions colour temp + brightness to match the real sky throughout the day; per-group "Follow sun" toggle. Presence-weighted: motion sensor data from F7 makes the engine room-aware, not house-wide.

#### Temporal Scene Composer
Visual timeline editor. Set device states at T=0, T+N minutes, T+M minutes. Coordinator executes transitions as a background task. Use cases: morning routine, movie night fade, bedtime sequence.

#### Reactive Graph (automation rules)
Plain-language rules saved in the coordinator. LLM parses each rule once at save time into a structured `{ trigger: Condition, action: LightCommand }` pair. Coordinator evaluates triggers on every switch/state event. No YAML, no subscriptions, no Home Assistant complexity.

#### 2D Room Layout
SVG canvas in the dashboard. Drag lights and sensors onto a simple floor plan (rooms as rectangles). Click a device on the map to control it. Layout persists in `localStorage`. Default template reflects the real installation: 3×3 grid, bottom row CT-only. Optional: show motion sensor coverage radii.

### F9 — Light Games
Interactive games using the 4×2 colour spot grid. See *The Innovations — Light Games* above for full descriptions. MVP: **Simon Says**. Coordinator generates sequences, manages game state, streams commands. Physical button input arrives via F7 switch events.

### F10 — Effects Engine
See *Future: Effects Engine* section above. Prerequisite: evaluate SLZB-06 + Z2M latency floor before committing to the fast-path crate architecture.
