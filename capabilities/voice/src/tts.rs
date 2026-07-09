//! Piper TTS: spawn one persistent `piper.http_server` per voice and
//! synthesize replies over HTTP. Mirrors `stt.rs`'s server-lifecycle
//! pattern, generalized from one child process to N (one per voice) so
//! switching voices is genuinely instant — every voice is always warm,
//! never loaded on demand.
//!
//! Piper (`OHF-Voice/piper1-gpl`) ships only as a pip package, not a
//! prebuilt binary like whisper.cpp/llama.cpp — a first for this repo.
//! It's still just a subprocess we talk HTTP to; running an unmodified
//! GPL-licensed program as a subprocess doesn't reach into our own code
//! (the standard "mere aggregation" boundary), so this stays consistent
//! with the rest of the mesh's licensing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::process::Child;
use tracing::{info, warn};

/// name, default model filename (without extension), port. Licenses
/// verified directly against each MODEL_CARD on
/// huggingface.co/rhasspy/piper-voices, not assumed.
///
/// **Commercial-use status** (this is a personal home project, so none
/// of this currently matters operationally — recorded here in case that
/// ever changes, e.g. this code or the models get shared/reused
/// elsewhere):
/// - **Cleared for commercial use**: `joe` (CC0 — public domain
///   dedication, no restriction at all), `kristin` (public domain),
///   `ljspeech` (public domain), `alba` (CC BY 4.0 — commercial use
///   allowed, attribution required).
/// - **Not cleared for commercial use**: `alan` ("All Rights Reserved",
///   Mycroft AI — no license grant at all, just accepted here as
///   low-risk for personal, non-commercial use).
const VOICES: &[(&str, &str, u16)] = &[
    ("joe", "en_US-joe-medium", 5001),
    ("kristin", "en_US-kristin-medium", 5002),
    ("ljspeech", "en_US-ljspeech-medium", 5003),
    ("alan", "en_GB-alan-medium", 5004),
    ("alba", "en_GB-alba-medium", 5005),
];

const DEFAULT_VOICE: &str = "alan";

fn venv_python() -> PathBuf {
    std::env::var("VOICE_TTS_VENV")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/piper"))
        .join("bin")
        .join("python3")
}

fn model_dir() -> PathBuf {
    std::env::var("VOICE_TTS_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".ai-mesh")
                .join("tts-models")
        })
}

/// The coordinator's own HTTP API — pi1 co-locates agent and coordinator,
/// same assumption the intent-routing work already relies on.
fn dashboard_base_url() -> String {
    std::env::var("VOICE_TTS_DASHBOARD_URL").unwrap_or_else(|_| "http://127.0.0.1:9001".into())
}

/// Base URL the ESPHome device's media_player fetches TTS clips from.
/// **Must be pi1's real LAN address, never loopback** — unlike
/// `dashboard_base_url()` (agent talking to the coordinator on the same
/// box, where 127.0.0.1 is correct), this URL is handed to a *different*
/// physical machine (the puck) which can't reach pi1's loopback at all.
/// Confirmed live 2026-07-09: defaulting this to `dashboard_base_url()`
/// produced a URL the device silently couldn't fetch — no error, no
/// playback, just nothing, because 127.0.0.1 on the puck points at
/// itself. `VOICE_TTS_BASE_URL` must be set explicitly (see
/// `nodes/pi1.env`); no safe default exists.
fn tts_media_base_url() -> String {
    std::env::var("VOICE_TTS_BASE_URL").unwrap_or_else(|_| dashboard_base_url())
}

/// **Not** under `~` like the other voice caches (STT clips, models) —
/// this one must also be readable by the *coordinator* process, which
/// runs with `ProtectHome=true` (deliberately, per its own unit file:
/// "no home-dir access is required") and so cannot see anything under
/// `/home` at all. Confirmed live 2026-07-09: defaulting this to
/// `~/.ai-mesh/tts-cache` made every clip 404 from the coordinator's
/// side with no error, just ENOENT-from-its-perspective, since the path
/// was invisible to it. `/var/lib/ai-mesh` is the coordinator's own
/// `StateDirectory` (already writable by the same `jonno` user the
/// agent runs as, no sandboxing on the agent's side) — sharing it here
/// keeps both processes able to see the same file without loosening
/// either one's security posture.
fn cache_dir() -> PathBuf {
    std::env::var("VOICE_TTS_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/ai-mesh/tts-cache"))
}

