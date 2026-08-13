# REAPER Plugin Stack & FX Automation

Curated third-party plugin list for the ai-mesh REAPER setup, weighted to the user's actual
workflow — **layering vocal & guitar takes** — with UK pricing and, crucially, **how each
plugin can be driven by the LLM / daemon bridge**.

See [`reaper.md`](reaper.md) for the integration itself (transport, structured track tools,
the ReaScript daemon bridge). This doc is about the *plugins* that run inside REAPER and the
FX-automation layer they imply.

---

## The automation reality

None of these plugins expose their own API — no REST, no OSC, no scripting interface. (The
vendor research is explicit about this for Guitar Rig, GTR ToolRack, and plugins in general.)
Control is **only** via REAPER's `TrackFX_*` ReaScript functions — which is exactly the lane
the daemon bridge already runs Lua in. So automation is uniform across every plugin and
extends the structured-tool pattern already in `coordinator/src/intent.rs`.

| ReaScript function | Purpose | Our use |
|---|---|---|
| `TrackFX_AddByName(track, name, false, -1)` | insert an FX/instrument on a track | "add Pro-Q to the vocal track" |
| `TrackFX_SetPreset(track, fx, "name")` | recall a named preset | **highest leverage** — "make it spacious" → Valhalla preset |
| `TrackFX_GetNumParams` + `TrackFX_GetParamName` | enumerate params (names↔indices) | param **discovery** — without it the LLM can't know "param 3 = threshold" |
| `TrackFX_SetParam` / `GetParam` (0.0–1.0 normalised) | set/read a single param | per-knob control; read meters (Youlean LUFS) |
| `TrackFX_GetFXName` | name of an FX at a chain index | resolve a plugin by name → index (never trust raw indices) |
| `TrackFX_SetEnabled` | bypass / enable | "bypass the reverb" |

**Consequence for design:** preset recall is the cheap, reliable win — it works with zero
param knowledge, so it's the headline path for small models. Per-param control needs a
discovery step. Some plugins are read-only to us (meters); one (Melodyne) isn't meaningfully
automatable by us at all (see below).

**LLM value** ratings below mean:

- **High** — preset-driven, stable, predictable (preset recall, no discovery needed).
- **Medium** — param-driven, needs a discovery step to map names↔indices.
- **Low** — creative instrument or read-only meter; not tracking-critical.
- **None** — not meaningfully automatable by us (offline / ARA tools).

---

## Plugin tiers

Three cumulative budget tiers. Priority within each is weighted to vocal + guitar tracking.

### Tier 1 — Free-first (the foundation)

| # | Plugin | Role (vocal/guitar) | Price | LLM value | Automation approach |
|---|---|---|---|---|---|
| 1 | **ReaPack + SWS/S&M** | enabler — package manager + extra ReaScript functions; installs the community plugins below | Free | — | Foundation, not automated itself. SWS also expands our own ReaScript surface. |
| 2 | **Analog Obsession** (LA-2A/1176 clones — *Fetish*, *LaLa* — + channel strips) | vocal & guitar compression / colour | Free | High | Few clear params (peak reduction, gain, output) + factory presets → easy preset recall and param sets. |
| 3 | **TDR Nova** (free) | dynamic EQ — de-mud vocals, tame guitar resonance | Free | High | Rich params + presets; preset recall ("clean up the vocal") is the clean LLM path. GE upgrade (6 bands) £50–60. |
| 4 | **Valhalla Supermassive** | lush reverb / delay on vocals & guitar | Free | High | Named modes/presets (Gemini, Andromeda…) via `SetPreset` = ideal LLM target ("make it spacious"); mix/feedback as params. |
| 5 | **Youlean Loudness Meter** (free) | LUFS / true-peak metering for delivery | Free | Low (read-only) | `GetParam` integrated LUFS / true peak → enables an AI "is this Spotify-ready (−14 LUFS)?" check. Pro (streaming presets, export) £35–40. |
| 6 | **Spitfire LABS** | free virtual instruments (pads, strings, piano) for composing | Free | Low | Insert as instrument + drive MIDI CC1/CC11; instrument choice not param-exposed, so low automation fit. |

> **Gap at this tier:** no guitar amp sim. Free amp sims exist via ReaPack, but the curated
> amp picks are paid (Tier 2/3). Worth being aware of for guitar tracking.

### Tier 2 — Free + key paid (small spend, big leverage)

Adds to Tier 1:

