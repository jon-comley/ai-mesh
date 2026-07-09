//! Voice capability — ESPHome Native API client for the Home Assistant
//! Voice hardware. ai-mesh plays the role Home Assistant normally would:
//! dial out to the device, subscribe to its voice-assistant events, capture
//! the audio a wake-word trigger streams, transcribe it (whisper.cpp, see
//! `stt`), and feed the transcript into the coordinator's intent pipeline
//! as a `MeshMessage::IntentRequest` over the agent's existing mesh
//! connection — the same `handle_intent` tool-calling path the dashboard
//! chat and `just intent` use, so a spoken "turn off the kitchen lights"
//! drives the real lighting tools. No TTS yet (the reply is logged, not
//! spoken) — see `plans/voice-assistant-integration.md`.
//!
//! Stock ESPHome firmware, unmodified — see "Why stock firmware, not
//! custom" in `plans/voice-assistant-integration.md`: the XMOS XU316
//! mic-array DSP the device ships with is the actual hard part, and
//! STT/TTS/intent can never run on the ESP32-S3 either way, so there's
//! nothing to gain from replacing it.

pub mod stt;
pub mod tts;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use capability_core::Capability;
use esphome_client::{
    EspHomeClient,
    types::{
        DeviceInfoRequest, EspHomeMessage, LightCommandRequest, ListEntitiesRequest,
        SubscribeVoiceAssistantRequest, VoiceAssistantEvent, VoiceAssistantEventData,
        VoiceAssistantEventResponse, VoiceAssistantResponse,
    },
};
use shared::{IntentRequest, IntentResponse, MeshMessage};
use tokio::sync::mpsc::Sender;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::{debug, info, warn};

/// `VoiceAssistantSubscribeFlag::API_AUDIO` — tells the device to stream
/// audio in-band as `VoiceAssistantAudio` messages over this same TCP
/// connection, instead of a separate UDP socket. The bundled `api.proto`
/// comment says this is bit 0 (value 1); that's stale — the real protocol
/// moved it to bit 2. Confirmed against `aioesphomeapi`'s
/// `VoiceAssistantSubscriptionFlag.API_AUDIO = 1 << 2` and by capturing its
/// actual wire frame against this exact device. (An earlier attempt at
/// this mode appeared to receive no audio at all and was reverted to a UDP
/// socket instead — that turned out to be this dev sandbox's WSL2
/// networking silently dropping *inbound* UDP from the LAN, not a firmware
/// issue. Re-confirmed working end-to-end once tested from a real Linux
/// box (pi1). Using API_AUDIO — one TCP connection, no separate socket or
/// port to negotiate.)
const VOICE_ASSISTANT_SUBSCRIBE_API_AUDIO: u32 = 1 << 2;

// ── End-of-speech detection tuning ───────────────────────────────────────
// The device's own VAD doesn't stop the audio stream in any reasonable
// time on this firmware (observed 500KB+ / 30s+ of continuous audio on a
// single trigger) — it just keeps "listening", unlike e.g. Alexa, which
// reacts within a second of the speaker going quiet. So we detect the end
// of speech ourselves: once the peak amplitude has stayed below
// SILENCE_THRESHOLD for SILENCE_DURATION, the capture ends.
//
// Threshold calibrated against a real captured clip: speech peaked at
// 15,000-28,000 (16-bit PCM, full scale ±32,767); quiet-room ambient sat
// at 700-3,000 with incidental spikes to 6,000-10,000. The original guess
// of 400 was inside the noise floor — every chunk registered as "loud"
// and the detector never fired.
//
// Raised from 5000 to 8000 on 2026-07-08 (same day, first attempt): two live
// STT captures never triggered silence detection at all (ran the full
// MAX_CAPTURE window) — ambient noise crossed 5000 at least once every
// 630-840ms throughout, never leaving the required 1200ms gap.
//
// Reverted 8000 back to 5000 later the same day: a quieter utterance peaked
// at only 6,693 — below 8000 — so the *entire* capture was misread as
// silence from the first chunk, cutting off after the 1200ms floor before
// any real speech was captured (transcript came back as a Whisper
// non-speech tag, "(air whooshing)"). Quiet real speech (6,693) and noisy-
// room ambient spikes (up to 10,000) overlap — no single fixed threshold
// cleanly separates them. Traded the other way instead: keep the threshold
// low enough to always catch quiet speech, and require a longer clean gap
// (SILENCE_DURATION 1200ms -> 2500ms) so a brief ambient spike doesn't
// restart the clock as easily. A persistently noisy room may still hit
// MAX_CAPTURE occasionally, but quiet speech won't get cut short anymore.
//
// Reverted 2500ms back to 1200ms later the same day: the halo staying lit
// noticeably longer after speech ended was worse in practice than the
// occasional noisy-room full-capture problem it was fixing. Back to the
// original calibrated values (5000/1200ms) — accept that a persistently
// noisy moment may again run the full MAX_CAPTURE window.
const SILENCE_THRESHOLD: u16 = 5000;
const SILENCE_DURATION: Duration = Duration::from_millis(1200);
/// Loud audio inside this initial window is the tail of the wake word
/// itself, not the user's request — measured live 2026-07-08: "…Nabu"
/// bled ~250ms of 18k-28k peaks into the start of every capture. Counting
/// that as speech onset meant a natural pause between wake word and
/// question ran out the 1200ms silence clock before the user said
/// anything (capture closed at 1.47s with the question never captured).
const WAKE_WORD_TAIL: Duration = Duration::from_millis(500);
/// How long to wait for the user's speech to *begin* (measured from
/// capture start, ignoring the wake-word tail) before giving up. More
/// generous than SILENCE_DURATION — "Okay Nabu … ⟨think⟩ … what's the
/// temperature?" is normal usage, and closing during that think-pause was
/// exactly the live failure above. Trimmed 4s → 2.5s after live use: 4s
/// made a no-speech trigger feel sluggish, and 2.5s still covers a
/// natural post-wake-word beat (the observed think-pause was ~1.5s).
const SPEECH_START_TIMEOUT: Duration = Duration::from_millis(2500);
/// Outer safety net if silence detection never fires (e.g. continuous
/// background noise).
const MAX_CAPTURE: Duration = Duration::from_secs(15);