static TTS_CHILDREN: OnceLock<Mutex<HashMap<&'static str, Child>>> = OnceLock::new();

fn children_lock() -> &'static Mutex<HashMap<&'static str, Child>> {
    TTS_CHILDREN.get_or_init(|| Mutex::new(HashMap::new()))
}

const HEALTH_TIMEOUT_SECS: u64 = 60;

fn health_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/voices")
}

/// Spawn every voice's `piper.http_server` if not already running and
/// tracked. Safe to call repeatedly (e.g. once per capability start) —
/// a no-op for any voice whose tracked child is still alive.
pub async fn ensure_servers_running() -> Result<(), String> {
    for &(name, model, port) in VOICES {
        {
            let mut guard = children_lock().lock().unwrap();
            if let Some(child) = guard.get_mut(name) {
                if matches!(child.try_wait(), Ok(None)) {
                    continue; // already running
                }
                guard.remove(name);
            }
        }

        let model_path = model_dir().join(format!("{model}.onnx"));
        if !model_path.exists() {
            warn!(
                voice = name,
                path = %model_path.display(),
                "voice: TTS model not found — skipping this voice"
            );
            continue;
        }

        let mut cmd = tokio::process::Command::new(venv_python());
        cmd.arg("-m")
            .arg("piper.http_server")
            .arg("-m")
            .arg(&model_path)
            .arg("--port")
            .arg(port.to_string());

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!(voice = name, error = %e, "voice: failed to start piper.http_server");
                continue;
            }
        };
        children_lock().lock().unwrap().insert(name, child);

        if let Err(e) = wait_for_health(name, port).await {
            warn!(voice = name, error = %e, "voice: piper.http_server did not become healthy");
        }
    }
    Ok(())
}

async fn wait_for_health(name: &str, port: u16) -> Result<(), String> {
    let client = reqwest::Client::new();
    let url = health_url(port);
    for elapsed in 0..HEALTH_TIMEOUT_SECS {
        tokio::time::sleep(Duration::from_secs(1)).await;
        {
            let mut guard = children_lock().lock().unwrap();
            if let Some(child) = guard.get_mut(name) {
                if let Ok(Some(status)) = child.try_wait() {
                    guard.remove(name);
                    return Err(format!("piper.http_server exited during startup: {status}"));
                }
            } else {
                return Err("piper.http_server was stopped during startup".into());
            }
        }
        if let Ok(r) = client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            && r.status().is_success()
        {
            info!(voice = name, elapsed, "voice: piper.http_server ready");
            return Ok(());
        }
    }
    Err(format!(
        "piper.http_server did not become healthy within {HEALTH_TIMEOUT_SECS}s"
    ))
}

/// Fetches the dashboard's whole preferences map fresh on every call — no
/// caching, so a change on the dashboard takes effect on the very next
/// reply. `None` on any failure (network, auth, bad JSON): callers must
/// treat a broken fetch as "no preferences set," never as an error to
/// propagate — a preferences outage must not block a voice reply.
async fn fetch_prefs() -> Option<std::collections::HashMap<String, String>> {
    let token = std::env::var("MESH_AUTH_TOKEN").unwrap_or_default();
    let url = format!("{}/api/preferences?token={token}", dashboard_base_url());
    reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()
}

/// The live-selected voice — see `fetch_prefs`' doc for the "why fetch
/// every time" reasoning.
pub async fn current_voice() -> &'static str {
    let Some(prefs) = fetch_prefs().await else {
        return DEFAULT_VOICE;
    };
    match prefs.get("tts-voice").map(String::as_str) {
        Some(name) if VOICES.iter().any(|&(n, _, _)| n == name) => {
            VOICES.iter().find(|&&(n, _, _)| n == name).unwrap().0
        }
        _ => DEFAULT_VOICE,
    }
}

