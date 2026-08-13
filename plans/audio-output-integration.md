# Audio output integration — TTS, soundbar, and the Samsung audio ecosystem

Written 2026-07-08. Companion to `plans/voice-assistant-integration.md`
(whose one remaining item — TTS — is Phase 1 here) and
`plans/frame-tv-art-display.md` (whose Pi placement decision this
reverses, see below). The `VoiceExchange` dashboard event and the
coordinator-side hook in `coordinator/src/server.rs` (IntentRequest arm)
were explicitly built as the seam for audio output sinks — this plan is
what plugs into it.

## Audio ecosystem reality (hardware truths)

- **Samsung S701D soundbar** — a Wi‑Fi smart audio device in its own
  right: Spotify Connect, AirPlay 2, Chromecast, Dolby Atmos, and full
  SmartThings control. Reachable over the LAN without the TV.
- **Samsung Bluetooth speaker + bass box** — only accepts Bluetooth from
  Samsung TVs/phones. **A Pi cannot pair with it, period** — Samsung
  locks the BT sink to their ecosystem. The only route to it is via the
  Frame TV, which can route its audio output to the BT speaker.
- **Samsung Frame 32"** — acts as the audio router: HDMI in from a Pi,
  audio out to either the soundbar (HDMI eARC / Wi‑Fi Atmos) or the BT
  speaker. Also the art-mode display (see frame-tv plan).
- **Home Assistant Voice PE puck** — has its own small speaker, driven by
  the ESPHome voice pipeline's `tts-start`/`tts-end` events (the device
  fetches a media URL and plays it). Low quality but zero routing
  dependencies — always available, instant.
- **Generic Bluetooth speakers** (decided 2026-07-08: a secondhand
  **Blaupunkt BL2621, 2×20W**, for the kitchen — cabinet-top placement)
  — standard A2DP, no ecosystem lock-in. **A Pi pairs with these
  directly** (BlueZ), unlike the Samsung BT speaker above. This is the
  answer to routing voice replies without touching the TV/soundbar at
  all: a dedicated per-room speaker, always on, no volume contention
  with whatever's playing on the TV.

### What works and what never will

| Path | Status |
|---|---|
| TV → soundbar (eARC / Wi‑Fi Atmos) | ✔ native |
| Pi → HDMI → TV → soundbar | ✔ works, TV must be on |
| Soundbar ← Spotify Connect / AirPlay / Chromecast | ✔ best quality, no TV needed |
| TV → Samsung BT speaker | ✔ native routing |
| Pi → Samsung BT speaker directly | ✖ impossible — Samsung BT lock-in. Accepted; route via TV instead |
| Pi → generic Bluetooth speaker (Blaupunkt etc.) directly | ✔ standard BlueZ pairing, no lock-in |

## Placement decision (reverses the frame-tv plan's earlier lean)

**The Pi 4 stays behind the Frame TV permanently; the Pi Zero 2W goes
elsewhere as a satellite.** The frame-tv plan originally treated the Pi 4
as the dev mule with a Zero 2W as the final in-wall hardware. Reversed
because the behind-TV node is becoming the AV workhorse, and the Zero 2W
can't do the job: no reliable 1080p HDMI video path, and no headroom for
anything beyond audio streaming.

- **Pi 4 (behind the Frame, currently node `pi2`, `NODE_FEATURES=art`)**:
  art mode + HDMI output (already its job), plus this plan's new role —
  **HDMI audio injection** (Pi → TV → soundbar/BT speaker) as the mesh's
  audio output sink. Optionally later: SmartThings/TV control, a Spotify
  Connect endpoint, or spare STT/LLM capacity — but note STT and LLM are
  currently well-served by pi1 + the Beelink offload
  (`VOICE_STT_REMOTE`), so those move only if a real need appears, not
  by default.
- **Pi Zero 2W (satellite)**: leading candidate is hosting the kitchen
  Bluetooth speaker (see Phase 2) — good fit per the original hardware
  notes (fine for audio-only streaming, not video), and it gives the
  Zero a concrete job instead of a vague "sensors/lighting" placeholder.
  Not committed; still nothing else in this plan depends on it.

## Target architecture