/// Without this, a peer that vanishes without a FIN/RST (power cut, WiFi
/// drop) leaves `try_read` parked forever — no error ever surfaces, the
/// reconnect loop never fires, and the capability is silently dead until
/// an agent restart. Same failure family as the mesh TCP port-9000
/// incident (see `project_mesh_tcp_supervision`).
///
/// 90s was the first guess (based on one earlier session where a
/// `PingRequest` happened to arrive around then) and was wrong — live on
/// pi1 the device went fully idle-silent for 90s+ *repeatedly* during
/// completely normal operation, so this was tearing down and
/// reconnecting a healthy connection every ~90s, which is almost
/// certainly why wake-word triggers were getting missed. There's no
/// evidence this firmware sends unprompted keepalives at all, so this is
/// a generous backstop for a genuinely wedged connection, not a
/// liveness check tuned to a real ping interval. A client-initiated
/// `PingRequest` heartbeat would be the correct fix if a tighter
/// detection window is ever needed.
const IDLE_READ_TIMEOUT: Duration = Duration::from_secs(600);

/// One in-progress wake-word capture: the accumulated PCM plus the timing
/// state that decides when it ends.
struct Capture {
    clip: Vec<u8>,
    started: Instant,
    last_loud_at: Instant,
    /// Set once a loud chunk arrives *after* the wake-word tail window —
    /// i.e. the user's actual request has begun. Until then the capture is
    /// in "waiting for speech" mode with its more generous timeout.
    speech_started: bool,
}

impl Capture {
    fn begin() -> Self {
        let now = Instant::now();
        Self {
            clip: Vec::new(),
            started: now,
            last_loud_at: now,
            speech_started: false,
        }
    }

    fn push_chunk(&mut self, pcm: &[u8]) {
        self.note_peak(peak_amplitude(pcm), Instant::now());
        self.clip.extend_from_slice(pcm);
    }

    /// Timing state update, split from `push_chunk` so tests can drive it
    /// with synthetic instants.
    fn note_peak(&mut self, peak: u16, now: Instant) {
        if peak > SILENCE_THRESHOLD {
            if now >= self.started + WAKE_WORD_TAIL {
                self.speech_started = true;
            }
            self.last_loud_at = now;
        }
    }

    /// When this capture should end for lack of sound. Before the user's
    /// speech begins (wake-word tail excluded), that's the generous
    /// SPEECH_START_TIMEOUT — people pause to think after the wake word.
    /// Once speech has begun, SILENCE_DURATION past the last loud chunk.
    fn silence_deadline(&self) -> Instant {
        if self.speech_started {
            self.last_loud_at + SILENCE_DURATION
        } else {
            self.started + SPEECH_START_TIMEOUT
        }
    }

    fn hard_deadline(&self) -> Instant {
        self.started + MAX_CAPTURE
    }
}

/// State shared between the capability object (which the agent's dispatch
/// loop calls into) and the long-lived device task: the current mesh
/// connection's sender, and the intent requests awaiting a coordinator
/// reply. Both use std `Mutex` — every hold is a short insert/remove/clone,
/// never across an await.
struct VoiceShared {
    /// Sender for the agent's *current* coordinator connection. `start()`
    /// refreshes this on every call (main.rs re-runs start() per coordinator
    /// reconnect — that's the hook that keeps it current), so an intent sent
    /// after a reconnect uses the live connection, not the first one ever
    /// seen.
    mesh_tx: Mutex<Option<Sender<MeshMessage>>>,
    /// In-flight intent requests: request_id → the pipeline task waiting on
    /// the coordinator's IntentResponse. `handles()` claims exactly these.
    pending: Mutex<HashMap<String, oneshot::Sender<IntentResponse>>>,
    /// In-flight room-routed announcements: request_id → the pipeline task
    /// waiting to learn whether the coordinator actually delivered the clip
    /// to the room's sink, so it can fall back to the puck's own speaker
    /// when the sink turns out to be unreachable rather than losing the
    /// reply entirely. Separate map from `pending` since the reply shape
    /// (a bare delivered bool) differs from IntentResponse.
    pending_announce: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

pub struct VoiceCapability {
    node_id: String,
    shared: Arc<VoiceShared>,
}

impl VoiceCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            shared: Arc::new(VoiceShared {
                mesh_tx: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                pending_announce: Mutex::new(HashMap::new()),
            }),
        }
    }
}

#[async_trait]
impl Capability for VoiceCapability {
    fn name(&self) -> &'static str {
        "voice"
    }

    fn handles(&self, msg: &MeshMessage) -> bool {
        // Claim only IntentResponses this capability itself requested —
        // matching on request_id keeps any future intent traffic through the
        // agent from being swallowed here.
        match msg {
            MeshMessage::IntentResponse(r) => self
                .shared
                .pending
                .lock()
                .unwrap()
                .contains_key(&r.request_id),
            // Coordinator-initiated synthesis (a broadcast/announcement
            // that didn't start as a spoken request) — see
            // coordinator/src/audio.rs's request_tts.
            MeshMessage::TtsRequest(_) => true,
            // Delivery result for a room-routed reply this capability
            // itself sent — see the puck-fallback logic in `pipeline()`.
            MeshMessage::AudioAnnounceResult(r) => self
                .shared
                .pending_announce
                .lock()
                .unwrap()
                .contains_key(&r.request_id),
            _ => false,
        }
    }

    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String> {
        let Some(address) = device_host() else {
            info!("voice: VOICE_DEVICE_HOST not set — running as stub");
            return Ok(());
        };
        // Refresh the mesh sender on *every* start() call — this runs once
        // per coordinator reconnect, which is exactly when the previous
        // sender goes stale.
        *self.shared.mesh_tx.lock().unwrap() = Some(tx);

        // main.rs's coordinator-reconnect loop calls every capability's
        // start() again on each reconnect (agent <-> coordinator, not agent
        // <-> ESPHome device). Without this guard, each coordinator reconnect
        // would spawn another concurrent `run()` fighting the previous ones
        // over the device's single connection slot — observed live as
        // continuous "connection reset by peer" thrashing once the agent had
        // reconnected to the coordinator a few times. The ESPHome connection
        // is independent of any particular coordinator connection, so it
        // only ever needs to start once per process.
        static STARTED: std::sync::Once = std::sync::Once::new();
        let mut already_started = true;
        STARTED.call_once(|| already_started = false);
        if already_started {
            return Ok(());
        }

        tokio::spawn(async move {
            if let Err(e) = stt::ensure_server_running().await {
                warn!(error = %e, "voice: failed to start whisper-server — STT will be unavailable");
            }
        });
        tokio::spawn(async move {
            if let Err(e) = tts::ensure_servers_running().await {
                warn!(error = %e, "voice: failed to start piper.http_server — TTS will be unavailable");
            }
        });
        let node_id = self.node_id.clone();
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move {
            run(&node_id, &address, &shared).await;
        });
        Ok(())
    }

    async fn handle(&self, msg: MeshMessage, tx: Sender<MeshMessage>) {
        match msg {
            MeshMessage::IntentResponse(resp) => {
                let waiter = self.shared.pending.lock().unwrap().remove(&resp.request_id);
                match waiter {
                    // send() failing means the pipeline task already gave up
                    // (timeout) — nothing left to notify.
                    Some(otx) => drop(otx.send(resp)),
                    None => warn!(
                        request_id = %resp.request_id,
                        "voice: IntentResponse arrived for an unknown request"
                    ),
                }
            }
            // Coordinator wants text synthesized without a live spoken
            // request behind it (e.g. a broadcast announcement) — see
            // coordinator/src/audio.rs's request_tts.
            MeshMessage::TtsRequest(req) => {
                let response = match tts::synthesize(&req.text).await {
                    Ok(wav) => match tts::save_and_get_url(&wav).await {
                        Ok(url) => shared::TtsResponse {
                            request_id: req.request_id,
                            url: Some(url),
                            error: None,
                        },
                        Err(e) => shared::TtsResponse {
                            request_id: req.request_id,
                            url: None,
                            error: Some(e),
                        },
                    },
                    Err(e) => shared::TtsResponse {
                        request_id: req.request_id,
                        url: None,
                        error: Some(e),
                    },
                };
                let _ = tx.send(MeshMessage::TtsResponse(response)).await;
            }
            MeshMessage::AudioAnnounceResult(result) => {
                let waiter = self
                    .shared
                    .pending_announce
                    .lock()
                    .unwrap()
                    .remove(&result.request_id);
                match waiter {
                    // send() failing means the pipeline task already gave up
                    // (timeout) and fell back to the puck on its own —
                    // nothing left to notify.
                    Some(otx) => drop(otx.send(result.delivered)),
                    None => warn!(
                        request_id = %result.request_id,
                        "voice: AudioAnnounceResult arrived for an unknown request"
                    ),
                }
            }
            _ => {}
        }
    }
}

