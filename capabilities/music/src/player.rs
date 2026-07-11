//! librespot supervisor — the playback half of the music capability.
//!
//! Runs librespot as a child process with the `pipe` backend (raw PCM on
//! stdout — no ALSA/pulse C deps, so it cross-compiles with the agent) and
//! pumps that PCM into `pacat` towards the node's paired Bluetooth sink,
//! the same PipeWire sink TTS announcements use. Multi-room later swaps the
//! pacat stage for a snapserver FIFO — nothing outside this module changes
//! (plans/spotify-music.md Phase 6).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// Restart backoff: fast first retry, capped so a broken setup doesn't spin,
/// reset after a run long enough to prove the pipeline was healthy.
const BACKOFF_START: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const HEALTHY_RUN: Duration = Duration::from_secs(300);
/// Poll interval while prerequisites (binary, credentials) are missing.
const MISSING_PREREQ_RETRY: Duration = Duration::from_secs(60);

fn librespot_bin() -> PathBuf {
    std::env::var("SPOTIFY_LIBRESPOT_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join("librespot"))
}

fn cache_dir() -> PathBuf {
    std::env::var("SPOTIFY_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".ai-mesh")
                .join("spotify-cache")
        })
}

fn librespot_args(device_name: &str, cache: &std::path::Path) -> Vec<String> {
    vec![
        "--name".into(),
        device_name.into(),
        "--backend".into(),
        "pipe".into(),
        "--cache".into(),
        cache.display().to_string(),
        "--format".into(),
        "S16".into(),
        "--bitrate".into(),
        "160".into(),
        "--initial-volume".into(),
        "60".into(),
    ]
}

/// pacat (raw-mode paplay, same binary) args matching librespot's pipe
/// output: s16le / 44.1 kHz / stereo. No sink → PipeWire's default sink.
fn pacat_args(sink: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(s) = sink {
        args.push(format!("--device={s}"));
    }
    args.extend(
        ["--raw", "--format=s16le", "--rate=44100", "--channels=2"]
            .iter()
            .map(|a| a.to_string()),
    );
    args
}

/// Forever-loop spawned once from `Capability::start` (Once-guarded there).
pub async fn supervisor_loop(device_name: String) {
    let cache = cache_dir();
    reap_orphans(&cache);
    let mut backoff = BACKOFF_START;
    loop {
        let bin = librespot_bin();
        if !bin.exists() {
            warn!(
                "music: librespot binary not found at {} — run 'just deploy-librespot' (retrying in 60s)",
                bin.display()
            );
            tokio::time::sleep(MISSING_PREREQ_RETRY).await;
            continue;
        }
        if !cache.join("credentials.json").exists() {
            warn!(
                "music: no librespot credentials in {} — run 'just spotify-login' (retrying in 60s)",
                cache.display()
            );
            tokio::time::sleep(MISSING_PREREQ_RETRY).await;
            continue;
        }

        let started = Instant::now();
        let outcome = run_pipeline(&device_name, &cache).await;
        if started.elapsed() >= HEALTHY_RUN {
            backoff = BACKOFF_START;
        }
        match outcome {
            Ok(reason) => {
                info!("music: librespot pipeline ended ({reason}) — restarting in {backoff:?}")
            }
            Err(e) => warn!("music: librespot pipeline failed: {e} — restarting in {backoff:?}"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// One librespot→pacat run; returns when either side exits.
async fn run_pipeline(device_name: &str, cache: &std::path::Path) -> Result<String, String> {
    // Resolved fresh each spawn so re-pairing takes effect on next restart.
    let sink = capability_audio::paired_bluetooth_sink();
    match &sink {
        Some(s) => info!("music: piping librespot into Bluetooth sink '{s}'"),
        None => warn!("music: no paired Bluetooth sink — playing to the default sink"),
    }

    // librespot logs to stderr; leave it inherited so it lands in the journal.
    let mut librespot = tokio::process::Command::new(librespot_bin())
        .args(librespot_args(device_name, cache))
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawning librespot: {e}"))?;
    let mut pacat = tokio::process::Command::new("pacat")
        .args(pacat_args(sink.as_deref()))
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            let _ = librespot.start_kill();
            format!("spawning pacat: {e}")
        })?;

    let mut pcm_out = librespot.stdout.take().expect("librespot stdout was piped");
    let mut pcm_in = pacat.stdin.take().expect("pacat stdin was piped");
    let copied = tokio::io::copy(&mut pcm_out, &mut pcm_in).await;

    // Either side exiting ends the copy; take the other one down with it so
    // the loop restarts them as a pair. Exit statuses go in the log line —
    // "pacat exited 1" (bad sink) vs "librespot exited" (session/auth) is
    // the first question when diagnosing over journalctl.
    let _ = librespot.start_kill();
    let _ = pacat.start_kill();
    let librespot_status = wait_status(librespot).await;
    let pacat_status = wait_status(pacat).await;

    Ok(match copied {
        Ok(bytes) => format!(
            "pipe closed after {bytes} bytes; librespot {librespot_status}, pacat {pacat_status}"
        ),
        Err(e) => format!("pipe error: {e}; librespot {librespot_status}, pacat {pacat_status}"),
    })
}

async fn wait_status(mut child: tokio::process::Child) -> String {
    match child.wait().await {
        Ok(status) => status.to_string(),
        Err(e) => format!("unwaitable ({e})"),
    }
}

/// Agent deploys stop the service with `systemctl kill`, which orphans our
/// children — reap them before spawning replacements. Matching on OUR cache
/// dir (not just the binary or device name) means a manually-launched debug
/// librespot with its own cache is never touched.
fn reap_orphans(cache: &std::path::Path) {
    let pattern = format!("librespot.*--cache {}", cache.display());
    match std::process::Command::new("pkill")
        .arg("-f")
        .arg(&pattern)
        .status()
    {
        Ok(status) if status.success() => {
            info!("music: reaped orphaned librespot from a previous agent run")
        }
        // pkill exits 1 when nothing matched — the normal case.
        Ok(_) => {}
        Err(e) => warn!("music: pkill for orphaned librespot failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn librespot_args_use_pipe_backend_and_name() {
        let args = librespot_args(
            "AI Mesh",
            std::path::Path::new("/home/x/.ai-mesh/spotify-cache"),
        );
        let joined = args.join(" ");
        assert!(joined.contains("--name AI Mesh"), "{joined}");
        assert!(joined.contains("--backend pipe"), "{joined}");
        assert!(
            joined.contains("--cache /home/x/.ai-mesh/spotify-cache"),
            "{joined}"
        );
        assert!(joined.contains("--format S16"), "{joined}");
    }

    #[test]
    fn pacat_args_match_librespot_pcm_format() {
        let with_sink = pacat_args(Some("bluez_output.AA_BB.1"));
        assert_eq!(with_sink[0], "--device=bluez_output.AA_BB.1");
        assert!(with_sink.contains(&"--raw".to_string()));
        assert!(with_sink.contains(&"--format=s16le".to_string()));
        assert!(with_sink.contains(&"--rate=44100".to_string()));
        assert!(with_sink.contains(&"--channels=2".to_string()));

        // No sink → no --device flag, PipeWire default sink.
        assert!(!pacat_args(None).iter().any(|a| a.starts_with("--device")));
    }
}
