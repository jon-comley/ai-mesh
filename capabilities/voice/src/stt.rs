//! whisper.cpp STT: spawn `whisper-server` as a persistent child process and
//! transcribe captured clips over its HTTP `/inference` endpoint. Mirrors
//! `capabilities/llm/src/llama.rs`'s server-lifecycle pattern (spawn, track
//! the child, health-check, talk HTTP), simplified — one fixed model for the
//! capability's lifetime, no dynamic load/unload.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::process::Child;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

fn server_bin() -> String {
    std::env::var("VOICE_STT_SERVER_BIN").unwrap_or_else(|_| "whisper-server".into())
}

fn server_bin_path() -> PathBuf {
    PathBuf::from(server_bin())
}

fn model_path() -> PathBuf {
    if let Ok(p) = std::env::var("VOICE_STT_MODEL") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ai-mesh")
        .join("voice-models")
        .join("ggml-base.en.bin")
}

fn host() -> String {
    std::env::var("VOICE_STT_HOST").unwrap_or_else(|_| "127.0.0.1".into())
}

fn port() -> u16 {
    std::env::var("VOICE_STT_PORT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(8081)
}

fn base_url() -> String {
    format!("http://{}:{}", host(), port())
}

static STT_CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn child_lock() -> &'static Mutex<Option<Child>> {
    STT_CHILD.get_or_init(|| Mutex::new(None))
}

/// Bounded wait for `whisper-server` to report ready — fixed ceiling since,
/// unlike LLM GGUFs, whisper models are small (tens to a few hundred MB) and
/// load in a couple of seconds even on a Pi's CPU.
const HEALTH_TIMEOUT_SECS: u64 = 60;

/// Start `whisper-server` if it isn't already running under our tracking.
/// Safe to call repeatedly (e.g. once per capture) — a no-op once the
/// tracked child is alive.
pub async fn ensure_server_running() -> Result<(), String> {
    {
        let mut guard = child_lock().lock().await;
        if let Some(child) = guard.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                return Ok(());
            }
            guard.take();
        }
    }

    let model = model_path();
    if !model.exists() {
        return Err(format!(
            "whisper model not found at {} (set VOICE_STT_MODEL)",
            model.display()
        ));
    }

    kill_stray_on_port(port());

    let mut cmd = tokio::process::Command::new(server_bin());
    cmd.arg("--model")
        .arg(&model)
        .arg("--host")
        .arg(host())
        .arg("--port")
        .arg(port().to_string());

    // whisper-server's .so files live alongside its binary. Set this
    // explicitly for the child only, rather than relying on the agent
    // process's own (possibly llama.cpp-pointing) LD_LIBRARY_PATH — llama.cpp
    // and whisper.cpp each ship a same-named ggml backend plugin
    // (libggml-cpu.so) that aren't binary-compatible across projects/
    // versions, so a merged search path risks one server silently loading
    // the other's plugin. Confirmed live 2026-07-08: merging them at the
    // systemd level broke llama-server ("no CPU backend found") the moment
    // whisper.cpp's directory came first in the combined path.
    if let Some(lib_dir) = server_bin_path().parent() {
        cmd.env("LD_LIBRARY_PATH", lib_dir);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to start whisper-server: {e}"))?;
    *child_lock().lock().await = Some(child);

    let health_url = format!("{}/health", base_url());
    let client = reqwest::Client::new();
    for elapsed in 0..HEALTH_TIMEOUT_SECS {
        tokio::time::sleep(Duration::from_secs(1)).await;
        {
            let mut guard = child_lock().lock().await;
            match guard.as_mut() {
                Some(child) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        guard.take();
                        return Err(format!("whisper-server exited during startup: {status}"));
                    }
                }
                None => return Err("whisper-server was stopped during startup".into()),
            }
        }
        if let Ok(r) = client
            .get(&health_url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            && r.status().is_success()
        {
            info!(elapsed, "voice: whisper-server ready");
            return Ok(());
        }
    }

    Err(format!(
        "whisper-server did not become healthy within {HEALTH_TIMEOUT_SECS}s"
    ))
}

#[derive(serde::Deserialize)]
struct InferenceResponse {
    text: String,
}

