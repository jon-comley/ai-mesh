//! Voice capability — crawl-phase ESPHome Native API client for the Home
//! Assistant Voice hardware. ai-mesh plays the role Home Assistant normally
//! would: dial out to the device, subscribe to its voice-assistant events,
//! and (for now) just capture the raw audio clip it sends on a wake-word
//! trigger to a file. No speech-to-text/intent/text-to-speech wiring yet —
//! see `docs/roadmap.md` and `plans/multi-domain-home.md` Phase C.
//!
//! Stock ESPHome firmware, unmodified — see "Why stock firmware, not
//! custom" in `plans/voice-assistant-integration.md`: the XMOS XU316
//! mic-array DSP the device ships with is the actual hard part, and
//! STT/TTS/intent can never run on the ESP32-S3 either way, so there's
//! nothing to gain from replacing it.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use capability_core::Capability;
use esphome_client::{
    EspHomeClient,
    types::{
        DeviceInfoRequest, EspHomeMessage, SubscribeVoiceAssistantRequest, VoiceAssistantEvent,
        VoiceAssistantEventData, VoiceAssistantEventResponse, VoiceAssistantResponse,
    },
};
use shared::MeshMessage;
use tokio::sync::mpsc::Sender;
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
const SILENCE_THRESHOLD: u16 = 5000;
const SILENCE_DURATION: Duration = Duration::from_millis(1200);
/// Floor before silence can end a capture — the mic opens before the user
/// starts speaking, so an instant cutoff would capture nothing.
const MIN_CAPTURE: Duration = Duration::from_millis(400);
/// Outer safety net if silence detection never fires (e.g. continuous
/// background noise).
const MAX_CAPTURE: Duration = Duration::from_secs(15);

/// The device pings us on its own every ~60s (observed live), so a read
/// gap well past that means the peer is gone without a FIN/RST — power
/// cut, WiFi drop. Without this, `try_read` parks forever, no error ever
/// surfaces, and the reconnect loop never fires: the capability would be
/// silently dead until an agent restart. Same failure family as the mesh
/// TCP port-9000 incident (see `project_mesh_tcp_supervision`).
const IDLE_READ_TIMEOUT: Duration = Duration::from_secs(90);

/// One in-progress wake-word capture: the accumulated PCM plus the timing
/// state that decides when it ends.
struct Capture {
    clip: Vec<u8>,
    started: Instant,
    last_loud_at: Instant,
}

impl Capture {
    fn begin() -> Self {
        let now = Instant::now();
        Self {
            clip: Vec::new(),
            started: now,
            last_loud_at: now,
        }
    }

    fn push_chunk(&mut self, pcm: &[u8]) {
        if peak_amplitude(pcm) > SILENCE_THRESHOLD {
            self.last_loud_at = Instant::now();
        }
        self.clip.extend_from_slice(pcm);
    }

    /// When silence should end this capture — SILENCE_DURATION past the
    /// last loud chunk, but never before MIN_CAPTURE from the start.
    fn silence_deadline(&self) -> Instant {
        (self.last_loud_at + SILENCE_DURATION).max(self.started + MIN_CAPTURE)
    }

    fn hard_deadline(&self) -> Instant {
        self.started + MAX_CAPTURE
    }
}

pub struct VoiceCapability {
    node_id: String,
}

impl VoiceCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
        }
    }
}

#[async_trait]
impl Capability for VoiceCapability {
    fn name(&self) -> &'static str {
        "voice"
    }

    fn handles(&self, _msg: &MeshMessage) -> bool {
        false
    }

    async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> {
        let Some(address) = device_host() else {
            info!("voice: VOICE_DEVICE_HOST not set — running as stub");
            return Ok(());
        };
        let node_id = self.node_id.clone();
        tokio::spawn(async move {
            run(&node_id, &address).await;
        });
        Ok(())
    }

    async fn handle(&self, _msg: MeshMessage, _tx: Sender<MeshMessage>) {
        // handles() is always false — this capability takes no commands yet.
    }
}

/// The ESPHome device's `host:port`, from `VOICE_DEVICE_HOST`. Shared by
/// the capability itself and the diagnostic examples so nothing hardcodes
/// an address.
pub fn device_host() -> Option<String> {
    std::env::var("VOICE_DEVICE_HOST").ok()
}

