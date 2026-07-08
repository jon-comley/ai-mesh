# Multi-Domain Home: Sensors First, Local AI Voice as the Differentiator

## Context

The home is growing past lights (~7 blinds + aircon coming; sensors chosen as the first new domain — cheapest real hardware, and they feed HVAC logic later). Phase 11.8 already settled the navigation model to match Home Assistant/Apple: **rooms are the primary control surface; device-type views are for management** — no separate daily-driver "Lights tab vs Rooms tab". The backend seam (api/lights.rs vs api/rooms.rs) shipped in the domain split.

**The differentiator that anchors this plan: the local AI voice of the home.** Every serious platform makes you tap widgets; ai-mesh's natural-language control runs on *your own hardware* — private, subscription-free, already half-built (intents + chat + local models). The plan makes that the product's spine: every new domain lands with its LLM tools and context the same day it lands with widgets. Supporting differentiators phased in behind it: the sun-aware spatial engine (blinds driven by real sun geometry through your actual windows, not lux sensors), the hub-that-is-also-your-private-AI bundle, and the effects engine.

What's verifiably true today (from the coupling audit):
- **Already type-agnostic:** `room_devices`, `device_names`, `device_room_name_map`, `RoomRecord.device_ids`, MQTT plumbing/availability, the capability-crate structure, the HTTP api/ module seam.
- **Load-bearing lights assumptions:** registry `light_devices` inventory (no `device_type` column anywhere) + `light_states` blob typed `LightStateReport`; wire messages all lighting-named; `zigbee/discovery.rs` **discards `definition.exposes`** (the device class!); `client.rs parse_state_report` drops anything without light fields; `intent.rs build_device_context` + `tool_schemas_for_feature` are light-shaped; `DashboardEvent` is per-domain with only lighting variants.
- **Unsolved:** one pi1 Zigbee dongle, one MQTT stream — `ZigbeeClient` is privately owned by `LightingCapability`; a second zigbee-backed capability needs the client hoisted to a shared per-node service (its `broadcast::Sender<ZigbeeEvent>` is already fan-out-shaped).

## Phase A — Enabling refactor (no new hardware; everything after gets cheap)

1. **Typed device inventory.** New registry `devices` table `(device_id PK, node_id, device_type TEXT)` replacing the `light_devices` JSON blob (no-shims: drop the old table + `all_light_device_names` reshapes to `devices_of_type("light")` + `light_groups(node_id, groups)` keeps the lighting-only group concept). `delete_device` clears it.
2. **Discovery keeps the type.** `zigbee/discovery.rs DeviceInfo` gains `device_type`, classified from z2m's `definition.exposes` (light / sensor / cover / climate / unknown). `ZigbeeEvent::DeviceListUpdated` carries it.
3. **Shared Zigbee service.** Hoist `ZigbeeClient` construction out of `LightingCapability::start` to one per-node instance (agent builds it when any zigbee feature is enabled); capabilities `.subscribe()` and filter events by device class. One MQTT connection, N domain consumers.
4. **Wire rename** (WIRE_VERSION bump, coordinated deploy): `LightDeviceListReport` → `DeviceListReport { node_id, devices: Vec<DeviceEntry { id, device_type }>, groups }`.
5. **Feature enum.** Replace raw feature strings (`"lighting"`, `"reaper"`, `"llm"`, incoming `"sensors"`) with a shared enum — 11.8 deferred this until enough variants existed; sensors is the trigger.

**Migration & deploy note (settled up front):** no data migration and no hard
reset. Rooms, scenes, device names, and positions live in separate tables the
refactor never touches. The `light_devices` inventory being dropped is
*derived* data — z2m republishes the full device list on every connect
(retained `bridge/devices` topic), so the new `devices` table populates
itself the first time the lighting node connects, exactly as the old blob did
at startup. The wire bump follows the established convention: fail-fast
serde, one coordinated deploy of coordinator + all agents, no dual-format
window (same playbook as wire v3 and v4).

