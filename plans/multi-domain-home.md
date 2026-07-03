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
4. Marker for later (not this phase): voice input via local whisper on the mesh — the "no cloud account, nothing leaves the house" story extended to speech.

## Phase D — UI: Home becomes the home

1. Rename the Lighting tab → **Home**. Room cards render mixed-domain members: `devicesMap` entries gain `device_type`; card picks the widget — existing light controls for lights, a compact climate/motion readout strip for sensors. (RoomsUpdate/membership needs no change — it's already untyped ids.)
2. **One single Devices tab from day one** (amends 11.8's per-type-tabs-then-hub sequencing — Zigbee decides this for us: **pairing is bridge-wide**, permit-join accepts whatever announces, so "add device" can't live on a per-type tab). The tab holds: a Pair Device flow (permit-join + a live "joined: <model>" feed — this is what the long-deferred `bridge/event` subscription is for), and the device inventory grouped by type below it (rename, remove, battery, room assignment). Phase A's `exposes` classification means a new device lands in the right section automatically, with "assign to room" as the follow-up prompt.
3. One widget component per type, shared between Home cards and the Devices view (11.8's "never build control logic twice").

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
- Phase B: pair one temp/humidity + one motion sensor; `SensorState` visible in registry + dashboard snapshot; availability + battery reported.
- Phase C: `just intent "what temperature is the living room?"` answers from real sensor data; multi-command intent executes both actions.
- Phase D: room card shows light controls + sensor strip together; Devices view lists both domains; no browser on WSL2 — REST/WS contract via curl, visual check on phone (pi1:9001).

## Sequencing / effort

A and B are one arc (A is pointless alone; B proves it) — the next big build. C is small-medium and lands immediately after B (same files the audit mapped). D is frontend-scoped and can trail B/C. E/F wait for hardware. Roadmap: this plan supersedes the "(Design)" status of Phase 11.8 — copy to `plans/multi-domain-home.md`, mark 11.8 as "plan ratified, executing", link the phases.