/// Silence prepended to every clip before whisper.cpp sees it. Captures
/// begin exactly at the wake-word trigger with no lead-in — live testing
/// 2026-07-08 showed the model mis-transcribing/dropping the first words of
/// a clip that started right on speech with no quiet run-up. 300ms is a
/// starting guess (not yet independently tuned) to give the model's
/// windowing something to settle into before real speech starts.
const PREROLL_MS: u32 = 300;

fn silence_preroll() -> Vec<u8> {
    let samples = (16_000 * PREROLL_MS / 1000) as usize;
    vec![0u8; samples * 2] // 16-bit silence, 2 bytes/sample
}

// ── Remote STT offload ────────────────────────────────────────────────────
// pi1's CPU takes ~5s to transcribe a 3s clip with base.en — the dominant
// share of the perceived response time (measured live 2026-07-08). A
// faster mesh node (the Beelink) can do the same in well under a second,
// so when `VOICE_STT_REMOTE` (host:port) is set, clips go there first and
// the local whisper-server becomes the fallback: voice keeps working when
// the remote box is off, just slower. Direct HTTP rather than a mesh
// Feature::Stt on purpose — the right shape for a two-node reality; the
// coordinator-routed version is the eventual design if STT ever needs
// scheduling across 3+ nodes.

fn remote_stt() -> Option<String> {
    std::env::var("VOICE_STT_REMOTE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Remote attempts get a tight budget: the fast box answers in well under
/// a second, so anything slower than this means it's down/overloaded and
/// the local fallback will beat waiting for it.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(5);
const LOCAL_TIMEOUT: Duration = Duration::from_secs(30);
/// After a remote failure, don't re-try the remote for this long — an
/// offline Beelink must not add a connection-timeout penalty to every
/// single utterance.
const REMOTE_COOLDOWN_MS: u64 = 60_000;

static REMOTE_FAILED_AT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Pure decision: is the remote still inside its post-failure cooldown?
fn cooldown_active(failed_at_ms: u64, now_ms: u64) -> bool {
    failed_at_ms != 0 && now_ms.saturating_sub(failed_at_ms) < REMOTE_COOLDOWN_MS
}

/// POST one WAV to a whisper-server `/inference` endpoint.
async fn post_inference(url: &str, wav: Vec<u8>, timeout: Duration) -> Result<String, String> {
    let part = reqwest::multipart::Part::bytes(wav)
        .file_name("clip.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json");

    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .multipart(form)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("whisper-server returned {}", resp.status()));
    }
    let body: InferenceResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body.text.trim().to_string())
}

/// Transcribe one captured clip. `pcm` is 16-bit little-endian, 16 kHz, mono
/// — the format `lib.rs` already documents and captures from the ESPHome
/// device — prefixed with a silence pre-roll, wrapped in a WAV header, and
/// posted to the remote whisper-server when configured (local fallback),
/// else the local one.
pub async fn transcribe(pcm: &[u8]) -> Result<String, String> {
    use std::sync::atomic::Ordering;

    let mut padded = silence_preroll();
    padded.extend_from_slice(pcm);
    let wav = wrap_wav(&padded);

    if let Some(remote) = remote_stt() {
        if cooldown_active(REMOTE_FAILED_AT_MS.load(Ordering::Relaxed), epoch_ms()) {
            debug!(%remote, "voice: remote STT in cooldown — using local");
        } else {
            let started = std::time::Instant::now();
            match post_inference(
                &format!("http://{remote}/inference"),
                wav.clone(),
                REMOTE_TIMEOUT,
            )
            .await
            {
                Ok(text) => {
                    REMOTE_FAILED_AT_MS.store(0, Ordering::Relaxed);
                    info!(%remote, ms = started.elapsed().as_millis() as u64, "voice: transcribed remotely");
                    return Ok(text);
                }
                Err(e) => {
                    REMOTE_FAILED_AT_MS.store(epoch_ms(), Ordering::Relaxed);
                    warn!(%remote, error = %e, "voice: remote STT failed — falling back to local for 60s");
                }
            }
        }
    }
    post_inference(&format!("{}/inference", base_url()), wav, LOCAL_TIMEOUT).await
}