async fn run(node_id: &str, address: &str) {
    loop {
        if let Err(e) = connect_and_listen(node_id, address).await {
            warn!(%address, error = %e, "voice: connection lost, retrying in 5s");
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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

/// Close out a capture cleanly: SttVadEnd (device animates "thinking"),
/// then an `Error` event with code `stt-no-text-recognized`, then RunEnd
/// (device resets to idle). The error event is the honest way to close a
/// pipeline that ran no real STT — it's the exact code Home Assistant's
/// own Assist pipeline reports when STT hears nothing usable, and the
/// device's firmware explicitly whitelists it away from the error LED
/// (`code != "stt-no-text-recognized"` in its `on_error` YAML). Once real
/// STT lands, this becomes `SttEnd{text}` → intent → TTS events instead.
///
/// (Historical note: the red "error" ring chased during bring-up was never
/// about this sequence — it's the firmware's "no API client connected"
/// twinkle, triggered whenever a short-lived test harness dropped the
/// connection. See plans/voice-assistant-integration.md.)
async fn end_capture(client: &mut EspHomeClient) -> Result<(), String> {
    send_event(client, VoiceAssistantEvent::VoiceAssistantSttVadEnd).await?;
    client
        .try_write(VoiceAssistantEventResponse {
            event_type: VoiceAssistantEvent::VoiceAssistantError as i32,
            data: vec![VoiceAssistantEventData {
                name: "code".to_string(),
                value: "stt-no-text-recognized".to_string(),
            }],
        })
        .await
        .map_err(|e| e.to_string())?;
    send_event(client, VoiceAssistantEvent::VoiceAssistantRunEnd).await
}

/// Save the finished clip (saving first, so a device write failure can't
/// lose already-received audio) and close the pipeline out on the device.
async fn finish_capture(
    client: &mut EspHomeClient,
    capture: Capture,
    reason: &str,
) -> Result<(), String> {
    info!(bytes = capture.clip.len(), reason, "voice: capture ended");
    if capture.clip.is_empty() {
        // A trigger that produced zero audio (e.g. instant device-side
        // cancel) — nothing worth a 0-byte file.
        info!("voice: no audio received — nothing to save");
    } else {
        save_clip(&capture.clip).await;
    }
    end_capture(client).await
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

async fn connect_and_listen(node_id: &str, address: &str) -> Result<(), String> {
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

    loop {
        tokio::select! {
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
                    }
                    EspHomeMessage::VoiceAssistantRequest(req) if req.start => {
                        info!(
                            conversation_id = %req.conversation_id,
                            wake_word = %req.wake_word_phrase,
                            "voice: wake word triggered, starting capture"
                        );
                        // A wake word arriving mid-capture shouldn't drop the
                        // in-flight clip unsaved — close it out, then restart.
                        if let Some(prev) = capture.take() {
                            finish_capture(&mut client, prev, "superseded by a new wake word")
                                .await?;
                        }
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
                        finish_capture(&mut client, done, "device requested stop").await?;
                    }
                    EspHomeMessage::VoiceAssistantAudio(audio) if capture.is_some() => {
                        let cap = capture.as_mut().expect("guarded by capture.is_some()");
                        cap.push_chunk(&audio.data);
                        debug!(bytes = audio.data.len(), total = cap.clip.len(), "voice: audio chunk");
                        if audio.end {
                            let done = capture.take().expect("guarded by capture.is_some()");
                            finish_capture(&mut client, done, "device signalled end").await?;
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
                finish_capture(&mut client, done, "silence detected").await?;
            }
            () = tokio::time::sleep_until(
                capture.as_ref().map(Capture::hard_deadline).unwrap_or_else(Instant::now)
            ), if capture.is_some() => {
                let done = capture.take().expect("guarded by capture.is_some()");
                finish_capture(&mut client, done, "max capture window elapsed").await?;
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

/// Crawl-phase only: dump raw PCM bytes to disk so a clip can be proven to
/// have arrived at all (`ffplay -f s16le -ar 16000 -ac 1 <file>` — ESPHome's
/// documented default assistant audio format — should play it back).
/// Nothing downstream consumes this yet; STT wiring is a later phase.
async fn save_clip(data: &[u8]) {
    let dir = cache_dir();
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        warn!(error = %e, "voice: could not create cache dir");
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("clip-{ts}.raw"));
    match tokio::fs::write(&path, data).await {
        Ok(()) => info!(path = %path.display(), bytes = data.len(), "voice: captured clip saved"),
        Err(e) => warn!(error = %e, "voice: failed to save clip"),
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
    fn silence_deadline_never_precedes_min_capture() {
        // A capture that never hears anything loud must still last
        // MIN_CAPTURE — the mic opens before the user starts speaking.
        let c = Capture::begin();
        assert!(c.silence_deadline() >= c.started + MIN_CAPTURE);
        assert_eq!(c.hard_deadline(), c.started + MAX_CAPTURE);
    }

    // ── Capability plumbing ──────────────────────────────────────────────────

    #[test]
    fn capability_takes_no_commands() {
        let cap = VoiceCapability::new("test-node");
        assert_eq!(cap.name(), "voice");
        assert!(!cap.handles(&MeshMessage::Acknowledge));
    }
}
