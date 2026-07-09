# AV devices in the dashboard — where speakers, TVs, and soundbars live in the UI

Written 2026-07-09. Overview/direction plan only — no implementation
detail yet. Companion to `plans/audio-output-integration.md` (which built
the backend this UI would drive) and `plans/home-ui-redesign.md` /
`plans/phase11f-rooms.md` (which built the room cards and drag-and-drop
this could reuse).

## The problem

The Devices tab is a Zigbee inventory. Every row in it comes from a z2m
interview, has a `DeviceType` (light/sensor/cover/climate/switch), and
its room assignment writes to the registry's `devices` table. That model
now has neighbours that don't fit it at all:

| Thing | Transport | Identity today | Room binding today |
|---|---|---|---|
| pi2's HDMI out (→ Frame TV → soundbar) | mesh node backend | `pi2` + `hdmi` in `AUDIO_BACKENDS` | `room-audio-sink:<room>` = `pi2:hdmi` preference |
| pi2's Bluetooth out (→ Fishman amp, later per-room speakers) | mesh node backend | `pi2` + `bluetooth` | `room-audio-sink:<room>` = `pi2:bluetooth` |
| Samsung S701D soundbar | Wi‑Fi appliance | `soundbar-ip` preference | none (house-wide singleton) |
| Samsung Frame TV | Wi‑Fi appliance | `tv-ip` / `tv-mac` / `tv-token` preferences | none (house-wide singleton) |
| HA Voice PE puck | Wi‑Fi appliance | `VOICE_DEVICE_HOST` env on pi1 | `VOICE_PUCK_ROOM` env on pi1 |

Five things, four different identity schemes, three different room-binding
mechanisms, zero dashboard presence. The user-facing wish is simple:
**see all of these as devices and drop them into rooms**, the way Zigbee
devices already work.

Backend is fixable/negotiable — the decision this plan needs is the UI
shape. Three genuinely different directions below, then a recommendation.

---

## Option A — One inventory: everything is a device, transports are badges

Extend the existing Devices tab (and the room cards' drag-and-drop on the
Home tab) so *every* endpoint appears as a device row/chip, whatever its
transport. New rows like:

- 🔊 **Kitchen speaker** · `bluetooth via pi2` · [room ▾]
- 🖥 **Frame TV chain** · `hdmi via pi2` · [room ▾]
- 🎵 **S701D soundbar** · `wifi · 10.0.0.x` · [room ▾]
- 🗣 **Voice puck** · `wifi` · [room ▾]

A `transport` badge (zigbee / wifi / bluetooth / hdmi) replaces the
current implicit "everything is Zigbee". Grouped under a new **Speakers &
displays** category heading, exactly like the existing Lights / Sensors /
Switches sections. Dropping one into a room card on the Home tab works
identically to dropping a bulb — same gesture, same code path in
`rooms.js`, different write underneath (`room-audio-sink:<room>` instead
of the devices table).

- **Pro:** one mental model, one place to look; "drop into a room" comes
  almost free from the existing room-card drag-drop; smallest new-UI
  surface.
- **Pro:** naturally extends later (Cast targets, AirPlay, a second TV) —
  they're just more rows with different badges.
- **Con:** the Devices tab's verbs are Zigbee verbs — pair, unpair,
  delete, rename-in-z2m. None apply to a node backend or a LAN appliance,
  so the row actions must differ by kind (edit-IP for soundbar/TV,
  nothing destructive for node backends). Needs care to not look
  inconsistent.
- **Con:** stretches "device" to include things that are really *node
  capabilities* — pi2's HDMI out isn't a device you own, it's a port on a
  computer. Naming/iconography has to carry that or it reads oddly.

## Option B — Audio as its own domain: a routing view, not an inventory

Don't force audio into the device-inventory metaphor. Audio's real shape
is *routing* — "when a reply/announcement happens in room X, where does
the sound come out?" — so give it a routing-first panel (a section on the
Home tab, or its own tab):

- One row per **room**, showing its current audio chain:
  `Kitchen → 🔊 Fishman (bluetooth via pi2), fallback → 🗣 puck`.
- A palette of available **outputs** (every backend of every connected
  audio node, plus puck/soundbar/TV) to drag *onto* a room row.
