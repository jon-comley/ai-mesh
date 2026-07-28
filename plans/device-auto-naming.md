# Device auto-naming from model

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
device's model is known), using a curated model → product-line lookup.
The user can always override it afterwards through the existing rename
endpoint — this only ever writes a name once, on first successful
interview, and never touches a device that already has a custom name
(whether auto-assigned earlier or set by hand).

This is a live-pairing-time feature, not a one-off backfill: the devices
already on the network at the time this was built keep their current
(hex) names unless manually renamed. The user is deleting and re-pairing
devices one at a time specifically to exercise this path.

### Correction (2026-07-07, after a live pairing test): `model_id` doesn't exist on this event

First cut of this feature keyed the catalog on `model_id` (the short code
z2m's `bridge/devices` full dump uses, e.g. `LCG006`), on the assumption
it was also present on the pairing event. A live re-pair of a GU10 spot
(`0x001788010fac846a`) proved that wrong — it interviewed successfully but
got no name. Captured the real `zigbee2mqtt/bridge/event` payload off the
z2m systemd journal on pi1:

```json
{"data":{"definition":{"description":"Hue White and color ambiance GU10 spot LED with Bluetooth","model":"929003666501","vendor":"Philips", "...exposes truncated..."},"friendly_name":"0x001788010fac846a","ieee_address":"0x001788010fac846a","status":"successful","supported":true},"type":"device_interview"}
```

No top-level `model_id` anywhere — `data` only ever carries `definition`
(vendor/model/description/exposes). `model_id` is exclusive to the richer
`bridge/devices` dump. `definition.model` is the only model identifier
this event ever has, and for most vendors (Philips here) it's a numeric
*retail SKU*, not the short code. Fixed by reverting
`parse_join_event` to read `/definition/model` only, and re-keying the
catalog on the actual SKU strings (below). SONOFF happens to use the same
short code for both `model_id` and `definition.model`, so those two
entries were already correct.

### Product-line catalog

`definition.model` value → product line, keyed on what the
`device_interview_successful` event actually carries (SKU for Philips,
short code for SONOFF — see correction above):

| `definition.model` | vendor / description | product line name | count seen |
|---|---|---|---|
| `929003666501` (LCG006) | Philips — Hue White and color ambiance GU10 spot LED w/ Bluetooth | `Hue GU10 Spot CCT/COL` | 8 |
| `8718696598283` (LTW013) | Philips — Hue white ambiance GU10 (CCT only, pre-Bluetooth) | `Hue GU10 Spot CCT` | 2 |
| `8719514392830` (LTA005) | Philips — Hue White Ambiance E27 filament screw globe | `Hue Filament Globe CCT` | 3 |
| `9290012574` (LCT010) | Philips — Hue White and Color Ambiance B22/E27 806lm | `Hue Color Ambiance Bulb` | 1 |
| `SNZB-02P` | SONOFF — Temperature and humidity sensor | `Sonoff Temp/Humidity Sensor` | 4 |
| `SNZB-03PR2` | SONOFF — motion sensor (numeric lux) | `Sonoff Motion Sensor` | 3 |
| `8719514440937/8719514440999` (RDM002) | Philips — Hue Tap dial switch | `Hue Tap Dial Switch` | 1 |
| `8718699693985` (ROM001) | Philips — Hue smart button | `Hue Smart Button` | 1 |
| `9290035639` (SOC001) | Philips — Hue Secure contact sensor | `Hue Secure Contact Sensor` | 2 |
| `EBF_RGB_Zm` | AwoX — LED with adjustable color temp + RGB strip, sold as EGLO connect.z Rovito-Z | `Eglo Rovito-Z Ceiling Light` | 1 |

Everything currently paired is Philips Hue (Signify) or SONOFF, nothing
else (originally surveyed via `mosquitto_sub -h 10.0.0.10 -t
'zigbee2mqtt/bridge/devices' -C 1 -W 10`).

Numbering is per product line, always appended (even for the currently
single-instance lines — a second Tap Dial or test bulb should still get a
"1"/"2" split instead of a naming collision later). Order = whichever
device gets successfully interviewed first, i.e. pairing order — there's
no room/position data available to sort by instead.

## Where it hooks in

1. **`capabilities/zigbee/src/client.rs::parse_join_event`** — extracts
   `/definition/model` from the `device_interview_successful` event data.
2. **`coordinator/src/device_catalog.rs`** — the `definition.model` SKU →
   product-line table above, plus the per-line numbering function.
3. **`coordinator/src/server.rs`**, `MeshMessage::ZigbeeJoin` arm — on
   `device_interview_successful` with a recognized model, if
   `device_names` has no entry yet for this `device_id`, compute the next
   name in that product line and persist it via
   `Registry::set_device_name`, then push the updated rooms/names
   snapshot (same pattern as `api::lights::rename_device`).

Related fixes made alongside this (same session, prompted by testing the
delete → re-pair workflow this feature needs):
`DELETE /api/lights/{device}` now also purges the device from the
dashboard's in-memory snapshots (`DashboardState::remove_device` — it
used to only clean the SQLite registry, so a deleted device kept showing
"Unassigned" forever), clears its `device_names` row (otherwise a
re-paired device silently keeps its old name and skips auto-naming), and
the z2m unpair request now sets `force: true` (otherwise a sleeping
battery sensor or a bulb off at the wall can silently fail to unpair).

## Status

- [x] Live device inventory pulled and catalog table drafted.
- [x] `parse_join_event` extracts `/definition/model`.
- [x] `device_catalog` module (lookup + numbering).
- [x] Auto-name hook in the `ZigbeeJoin` arm of `process_message`.
- [x] Tests: parse_join_event, catalog numbering, auto-name-on-interview
      (fresh device, already-named device, unknown model).
- [x] First manual validation (GU10 spot `0x001788010fac846a`) caught the
      `model_id` assumption being wrong — fixed, see correction above.
- [ ] Re-validate: delete + re-pair that same device again with the fix
      deployed, confirm the dashboard shows `Hue GU10 Spot CCT/COL 1`
      without any manual rename call.
