# Voice assistant integration — Home Assistant Voice PE, no HA server

Written 2026-07-08. Companion to `plans/multi-domain-home.md` Phase C
("the local AI voice grows senses" — this is that voice actually gaining
ears). New hardware: Home Assistant Voice Preview Edition
(`home-assistant-voice-0a6d24`, `10.0.0.14`, ESP32-S3 + XMOS XU316 mic
array, stock ESPHome 2025.5.1 firmware, unmodified).

## Why stock firmware, not custom

Considered writing custom ESP32 firmware instead of talking to the stock
ESPHome image. Decided against it: the XMOS co-processor's mic-array
DSP (beamforming/echo-cancellation/noise-suppression) is the actual hard
engineering in this device, and it's already open-source and already
tuned for this exact hardware. Speech-to-text/intent/text-to-speech can
never run on the ESP32-S3 either way — that logic always lives on the
server side (ai-mesh's coordinator/a new capability), identical effort
regardless of what firmware the device runs. Going custom would only add
a large front of work (re-doing the DSP tuning) without shortening the
part that's actually novel here.

## Architecture

The device runs ESPHome's "Native API" — protobuf-over-TCP, port 6053.
Normally Home Assistant is the "brain" that dials out to the device,
subscribes to its voice-assistant events, and drives
STT → intent → TTS. Since ai-mesh isn't Home Assistant, `capability-voice`
(new crate, `capabilities/voice/`) plays that role directly, built on the
`esphome-client` crate (crates.io, MIT, protobuf codegen via `protoc`).

Confirmed **not** viable: `esphome-native-api` (a different crate) —
only implements the ESPHome *device* role (for software-emulating a fake
device HA connects to), never sends `HelloRequest`, structurally cannot
play the client role this needs. Caught by reading its source before
writing any code against it.

## Bugs found and fixed (all confirmed via packet capture / the official
## Python reference client, `aioesphomeapi`)

1. **Wrong model-identifier field for auto-naming** — unrelated capability
   (`device_catalog.rs`), found the same night; see
   `plans/device-auto-naming.md`. Not a voice bug, but same debugging
   session.
2. **`esphome-client` 0.2.0 defaults to API version 1.14's schema.** The
   real device speaks 1.10 (ESPHome 2025.5.1). Fixed by pinning
   `default-features = false, features = ["api-1-10"]` in
   `capabilities/voice/Cargo.toml`. Symptom without the fix: connection
   silently closed right after `DeviceInfoResponse`.
3. **`VoiceAssistantSubscribeFlag::API_AUDIO` is bit 2 (value 4), not bit
   0 (value 1) as the crate's bundled `api.proto` comment claims.**
   Confirmed against `aioesphomeapi`'s `VoiceAssistantSubscriptionFlag.API_AUDIO
   = 1 << 2` and by capturing its actual wire frame
   (`00 04 59 08 01 10 04`) against this exact device.
4. **The real bug**: `esphome-client` 0.2.0's `connection_setup` only
   sends the required `ConnectRequest`/`AuthenticationRequest` handshake
   `if password.is_some()` — skipping it entirely when no password is
   configured, instead of always sending it with an empty password (which
   is what the real protocol and every other client, including
   `aioesphomeapi`, does). A device tolerates a stateless
   `DeviceInfoRequest` on this half-finished session, but silently drops
   the connection the instant something stateful like a voice-assistant
   subscription is attempted. **Workaround** (no crate patch needed):
   call `.password("")` on the builder — `Some("")` still satisfies
   `is_some()`, forcing the handshake to actually fire. This one cost the
   most debugging time (looked like a device firmware bug, an API version
   mismatch, and a flags bug before packet capture proved the request
   bytes were byte-identical to the working Python client and the actual
   divergence was upstream of that, in the handshake).
5. ~~**`API_AUDIO` (in-band TCP audio) doesn't deliver audio on this
   firmware**~~ — **retracted, was a false lead.** First attempt (from the
   WSL2 dev sandbox) saw the wake word fire but no `VoiceAssistantAudio`
   ever arrive, so switched to the classic mechanism (`flags: 0`, a UDP
   socket, device sends its port in `VoiceAssistantResponse`) — which
   *did* work. Re-tested `API_AUDIO` directly on pi1 afterward (prompted
   by a good challenge: "could that have been WSL2 all along?") and it
   worked perfectly — 140KB+ streamed cleanly in-band over the same TCP
   connection. The original UDP "fix" was never fixing a firmware bug; it
   was working around the WSL2 UDP-inbound issue below by picking a
   transport that doesn't need a second inbound socket. Settled on
   `API_AUDIO` as the final choice — not just because it works, but
   because the TCP connection is mandatory infrastructure regardless
   (every ESPHome Native API control message — Hello, Subscribe, the
   wake-word event itself — is TCP-only; there's no "UDP-only" mode).
   `API_AUDIO` means *one* connection to manage on a resource-constrained
   device instead of TCP-plus-a-second-UDP-socket, and avoids UDP's
   classic weakness (a dynamically-negotiated port that something in the
   network path — a firewall, a NAT, apparently even a dev sandbox — can
   silently drop). The per-packet overhead difference between TCP and UDP
   is irrelevant here: this isn't a live two-way call, and any retransmit
   delay is dwarfed by the STT→intent→TTS pipeline latency that follows.

## Non-bugs (real hardware/config state, not code)

- **Physical mute switch was engaged** — the single biggest time sink of
  the night. LED ring looked like idle "ready" (blue breathing) the whole
  time, giving zero visual indication of mute. Diagnosed conclusively (not
  guessed) by querying the device's own exposed `mute` switch entity via
  `ListEntitiesRequest`/`SubscribeStatesRequest` — `state: true`. A
  software `SwitchCommandRequest` to force it off did *not* stick
  (hardware correctly overrides software for a privacy mute switch),
  confirming the physical switch itself needed moving. Lesson: don't
  trust the LED ring alone — query entity state directly when something
  that should be observable isn't.
- **Wake-word sensitivity defaulted to "Slightly sensitive"** (the lowest
  of three options) — bumped to "Very sensitive" via `SelectCommandRequest`
  once mute was confirmed off and it still wasn't picking up speech.
  Possibly contributed, possibly not — wasn't isolated independently
  before the successful capture.
- **WSL2 (this dev sandbox) cannot receive unsolicited inbound UDP from
  the LAN at all** — confirmed with a bare Python socket test (nothing
  arrived from a packet sent by pi1). Not a device or protocol bug, and
  it's *why* the `API_AUDIO`-doesn't-work finding above was a false lead
  — the sandbox can dial out over TCP fine (that's a locally-initiated
  connection), but any transport needing a fresh *inbound* socket (UDP
  audio, or, it turned out, nothing else actually — TCP-in-band never
  needed inbound at all) silently fails here specifically. The actual
  test needs to run wherever this capability will really live —
  cross-compiled for `aarch64-unknown-linux-gnu` and run directly on pi1,
  which is a normal Linux box with no NAT/firewall complications. This is
  also just where it belongs long-term.
- **Only 3 wake words ship on this firmware**: "Okay Nabu" (active
  default), "Hey Jarvis", "Hey Mycroft" — confirmed via
  `VoiceAssistantConfigurationResponse.available_wake_words`. No "Okay
  Computer" option. Switching *among* those three is possible in this API
  version (`VoiceAssistantSetConfiguration.active_wake_words` exists in
  api-1-10's proto), but a genuinely custom phrase needs a custom-trained
  wake-word model (openWakeWord/microWakeWord) pushed via
  `VoiceAssistantConfigurationRequest.external_wake_words` — a field that
  only exists in a *later* API version than this device's firmware
  speaks. Not attempted; the device does have a "Beta firmware" switch
  entity that might unlock it, untested.
- **The device's own end-of-speech VAD doesn't stop the audio stream in
  any reasonable time** — observed 500KB+ / 30s+ of continuous audio on a
  single trigger with no stop signal, unlike e.g. Alexa, which visibly
  reacts within about a second of the user going quiet. Fixed properly
  (not worked around): `capability-voice` now does its own simple
  energy-based silence detection on each incoming `VoiceAssistantAudio`
  chunk (`peak_amplitude` — max absolute 16-bit PCM sample) and calls
  `end_capture()` once the peak has stayed under `SILENCE_THRESHOLD` for
  `SILENCE_DURATION` (1.2s), instead of waiting out a fixed window.
  `MAX_CAPTURE` (15s) remains as an outer safety net only. Calibrated
  against a real captured clip: actual speech peaked at 15,000-28,000
  (16-bit signed, full scale ±32,767); quiet-room ambient sat at
  700-3,000 with occasional incidental spikes to 6,000-10,000. The first
  threshold guess (400) was *inside* the noise floor — literally every
  chunk registered as "loud" and the detector never fired. Re-calibrated
  to 5,000. Confirmed working: a real trigger-to-quiet cycle now ends in
  ~1.3s (43,008 bytes) instead of running to the full fixed window.

## Resolved: the red LED ring was never a pipeline error

A long stretch of the session went to chasing a red "error" ring that
kept appearing after captures. Four event-sequence theories were tried
(SttVadStart/End pairs, SttStart, an explicit whitelisted
`stt-no-text-recognized` error event) — none fixed it, because none of
them were the cause.

**The answer, from the device's own YAML** (line ~1162,
`control_leds_no_ha_connection_state`): a red *Twinkle* at 66% brightness
is the firmware's **"I have no Home Assistant connection" indicator**,
and `control_leds` checks `!api_id.is_connected()` *before* it ever looks
at the voice-pipeline phase. Every red observed all night coincided with
a test process dying — my `timeout N` wrappers, the systemd
session-linger reaping (below), a 60s sleep baked into the `listen`
example harness, or a manual interrupt. Each death dropped the API
connection; the device then correctly reported "my brain is gone."
Two decoys made it look like a pipeline problem: the reds often appeared
seconds *after* a capture finished (when the harness timer expired), and
one appeared "spontaneously" (a test had just been killed between
messages).

**Proved 2026-07-08** by running the listener with no expiry at all:
ring black at idle (black IS the connected-idle state — the YAML's idle
script is `light.turn_off`), blue animation during a wake-word capture,
back to black on silence-cutoff, and *no red, ever*, until the client is
deliberately killed. In production this never bites: the capability runs
under the agent's systemd service and its `run()` loop reconnects within
5s of any genuine drop.

Diagnosis lesson for future hardware bring-up: when an LED/status
indicator misbehaves, first ask "what does this device think its
*connection* state is" before auditing your protocol traffic — and check
the indicator's own priority order in firmware source if it's available.

## Where things stand

`capability-voice` (new crate) connects to the device, completes the
handshake, subscribes to voice-assistant events with the `API_AUDIO` flag
(in-band over the existing TCP connection — see the retracted finding
above), and on a wake-word trigger captures and saves a raw PCM clip
(`~/.ai-mesh/voice-cache/clip-<ts>.raw`, 16-bit/16kHz/mono per ESPHome's
documented default). **Proved live on pi1, 2026-07-08**: said "Okay Nabu"
→ device fired `VoiceAssistantRequest` → 254,976 bytes of real audio
captured over TCP (verified non-trivial: full-range 16-bit samples,
99.96% non-zero, stdev ~5262 — not silence, not garbage). Cross-checked
against the earlier UDP-based capture (258,048 bytes, same 8s window) —
consistent, confirming both transports carry the identical underlying
audio; `API_AUDIO` was kept as the simpler final choice.

Event sequence sent per capture (all confirmed via live testing):
`RunStart` → `SttStart` → `SttVadStart` (all three immediately on wake
word) → ... audio streams, silence-detected or `MAX_CAPTURE` hits ... →
`SttVadEnd` → `Error{code: stt-no-text-recognized}` → `RunEnd`
(`end_capture()`). This is an honest description of the crawl phase (no
real STT ran), not a fabricated success — it's the same code HA's own
Assist pipeline reports when STT hears nothing usable, and the firmware
explicitly whitelists it away from the error LED.

Not yet built: `handles()`/`tools()` are still stubs (this capability
takes no coordinator commands and exposes no intent tools yet). No real
STT/intent/TTS wiring — clips are saved to disk and nothing else consumes
them. Not wired into any `nodes/*.env` `NODE_FEATURES` yet (tested via
ad-hoc cross-compiled binaries copied to pi1, not the normal
`just deploy-node` path).

## Next phase (not started)

1. Speech-to-text engine (need to pick one — likely whisper.cpp on
   whichever mesh node has spare compute; check `beelink-model-guide.md`
   for what's already benchmarked there) fed the captured clip.
2. Wire the transcript into the *existing* `coordinator/src/intent.rs`
   chat/command pipeline — this part is mostly reuse, not new work, since
   ai-mesh's LLM intent handling already exists for text.
3. Text-to-speech (Piper is the common lightweight choice) for the
   response, served over HTTP so the device can fetch it — the
   `VoiceAssistantEventResponse` TTS-end event carries a media reference
   the device fetches directly.
4. Replace the crawl phase's `stt-no-text-recognized` placeholder with
   the real sequence once STT exists:
   `stt-end` (with actual recognized text) → `intent-start`/`intent-end`
   → `tts-start`/`tts-end` → `run-end`.
5. Register `capability-voice` as a real agent feature (`NODE_FEATURES`)
   on whichever node should own it — probably pi1. Networking-wise this
   should now be simpler than first thought: `API_AUDIO` only needs a
   normal outbound TCP connection to the device, same as everything else
   the agent already does — no special inbound-port requirement to
   re-verify if this ever moves to a different node.
6. Clip-cache eviction: `~/.ai-mesh/voice-cache/` accumulates ~0.5MB per
   wake word forever (pi1 runs on an SD card). Once STT consumes clips,
   delete-after-transcribe (or a small ring of recent clips for
   debugging) replaces the cache entirely.
7. (Known, accepted) esphome-client's `try_read` auto-answers the
   device's `PingRequest` internally; if a capture-deadline `select!`
   branch wins while that write is pending, the PingResponse is dropped —
   worst case the device drops the connection and the 5s reconnect loop
   heals it. Microsecond window, self-healing, not worth restructuring
   the reader for; noted from code review 2026-07-08.