/// The puck's room, if it both has one assigned AND that room has a
/// dedicated speaker configured — the two conditions under which a spoken
/// reply should route to the room's sink instead of the puck's own
/// speaker. Both live in the same dashboard K/V store `tts-voice` uses:
/// the puck's room is the `av-room:puck` preference (set from the
/// Speakers & displays room dropdown), the room's speaker is
/// `room-audio-sink:<room>`. `None` for "not configured" and "couldn't
/// check" alike — both mean the same thing to the caller: fall back to
/// the puck's own speaker.
pub async fn room_with_audio_sink() -> Option<String> {
    let prefs = fetch_prefs().await?;
    let room = prefs.get("av-room:puck")?;
    if prefs.contains_key(&format!("room-audio-sink:{room}")) {
        Some(room.clone())
    } else {
        None
    }
}

#[derive(serde::Serialize)]
struct SynthesizeRequest<'a> {
    text: &'a str,
}

/// Synthesize `text` in the currently-selected voice. Returns WAV bytes.
pub async fn synthesize(text: &str) -> Result<Vec<u8>, String> {
    let voice = current_voice().await;
    let port = VOICES
        .iter()
        .find(|&&(n, _, _)| n == voice)
        .map(|&(_, _, p)| p)
        .ok_or_else(|| format!("unknown voice: {voice}"))?;

    // The Flask server's synthesis route is POST / (root) — confirmed by
    // reading piper.http_server's actual @app.route decorators live on
    // pi1; there is no /synthesize path, unlike whisper-server's
    // /inference convention this was originally modeled on.
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/"))
        .json(&SynthesizeRequest { text })
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!(
            "piper.http_server ({voice}) returned {}",
            resp.status()
        ));
    }
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

/// Persist synthesized WAV bytes to the shared TTS cache and return the
/// URL the ESPHome device's media_player fetches it from. Mirrors
/// `lib.rs`'s `save_clip` pattern (save first, serve/clean up after).
/// A clip only gets deleted (by `coordinator/src/http/api/voice.rs`) once
/// the device actually fetches it. A `TtsEnd` event that never reaches the
/// device, or a fetch it never makes, leaves the file with no cleanup —
/// unbounded accumulation, same failure shape as the disk-space incidents
/// hit twice earlier today. Rather than a background sweep task, piggyback
/// a stale-file check onto every write: cheap, and this dir is only ever
/// touched here.
const ORPHAN_CLIP_MAX_AGE: Duration = Duration::from_secs(300);

async fn sweep_orphaned_clips(dir: &std::path::Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let age = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok());
        if age.is_some_and(|a| a > ORPHAN_CLIP_MAX_AGE) {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

pub async fn save_and_get_url(wav: &[u8]) -> Result<String, String> {
    let dir = cache_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    sweep_orphaned_clips(&dir).await;
    let id = uuid::Uuid::new_v4();
    let path = dir.join(format!("{id}.wav"));
    tokio::fs::write(&path, wav)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("{}/api/voice/tts/{id}", tts_media_base_url()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sweep_removes_only_stale_clips() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh.wav");
        let stale = dir.path().join("stale.wav");
        tokio::fs::write(&fresh, b"x").await.unwrap();
        tokio::fs::write(&stale, b"x").await.unwrap();
        // Backdate the "stale" file's mtime past ORPHAN_CLIP_MAX_AGE.
        let old = std::time::SystemTime::now() - ORPHAN_CLIP_MAX_AGE - Duration::from_secs(1);
        let file = std::fs::File::open(&stale).unwrap();
        file.set_modified(old).unwrap();

        sweep_orphaned_clips(dir.path()).await;

        assert!(fresh.exists());
        assert!(!stale.exists());
    }

    #[test]
    fn voices_have_unique_ports_and_names() {
        let mut ports: Vec<u16> = VOICES.iter().map(|&(_, _, p)| p).collect();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(ports.len(), VOICES.len());

        let mut names: Vec<&str> = VOICES.iter().map(|&(n, _, _)| n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), VOICES.len());
    }

    #[test]
    fn default_voice_is_a_real_voice() {
        assert!(VOICES.iter().any(|&(n, _, _)| n == DEFAULT_VOICE));
    }
}