/// The ESPHome device's `host:port`, from `VOICE_DEVICE_HOST`. Shared by
/// the capability itself and the diagnostic examples so nothing hardcodes
/// an address.
pub fn device_host() -> Option<String> {
    std::env::var("VOICE_DEVICE_HOST").ok()
}

async fn run(node_id: &str, address: &str, shared: &Arc<VoiceShared>) {
    loop {
        if let Err(e) = connect_and_listen(node_id, address, shared).await {
            warn!(%address, error = %e, "voice: connection lost, retrying in 5s");
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// How long the pipeline waits for the coordinator's IntentResponse before
/// closing the device run out with an error. Measured live on pi1
/// (2026-07-08): a *cold-cache* intent (first after model load) took 41s —
/// 40.7s of it prefilling the ~3k-token system prompt at ~72 tok/s on the
/// Pi's CPU; the original 30s default fired 11s early. Warm-cache
/// follow-ups reuse llama's prompt prefix cache and finish in seconds, so
/// 60s covers the cold case without leaving a genuinely-stuck run
/// spinning forever. Overridable via `VOICE_INTENT_TIMEOUT_SECS`.
fn intent_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("VOICE_INTENT_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(60),
    )
}

/// How long to wait for the coordinator's `AudioAnnounceResult` before
/// assuming a room-routed reply didn't land and falling back to the
/// puck. Short — this is just "is the sink node connected and did the
/// mesh send succeed," not a synthesis or network-media round trip.
const ANNOUNCE_RESULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Build one device pipeline event with optional name/value data pairs.
fn event(event_type: VoiceAssistantEvent, data: &[(&str, &str)]) -> VoiceAssistantEventResponse {
    VoiceAssistantEventResponse {
        event_type: event_type as i32,
        data: data
            .iter()
            .map(|(name, value)| VoiceAssistantEventData {
                name: name.to_string(),
                value: value.to_string(),
            })
            .collect(),
    }
}

/// Events produced by a detached pipeline task, tagged with the capture
/// generation they belong to. The connection loop writes them to the device
/// only while that generation is still the current one — a pipeline that
/// outlives its run (e.g. a new wake word arrived) must not reset the LED
/// state of the newer run.
type PipeEvent = (u64, VoiceAssistantEventResponse);

/// True when a Whisper result carries no usable speech: empty, a bare
/// non-speech annotation like "(air whooshing)" / "[BLANK_AUDIO]", or pure
/// punctuation like ".".
fn is_no_speech(text: &str) -> bool {
    let t = text.trim();
    if (t.starts_with('(') && t.ends_with(')')) || (t.starts_with('[') && t.ends_with(']')) {
        return true;
    }
    t.chars().all(|c| !c.is_alphanumeric())
}

/// Close a device run from the pipeline: error event (the device's firmware
/// whitelists `stt-no-text-recognized` away from the red error LED; any
/// other code shows a brief red twinkle — honest "that didn't work"
/// feedback), then RunEnd to return it to idle.
async fn close_run_with_error(pipe_tx: &mpsc::Sender<PipeEvent>, generation: u64, code: &str) {
    let error = event(
        VoiceAssistantEvent::VoiceAssistantError,
        &[("code", code), ("message", code)],
    );
    let run_end = event(VoiceAssistantEvent::VoiceAssistantRunEnd, &[]);
    let _ = pipe_tx.send((generation, error)).await;
    let _ = pipe_tx.send((generation, run_end)).await;
}

/// Synthesize `text` and get it onto disk where the coordinator's HTTP
/// route can serve it to the device's media_player. Errors are the
/// caller's to log — this just chains the two steps.
async fn speak(text: &str) -> Result<String, String> {
    let wav = tts::synthesize(text).await?;
    tts::save_and_get_url(&wav).await
}

/// The post-capture pipeline, detached from the device connection loop so a
/// new wake word never has to wait on transcription or the LLM: STT →
/// SttEnd/IntentStart events → IntentRequest over the agent's mesh
/// connection → IntentEnd/RunEnd. The coordinator executes any tool calls
/// (lights, scenes, sensors) itself before replying — by the time the
/// IntentResponse arrives here, the action has already happened; this task
/// just reports and closes out the device run. The reply text is logged
/// only — TTS is the next phase.
async fn pipeline(
    clip: Vec<u8>,
    clip_path: Option<PathBuf>,
    generation: u64,
    pipe_tx: mpsc::Sender<PipeEvent>,
    shared: Arc<VoiceShared>,
) {
    let transcript = match stt::transcribe(&clip).await {
        Ok(t) => t,
        Err(e) => {
            // Clip file is kept for debugging failed transcriptions.
            warn!(error = %e, "voice: STT failed");
            close_run_with_error(&pipe_tx, generation, "stt-no-text-recognized").await;
            return;
        }
    };
    if is_no_speech(&transcript) {
        info!(transcript = %transcript, "voice: no usable speech in capture");
        close_run_with_error(&pipe_tx, generation, "stt-no-text-recognized").await;
        return;
    }
    info!(transcript = %transcript, "voice: STT result");
    // Transcript in hand — the clip has served its purpose. Deleting here
    // (rather than never) keeps ~0.5MB/wake-word from accumulating on pi1's
    // SD card; failed transcriptions above keep theirs for debugging.
    if let Some(path) = clip_path
        && let Err(e) = tokio::fs::remove_file(&path).await
    {
        warn!(path = %path.display(), error = %e, "voice: could not delete transcribed clip");
    }

    let stt_end = event(
        VoiceAssistantEvent::VoiceAssistantSttEnd,
        &[("text", transcript.as_str())],
    );
    let _ = pipe_tx.send((generation, stt_end)).await;
    let _ = pipe_tx
        .send((
            generation,
            event(VoiceAssistantEvent::VoiceAssistantIntentStart, &[]),
        ))
        .await;

    let request_id = uuid::Uuid::new_v4().to_string();
    let (otx, orx) = oneshot::channel();
    shared
        .pending
        .lock()
        .unwrap()
        .insert(request_id.clone(), otx);
    // Clone the *current* connection's sender out of the lock; the send
    // itself must not hold the mutex across an await.
    let mesh_tx = shared.mesh_tx.lock().unwrap().clone();
    let sent = match mesh_tx {
        Some(tx) => tx
            .send(MeshMessage::IntentRequest(IntentRequest {
                request_id: request_id.clone(),
                text: transcript.clone(),
                model_name: None, // largest ready model mesh-wide, same as dashboard chat
                context: vec![],  // stateless v1 — no cross-utterance memory yet
                source: shared::IntentSource::Voice,
            }))
            .await
            .is_ok(),
        None => false,
    };
    if !sent {
        warn!("voice: no live coordinator connection — cannot run intent");
        shared.pending.lock().unwrap().remove(&request_id);
        close_run_with_error(&pipe_tx, generation, "intent-failed").await;
        return;
    }

    match tokio::time::timeout(intent_timeout(), orx).await {
        Ok(Ok(resp)) => {
            if let Some(err) = &resp.error {
                warn!(request_id = %resp.request_id, error = %err, "voice: intent failed");
                close_run_with_error(&pipe_tx, generation, "intent-failed").await;
                return;
            }
            let tools: Vec<&str> = resp.tool_calls.iter().map(|t| t.tool.as_str()).collect();
            info!(
                request_id = %resp.request_id,
                model = %resp.model_name,
                total_ms = resp.total_ms,
                tool_calls = ?tools,
                response = %resp.text.as_deref().unwrap_or(""),
                "voice: intent complete"
            );
            let _ = pipe_tx
                .send((
                    generation,
                    event(VoiceAssistantEvent::VoiceAssistantIntentEnd, &[]),
                ))
                .await;
            // Speak the reply if there's text to speak — tool-call-only
            // responses (e.g. a light command with no narrated answer)
            // skip straight to RunEnd rather than inventing filler
            // speech, matching the no-text-recognized precedent above.
            let text = resp.text.as_deref().unwrap_or("").trim();
            if !text.is_empty() {
                // Phase 6 room routing: if the puck's room (the
                // `av-room:puck` preference, set from the dashboard's
                // Speakers & displays section) has a dedicated speaker
                // configured, the reply goes THERE instead of the
                // puck — never both (that's double-speaking the same
                // reply). Sending TtsEnd{url} to the puck as well as
                // routing to a room sink would make the puck's own
                // media_player fetch and play the identical clip, since
                // that's literally what a tts-end event does on the
                // device side.
                if let Some(room) = tts::room_with_audio_sink().await {
                    match speak(text).await {
                        Ok(url) => {
                            let request_id = uuid::Uuid::new_v4().to_string();
                            let (otx, orx) = oneshot::channel();
                            shared
                                .pending_announce
                                .lock()
                                .unwrap()
                                .insert(request_id.clone(), otx);

                            let mesh_tx = shared.mesh_tx.lock().unwrap().clone();
                            let sent = match mesh_tx {
                                Some(tx) => tx
                                    .send(MeshMessage::AudioAnnounce(
                                        shared::AudioAnnounceRequest {
                                            request_id: request_id.clone(),
                                            url: url.clone(),
                                            room: Some(room),
                                            broadcast: false,
                                        },
                                    ))
                                    .await
                                    .is_ok(),
                                None => false,
                            };

                            // Wait to learn whether the coordinator actually
                            // delivered the clip — the room's sink may be
                            // configured but currently disconnected, in
                            // which case the reply must still reach the
                            // user somehow rather than going nowhere.
                            let delivered = sent
                                && matches!(
                                    tokio::time::timeout(ANNOUNCE_RESULT_TIMEOUT, orx).await,
                                    Ok(Ok(true))
                                );

                            if !delivered {
                                shared.pending_announce.lock().unwrap().remove(&request_id);
                                warn!(
                                    request_id,
                                    "voice: room-routed announcement undelivered — \
                                     falling back to the puck"
                                );
                                let _ = pipe_tx
                                    .send((
                                        generation,
                                        event(VoiceAssistantEvent::VoiceAssistantTtsStart, &[]),
                                    ))
                                    .await;
                                let _ = pipe_tx
                                    .send((
                                        generation,
                                        event(
                                            VoiceAssistantEvent::VoiceAssistantTtsEnd,
                                            &[("url", url.as_str())],
                                        ),
                                    ))
                                    .await;
                            }
                        }
                        Err(e) => warn!(error = %e, "voice: TTS failed — replying silently"),
                    }
                } else {
                    let _ = pipe_tx
                        .send((
                            generation,
                            event(VoiceAssistantEvent::VoiceAssistantTtsStart, &[]),
                        ))
                        .await;
                    match speak(text).await {
                        Ok(url) => {
                            let _ = pipe_tx
                                .send((
                                    generation,
                                    event(
                                        VoiceAssistantEvent::VoiceAssistantTtsEnd,
                                        &[("url", url.as_str())],
                                    ),
                                ))
                                .await;
                        }
                        Err(e) => warn!(error = %e, "voice: TTS failed — replying silently"),
                    }
                }
            }
            let _ = pipe_tx
                .send((
                    generation,
                    event(VoiceAssistantEvent::VoiceAssistantRunEnd, &[]),
                ))
                .await;
        }
        // Timeout, or the response channel dropped. Either way remove the
        // pending entry (a late IntentResponse then logs "unknown request"
        // and is dropped — correct, its waiter is gone).
        _ => {
            warn!(request_id = %request_id, "voice: intent timed out");
            shared.pending.lock().unwrap().remove(&request_id);
            close_run_with_error(&pipe_tx, generation, "intent-failed").await;
        }
    }
}

/// Send one pipeline-progress event. The device's LED animation (its
/// "voice_assistant_phase") is driven entirely by which of these it
/// receives from us — see `plans/voice-assistant-integration.md` for the
/// firmware's actual `on_stt_vad_start`/`on_stt_vad_end`/`on_end` handlers
/// this maps to.
async fn send_event(
    client: &mut EspHomeClient,
    event_type: VoiceAssistantEvent,
) -> Result<(), String> {
    client
        .try_write(VoiceAssistantEventResponse {
            event_type: event_type as i32,
            data: vec![],
        })
        .await
        .map_err(|e| e.to_string())
}

// ── LED ring fade-out ─────────────────────────────────────────────────────
// The firmware's own run-end handling snaps the ring to its idle state
// (off) instantly. The stock firmware also exposes the ring as a normal
// user-controllable light entity ("LED Ring") whose state the firmware
// *restores* when a voice phase ends — so a soft fade is possible without
// custom firmware: set the user light to a dim glow just before RunEnd
// (the idle restore then lands on the glow, not on black), and fade it to
// off over FADE_MS. The final state is off, so the ring stays a pure
// status indicator.
const FADE_MS: u32 = 900;
const FADE_BRIGHTNESS: f32 = 0.4;
const FADE_RGB: (f32, f32, f32) = (0.25, 0.5, 1.0); // soft blue, matches "listening"

fn ring_glow(key: u32) -> LightCommandRequest {
    LightCommandRequest {
        key,
        has_state: true,
        state: true,
        has_brightness: true,
        brightness: FADE_BRIGHTNESS,
        has_rgb: true,
        red: FADE_RGB.0,
        green: FADE_RGB.1,
        blue: FADE_RGB.2,
        has_transition_length: true,
        transition_length: 0,
        ..Default::default()
    }
}

fn ring_fade_off(key: u32) -> LightCommandRequest {
    LightCommandRequest {
        key,
        has_state: true,
        state: false,
        has_transition_length: true,
        transition_length: FADE_MS,
        ..Default::default()
    }
}

/// End the device's pipeline run, fading the ring out instead of the
/// firmware's abrupt cut when the ring's light entity is known.
async fn end_run_with_fade(client: &mut EspHomeClient, ring: Option<u32>) -> Result<(), String> {
    if let Some(key) = ring {
        let _ = client.try_write(ring_glow(key)).await;
    }
    send_event(client, VoiceAssistantEvent::VoiceAssistantRunEnd).await?;
    if let Some(key) = ring {
        let _ = client.try_write(ring_fade_off(key)).await;
    }
    Ok(())
}

/// Close a run that produced no audio at all: SttVadEnd, the whitelisted
/// `stt-no-text-recognized` error (the exact code Home Assistant's own
/// Assist pipeline reports when STT hears nothing usable — the device's
/// firmware keeps it off the red error LED via `code !=
/// "stt-no-text-recognized"` in its `on_error` YAML), then RunEnd.
///
/// (Historical note: the red "error" ring chased during bring-up was never
/// about this sequence — it's the firmware's "no API client connected"
/// twinkle, triggered whenever a short-lived test harness dropped the
/// connection. See plans/voice-assistant-integration.md.)
async fn close_empty_run(client: &mut EspHomeClient, ring: Option<u32>) -> Result<(), String> {
    send_event(client, VoiceAssistantEvent::VoiceAssistantSttVadEnd).await?;
    client
        .try_write(event(
            VoiceAssistantEvent::VoiceAssistantError,
            &[("code", "stt-no-text-recognized")],
        ))
        .await
        .map_err(|e| e.to_string())?;
    end_run_with_fade(client, ring).await
}

/// End a capture: SttVadEnd immediately (the listening halo must track when
/// the *speaker* stopped, not when transcription finished — learned live
/// 2026-07-08 when the halo outstayed its welcome), save the clip, then
/// hand off to the detached [`pipeline`] task for STT → intent → run-close
/// events. Detached so a new wake word never waits on a model.
async fn finish_capture(
    client: &mut EspHomeClient,
    capture: Capture,
    reason: &str,
    generation: u64,
    pipe_tx: &mpsc::Sender<PipeEvent>,
    shared: &Arc<VoiceShared>,
    ring: Option<u32>,
) -> Result<(), String> {
    info!(bytes = capture.clip.len(), reason, "voice: capture ended");
    if capture.clip.is_empty() {
        // A trigger that produced zero audio (e.g. instant device-side
        // cancel) — nothing worth a 0-byte file, and nothing to transcribe.
        info!("voice: no audio received — nothing to save");
        return close_empty_run(client, ring).await;
    }
    send_event(client, VoiceAssistantEvent::VoiceAssistantSttVadEnd).await?;
    let mut clip = capture.clip;
    trim_trailing_silence(&mut clip);
    // Save first so an agent crash mid-pipeline can't lose received audio;
    // the pipeline deletes the file once a transcript is safely extracted.
    let clip_path = save_clip(&clip).await;
    tokio::spawn(pipeline(
        clip,
        clip_path,
        generation,
        pipe_tx.clone(),
        Arc::clone(shared),
    ));
    Ok(())
}

/// Best-effort save of a capture interrupted by connection loss — the
/// audio already received is real; don't throw it away with the error.
async fn salvage_clip(capture: Option<Capture>) {
    if let Some(c) = capture
        && !c.clip.is_empty()
    {
        warn!(
            bytes = c.clip.len(),
            "voice: connection lost mid-capture — salvaging clip"
        );
        save_clip(&c.clip).await;
    }
}

async fn connect_and_listen(
    node_id: &str,
    address: &str,
    shared: &Arc<VoiceShared>,
) -> Result<(), String> {
    info!(%address, "voice: connecting to ESPHome device");
    let mut client = EspHomeClient::builder()
        .address(address)
        .client_info(&format!("ai-mesh ({node_id})"))
        // esphome-client 0.2.0 only sends the required ConnectRequest
        // handshake when a password is configured (`if password.is_some()`
        // in its connection_setup) instead of always sending it with an
        // empty password like the real protocol and every other client
        // does. Skipping it leaves the device considering the session not
        // fully connected — harmless for a stateless DeviceInfoRequest, but
        // it silently drops the connection the moment something stateful
        // like a voice-assistant subscription is attempted. `.password("")`
        // still satisfies `is_some()`, forcing the handshake to actually
        // happen. Confirmed via packet capture against the real device.
        .password("")
        .connect()
        .await
        .map_err(|e| e.to_string())?;
    info!(%address, "voice: connected, handshake complete");

    client
        .try_write(DeviceInfoRequest {})
        .await
        .map_err(|e| e.to_string())?;

    let mut capture: Option<Capture> = None;
    // The ring's user-light entity key, learned from ListEntities — enables
    // the fade-out at run end (None until discovered; fade silently skipped).
    let mut ring_light: Option<u32> = None;
    // Post-capture pipelines run detached (STT + the LLM can take seconds;
    // the device loop must keep servicing wake words) and send their device
    // events back through this channel. Each capture gets a generation
    // number; events from a generation that's no longer current are dropped
    // so a slow pipeline can't reset the LED state of a newer run.
    let (pipe_tx, mut pipe_rx) = mpsc::channel::<PipeEvent>(16);
    let mut generation: u64 = 0;

    loop {
        tokio::select! {
            Some((event_generation, ev)) = pipe_rx.recv() => {
                if event_generation == generation {
                    // RunEnd gets the ring fade choreography instead of the
                    // firmware's abrupt cut.
                    if ev.event_type == VoiceAssistantEvent::VoiceAssistantRunEnd as i32 {
                        end_run_with_fade(&mut client, ring_light).await?;
                    } else {
                        client.try_write(ev).await.map_err(|e| e.to_string())?;
                    }
                } else {
                    debug!(
                        event_generation,
                        current = generation,
                        "voice: dropping pipeline event from a superseded run"
                    );
                }
            }
            read = tokio::time::timeout(IDLE_READ_TIMEOUT, client.try_read()) => {
                let message = match read {
                    Ok(Ok(message)) => message,
                    Ok(Err(e)) => {
                        salvage_clip(capture.take()).await;
                        return Err(e.to_string());
                    }
                    Err(_elapsed) => {
                        salvage_clip(capture.take()).await;
                        return Err(format!(
                            "no traffic from device in {IDLE_READ_TIMEOUT:?} — peer presumed dead"
                        ));
                    }
                };
                match message {
                    EspHomeMessage::DeviceInfoResponse(info_resp) => {
                        info!(
                            name = %info_resp.name,
                            friendly_name = %info_resp.friendly_name,
                            model = %info_resp.model,
                            manufacturer = %info_resp.manufacturer,
                            esphome_version = %info_resp.esphome_version,
                            "voice: device info"
                        );
                        // Sent only once DeviceInfoRequest/Response round-tripped —
                        // firing both requests back-to-back right after connect
                        // reliably got the device to close the connection right
                        // after DeviceInfoResponse, before this ever got processed.
                        client
                            .try_write(SubscribeVoiceAssistantRequest {
                                subscribe: true,
                                flags: VOICE_ASSISTANT_SUBSCRIBE_API_AUDIO,
                            })
                            .await
                            .map_err(|e| e.to_string())?;
                        info!("voice: subscribed to voice assistant events (API_AUDIO)");
                        // Discover the ring's user-light entity for the
                        // run-end fade (see end_run_with_fade).
                        client
                            .try_write(ListEntitiesRequest {})
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                    EspHomeMessage::ListEntitiesLightResponse(light) => {
                        info!(name = %light.name, key = light.key, "voice: ring light entity found");
                        ring_light = Some(light.key);
                    }
                    EspHomeMessage::VoiceAssistantRequest(req) if req.start => {
                        info!(
                            conversation_id = %req.conversation_id,
                            wake_word = %req.wake_word_phrase,
                            "voice: wake word triggered, starting capture"
                        );
                        // A wake word arriving mid-capture shouldn't drop the
                        // in-flight clip unsaved — close it out, then restart.
                        // Its pipeline still runs to completion (a spoken
                        // command should still execute), but bumping the
                        // generation below means its device events are
                        // dropped — they belong to a run the device has
                        // already left.
                        if let Some(prev) = capture.take() {
                            finish_capture(
                                &mut client, prev, "superseded by a new wake word",
                                generation, &pipe_tx, shared, ring_light,
                            ).await?;
                        }
                        generation += 1;
                        capture = Some(Capture::begin());
                        // port: 0 — audio arrives in-band as VoiceAssistantAudio
                        // messages below, not via a UDP port.
                        client
                            .try_write(VoiceAssistantResponse {
                                port: 0,
                                error: false,
                            })
                            .await
                            .map_err(|e| e.to_string())?;
                        // The device's LED animation is driven entirely by
                        // which pipeline events we send: RunStart alone isn't
                        // enough for the "listening" animation, SttVadStart
                        // is. SttStart marks the STT stage beginning, matching
                        // end_capture's stt-no-text-recognized close (no real
                        // STT runs in the crawl phase, so this is an honest
                        // "began, found nothing" shape, not an invented one).
                        send_event(&mut client, VoiceAssistantEvent::VoiceAssistantRunStart).await?;
                        send_event(&mut client, VoiceAssistantEvent::VoiceAssistantSttStart).await?;
                        send_event(&mut client, VoiceAssistantEvent::VoiceAssistantSttVadStart).await?;
                    }
                    EspHomeMessage::VoiceAssistantRequest(_) if capture.is_some() => {
                        // start == false (the arm above matched start == true):
                        // a device-initiated cancel — physical mute flipped
                        // mid-capture, or its own run timeout. Close our side
                        // out now instead of recording ambient noise until the
                        // silence/max deadline for a run it already abandoned.
                        let done = capture.take().expect("guarded by capture.is_some()");
                        finish_capture(
                            &mut client, done, "device requested stop",
                            generation, &pipe_tx, shared, ring_light,
                        ).await?;
                    }
                    EspHomeMessage::VoiceAssistantAudio(audio) if capture.is_some() => {
                        let cap = capture.as_mut().expect("guarded by capture.is_some()");
                        cap.push_chunk(&audio.data);
                        debug!(bytes = audio.data.len(), total = cap.clip.len(), "voice: audio chunk");
                        if audio.end {
                            let done = capture.take().expect("guarded by capture.is_some()");
                            finish_capture(
                                &mut client, done, "device signalled end",
                                generation, &pipe_tx, shared, ring_light,
                            ).await?;
                        }
                    }
                    EspHomeMessage::VoiceAssistantAudio(_) => {
                        // Trailing chunks after a capture already closed —
                        // expected briefly on every cutoff. Kept out of the
                        // catch-all below so ~1KB of raw PCM isn't
                        // Debug-formatted per message.
                    }
                    other => {
                        debug!(?other, "voice: unhandled message");
                    }
                }
            }
            // Audio chunks arrive continuously (~30ms apart) while a capture
            // is live, so these deadlines are re-evaluated constantly.
            () = tokio::time::sleep_until(
                capture.as_ref().map(Capture::silence_deadline).unwrap_or_else(Instant::now)
            ), if capture.is_some() => {
                let done = capture.take().expect("guarded by capture.is_some()");
                finish_capture(
                    &mut client, done, "silence detected",
                    generation, &pipe_tx, shared, ring_light,
                ).await?;
            }
            () = tokio::time::sleep_until(
                capture.as_ref().map(Capture::hard_deadline).unwrap_or_else(Instant::now)
            ), if capture.is_some() => {
                let done = capture.take().expect("guarded by capture.is_some()");
                finish_capture(
                    &mut client, done, "max capture window elapsed",
                    generation, &pipe_tx, shared, ring_light,
                ).await?;
            }
        }
    }
}

/// Peak absolute sample value in a chunk of little-endian 16-bit PCM
/// (ESPHome's documented default assistant audio format), as a `u16` since
/// `i16::MIN.unsigned_abs()` (32768) doesn't fit back in an `i16`. A
/// partial trailing byte (odd-length chunk) is dropped rather than
/// panicking.
fn peak_amplitude(pcm16_le: &[u8]) -> u16 {
    pcm16_le
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
        .max()
        .unwrap_or(0)
}

/// Bytes per TRIM_CHUNK_MS of 16-bit/16kHz/mono PCM, and the quiet padding
/// kept when trimming (whisper behaves better with a little tail room).
const TRIM_CHUNK_MS: usize = 30;
const TRIM_CHUNK_BYTES: usize = 16_000 * 2 * TRIM_CHUNK_MS / 1000;
const TRIM_KEEP_QUIET_CHUNKS: usize = 8; // ~240ms of tail padding

/// Drop the trailing silence a capture always carries (SILENCE_DURATION of
/// dead air by construction — the capture only ends after that much quiet).
/// On pi1's CPU, whisper time scales with clip length, so shipping ~1.2s of
/// silence per utterance was pure added latency (~1-2s of prefill for
/// nothing). Keeps ~240ms of quiet tail padding; never trims a clip that's
/// all quiet (leading audio is the wake-word tail — STT's no-speech path
/// handles it).
fn trim_trailing_silence(clip: &mut Vec<u8>) {
    let chunks = clip.len() / TRIM_CHUNK_BYTES;
    let mut quiet_tail = 0;
    for i in (0..chunks).rev() {
        let start = i * TRIM_CHUNK_BYTES;
        if peak_amplitude(&clip[start..start + TRIM_CHUNK_BYTES]) > SILENCE_THRESHOLD {
            break;
        }
        quiet_tail += 1;
    }
    if quiet_tail > TRIM_KEEP_QUIET_CHUNKS && quiet_tail < chunks {
        let drop_chunks = quiet_tail - TRIM_KEEP_QUIET_CHUNKS;
        clip.truncate(clip.len() - drop_chunks * TRIM_CHUNK_BYTES);
    }
}

/// Persist raw PCM to the clip cache and return its path (None if the write
/// failed — the pipeline still transcribes from memory in that case, there's
/// just no file to clean up). The file exists to survive a crash between
/// capture and transcription; the pipeline deletes it once a transcript is
/// extracted, and keeps it for debugging when STT fails.
/// (`ffplay -f s16le -ar 16000 -ac 1 <file>` plays one back — ESPHome's
/// documented default assistant audio format.)
async fn save_clip(data: &[u8]) -> Option<PathBuf> {
    let dir = cache_dir();
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        warn!(error = %e, "voice: could not create cache dir");
        return None;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("clip-{ts}.raw"));
    match tokio::fs::write(&path, data).await {
        Ok(()) => {
            info!(path = %path.display(), bytes = data.len(), "voice: captured clip saved");
            Some(path)
        }
        Err(e) => {
            warn!(error = %e, "voice: failed to save clip");
            None
        }
    }
}

fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("VOICE_CACHE_DIR") {
        return PathBuf::from(d);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ai-mesh")
        .join("voice-cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    // ── peak_amplitude ───────────────────────────────────────────────────────

    #[test]
    fn peak_amplitude_empty_is_zero() {
        assert_eq!(peak_amplitude(&[]), 0);
    }

    #[test]
    fn peak_amplitude_takes_max_absolute_value() {
        assert_eq!(peak_amplitude(&pcm(&[100, -25000, 3000])), 25000);
    }

    #[test]
    fn peak_amplitude_handles_i16_min() {
        // i16::MIN.abs() would overflow i16 — must come back as 32768.
        assert_eq!(peak_amplitude(&pcm(&[i16::MIN])), 32768);
    }

    #[test]
    fn peak_amplitude_drops_odd_trailing_byte() {
        let mut bytes = pcm(&[1000]);
        bytes.push(0x7f); // half a sample — dropped, not misread or panicked on
        assert_eq!(peak_amplitude(&bytes), 1000);
    }

    // ── Capture ──────────────────────────────────────────────────────────────

    #[test]
    fn capture_accumulates_chunks() {
        let mut c = Capture::begin();
        c.push_chunk(&pcm(&[1, 2]));
        c.push_chunk(&pcm(&[3]));
        assert_eq!(c.clip.len(), 6);
    }

    #[test]
    fn quiet_chunk_does_not_reset_the_silence_clock() {
        let mut c = Capture::begin();
        let before = c.last_loud_at;
        // Just under the threshold — ambient room noise, not speech.
        c.push_chunk(&pcm(&[(SILENCE_THRESHOLD - 1) as i16]));
        assert_eq!(c.last_loud_at, before);
        assert!(!c.speech_started);
    }

    #[test]
    fn loud_chunk_resets_the_silence_clock() {
        let mut c = Capture::begin();
        let before = c.last_loud_at;
        std::thread::sleep(std::time::Duration::from_millis(5));
        c.push_chunk(&pcm(&[(SILENCE_THRESHOLD + 1) as i16]));
        assert!(c.last_loud_at > before);
    }

    #[test]
    fn no_speech_yet_waits_the_full_start_timeout() {
        // A capture that never hears anything loud closes at the (generous)
        // speech-start timeout, not the (tight) post-speech silence window
        // — people pause to think after the wake word.
        let c = Capture::begin();
        assert_eq!(c.silence_deadline(), c.started + SPEECH_START_TIMEOUT);
        assert_eq!(c.hard_deadline(), c.started + MAX_CAPTURE);
    }

    #[test]
    fn wake_word_tail_does_not_count_as_speech_onset() {
        // Loud audio inside the tail window is "…Nabu" itself; the capture
        // must stay in waiting-for-speech mode (observed live: counting it
        // closed the capture during the user's think-pause).
        let mut c = Capture::begin();
        c.note_peak(
            SILENCE_THRESHOLD + 1,
            c.started + Duration::from_millis(100),
        );
        assert!(!c.speech_started);
        assert_eq!(c.silence_deadline(), c.started + SPEECH_START_TIMEOUT);
    }

    #[test]
    fn speech_after_the_tail_switches_to_the_silence_window() {
        let mut c = Capture::begin();
        let onset = c.started + WAKE_WORD_TAIL + Duration::from_millis(300);
        c.note_peak(SILENCE_THRESHOLD + 1, onset);
        assert!(c.speech_started);
        assert_eq!(c.silence_deadline(), onset + SILENCE_DURATION);
    }

    // ── Trailing-silence trim ────────────────────────────────────────────────

    /// N chunks of constant-amplitude PCM, TRIM_CHUNK_BYTES each.
    fn chunks_of(amplitude: i16, n: usize) -> Vec<u8> {
        let sample = amplitude.to_le_bytes();
        sample
            .iter()
            .copied()
            .cycle()
            .take(TRIM_CHUNK_BYTES * n)
            .collect()
    }

    #[test]
    fn trim_drops_long_quiet_tail_keeping_padding() {
        // 10 loud chunks + 40 quiet ones (the SILENCE_DURATION dead air).
        let mut clip = chunks_of(20_000, 10);
        clip.extend(chunks_of(100, 40));
        trim_trailing_silence(&mut clip);
        assert_eq!(clip.len(), TRIM_CHUNK_BYTES * (10 + TRIM_KEEP_QUIET_CHUNKS));
    }

    #[test]
    fn trim_leaves_short_tails_alone() {
        let mut clip = chunks_of(20_000, 10);
        clip.extend(chunks_of(100, TRIM_KEEP_QUIET_CHUNKS));
        let before = clip.len();
        trim_trailing_silence(&mut clip);
        assert_eq!(clip.len(), before);
    }

    #[test]
    fn trim_never_empties_an_all_quiet_clip() {
        // All-quiet clips go to STT's no-speech path intact.
        let mut clip = chunks_of(100, 40);
        let before = clip.len();
        trim_trailing_silence(&mut clip);
        assert_eq!(clip.len(), before);
    }

    // ── Capability plumbing ──────────────────────────────────────────────────

    fn intent_response(request_id: &str) -> IntentResponse {
        IntentResponse {
            request_id: request_id.into(),
            node_id: String::new(),
            model_name: String::new(),
            text: Some("done".into()),
            tool_calls: vec![],
            error: None,
            duration_ms: 0,
            tokens_generated: 0,
            prompt_eval_ms: 0,
            total_ms: 0,
            compression_applied: false,
            prompt_tokens_before: 0,
            prompt_tokens_after: 0,
        }
    }

    #[test]
    fn capability_takes_no_commands() {
        let cap = VoiceCapability::new("test-node");
        assert_eq!(cap.name(), "voice");
        assert!(!cap.handles(&MeshMessage::Acknowledge));
    }

    #[test]
    fn handles_claims_only_pending_intent_responses() {
        let cap = VoiceCapability::new("test-node");
        let (otx, _orx) = oneshot::channel();
        cap.shared
            .pending
            .lock()
            .unwrap()
            .insert("req-1".into(), otx);
        assert!(cap.handles(&MeshMessage::IntentResponse(intent_response("req-1"))));
        // Someone else's intent traffic must not be swallowed.
        assert!(!cap.handles(&MeshMessage::IntentResponse(intent_response("req-2"))));
    }

    #[tokio::test]
    async fn handle_completes_the_pending_waiter() {
        let cap = VoiceCapability::new("test-node");
        let (otx, orx) = oneshot::channel();
        cap.shared
            .pending
            .lock()
            .unwrap()
            .insert("req-1".into(), otx);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        cap.handle(MeshMessage::IntentResponse(intent_response("req-1")), tx)
            .await;
        let resp = orx.await.expect("waiter should be completed");
        assert_eq!(resp.request_id, "req-1");
        assert!(cap.shared.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn handles_claims_only_pending_audio_announce_results() {
        let cap = VoiceCapability::new("test-node");
        let (otx, _orx) = oneshot::channel();
        cap.shared
            .pending_announce
            .lock()
            .unwrap()
            .insert("req-1".into(), otx);
        assert!(cap.handles(&MeshMessage::AudioAnnounceResult(
            shared::AudioAnnounceResult {
                request_id: "req-1".into(),
                delivered: true,
            }
        )));
        assert!(!cap.handles(&MeshMessage::AudioAnnounceResult(
            shared::AudioAnnounceResult {
                request_id: "req-2".into(),
                delivered: true,
            }
        )));
    }

    #[tokio::test]
    async fn handle_completes_the_pending_announce_waiter() {
        let cap = VoiceCapability::new("test-node");
        let (otx, orx) = oneshot::channel();
        cap.shared
            .pending_announce
            .lock()
            .unwrap()
            .insert("req-1".into(), otx);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        cap.handle(
            MeshMessage::AudioAnnounceResult(shared::AudioAnnounceResult {
                request_id: "req-1".into(),
                delivered: true,
            }),
            tx,
        )
        .await;
        let delivered = orx.await.expect("waiter should be completed");
        assert!(delivered);
        assert!(cap.shared.pending_announce.lock().unwrap().is_empty());
    }

    // ── No-speech heuristic ──────────────────────────────────────────────────

    #[test]
    fn no_speech_detects_empty_and_whisper_tags() {
        assert!(is_no_speech(""));
        assert!(is_no_speech("   "));
        assert!(is_no_speech("(air whooshing)"));
        assert!(is_no_speech("[BLANK_AUDIO]"));
        assert!(is_no_speech("."));
    }

    #[test]
    fn no_speech_accepts_real_commands() {
        assert!(!is_no_speech("Turn off the kitchen lights."));
        assert!(!is_no_speech("Testing, 1, 2, 3."));
    }

    // ── Pipeline events ──────────────────────────────────────────────────────

    #[test]
    fn event_builds_name_value_pairs() {
        let ev = event(
            VoiceAssistantEvent::VoiceAssistantSttEnd,
            &[("text", "turn off the lights")],
        );
        assert_eq!(
            ev.event_type,
            VoiceAssistantEvent::VoiceAssistantSttEnd as i32
        );
        assert_eq!(ev.data.len(), 1);
        assert_eq!(ev.data[0].name, "text");
        assert_eq!(ev.data[0].value, "turn off the lights");
    }
}