- Per-row extras that an inventory can't express: a **test-play button**
  (speak "test" through exactly that chain), the fallback order, volume,
  and the broadcast set ("which sinks does 'announce to everyone' hit").
- Soundbar/TV appear here too, as outputs with their control surface
  (volume/mute/keys) attached — closer to a mixer channel strip than a
  device row.

- **Pro:** matches how the backend actually behaves (per-room sink →
  puck fallback chain, broadcast fan-out) — the UI *is* the routing
  table, so what you see is literally what will happen. Test buttons and
  fallback chains have an obvious home.
- **Pro:** room assignment isn't a dropdown afterthought; it's the whole
  point of the screen.
- **Con:** a second place that manages device-like things — splits the
  "where are my devices" answer across two tabs.
- **Con:** more novel UI = more new code; nothing reused from the
  devices/rooms drag-drop.

## Option C — Floor-plan-first: place speakers on the map like bulbs

The Home tab already has a per-room floor plan (2D SVG + 3D three.js)
where bulbs are physically placed. Extend the palette: drag a **speaker /
TV / soundbar glyph** onto the plan. Placement *is* assignment — the room
containing the glyph gets that sink, position is remembered like bulb
positions, and the 3D view renders a small speaker mesh.

- **Pro:** the coolest and the most consistent with where the dashboard
  has been heading (spatial rooms, sun modelling, 3D view). A placed
  speaker also future-proofs *spatial* audio decisions — "nearest sink to
  the sensor that triggered" needs coordinates, which only this option
  captures.
- **Pro:** zero ambiguity about singletons — the TV glyph sits where the
  TV physically is.
- **Con:** heaviest option; and the floor plan is currently a per-room
  editing canvas, so it still needs a list-level entry point ("which room
  do I open to place this?") — i.e. it half-depends on Option A or B
  existing anyway for discovery/inventory.
- **Con:** placement precision is meaningless today — the backend only
  uses room granularity. The extra fidelity is decoration until a
  spatial-audio feature consumes it.

---

## Recommendation: A now, B's best parts folded into the room cards, C later

Phased, not either/or:

1. **Option A as the base.** Everything appears in the Devices tab under
   a "Speakers & displays" section with transport badges, and becomes
   draggable onto Home-tab room cards exactly like bulbs. This answers
   "drop them into a room" with the least new UI and one mental model.
2. **Steal B's two killer features without building the full routing
   panel:** each **room card** on the Home tab grows a small speaker chip
   showing its assigned sink (tap → change / clear / test-play), and the
   assignment write is the fallback-aware one. The routing table stays
   visible where you already look at rooms, without a new tab.
3. **C when spatial audio earns it.** Once something actually consumes
   coordinates (nearest-sink routing, volume by distance), add speaker
   glyphs to the layout canvas. The Option A inventory rows become the
   palette source, so nothing is thrown away.

### Backend implication (deliberately brief — "back end can be fixed")

Whichever option wins, the dashboard needs one thing the backend doesn't
have yet: **a single endpoint that lists all AV endpoints uniformly**
(node backends from connected `Feature::Audio` nodes + configured
appliances + the puck), each with a stable id (`pi2:bluetooth`,
`appliance:soundbar`, …), display name, transport, online/offline, and
current room. Writes stay thin wrappers over what already exists
(`room-audio-sink:<room>` prefs, `soundbar-ip`/`tv-ip` prefs). Renaming
("Fishman amp" instead of "pi2:bluetooth") is a preference too. No new
tables required for phase 1; a unified endpoint registry can come later
if appliances multiply.

### Open questions for review

- Should the **puck** be room-assignable in the UI (it's currently
  `VOICE_PUCK_ROOM`, an env var on pi1 — moving that into a preference
  would make it UI-editable, one less env knob)?
- Singletons: is one soundbar / one TV an acceptable assumption for the
  UI's first cut, or model multiples from day one?
- Does "drop into a room" *replace* the room's sink or *append* to a
  fallback chain? (Backend currently: one sink per room, puck fallback is
  implicit.) The room-card chip in step 2 should probably show the
  implicit fallback so the behaviour isn't a surprise.
- Naming: "Speakers & displays"? "Audio & video"? "Outputs"? The section
  name shapes the mental model.