```
                        ┌──────────────┐
 Voice PE puck ────────►│              │   IntentRequest / VoiceExchange
 (wake word, STT feed,  │ coordinator  │◄──────── dashboard chat / CLI
  quick TTS replies)    │    (pi1)     │
                        └──────┬───────┘
                       AudioPlay (mesh) — new, fans out to whichever
                       sink(s) a reply/announcement targets
                          │                              │
                          ▼                              ▼
                ┌──────────────────┐            ┌──────────────┐  HDMI   ┌─────────┐ eARC ┌──────────┐
                │ kitchen speaker  │            │ Pi 4 (pi2)   ├────────►│ Frame TV├─────►│ S701D    │
                │ (Blaupunkt, BT,  │            │ art + audio  │         │ (router)│  BT  │ soundbar │
                │  direct-paired)  │            └──────────────┘         └────┬────┘─────►└──────────┘
                └──────────────────┘                                        ▼
                                                                     Samsung BT speaker
```

Three TTS/audio sinks, cheapest-and-fastest to richest-and-most-dependent:
- **Puck speaker**: instant, zero setup, works with the TV off — the
  fallback default for any room without its own dedicated speaker.
- **Dedicated room Bluetooth speaker (Blaupunkt, kitchen)**: better
  quality than the puck, still TV-independent, and — this is the point —
  it means routine voice-command feedback never touches the TV/soundbar
  at all. Avoids the exact annoyance of the soundbar's volume jumping
  around every time you ask to dim a light while watching something.
  Becomes the *default* reply sink for any room that has one.
- **Soundbar chain (Pi 4 → HDMI → TV, or direct cast per Phase 4)**:
  reserved for what it's actually good for — music, media, deliberate
  announcements — not routine command acknowledgements. Depends on the
  TV being on (or CEC-wakeable) via the HDMI path; Phase 4.3 may avoid
  that dependency entirely for this sink too.

**Important distinction the Blaupunkt question surfaced**: ai-mesh's own
audio (TTS replies, alerts) can trivially play on *multiple* sinks at
once — the coordinator just sends the same clip to each independent
path, no TV feature required (this is the Phase 6 broadcast case). That
is completely different from asking the **TV's own live audio** (whatever
show/movie is actually playing) to be duplicated to both the soundbar and
a separately-paired Bluetooth speaker simultaneously — that depends
entirely on the Frame TV's own `Sound Output` menu (Samsung's
"Multi-Output Audio" / "Dual Audio Output", naming and capability vary by
model/firmware year) and is outside ai-mesh's control. Worth checking
directly on this unit if simultaneous native-TV-audio output matters;
not something to design around here.

## Implementation phases

### Phase 1 — Piper TTS + puck playback (closes the voice roadmap)

The last open item from `plans/voice-assistant-integration.md`. No new
hardware involved.

1. Piper as a persistent local service on pi1 (same
   spawn/health/HTTP shape as `stt.rs` — the third instance of the
   llama.rs pattern). Voice pipeline synthesizes the IntentResponse text
   to a WAV.
