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

### What works and what never will

| Path | Status |
|---|---|
| TV → soundbar (eARC / Wi‑Fi Atmos) | ✔ native |
| Pi → HDMI → TV → soundbar | ✔ works, TV must be on |
| Soundbar ← Spotify Connect / AirPlay / Chromecast | ✔ best quality, no TV needed |
| TV → Samsung BT speaker | ✔ native routing |
| Pi → Samsung BT speaker directly | ✖ impossible — Samsung BT lock-in. Accepted; route via TV instead |

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
- **Pi Zero 2W (satellite, placement TBD)**: sensors/lighting-adjacent
  duties, lightweight ai-mesh node, optional audio-only Spotify Connect
  endpoint. Nothing in this plan depends on it.

## Target architecture

```
                        ┌──────────────┐
 Voice PE puck ────────►│              │   IntentRequest / VoiceExchange
 (wake word, STT feed,  │ coordinator  │◄──────── dashboard chat / CLI
  quick TTS replies)    │    (pi1)     │
                        └──────┬───────┘
                               │ AudioPlay (mesh) — new
                               ▼
                        ┌──────────────┐  HDMI   ┌─────────┐ eARC ┌──────────┐
                        │ Pi 4 (pi2)   ├────────►│ Frame TV├─────►│ S701D    │
                        │ art + audio  │         │ (router)│  BT  │ soundbar │
                        └──────────────┘         └────┬────┘─────►└──────────┘
                                                      ▼
                                              Samsung BT speaker
```

Two TTS sinks with different jobs:
- **Puck speaker**: the *default* voice-assistant reply path — instant,
  works with the TV off, right next to the person who spoke. Quality is
  fine for "The kitchen is 26.9°C."
- **Soundbar chain (Pi 4 → HDMI → TV)**: the *premium* path — long-form
  responses, media, announcements, music. Depends on the TV being on
  (or CEC-wakeable), so it's an upgrade target, not the default.

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
   spoken-word trim (prompt tweak in `coordinator/src/intent.rs`).

Success: "Okay Nabu, what temperature is the kitchen?" is *answered out
loud* by the puck.

### Phase 2 — capability-audio on the Pi 4 (HDMI sink)

New crate `capabilities/audio`, feature `audio`, on pi2 alongside `art`.

1. Plays audio out HDMI (ALSA/`aplay` for WAV; `mpv` if formats grow) —
   the TV routes onward to soundbar or BT speaker per its own audio
   output setting.
2. New mesh message (`AudioPlay { url | bytes, … }`) the coordinator
   sends to the audio node — mirrors how LightCommand fans out. The
   coordinator-side hook is the same place `push_voice_exchange` fires.
3. Wire as an *optional* voice reply sink: config/preference decides
   puck vs soundbar per response (start with an env/preference toggle;
   room-aware routing is Phase 5).
4. Physical: confirm the Pi 4's HDMI audio is accepted by the Frame
   while in art mode / on the Pi's input (needs live testing — art mode
   vs HDMI-input audio behavior is undocumented territory; the frame-tv
   plan's CEC learnings apply).

### Phase 3 — soundbar as a first-class device (no TV involved)

The S701D is LAN-controllable on its own:
1. Investigate control surface: SmartThings cloud API vs local
   (SoundTouch-style local APIs, Chromecast target, AirPlay). Prefer
   local control per the project's "never leaves your house" stance;
   SmartThings only if local proves impossible.
2. Volume/input/mute as coordinator tools → "Okay Nabu, turn it down".
3. Chromecast/AirPlay target for TTS/announcements **directly to the
   soundbar with the TV off** — if this works well it may beat the HDMI
   path for most audio and demote Phase 2 to art-mode-adjacent audio
   only. Evaluate before building Phase 2 deeply.

### Phase 4 — TV as audio router (BT speaker access)

The only path to the Samsung BT speaker:
1. TV control (local Tizen websocket API — `samsungtvws`-style — or
   SmartThings): switch the TV's audio output between soundbar and BT
   speaker, power/input management, CEC wake from the Pi.
2. Expose as coordinator tools: "play this on the bedroom speaker" →
   TV on (if needed) → audio output to BT → AudioPlay via Pi 4 HDMI.
3. Reality check: this chain (wake TV → switch output → play) has real
   latency; treat the BT speaker as a media/announcement target, never
   the voice-reply path.

### Phase 5 — room-aware response routing

The "divert the response somewhere else" endgame: the VoiceExchange/
reply-routing decision becomes per-room policy (puck answers in the room
that asked; music/announcements go to the best speaker in that room;
whole-house announce fans out). Needs Phases 1-4 plus room metadata the
registry already has. Design then, not now.

## Risks / open questions

- **TV-on dependency**: every soundbar/BT path through HDMI needs the
  TV awake. CEC wake works per the frame-tv plan, but adds seconds. The
  puck (Phase 1) and direct-to-soundbar casting (Phase 3.3) are the
  hedges — validate 3.3 early since it could simplify everything.
- **Art mode vs HDMI audio**: unverified whether the Frame accepts and
  routes Pi HDMI audio while displaying art (vs switched to the Pi's
  input). Live test before committing to Phase 2's design.
- **Spotify Connect on the soundbar** needs no work of ours (native) —
  but "Okay Nabu, play X on Spotify" needs Spotify Web API tooling in
  the coordinator eventually. Out of scope until the audio plumbing
  lands.
- **Samsung BT speaker stays TV-captive** — accepted permanently; no
  workaround exists.
- **Zero 2W redeployment** — where it lands (sensors satellite? Connect
  endpoint?) is deliberately unplanned here; nothing blocks on it.

## Sequencing note

Phase 1 first — it completes the voice assistant with zero new hardware
dependencies and its Piper/HTTP plumbing is reused by every later phase.
Then Phase 3.3's direct-cast experiment *before* deep Phase 2 work, since
a good TV-off casting path to the soundbar would reshape how much the
HDMI chain matters.