**ZigbeeClient lifecycle (settled up front):** capabilities are compile-time
features, so the shared client's lifetime is simply the agent process
lifetime — built once at startup when any zigbee feature is enabled, no
dynamic subscribe/teardown protocol needed. Fan-out uses the existing bounded
`tokio::broadcast`, whose semantics already isolate a slow subscriber (it
lags and drops oldest events; it can never block sibling capabilities or the
MQTT poll loop).

## Phase B — capability-sensors (first real second domain)

1. New `capabilities/sensors` crate (thin sibling of lighting): consumes shared ZigbeeClient events, parses temp/humidity/battery/occupancy/contact (`parse_sensor_report` beside the light parser; same `/state` topics already subscribed), forwards `MeshMessage::SensorState(SensorReport { node_id, device_id, temperature, humidity, battery, occupancy, contact, online })`.
2. Registry `sensor_states` table mirroring the `(device_id, node_id, json)` shape of `light_states`; accessor pair paralleling the lighting ones.
3. `DashboardEvent::SensorUpdate` + `sensor_snapshot` in state.rs, replayed on WS connect like lighting.
4. `api/sensors.rs` — the second domain module, exactly per the api/mod.rs recipe (read-only: list, history later).
5. Fold in the deferred scene-recall-through-lights-primitives cleanup (its trigger was "first new device domain").
6. Agent `--features sensors` on pi1; provision docs.

## Phase C — The local AI voice grows senses (anchor differentiator)