| # | Plugin | Role | Price (UK) | LLM value | Automation approach |
|---|---|---|---|---|---|
| 7 | **FabFilter Pro-Q 4** | surgical + dynamic EQ — the headline mixing EQ for vocals & guitar | ~£126–127 retail / £149 direct | High | Rich params + extensive presets; the flagship automation target (preset recall + per-band params via discovery). |
| 8 | **Waves GTR ToolRack** | budget guitar amp/cabinet/pedal sim — fills the Tier 1 amp gap | £25–40 on sale (£120–180 list) | Medium | Per-stomp params (9 continuous + 1 toggle per slot) via DAW automation + MIDI Learn; no clean preset-name API, so param-level control after discovery. |
| 9 | **Melodyne Essential** | vocal pitch correction (monophonic) — gold standard | £79–89 | None | **Caveat:** ARA / offline note editing — no ReaScript path into its note grid. We can insert it, but cannot "auto-tune via LLM". A manual tool, not an automation target. |

### Tier 3 — Full stack (premium)

Adds to Tier 2:

| # | Plugin | Role | Price | LLM value | Automation approach |
|---|---|---|---|---|---|
| 10 | **Guitar Rig 7 Pro** | premium modular guitar amp / FX | ~£179 | Medium | **Macros 1–16 are the stable automation IDs** (effect params are dynamic per-preset) — map LLM control to macros via `SetParam`. |
| 11 | **iZotope Ozone** (Standard) | all-in-one mastering suite (AI assistant) | ~$399 perpetual / iZotope Plus $12.50/mo | Medium | Params + presets automatable; its internal "Master Assistant" AI is not scriptable by us. Higher value for mastering than tracking. |
| 12 | **Xfer Serum** | wavetable synth | ~£145–150 (£189 list) / Splice £9.99 × 25 mo | Medium | Rich params/presets, but **lowest priority** for a vocal/guitar tracker — listed for completeness. |

> Melodyne upgrade path (if polyphonic / guitar tuning is ever wanted): Assistant £175–194
> (DNA polyphonic), Editor £289–349, Studio £533–599 — all still **None** for our automation.

---

## Why not X? (considered and left out)

- **Free amp sims (ReaPack):** usable, but a real quality gap vs GTR ToolRack / Guitar Rig
  for guitar tracking — hence the Tier-1 amp gap is flagged rather than papered over.
- **Waves Tune (classic):** same offline / graphical limitation as Melodyne (**None** for
  us). *Tune Real-Time* is automatable, but it's paid and Melodyne Essential already covers
  the monophonic vocal case — so neither earns a slot.
- **Neural DSP:** controllable via params, but CPU-heavy and paid per-suite with no
  preset-name advantage over Guitar Rig macros — not worth it for this workflow.
- **FabFilter Pro-C:** Analog Obsession's free LA-2A / 1176 clones cover tracking compression
  well enough; Pro-C is a "nice to have", not a requirement.

---

## Natural-language → automation

Why preset recall is the headline path for small models — these intents map cleanly:

| Intent | Maps to |
|---|---|
| "Make the vocal airy" | Pro-Q preset + high-shelf param tweak |
| "Give the guitar some space" | Valhalla Supermassive preset (`SetPreset`) |
| "Clean up the vocal" | TDR Nova preset |
| "Make it Spotify-ready" | Youlean integrated-LUFS read (`GetParam`), report vs −14 LUFS |
| "Add a bit of grit to the guitar" | Guitar Rig macro (`SetParam` on macro 1–16) |

---

## Proposed FX-automation tools (future work)

A generic FX-control layer on the existing structured-tool pattern (builders in
`coordinator/src/intent.rs`, schemas in the tool list, run via the daemon bridge). Mirrors
`build_add_track_lua` / `build_set_tempo_lua`:

- ✓ `reaper_add_fx` — `TrackFX_AddByName` on a named track (resolves the track by name first,
  reusing the title-case / name-match helpers in `intent.rs`).
- ✓ `reaper_list_fx` — `TrackFX_GetFXName` across a track's chain, returning **name + 1-based
  slot** (+ bypass flag) for each FX so the coordinator (never the LLM) resolves indices.
- ✓ `reaper_list_fx_params` — `GetNumParams` + `GetParamName` + formatted/raw value. The
  discovery primitive that makes per-knob control possible for the LLM; resolves the FX by name
  match, reports a 0-param result (lazy init) rather than an empty list.
- `reaper_set_fx_preset` — `TrackFX_SetPreset` by name. **Next** — highest leverage, no param
  discovery; covers Valhalla / Pro-Q / TDR Nova "vibe" requests.
- `reaper_set_fx_param` / `reaper_get_fx_param` — `SetParam` / `GetParam` (0–1 normalised).
  `get` doubles as the Youlean LUFS read for a delivery check.
- `reaper_bypass_fx` — `TrackFX_SetEnabled`.