2. Serve the WAV over HTTP from the agent (a tiny one-shot file server,
   or via the coordinator's existing HTTP layer) so the puck can fetch
   it: send `tts-start` → `tts-end{url}` → `run-end` in the pipeline's
   event choreography (the slots are already stubbed in the event
   sequence; the ring's fade lands after playback).
3. Voice selection/quality pass on real hardware; keep responses terse —
   intent replies are currently written for a chat window and may need a
   spoken-word trim (e.g. "respond as a terse voice interface, omit
   conversational filler" appended to the system prompt in
   `coordinator/src/intent.rs`). The `IntentSource` tag already shipped
   today (`shared::IntentSource::Voice` on `IntentRequest`) is exactly
   the hook for this — apply the constraint only when `source == Voice`,
   so typed dashboard chat keeps its normal (non-terse) replies.

Success: "Okay Nabu, what temperature is the kitchen?" is *answered out
loud* by the puck.

**Implemented 2026-07-09 — 5 voices, switchable live from the
dashboard**, not a single fixed pick. `capability-voice`'s
`tts.rs` spawns one persistent `piper.http_server` per voice (Piper
moved to `OHF-Voice/piper1-gpl`, pip-only, no prebuilt binary — a first
for this repo; still just a subprocess, so its GPL-3.0 doesn't reach our
code) so switching is genuinely instant — every voice is always warm.
The active voice is a dashboard preference (`tts-voice`, same
`prefs.js`/`/api/preferences` mechanism as the "voice-in-chat" checkbox),
fetched fresh on every synthesis call — no restart needed to switch.

Licenses verified directly against each voice's `MODEL_CARD` on
`huggingface.co/rhasspy/piper-voices` (not assumed):

| Voice | Accent/gender | License | Commercial use |
|---|---|---|---|
| `joe` | US male | CC0 | ✔ cleared, no restriction |
| `kristin` | US female | public domain | ✔ cleared, no restriction |
| `ljspeech` | US female | public domain | ✔ cleared, no restriction |
| `alba` | GB female (Scottish) | CC BY 4.0 | ✔ cleared, attribution required |
| `alan` | GB male | "All Rights Reserved" (Mycroft AI) | ✘ not cleared — no license grant at all; accepted here only as low-risk for personal, non-commercial use |

Not currently operationally relevant (personal home project), but
recorded in case this code or the models are ever shared/reused
elsewhere. `en_US-hfc_female` (CC BY-NC-SA 4.0) was considered and
dropped for the same reason as `alan` — non-commercial-only — since
`alba` already covers a female GB voice with a cleaner license.

### Phase 2 — local Bluetooth speakers (capability-bluetooth-audio)

The kitchen Blaupunkt BL2621. Simpler than Phase 3's HDMI work and
delivers the "don't disturb the TV for a light command" win immediately
— do this before, not after, the HDMI sink.

1. New crate `capabilities/bluetooth-audio` (or fold into the existing
   `audio` crate from the start, shared `AudioPlay` message — see
   Phase 3.2): standard Linux BT audio, `bluetoothctl`/BlueZ pairing
   once, then playback via `pw-play`/`paplay` against the paired A2DP
   sink. No Samsung-style lock-in to work around — this is the easy
   case the Samsung BT speaker never was.
2. Host node: whichever Pi is in Bluetooth range of the kitchen cabinet
   — leading candidate is the Pi Zero 2W (see "Placement" above); confirm
   range/reliability live before committing hardware placement. Test
   under real kitchen conditions, not just a quiet room — microwaves and
   other appliances share the 2.4GHz band with Bluetooth and are a
   known, specific interference source in exactly this room.
3. Wire as the kitchen room's default voice-reply sink once the Voice PE
   puck lives in the kitchen (it does, per today's testing) — replies to
   kitchen-originated requests go here instead of the puck's tiny
   speaker; falls back to the puck if the Bluetooth link is down.
4. Same pattern extends to any future room speaker (generic BT, no
   lock-in) — this phase's code is written once, reused per room.

### Phase 3 — capability-audio on the Pi 4 (HDMI sink)

New crate `capabilities/audio`, feature `audio`, on pi2 alongside `art`.

1. Plays audio out HDMI (ALSA/`aplay` for WAV; `mpv` if formats grow) —
   the TV routes onward to soundbar or BT speaker per its own audio
   output setting.
2. New mesh message (`AudioPlay { url | bytes, … }`) the coordinator
   sends to the audio node — mirrors how LightCommand fans out, and is
   shared with Phase 2's Bluetooth sink (same message, different node
   advertises it). The coordinator-side hook is the same place
   `push_voice_exchange` fires.
3. Wire as an *optional* voice reply sink: config/preference decides
   puck vs local speaker vs soundbar per response (start with an
   env/preference toggle; room-aware routing is Phase 6).
4. Physical: confirm the Pi 4's HDMI audio is accepted by the Frame
   while in art mode / on the Pi's input (needs live testing — art mode
   vs HDMI-input audio behavior is undocumented territory; the frame-tv
   plan's CEC learnings apply).

### Phase 4 — soundbar as a first-class device (no TV involved)

The S701D is LAN-controllable on its own:
1. Investigate control surface: SmartThings cloud API vs local. Candidates
   to check on this exact unit (unconfirmed, starting points not facts):
   a DLNA/UPnP media renderer profile (common alongside Chromecast
   built-in on smart soundbars) or the local Google Cast v2 protocol —
   either would mean streaming a WAV directly over the LAN with no cloud
   round-trip. Prefer local control per the project's "never leaves your
   house" stance; SmartThings cloud only as the last resort, since its
   round-trip latency would make voice interactions feel sluggish.
2. Volume/input/mute as coordinator tools → "Okay Nabu, turn it down".
3. Chromecast/AirPlay target for TTS/announcements **directly to the
   soundbar with the TV off** — if this works well it may beat the HDMI
   path for most audio and demote Phase 3 to art-mode-adjacent audio
   only. Evaluate before building Phase 3 deeply.

### Phase 5 — TV as audio router (BT speaker access)

The only path to the Samsung BT speaker:
1. TV control (local Tizen websocket API — `samsungtvws`-style — or
   SmartThings): switch the TV's audio output between soundbar and BT
   speaker, power/input management, CEC wake from the Pi.
2. Expose as coordinator tools: "play this on the bedroom speaker" →
   TV on (if needed) → audio output to BT → AudioPlay via Pi 4 HDMI.
3. Reality check: this chain (wake TV → switch output → play) has real
   latency; treat the BT speaker as a media/announcement target, never
   the voice-reply path.

### Phase 6 — room-aware response routing + broadcast alerts

The "divert the response somewhere else" endgame: the VoiceExchange/
reply-routing decision becomes per-room policy (puck or room speaker
answers in the room that asked; music/announcements go to the best
speaker in that room). Needs Phases 1-5 plus room metadata the registry
already has. The policy layer, once designed, needs at minimum:
- a per-room default sink (room speaker if one exists, else the puck
  that heard the request);
- a fallback chain when the default is unreachable (room speaker → puck
  → nothing, never silently escalate to the soundbar for a routine
  reply);
- a media-vs-reply distinction (replies stay local/quiet per-room;
  explicit media/music requests may target the soundbar); and
- a broadcast flag, distinct from normal per-room routing (below).

Includes the explicit **broadcast-to-all-speakers** case for alerts
("someone's at the door", a smoke-sensor trigger): the coordinator sends
one `AudioPlay` to every known sink at once — puck(s), any room Bluetooth
speakers, and the soundbar chain — independent parallel sends, no TV
feature required (see the distinction note under "Target architecture").
Design this phase later; the broadcast mechanic itself is cheap once
Phases 1-5 exist, since it's just "send to every sink" rather than "pick
one."

**Puck-as-broadcast-target mechanism confirmed feasible** (live incident +
investigation, 2026-07-13): see `../ROADMAP.md`'s "Puck as Last-Resort
Announcement Target" entry for the full writeup. Short version — the puck
exposes a stock ESPHome `media_player` entity
(`MediaPlayerCommandRequest`/`has_announcement=true`) that accepts a
pushed media URL independent of any voice-assistant session, confirmed
live against the real device. That's the missing piece for wiring the
puck into `broadcast_announcement`'s fan-out
(`coordinator/src/audio.rs:464`) as the last-resort target this Phase
describes — not yet implemented.

## Risks / open questions

- **TV-on dependency**: the soundbar/Samsung-BT paths through HDMI need
  the TV awake. CEC wake works per the frame-tv plan, but adds seconds.
  The puck (Phase 1) and the kitchen Bluetooth speaker (Phase 2) sidestep
  this entirely for voice replies; direct-to-soundbar casting (Phase
  4.3) is the hedge for media specifically — validate it early since it
  could simplify how much the HDMI chain (Phase 3) needs to do.
- **Art mode vs HDMI audio**: unverified whether the Frame accepts and
  routes Pi HDMI audio while displaying art (vs switched to the Pi's
  input) — Samsung's Tizen is aggressive about power/input state in art
  mode, so don't assume it just works. Live test before committing to
  Phase 3's design: `Pi 4 → HDMI → Frame TV (in art mode) → play a WAV`,
  and check (a) does the soundbar receive audio at all, (b) does a
  separately-paired BT speaker receive it simultaneously if the TV's
  own multi-output is on, (c) does CEC wake still work from this state.
  A "no" on (a) makes Phase 4.3 (direct soundbar casting) load-bearing
  rather than just a nice-to-have.
- **Bluetooth range/reliability** (Phase 2): whichever Pi hosts the
  kitchen speaker pairing needs to actually be in range of the
  cabinet-top placement — confirm live before finalizing which node
  that is.
- **Spotify Connect on the soundbar** needs no work of ours (native) —
  but "Okay Nabu, play X on Spotify" needs Spotify Web API tooling in
  the coordinator eventually. Out of scope until the audio plumbing
  lands.
- **Samsung BT speaker stays TV-captive** — accepted permanently; no
  workaround exists. (The generic Blaupunkt has no such limitation —
  that's the whole point of Phase 2.)
- **Simultaneous native-TV-audio output** (soundbar + a separate BT
  speaker, for whatever's actually playing on the TV, not ai-mesh's own
  audio) depends on the Frame's own `Sound Output` / Multi-Output Audio
  capability — a TV feature, not something ai-mesh can build around.
  Check the unit directly if this matters; out of scope either way.

## Sequencing note

Phase 1 first — it completes the voice assistant with zero new hardware
dependencies and its Piper/HTTP plumbing is reused by every later phase.
Phase 2 (kitchen Bluetooth speaker) next — cheapest win, directly
addresses the "don't disturb the TV for a light command" annoyance, and
shares its `AudioPlay` message with Phase 3. Then Phase 4.3's direct-cast
experiment *before* deep Phase 3 (HDMI) work, since a good TV-off casting
path to the soundbar would reshape how much the HDMI chain matters.
