//! librespot + snapclient supervisor — the playback half of the music
//! capability.
//!
//! Multi-room transport (plans/spotify-music.md Phase 6): librespot (pipe
//! backend — raw PCM, no ALSA/pulse C deps, cross-compiles with the agent)
//! writes into a FIFO read by snapserver (a system service the installer
//! sets up); a supervised snapclient plays snapserver's synced stream into
//! this node's paired Bluetooth sink, the same PipeWire sink TTS uses.
//! Another room later = another snapclient pointed at this snapserver —
//! nothing here changes.
//!
//! The two children are supervised independently: librespot dying (session
//! drop, auth) doesn't interrupt the snapclient's stream, and a snapclient
//! restart (sink re-pair) doesn't tear down the Spotify session.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// Restart backoff per child: fast first retry, capped so a broken setup
/// doesn't spin, reset after a run long enough to prove health.
const BACKOFF_START: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const HEALTHY_RUN: Duration = Duration::from_secs(300);
/// Poll interval while prerequisites (binary, credentials) are missing.
const MISSING_PREREQ_RETRY: Duration = Duration::from_secs(60);

/// Stable snapcast client id for THIS node's agent-supervised snapclient —
/// also the pkill marker that keeps orphan-reaping away from any manually
/// launched debug snapclient.
const SNAPCLIENT_HOST_ID: &str = "ai-mesh";

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

/// The FIFO librespot writes PCM into and snapserver reads from. The
/// installer bakes the same path into /etc/ai-mesh-snapserver.conf — keep
/// them in lockstep.
fn fifo_path() -> PathBuf {
    std::env::var("SPOTIFY_FIFO")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".ai-mesh")
                .join("spotify-fifo")
        })
}