/// Wrap raw PCM samples in a minimal 44-byte canonical WAV header.
/// `whisper-server` expects a real WAV file (its reader parses the RIFF/fmt/
/// data chunks), not headerless PCM — this is a pure function so it's
/// testable without a running server.
fn wrap_wav(pcm: &[u8]) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 16_000;
    const CHANNELS: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 16;
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * (BITS_PER_SAMPLE / 8) as u32;
    let block_align = CHANNELS * (BITS_PER_SAMPLE / 8);
    let data_len = pcm.len() as u32;
    let riff_len = 36 + data_len;

    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

// ── Stray-process-on-port cleanup ────────────────────────────────────────────
// Same approach as capabilities/llm/src/llama.rs: pure /proc, no lsof/fuser
// dependency, so an agent restart can reclaim the port from an orphaned
// whisper-server left over from a previous run.

#[cfg(unix)]
fn listening_inodes(port: u16) -> std::collections::HashSet<u64> {
    const TCP_LISTEN: &str = "0A";
    let mut inodes = std::collections::HashSet::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let (Some(local), Some(st), Some(inode)) = (cols.get(1), cols.get(3), cols.get(9))
            else {
                continue;
            };
            if *st != TCP_LISTEN {
                continue;
            }
            let Some((_ip, hex_port)) = local.rsplit_once(':') else {
                continue;
            };
            if u16::from_str_radix(hex_port, 16).ok() != Some(port) {
                continue;
            }
            if let Ok(i) = inode.parse::<u64>() {
                inodes.insert(i);
            }
        }
    }
    inodes
}

#[cfg(unix)]
fn proc_cmdline(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .map(|raw| {
            raw.split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("pid {pid}"))
}

#[cfg(unix)]
fn kill_stray_on_port(port: u16) {
    let inodes = listening_inodes(port);
    if inodes.is_empty() {
        return;
    }
    let self_pid = std::process::id();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in procs.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let matches_socket = target
                .to_str()
                .and_then(|s| s.strip_prefix("socket:["))
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|inode| inodes.contains(&inode));
            if matches_socket {
                warn!(
                    pid,
                    port,
                    cmdline = %proc_cmdline(pid),
                    "killing stray process holding whisper-server port"
                );
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
                break;
            }
        }
    }
}

#[cfg(not(unix))]
fn kill_stray_on_port(_port: u16) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_preroll_is_300ms_of_16khz_16bit_silence() {
        let preroll = silence_preroll();
        // 16000 samples/sec * 0.3s * 2 bytes/sample
        assert_eq!(preroll.len(), 9600);
        assert!(preroll.iter().all(|&b| b == 0));
    }

    #[test]
    fn cooldown_inactive_when_never_failed() {
        assert!(!cooldown_active(0, 1_000_000));
    }

    #[test]
    fn cooldown_active_inside_window_and_expires_after() {
        let failed = 1_000_000;
        assert!(cooldown_active(failed, failed + REMOTE_COOLDOWN_MS - 1));
        assert!(!cooldown_active(failed, failed + REMOTE_COOLDOWN_MS));
    }

    #[test]
    fn wrap_wav_header_is_44_bytes() {
        let pcm = vec![0u8; 100];
        let wav = wrap_wav(&pcm);
        assert_eq!(wav.len(), 44 + pcm.len());
    }

    #[test]
    fn wrap_wav_starts_with_riff_wave() {
        let wav = wrap_wav(&[1, 2, 3, 4]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
    }

    #[test]
    fn wrap_wav_declares_16khz_mono_16bit() {
        let wav = wrap_wav(&[0; 8]);
        let sample_rate = u32::from_le_bytes(wav[24..28].try_into().unwrap());
        let channels = u16::from_le_bytes(wav[22..24].try_into().unwrap());
        let bits_per_sample = u16::from_le_bytes(wav[34..36].try_into().unwrap());
        assert_eq!(sample_rate, 16_000);
        assert_eq!(channels, 1);
        assert_eq!(bits_per_sample, 16);
    }

    #[test]
    fn wrap_wav_data_chunk_length_matches_pcm() {
        let pcm = vec![7u8; 321];
        let wav = wrap_wav(&pcm);
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(data_len as usize, pcm.len());
        assert_eq!(&wav[44..], &pcm[..]);
    }

    #[test]
    fn wrap_wav_riff_length_is_data_plus_36() {
        let pcm = vec![9u8; 50];
        let wav = wrap_wav(&pcm);
        let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap());
        assert_eq!(riff_len as usize, 36 + pcm.len());
    }
}