1. **Sensor tools + context**: `"sensors"` arm in `tool_schemas_for_feature` (`get_climate { room? }` returning temp/humidity/motion/contact; answered from the coordinator's snapshot — no node round-trip since sensors are read-only); `build_sensor_context` injecting per-room lines ("Living Room: 21.4°C, 47% RH, motion 3m ago") beside the device context.
2. **Multi-command chat** (old chat-roadmap item 7): "turn off the kitchen lights and tell me the bedroom temperature" — parse/execute multiple tool calls per turn.
3. **Room-aware phrasing everywhere**: intent target resolution already maps room names; extend to sensors ("is the office warm?").
4. **Voice input, crawl phase started 2026-07-08** — see `plans/voice-assistant-integration.md`. Home Assistant Voice PE hardware talks directly to a new `capability-voice` (ai-mesh plays the role real Home Assistant normally would), no HA server involved. Proved live: wake word → real captured audio clip round-trip. STT/intent/TTS wiring (the "no cloud account, nothing leaves the house" story extended to speech) is the next phase, not yet started.

## Phase D — UI: Home becomes the home — ✓ shipped 2026-07-04

1. ~~Rename the Lighting tab → Home~~ **Done.** Room cards render mixed-domain members: `devicesMap` (state.js) entries gain `device_type` (`'light'`/`'sensor'`); the card picks the widget — existing light controls for lights, a read-only readout strip for sensors. RoomsUpdate/membership needed no code change, confirmed — it was already untyped ids.
2. ~~One single Devices tab~~ **Done, including the follow-up prompt.** Pair Device (permit-join + live join feed) moved here wholesale from the Home panel — it had briefly landed on the Home/Lighting panel in the pairing-feature slice before this phase; the plan always intended the Devices tab as its real home. Inventory grouped by type (Lights / Sensors) with rename, delete, and room-assignment via a dropdown (simpler than drag-and-drop for a flat management list — the Home tab's drag-and-drop stays as the primary lights UX). Battery shows via the sensor widget's existing readout formatting. A first pass shipped this without the "assign to room" follow-up prompt the plan calls for above — caught on review and added: a successful interview now shows an inline room picker right in the join feed line (`buildRoomSelect`, shared with the Devices tab's own row pickers), replaced with a plain confirmation once chosen.
3. ~~One widget component per type~~ **Done, with a scope note.** Sensors: one true shared widget (`buildSensorCard` in the new `devicewidgets.js`), used identically by Home room cards and the Devices tab — sensors have no controls either place, so there's nothing to duplicate. Lights: the Home tab keeps its rich interactive controls (`buildLightControls`, unchanged); the Devices tab intentionally uses a lighter read-only status line instead of embedding full sliders into an inventory list — these are genuinely different concerns (live control vs. management), not the same logic built twice.

**Fallout worth knowing about:** `room.device_ids` was already untyped before this phase (any id, no type marker) but had only ever held light ids in practice. Making it actually hold a sensor id for the first time surfaced four latent spots in rooms.js that assumed every room member was a light — the drag-ghost's "N bulbs" count, the scenes "all-paused" member-count check, the On/Off/brightness `empty`-room gate, and `sendRoomCommand`'s optimistic-update loop (which would have stamped fake `on`/`brightness` fields onto a sensor's cached state). All four now filter to `device_type !== 'sensor'` first. Also excluded sensors from the client's `inferZigbeeStatus` heuristic — a battery sensor's much longer offline timeout would have made "all devices offline ⇒ bridge down" considerably less reliable once sensors were counted alongside lights.

`lighting.js` was deleted outright rather than migrated: `dashboard.js` had unconditionally called `lighting.setRoomsActive()` since rooms.js took over the panel, so its own flat-list renderer (`render`/`patchCards`/the drag-reorder machinery) was already 100% dead code — confirmed by checking every call site before deleting, not assumed.

## Phase E/F — Blinds, then HVAC (hardware-gated outlines)

- **E (blinds/cover):** `capability-blinds` + `CoverState`/`CoverCommand` + `api/blinds.rs` + position/tilt widget — all seams exercised by B/D make this mechanical. **Differentiator #2 rides here:** the solar engine already knows sun azimuth/elevation and each room's openings/orientation — blinds that track actual sun geometry ("shade the sofa at 4pm without a lux sensor"), and effects/scenes gain covers.
- **F (HVAC/climate):** `capability-hvac` consuming sensor data (B) for room-temperature control; solar-gain anticipation from the same spatial model.

## Differentiation summary (what draws users, in order)

1. **"Talk to your house — and it never leaves your house."** Local-LLM natural language as a first-class control surface: no cloud account, no subscription, no audio/text exfiltration. Phases B+C make it *smarter than the widgets* (sensors answer questions widgets can't).
2. **Sun-aware spatial home** — the 3D layout + solar engine driving real-geometry automation (E/F). No mainstream platform models the sun through your actual windows.
3. **One mesh, two products** — the hub that runs your home also serves your private OpenAI-compatible assistant.
4. **Effects engine** — ambience (aurora/solar/candlelight) beyond stock offerings.

## Verification

- Phase A: full test suite (rename sweep compiler-driven); live regression `just intent "turn test_bulb on"`, dashboard rooms unchanged.
- Phase B: pair one temp/humidity + one motion sensor; `SensorState` visible in registry + dashboard snapshot; availability + battery reported. **Exceeded (2026-07-05):** all 7 paired and reporting (4× SNZB-02P temp/humidity, 3× SNZB-03P R2 motion) — see `plans/sensor-readout-and-completion.md` Part 2 for what's still unexercised (restart-survival, offline-dim, unpair).
- Phase C: `just intent "what temperature is the living room?"` answers from real sensor data; multi-command intent executes both actions.
- Phase D: room card shows light controls + sensor strip together; Devices view lists both domains; no browser on WSL2 — REST/WS contract via curl, visual check on phone (pi1:9001).

## Sequencing / effort

A and B are one arc (A is pointless alone; B proves it) — the next big build. C is small-medium and lands immediately after B (same files the audit mapped). D is frontend-scoped and can trail B/C. E/F wait for hardware. Roadmap: this plan supersedes the "(Design)" status of Phase 11.8 — copy to `plans/multi-domain-home.md`, mark 11.8 as "plan ratified, executing", link the phases.
