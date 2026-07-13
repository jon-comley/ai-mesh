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

## STT landed — whisper.cpp on pi1, 2026-07-08

Item 1 of "Next phase" (below) is done. `capabilities/voice/src/stt.rs`
spawns `whisper-server` (whisper.cpp's own HTTP server binary — same
subprocess+HTTP shape as `capability-llm`'s `llama-server` integration) once
per agent process, health-checks it, and `finish_capture` now posts each
saved clip to it (wrapped in a synthesized WAV header — whisper-server wants
a real WAV file, not headerless PCM) and logs the transcript. Deployed via
a new `has_feature voice` block in `scripts/install-node-linux.sh`
(downloads the prebuilt `whisper-bin-ubuntu-arm64.tar.gz` release asset and
`ggml-base.en.bin` from `ggerganov/whisper.cpp` on HuggingFace, same pattern
as the existing llama.cpp block). `nodes/pi1.env` now carries
`voice` in `NODE_FEATURES` and `VOICE_DEVICE_HOST=10.0.0.14:6053`.

Not yet done: feeding the transcript into `coordinator/src/intent.rs` (item
2), TTS (item 3), and replacing the `stt-no-text-recognized` placeholder
event sequence (item 4) — this only proves transcription itself works.

**Model**: `base.en`, chosen as an untested guess and then validated live —
proven accurate on real captured speech (see below), no need yet to drop to
`tiny.en` for latency.

**Bug found and fixed during this deploy, unrelated to STT itself**:
`main.rs`'s outer loop calls every capability's `start()` again on *every*
agent↔coordinator reconnect (not just once at boot) — expected/fine for
capabilities whose `start()` is naturally idempotent, but
`VoiceCapability::start()` unconditionally spawned a fresh `run()` task each
time, so each coordinator reconnect left one more concurrent connection to
the device's single connection slot, all fighting each other. Symptom
observed live: continuous `Stream error: Read error: Connection reset by
peer (os error 104)` a few seconds apart, forever. Fixed with a
`std::sync::Once` guard so `run()` (and the whisper-server spawn) only ever
starts once per process, since the ESPHome connection has nothing to do
with which coordinator connection happens to be current. A second,
unrelated contributor made this harder to diagnose at first: a stray
`/tmp/voice-listen` process from an earlier ad-hoc test session (started via
a backgrounded SSH command that had since disconnected) was still running
on pi1 and independently fighting the real agent for the same device
connection — killed manually; not a code bug, just leftover test-session
cruft.

**Proved live on pi1, 2026-07-08**: after the reconnect fix, said "Okay
Nabu, testing one two three" — wake word fired, 481,280-byte clip
captured and saved, and the logged STT result was
`"Testing, one, two, three."` — an exact transcript of real speech, through
the actual capture → save → transcribe code path (not a curl bypass).

**`SILENCE_THRESHOLD` raised 5000 → 8000, same day**: both live captures
above never triggered silence detection at all — each ran the full 15s
`MAX_CAPTURE` window instead. Replaying `push_chunk`'s exact logic against
the two saved clips (30ms chunks, same peak-amplitude check) showed why:
ambient noise crossed 5000 at least once every 630-840ms throughout each
clip, never leaving the 1200ms quiet gap `SILENCE_DURATION` requires. This
also explains clip 1's nonsense transcript ("How are you doing?" — nobody
said that) — a classic Whisper hallucination pattern when fed a long
stretch of mostly non-speech audio rather than real speech. Not a code
bug: the room was simply noisier during this live test than the original
calibration session assumed ("occasional incidental spikes to 6,000-
10,000" turned out to mean sub-second cadence, not rare, in this session).
Redeployed via `just deploy-node pi1`; not yet re-verified live at the new
threshold (do that before trusting silence-based cutoff again).

**Bug found and fixed in that same redeploy**: the `voice` install block's
`VOICE_ENV_BLOCK` set `Environment=LD_LIBRARY_PATH=/opt/whisper.cpp:/opt/llama.cpp`
at the systemd unit level, on the theory that whisper-server just needed
its own dir added alongside llama's. In practice this broke `llama-server`
immediately (`error loading model: make_cpu_buft_list: no CPU backend
found`) — llama.cpp and whisper.cpp each ship a same-named `libggml-cpu.so`
backend plugin, not binary-compatible across projects/versions, and with
whisper.cpp's dir first in the merged path, llama-server loaded the wrong
one. Fixed by not touching `LD_LIBRARY_PATH` at the systemd level for voice
at all — `stt.rs`'s `ensure_server_running()` now sets
`LD_LIBRARY_PATH=/opt/whisper.cpp` explicitly on the `whisper-server` child
process only (via `Command::env`), leaving the agent's own inherited
`LD_LIBRARY_PATH=/opt/llama.cpp` untouched for `llama-server`. Confirmed
fixed live: both servers loaded cleanly on the next redeploy
(`llama-server ready model="qwen2.5:0.5b"`,
`whisper-server ... whisper-server ready`).

Also investigated live that day: a wake-word attempt that produced no
`VoiceAssistantRequest` at all (no capture, no error, no reconnect —
connection stayed healthy throughout). Queried the device's own `mute`
switch and `wake_word_sensitivity` select entity directly (same technique
as the original bring-up's mute-switch diagnosis) rather than guess:
`mute: false`, `wake_word_sensitivity: "Very sensitive"` — both already
ruled out as the two known false leads from the original bring-up. No
software anomaly found on the agent side, so this particular miss looks
environmental (distance/volume/angle) rather than a regression — not
conclusively resolved, flagged here rather than closed.

## Intent wiring landed — voice drives the house, 2026-07-08

Items 2, 4, and 6 below done in one pass. A spoken command now flows wake
word → capture → whisper.cpp → `MeshMessage::IntentRequest` → the
coordinator's existing `handle_intent` tool-calling pipeline (lights,
scenes, sensors — the same machinery dashboard chat and `just intent` use)
→ `IntentResponse` back to the agent → logged reply + correct device event
choreography. No TTS yet: the action executes, the reply text is
journal-only.

**Key discovery**: no HTTP path was needed. The coordinator's
`process_message` (`coordinator/src/server.rs`) already handles
`MeshMessage::IntentRequest` from *any* connection (it's what `just intent`
uses) and replies on the same connection. The agent's reader loop routes
every unclaimed inbound message through `dispatch()` → `handles()`. So
`capability-voice` just sends the request down the mesh `tx` it receives in
`start()` and claims the matching response by `request_id` — no port, no
auth, no new transport, works if voice ever moves off pi1.

Implementation notes (all `capabilities/voice/src/lib.rs`):
- `VoiceShared` (Arc): `mesh_tx` refreshed on **every** `start()` call
  (start re-runs per coordinator reconnect — exactly when the old sender
  goes stale; the `Once` guard still keeps `run()` single-instance), plus a
  `pending` map request_id → oneshot for in-flight intents.
- Post-capture work runs in a detached `pipeline` task (STT + LLM can take
  seconds; the device loop must keep servicing wake words). Its device
  events come back through an mpsc channel tagged with a **capture
  generation** — a pipeline outliving its run (new wake word arrived) gets
  its events dropped instead of resetting the newer run's LED state. The
  superseded pipeline still completes its intent (a spoken command should
  still execute).
- Event sequence now real: `SttVadEnd` at capture end (halo off
  immediately — the transcription-latency lesson from earlier today),
  then `SttEnd{text}` → `IntentStart` → `IntentEnd` → `RunEnd` on success;
  `Error{code: intent-failed}` + `RunEnd` on failure/timeout (brief red
  twinkle = honest "that didn't work"); the whitelisted
  `stt-no-text-recognized` close for no-usable-speech (Whisper non-speech
  tags like "(air whooshing)" / pure punctuation are detected by
  `is_no_speech`).
- `VOICE_INTENT_TIMEOUT_SECS` (default 30) bounds the coordinator wait.
- `model_name: None` → `any_ready_llm_model()` picks the largest ready
  model mesh-wide — Beelink automatically when it's up, matching the
  "Beelink only when needed" decision with zero voice-specific code.
- Clip-cache eviction (item 6): clips delete after successful
  transcription; failed ones are kept for debugging. Save-first still
  protects against a crash mid-pipeline.

**Two serious bugs found by the live bring-up, both fixed same day:**

1. **Old coordinator killed every agent connection** ("kitchen lights
   bricked" was this). `deploy-node` only ships the agent; adding
   `Feature::Voice` to the shared enum meant the still-running old
   coordinator failed to deserialize the agent's Capabilities frame — and
   `server.rs` drops the whole connection on a payload parse error. The
   agent didn't notice for up to 20 min (half-open socket + LLM nodes'
   1200s read floor), so pi1 looked "up" while heartbeats, ModelStatus,
   and light commands all went nowhere. Fix: `just deploy-coordinator pi1`.
   **Lesson: any `shared/` wire-type change requires coordinator + agents
   deployed together.** (Tolerant enum deserialization would remove this
   failure class — candidate follow-up.)

2. **Same-connection intent deadlock.** The coordinator's per-connection
   read loop awaited `handle_intent` *inline*. A voice intent arrives on
   pi1-agent's connection; `handle_intent` dispatches inference to that
   same agent and waits for `ModelInferenceResult` — which arrives on the
   very connection whose reader is blocked waiting for it. The LLM
   finished in 42s; the response sat unread until the timeout. (Also
   explained the "frame timestamp is stale" warnings — heartbeats queued
   40s+ behind the blocked reader, then skew-rejected.) Fix: spawn the
   intent work and reply via the connection's sender
   (`coordinator/src/server.rs`, IntentRequest arm).

**Capture-tuning fixes from the same session** (`lib.rs`): the wake-word
tail (~250ms of "…Nabu" at 18k-28k peaks) no longer counts as speech
onset, and a capture waits `SPEECH_START_TIMEOUT` (4s) for the user's
speech to *begin* — the old fixed 1200ms window expired during a natural
think-pause after the wake word. Once speech starts, the 1200ms
silence-end rule applies as before. `VOICE_INTENT_TIMEOUT_SECS` default
raised 30→60: a *cold-cache* intent prefills ~3k tokens of system prompt
at ~72 tok/s on pi1's CPU (~41s); warm-cache follow-ups take well under
1s of intent time.

**Proved live, 2026-07-08 evening**: "Okay Nabu, what temperature is the
kitchen?" → capture 3.0s (silence-detected) → exact transcript → intent
complete in 688ms → response `[26.9°C]`, ~9s end-to-end including
speaking time; repeated immediately after at 456ms intent time. The
spoken question was answered by the house through the full mesh pipeline.

**STT offloaded to the Beelink (same evening)**: transcription dominated
the perceived wait (~4.9s of the ~6s thinking-flash — base.en on pi1's
CPU). `VOICE_STT_REMOTE` (host:port) now sends clips to a remote
whisper-server first with the local one as fallback (60s cooldown after a
remote failure so an offline Beelink adds no per-utterance timeout lag).
Beelink runs whisper-server as a **standalone NSSM service** on
`0.0.0.0:8081` (`install-node-windows.ps1 -SttServer true`, gated by
`STT_SERVER=true` in `nodes/beelink1.env`) — deliberately NOT an agent
child: no voice capability runs there, and it survives agent
deploy-restarts. It serves `small.en` (more accurate than pi1's base.en;
the fast box can afford it). Deliberately direct HTTP, not a mesh
`Feature::Stt` — right shape for two nodes; coordinator-routed STT is the
eventual design if it ever needs scheduling across 3+. **Proved live**:
end-of-speech → answer now ~2.9s (1.2 silence + 2.5 remote STT + 0.3
groq), down from ~6s, with an exact transcript. Same session also proved
the chat-window `VoiceExchange` WS event end-to-end. Also that evening:
LED ring now fades out (~900ms, via the ring's user-light entity + a dim
glow set just before RunEnd so the firmware's idle-restore lands on it),
trailing silence is trimmed from clips before STT, and
`SPEECH_START_TIMEOUT` settled at 2.5s.

**Online AI honored for voice/CLI intents (same evening)**: the mesh
`IntentRequest` path originally forced local inference (`None` gateway,
"mesh intents stay local"). Now it builds the same `GatewayInvocation`
as the dashboard chat path (`http/api/chat.rs`) from the Online AI tab's
config — cloud when the toggle is on and configured, with `handle_intent`'s
existing local-fallback-on-cloud-failure, and the same Gateway-tab stats
push. Privacy stance intact: "never leaves your house" holds unless the
user explicitly flips the Online AI toggle, which now applies to speech
exactly as it does to typed chat.

**Cloud provider cascade before local fallback, 2026-07-13**: a failed
primary cloud call no longer drops straight to local — `handle_intent`
now tries every other provider preset with a saved API key first
(`cloud::fallback_providers`, in preset order), and only falls back to
local once all of them have failed too. Discovered live: a burst of
`play_announcement` test calls tripped Groq's free-tier rate limit,
which sent a *voice* intent straight to local inference on pi1 — a 90s
wait (vs. Groq's usual sub-second reply) that read as the chat session
having died. Since keys are already stored per-endpoint from switching
providers in the Gateway tab, no new key-management UI was needed to
wire this up. Applies equally to voice/mesh intents and dashboard chat,
since both share `handle_intent`.

## Next phase (not started)

1. ~~Speech-to-text engine~~ — **done 2026-07-08**, see above.
2. ~~Wire the transcript into `coordinator/src/intent.rs`~~ — **done
   2026-07-08**, see "Intent wiring landed" above.
3. Text-to-speech (Piper is the common lightweight choice) for the
   response, served over HTTP so the device can fetch it — the
   `VoiceAssistantEventResponse` TTS-end event carries a media reference
   the device fetches directly. The `tts-start`/`tts-end` slots in the
   event sequence are the only placeholder left. **Now planned as Phase 1
   of `plans/audio-output-integration.md`** (2026-07-08), which also
   covers the soundbar/Frame-TV/BT-speaker audio ecosystem beyond the
   puck.
4. ~~Replace the crawl phase's `stt-no-text-recognized` placeholder~~ —
   **done 2026-07-08** (real `stt-end`/`intent-start`/`intent-end`
   sequence; TTS events await item 3).
5. ~~Register `capability-voice` as a real agent feature (`NODE_FEATURES`)~~
   — **done 2026-07-08**: pi1 runs `NODE_FEATURES=llm,lighting,sensors,voice`
   via the normal `just deploy-node pi1` path. STT (whisper.cpp) runs there
   too by default rather than being offloaded to Beelink — simplicity over
   speed; Beelink only enters the picture later if pi1's STT latency
   actually turns out to be a problem in practice.
6. ~~Clip-cache eviction~~ — **done 2026-07-08**: delete-after-transcribe
   (failed transcriptions keep their clip for debugging).
7. (Known, accepted) esphome-client's `try_read` auto-answers the
   device's `PingRequest` internally; if a capture-deadline `select!`
   branch wins while that write is pending, the PingResponse is dropped —
   worst case the device drops the connection and the 5s reconnect loop
   heals it. Microsecond window, self-healing, not worth restructuring
   the reader for; noted from code review 2026-07-08.
8. Review follow-ups (2026-07-08, external review): RMS/adaptive silence
   detection or a proper VAD (fixed peak threshold provably can't separate
   quiet speech from noisy-room spikes — measured overlap); STT-failure
   visibility to the coordinator (currently journal-only).
