# Device auto-naming from model_id

Written 2026-07-07. Resumes work lost from an earlier session (uncommitted,
never written down). Companion to `plans/sensor-readout-and-completion.md`
and `plans/phase11f-lighting.md` — both list the hardware this names.

## Problem

Every paired device shows up in the dashboard as its raw hex `device_id`
(z2m friendly_name, which defaults to the ieee address until renamed) —
e.g. `0x001788010fa6772b`. There's already a manual rename path
(`PATCH /api/lights/{device}/name`, backed by the `device_names` SQLite
table via `Registry::set_device_name`), but nothing suggests a sensible
default, so every new device sits unnamed until someone renames it by hand.

## What we're building

Auto-assign a default name the moment a device is *successfully paired*
(z2m's `device_interview_successful` event, the only point at which the
device's model is known), using a curated model_id → product-line lookup.
The user can always override it afterwards through the existing rename
endpoint — this only ever writes a name once, on first successful
interview, and never touches a device that already has a custom name
(whether auto-assigned earlier or set by hand).

This is a live-pairing-time feature, not a one-off backfill: the 21
devices already on the network keep their current (hex) names unless
manually renamed. The user is deleting and re-pairing devices one at a
time specifically to exercise this path.

### Product-line catalog (from live z2m `bridge/devices`, 2026-07-07)

Pulled directly off the mesh (`mosquitto_sub -h 10.0.0.10 -t
'zigbee2mqtt/bridge/devices' -C 1 -W 10`) — everything currently paired is
Philips Hue (Signify) or SONOFF, nothing else:

| model_id | vendor / description | product line name | count seen |
|---|---|---|---|
| `LCG006` | Philips — Hue White and color ambiance GU10 spot LED w/ Bluetooth | `Hue GU10 Spot CCT/COL` | 8 |
| `LTA005` | Philips — Hue White Ambiance E27 filament screw globe | `Hue Filament Globe CCT` | 3 |
| `LCT010` | Philips — Hue White and Color Ambiance B22/E27 806lm | `Hue Color Ambiance Bulb` | 1 |
| `SNZB-02P` | SONOFF — Temperature and humidity sensor | `Sonoff Temp/Humidity Sensor` | 4 |
| `SNZB-03PR2` | SONOFF — motion sensor (numeric lux) | `Sonoff Motion Sensor` | 3 |
| `RDM002` | Philips — Hue Tap dial switch | `Hue Tap Dial Switch` | 1 |
| `ROM001` | Philips — Hue smart button | `Hue Smart Button` | 1 |

Numbering is per product line, always appended (even for the currently
single-instance lines — a second Tap Dial or test bulb should still get a
"1"/"2" split instead of a naming collision later). Order = whichever
device gets successfully interviewed first, i.e. pairing order — there's
no room/position data available to sort by instead.

## Where it hooks in

1. **`capabilities/zigbee/src/client.rs::parse_join_event`** — was only
   extracting `/definition/model` (the numeric retail SKU, e.g.
   `929003666501` — useless for lookup). Now prefers the top-level
   `model_id` field (the short code, e.g. `LCG006`) and falls back to
   `/definition/model` if absent. This also makes the live join-feed
   display (`devices.js`: `` `Paired: ${evt.model ?? name} ✓` ``) show a
   recognizable code instead of a SKU number, as a side effect.
2. **`coordinator/src/device_catalog.rs`** (new) — the model_id →
   product-line table above, plus the per-line numbering function.
3. **`coordinator/src/server.rs`**, `MeshMessage::ZigbeeJoin` arm — on
   `device_interview_successful` with a recognized `model_id`, if
   `device_names` has no entry yet for this `device_id`, compute the next
   name in that product line and persist it via
   `Registry::set_device_name`, then push the updated rooms/names
   snapshot (same pattern as `api::lights::rename_device`).

## Status

- [x] Live device inventory pulled and catalog table drafted (this doc).
- [ ] `parse_join_event` prefers `model_id` over `/definition/model`.
- [ ] `device_catalog` module (lookup + numbering).
- [ ] Auto-name hook in the `ZigbeeJoin` arm of `process_message`.
- [ ] Tests: parse_join_event model_id preference, catalog numbering,
      auto-name-on-interview (fresh device, already-named device, unknown
      model_id).
- [ ] Manual validation: delete + re-pair a device, confirm the dashboard
      shows the auto-assigned name without any rename call.