fn librespot_args(device_name: &str, cache: &Path, fifo: &Path) -> Vec<String> {
    vec![
        "--name".into(),
        device_name.into(),
        "--backend".into(),
        "pipe".into(),
        "--device".into(),
        fifo.display().to_string(),
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

/// snapclient plays snapserver's stream via PipeWire's pulse interface;
/// the target sink rides in as PULSE_SINK (standard libpulse env), set by
/// the caller when a Bluetooth sink is paired.
fn snapclient_args() -> Vec<String> {
    vec![
        "--host".into(),
        "127.0.0.1".into(),
        "--player".into(),
        "pulse".into(),
        "--hostID".into(),
        SNAPCLIENT_HOST_ID.into(),
    ]
}

/// snapclient invocation with the sink riding in as PULSE_SINK (standard
/// libpulse env) — the entire sink-targeting mechanism, so built here where
/// a test can inspect it.
fn snapclient_command(sink: Option<&str>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("snapclient");
    cmd.args(snapclient_args()).kill_on_drop(true);
    if let Some(s) = sink {
        cmd.env("PULSE_SINK", s);
    }
    cmd
}

/// Entry point, spawned once from `Capability::start` (Once-guarded there).
pub async fn supervisor_loop(device_name: String) {
    let cache = cache_dir();
    let fifo = fifo_path();
    reap_orphans(&cache);
    tokio::spawn(snapclient_loop());
    librespot_loop(device_name, cache, fifo).await;
}

async fn librespot_loop(device_name: String, cache: PathBuf, fifo: PathBuf) {
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
        if let Err(e) = ensure_fifo(&fifo) {
            warn!("music: cannot create PCM fifo: {e} (retrying in 60s)");
            tokio::time::sleep(MISSING_PREREQ_RETRY).await;
            continue;
        }

        let started = Instant::now();
        // librespot logs to stderr; inherited so it lands in the journal.
        // Note: opening the FIFO for write blocks until snapserver (the
        // reader) is up — librespot idling at open is the desired wait.
        let outcome = match tokio::process::Command::new(&bin)
            .args(librespot_args(&device_name, &cache, &fifo))
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => {
                info!("music: librespot up, writing PCM to {}", fifo.display());
                wait_status(child).await
            }
            Err(e) => format!("failed to spawn ({e})"),
        };
        if started.elapsed() >= HEALTHY_RUN {
            backoff = BACKOFF_START;
        }
        info!("music: librespot exited ({outcome}) — restarting in {backoff:?}");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn snapclient_loop() {
    let mut backoff = BACKOFF_START;
    loop {
        // Resolved fresh each spawn so re-pairing takes effect on restart.
        let sink = capability_audio::paired_bluetooth_sink();
        match &sink {
            Some(s) => info!("music: snapclient playing into Bluetooth sink '{s}'"),
            None => warn!("music: no paired Bluetooth sink — snapclient uses the default sink"),
        }
        let mut cmd = snapclient_command(sink.as_deref());

        let started = Instant::now();
        let outcome = match cmd.spawn() {
            Ok(child) => wait_status(child).await,
            Err(e) => format!("failed to spawn ({e}) — is the snapclient package installed?"),
        };
        if started.elapsed() >= HEALTHY_RUN {
            backoff = BACKOFF_START;
        }
        info!("music: snapclient exited ({outcome}) — restarting in {backoff:?}");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Create the PCM FIFO if missing; replace a stray regular file (e.g. from
/// a librespot run before snapserver was configured) with a real FIFO.
fn ensure_fifo(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(path) {
        use std::os::unix::fs::FileTypeExt;
        if meta.file_type().is_fifo() {
            return Ok(());
        }
        std::fs::remove_file(path)
            .map_err(|e| format!("removing non-fifo {}: {e}", path.display()))?;
        warn!(
            "music: replaced non-fifo {} with a fifo (stray file from a previous run)",
            path.display()
        );
    }
    let status = std::process::Command::new("mkfifo")
        .arg("-m")
        .arg("0644")
        .arg(path)
        .status()
        .map_err(|e| format!("running mkfifo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("mkfifo {} exited {status}", path.display()))
    }
}

async fn wait_status(mut child: tokio::process::Child) -> String {
    match child.wait().await {
        Ok(status) => status.to_string(),
        Err(e) => format!("unwaitable ({e})"),
    }
}

/// Agent deploys stop the service with `systemctl kill`, which orphans our
/// children — reap them before spawning replacements. librespot is matched
/// on OUR cache dir and snapclient on OUR hostID, so manually-launched
/// debug instances (different cache / no hostID) are never touched.
fn reap_orphans(cache: &Path) {
    for (what, pattern) in [
        (
            "librespot",
            format!("librespot.*--cache {}", cache.display()),
        ),
        (
            "snapclient",
            format!("snapclient.*--hostID {SNAPCLIENT_HOST_ID}"),
        ),
    ] {
        match std::process::Command::new("pkill")
            .arg("-f")
            .arg(&pattern)
            .status()
        {
            Ok(status) if status.success() => {
                info!("music: reaped orphaned {what} from a previous agent run")
            }
            // pkill exits 1 when nothing matched — the normal case.
            Ok(_) => {}
            Err(e) => warn!("music: pkill for orphaned {what} failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn librespot_args_write_pcm_to_the_fifo() {
        let args = librespot_args(
            "AI Mesh",
            Path::new("/home/x/.ai-mesh/spotify-cache"),
            Path::new("/home/x/.ai-mesh/spotify-fifo"),
        );
        let joined = args.join(" ");
        assert!(joined.contains("--name AI Mesh"), "{joined}");
        assert!(joined.contains("--backend pipe"), "{joined}");
        assert!(
            joined.contains("--device /home/x/.ai-mesh/spotify-fifo"),
            "{joined}"
        );
        assert!(
            joined.contains("--cache /home/x/.ai-mesh/spotify-cache"),
            "{joined}"
        );
        assert!(joined.contains("--format S16"), "{joined}");
    }

    #[test]
    fn snapclient_args_use_local_server_and_stable_host_id() {
        let joined = snapclient_args().join(" ");
        assert!(joined.contains("--host 127.0.0.1"), "{joined}");
        assert!(joined.contains("--player pulse"), "{joined}");
        // The hostID doubles as the orphan-reap marker — keep them in sync.
        assert!(joined.contains("--hostID ai-mesh"), "{joined}");
    }

    #[test]
    fn snapclient_command_sets_pulse_sink_only_when_paired() {
        let cmd = snapclient_command(Some("bluez_output.AA_BB.1"));
        let sink_env = cmd
            .as_std()
            .get_envs()
            .find(|(k, _)| *k == "PULSE_SINK")
            .and_then(|(_, v)| v)
            .and_then(|v| v.to_str());
        assert_eq!(sink_env, Some("bluez_output.AA_BB.1"));

        // No paired sink → PULSE_SINK absent, PipeWire default sink.
        let cmd = snapclient_command(None);
        assert!(cmd.as_std().get_envs().all(|(k, _)| k != "PULSE_SINK"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_fifo_creates_and_replaces() {
        let dir = std::env::temp_dir().join(format!("music-fifo-test-{}", std::process::id()));
        let path = dir.join("fifo");
        // Fresh create
        ensure_fifo(&path).unwrap();
        use std::os::unix::fs::FileTypeExt;
        assert!(std::fs::metadata(&path).unwrap().file_type().is_fifo());
        // Idempotent
        ensure_fifo(&path).unwrap();
        // Replaces a stray regular file
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"stray").unwrap();
        ensure_fifo(&path).unwrap();
        assert!(std::fs::metadata(&path).unwrap().file_type().is_fifo());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