Once the primitives exist, add small **curated catalogs** for the high-value plugins (Valhalla
preset names, Guitar Rig macro map) so the coordinator can generate guaranteed-correct calls
for small models — the same philosophy that justifies `reaper_add_track`.

### ReaScript quirks the tools MUST guard against

Not optional polish — this is how `TrackFX_*` breaks in practice. Bake the mitigations into
the builders, not the LLM prompt:

1. **FX index drift.** `Set/GetParam` identify a plugin by its **zero-indexed chain
   position**, which shifts if the user manually inserts another FX. *The LLM must never pass
   a raw index.* Tools take a **plugin name** (and track name); the coordinator-built Lua
   resolves the index by matching the name (`TrackFX_GetFXName`), exactly as
   `build_remove_track_lua` matches track names today. `reaper_list_fx` returns name **and**
   index so the coordinator does the lookup, not the model.
2. **`AddByName` format-prefix instability.** A bare `"Guitar Rig 7"` may resolve to VST3 on
   the Windows box but miss the AU build on the (deferred) Mac. Prefer matching against the
   **installed** FX list over hardcoding; where a catalog string is needed, carry the explicit
   prefix (`VST3:` / `AU:` / `VST2:`). This is tied to the macOS provisioning deferral in
   [`reaper.md`](reaper.md).
3. **Lazy param-map init.** Some heavy plugins (GTR ToolRack, Kontakt) build their param map
   only when the UI initialises, so a `SetParam` fired in the same cycle as `AddByName` can
   race it. The builder should **validate `TrackFX_GetNumParams > 0`** (and re-poll within the
   daemon's `defer` loop) before mutating params — not a blind `sleep`.

---

## Installation

Plugin installation is **not automated** — it's a manual Windows-side step (download + run
the vendor installer, then let REAPER rescan). Scripting it later (mirroring
`scripts/install-reaper-windows.ps1`, which silently installs REAPER itself) is possible but
not a priority, so the manual steps below are the supported path for now.

The general pattern for any VST on the Windows REAPER host:

1. Download the plugin's Windows installer from the vendor.
2. Run it; accept the default install locations (VST3 → `C:\Program Files\Common Files\VST3`).
3. In REAPER: **Options → Preferences → Plug-ins → VST → Re-scan**, or just restart REAPER.
   The plugin then appears in the FX browser (and is addressable by name from our tools).

### Valhalla Supermassive (first target)

1. Go to <https://valhalladsp.com/shop/reverb/valhalla-supermassive/>. It's free and the
   download **auto-starts — no email or account is required**. There's a subscribe/email prompt
   on the page, but it's just a marketing capture you can skip; don't enter your email unless you
   want their newsletter. (Doing so can leave you with a second copy via an emailed link —
   harmless, just delete one.)
2. You get a **`.zip`** (e.g. `ValhallaSupermassiveWin_V5_0_0.zip`), not a bare installer.
   Extract it, then run the installer inside (defaults are fine; installs VST3 + VST2).
3. Re-scan REAPER: **Options → Preferences → Plug-ins → VST**. **Quirk:** REAPER may not have
   `C:\Program Files\Common Files\VST2` in its **VST plug-in paths** list by default — if so the
   plugin installs but stays invisible. Add that path, then click **Re-scan → Clear cache and
   re-scan all**. (The VST3 copy in `…\Common Files\VST3` is auto-scanned and needs no path entry.)
4. Confirm REAPER sees it before testing automation — search the Add-FX browser for `valhalla`.
   On the OmniLink1 box it listed as **`VST: ValhallaSupermassive (Valhalla DSP, LLC)`** — i.e.
   the **VST2** copy only; the **VST3 entry did not appear** despite being ticked at install and
   despite VST3 supposedly auto-scanning. **Design implication:** `reaper_add_fx` must match on
   the bare product name (`ValhallaSupermassive`) and must **not** hardcode a `VST3:` prefix —
   the available format varies per box. This is quirk #2 (format-prefix instability) hitting in
   practice on the very first plugin.

---

## Status

Slices 1–2 built (pending live verification): `reaper_add_fx` (insert by name) and the discovery
pair `reaper_list_fx` / `reaper_list_fx_params`. Next is Slice 3 — `reaper_set_fx_preset` + a
curated preset/mode catalog. Plugins are installed manually (see above). Work is **incremental,
free-first, one plugin at a time**, and every slice must be verifiable **without recording audio**
(insert + read-back) since the studio isn't built yet. First target: **Valhalla Supermassive**.
Slice breakdown and ordering are tracked under the Phase 11.7 REAPER section of
[`ROADMAP.md`](../ROADMAP.md).
