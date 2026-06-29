use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use capability_core::Capability;
use shared::{
    MeshMessage, ReaperCommandRequest, ReaperCommandResult, ReaperScriptRequest,
    ReaperScriptResult, ReaperStatusReport,
};
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};

/// Window after firing a launch during which we won't spawn REAPER again, so a burst
/// of "REAPER is offline" retries can't open several instances while it's still loading.
const REAPER_LAUNCH_COOLDOWN_SECS: u64 = 30;

pub struct ReaperCapability {
    node_id: String,
    coordinator_tx: Arc<Mutex<Option<Sender<MeshMessage>>>>,
    /// When we last spawned REAPER, for the cooldown above.
    last_launch: Arc<Mutex<Option<Instant>>>,
}

impl ReaperCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            coordinator_tx: Arc::new(Mutex::new(None)),
            last_launch: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns `host:port` for the REAPER web server.
    /// REAPER_HOST overrides the host (default: localhost).
    /// REAPER_PORT overrides the port (default: 8080).
    fn reaper_addr() -> String {
        let host = std::env::var("REAPER_HOST").unwrap_or_else(|_| "localhost".into());
        let port: u16 = std::env::var("REAPER_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8080);
        format!("{host}:{port}")
    }

    fn tx(&self) -> Option<Sender<MeshMessage>> {
        self.coordinator_tx.lock().unwrap().clone()
    }

    /// Spawn the REAPER application (it's closed, so the daemon/web server are down).
    /// Fire-and-forget: we don't wait for it to finish loading — the coordinator tells
    /// the user to retry shortly. A cooldown stops a burst of retries opening several
    /// instances. On WSL2 the binary is a Windows `.exe` launched via interop.
    fn launch_reaper(&self, cmd: &ReaperCommandRequest) -> ReaperCommandResult {
        let result = |ok: bool, message: String| ReaperCommandResult {
            request_id: cmd.request_id.clone(),
            ok,
            message,
        };
        {
            let mut last = self.last_launch.lock().unwrap();
            if let Some(t) = *last
                && t.elapsed() < Duration::from_secs(REAPER_LAUNCH_COOLDOWN_SECS)
            {
                return result(true, "REAPER is already starting".into());
            }
            *last = Some(Instant::now());
        }
        let exe = reaper_exe_path();
        match std::process::Command::new(&exe).spawn() {
            Ok(_child) => {
                info!("reaper: launched REAPER ({exe})");
                result(true, format!("launched REAPER ({exe})"))
            }
            Err(e) => {
                // Clear the cooldown so a corrected REAPER_EXE can be retried at once.
                *self.last_launch.lock().unwrap() = None;
                warn!("reaper: failed to launch REAPER at '{exe}': {e}");
                result(
                    false,
                    format!(
                        "failed to launch REAPER at '{exe}': {e} — set REAPER_EXE if it's installed elsewhere"
                    ),
                )
            }
        }
    }
}

/// Path to the REAPER executable. `REAPER_EXE` overrides it; otherwise default to the
/// standard install — a Windows `.exe` under `/mnt/c` (WSL2 interop runs it directly)
/// or the macOS app bundle binary.
fn reaper_exe_path() -> String {
    std::env::var("REAPER_EXE").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "/Applications/REAPER.app/Contents/MacOS/REAPER".into()
        } else {
            "/mnt/c/Program Files/REAPER (x64)/reaper.exe".into()
        }
    })
}

#[async_trait]
impl Capability for ReaperCapability {
    fn name(&self) -> &'static str {
        "reaper"
    }

    fn handles(&self, msg: &MeshMessage) -> bool {
        matches!(
            msg,
            MeshMessage::ReaperCommand(_) | MeshMessage::ReaperScript(_)
        )
    }

    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String> {
        *self.coordinator_tx.lock().unwrap() = Some(tx.clone());

        let node_id = self.node_id.clone();
        let addr = Self::reaper_addr();
        let url = format!("http://{}/_/TRANSPORT", addr);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| format!("reaper: failed to build HTTP client: {e}"))?;

        // Bind the poller to THIS connection's sender (moved in), not the shared
        // swappable `coordinator_tx`. start() is re-run on every coordinator
        // reconnect; if the poller read the shared slot it would find the freshly
        // swapped-in live sender, never hit the break, and survive — accumulating
        // one extra ReaperStatus poller per reconnect. Bound to its own `tx`, the
        // poller's send fails when this connection's receiver is dropped, so the
        // old poller exits and the new start() owns the only live one.
        tokio::spawn(async move {
            let mut first_success = true;
            loop {
                let report = match client.get(&url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        match parse_transport_response(resp, first_success).await {
                            Ok(fields) => {
                                first_success = false;
                                ReaperStatusReport {
                                    node_id: node_id.clone(),
                                    reaper_online: true,
                                    play_state: fields.0,
                                    position: fields.1,
                                    tempo: fields.2,
                                    ts_num: fields.3,
                                    ts_denom: fields.4,
                                }
                            }
                            Err(e) => {
                                debug!("reaper: failed to parse transport response: {e}");
                                offline_report(&node_id)
                            }
                        }
                    }
                    _ => {
                        debug!("reaper: REAPER web server unreachable at {url}");
                        offline_report(&node_id)
                    }
                };

                if tx.send(MeshMessage::ReaperStatus(report)).await.is_err() {
                    debug!("reaper: coordinator channel closed, stopping poller");
                    break;
                }

                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        Ok(())
    }

    async fn handle(&self, msg: MeshMessage, _tx: Sender<MeshMessage>) {
        let addr = Self::reaper_addr();
        match msg {
            MeshMessage::ReaperCommand(cmd) => {
                // "launch" is special: REAPER is closed, so there's no web server to
                // hit — spawn the app instead of issuing an HTTP action.
                let result = if cmd.action == "launch" {
                    self.launch_reaper(&cmd)
                } else {
                    execute_command(&cmd, &addr).await
                };
                if let Some(tx) = self.tx()
                    && tx
                        .send(MeshMessage::ReaperCommandResult(result))
                        .await
                        .is_err()
                {
                    warn!("reaper: coordinator channel closed while sending command result");
                }
            }
            MeshMessage::ReaperScript(cmd) => {
                let result = execute_script(&cmd, &addr).await;
                if let Some(tx) = self.tx()
                    && tx
                        .send(MeshMessage::ReaperScriptResult(result))
                        .await
                        .is_err()
                {
                    warn!("reaper: coordinator channel closed while sending script result");
                }
            }
            _ => {}
        }
    }
}

// ── Transport response parsing ────────────────────────────────────────────────

/// Returns (play_state, position, tempo, ts_num, ts_denom).
async fn parse_transport_response(
    resp: reqwest::Response,
    log_raw: bool,
) -> Result<(u8, f64, f64, u32, u32), String> {
    let body = resp
        .text()
        .await
        .map_err(|e| format!("failed to read body: {e}"))?;

    if log_raw {
        debug!("reaper: raw /_/TRANSPORT response: {:?}", body);
    }

    // Try JSON first (newer REAPER web server builds).
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
        let play_state = v["play"].as_u64().unwrap_or(0) as u8;
        let position = v["position"].as_f64().unwrap_or(0.0);
        let tempo = v["tempo"].as_f64().unwrap_or(120.0);
        let ts_num = v["ts_num"].as_u64().unwrap_or(4) as u32;
        let ts_denom = v["ts_denom"].as_u64().unwrap_or(4) as u32;
        return Ok((play_state, position, tempo, ts_num, ts_denom));
    }

    let parts: Vec<&str> = body.trim().split('\t').collect();

    // Format A (no header, 8 fields):
    // play_state \t play_rate \t repeat \t position \t loop_mode \t tempo \t ts_num \t ts_denom
    if parts.len() >= 8 && parts[0].parse::<u8>().is_ok() {
        let play_state = parts[0].parse::<u8>().unwrap_or(0);
        let position = parts[3].parse::<f64>().unwrap_or(0.0);
        let tempo = parts[5].parse::<f64>().unwrap_or(120.0);
        let ts_num = parts[6].parse::<u32>().unwrap_or(4);
        let ts_denom = parts[7].parse::<u32>().unwrap_or(4);
        return Ok((play_state, position, tempo, ts_num, ts_denom));
    }

    // Format B (TRANSPORT header, 6 fields):
    // TRANSPORT \t play_state \t position_secs \t repeat \t pos_str \t pos_str2
    if parts.first() == Some(&"TRANSPORT") && parts.len() >= 3 {
        let play_state = parts[1].parse::<u8>().unwrap_or(0);
        let position = parts[2].parse::<f64>().unwrap_or(0.0);
        return Ok((play_state, position, 120.0, 4, 4));
    }

    Err(format!(
        "unrecognised REAPER transport format (len={}, first={:?})",
        parts.len(),
        parts.first()
    ))
}

fn offline_report(node_id: &str) -> ReaperStatusReport {
    ReaperStatusReport {
        node_id: node_id.to_string(),
        reaper_online: false,
        play_state: 0,
        position: 0.0,
        tempo: 0.0,
        ts_num: 0,
        ts_denom: 0,
    }
}

// ── Command execution ─────────────────────────────────────────────────────────

/// Named transport actions → REAPER command IDs.
fn named_action_id(action: &str) -> Option<u32> {
    match action {
        "play" => Some(1008),
        "stop" => Some(1007),
        "pause" => Some(1016),
        "record" => Some(1013),
        "rewind" => Some(40042),
        "new_project" => Some(40023),
        "save" => Some(40022),
        _ => None,
    }
}

async fn execute_command(cmd: &ReaperCommandRequest, addr: &str) -> ReaperCommandResult {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ReaperCommandResult {
                request_id: cmd.request_id.clone(),
                ok: false,
                message: format!("failed to build HTTP client: {e}"),
            };
        }
    };

    // Resolve the action to a URL path segment.
    let action_path = if let Some(id) = named_action_id(&cmd.action) {
        // Named transport action (play/stop/etc.)
        id.to_string()
    } else if cmd.action.chars().all(|c| c.is_ascii_digit()) {
        // Numeric action ID passed as a string.
        cmd.action.clone()
    } else {
        // Named string action (e.g. SWS "_SWS_ABOUT") — url-encode it.
        url_encode(&cmd.action)
    };

    let url = format!("http://{}/_/{};", addr, action_path);
    debug!("reaper: sending command GET {}", url);

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 204 => {
            ReaperCommandResult {
                request_id: cmd.request_id.clone(),
                ok: true,
                message: "ok".into(),
            }
        }
        Ok(resp) => ReaperCommandResult {
            request_id: cmd.request_id.clone(),
            ok: false,
            message: format!("REAPER returned HTTP {}", resp.status()),
        },
        Err(e) => ReaperCommandResult {
            request_id: cmd.request_id.clone(),
            ok: false,
            message: format!("HTTP request failed: {e}"),
        },
    }
}

async fn execute_script(cmd: &ReaperScriptRequest, _addr: &str) -> ReaperScriptResult {
    let scripts_dir =
        std::env::var("REAPER_WSL_SCRIPTS_PATH").unwrap_or_else(|_| default_scripts_dir());
    let timeout = std::env::var("REAPER_SCRIPT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(5));
    run_script(cmd, std::path::Path::new(&scripts_dir), timeout).await
}

/// Default REAPER `Scripts` directory when `REAPER_WSL_SCRIPTS_PATH` is unset.
/// macOS runs the agent natively against a local REAPER (`~/Library/Application
/// Support/REAPER/Scripts`); WSL2/Linux reaches the Windows REAPER install over
/// `/mnt/c`. A *native* Linux REAPER host (`~/.config/REAPER/Scripts`) is
/// indistinguishable from WSL2 at compile time — no such node exists today, so it
/// must set `REAPER_WSL_SCRIPTS_PATH` explicitly rather than us guessing here.
fn default_scripts_dir() -> String {
    if cfg!(target_os = "macos") {
        match std::env::var("HOME") {
            Ok(home) => format!("{home}/Library/Application Support/REAPER/Scripts"),
            Err(_) => "/Library/Application Support/REAPER/Scripts".into(),
        }
    } else {
        // WSL2 reaching the Windows REAPER install: the user profile lives at
        // /mnt/c/Users/<user>. By our node-install convention the Windows and Linux
        // usernames match (the agent runs as that user), so derive it from $USER
        // rather than baking in a literal. If they ever differ, set
        // REAPER_WSL_SCRIPTS_PATH explicitly.
        let user = std::env::var("USER").unwrap_or_else(|_| "Public".into());
        format!("/mnt/c/Users/{user}/AppData/Roaming/REAPER/Scripts")
    }
}

/// Drive the daemon bridge: write the Lua + a fresh id, then poll the result file
/// the daemon writes back (`<id>\t<ok|err>\t<message>`). Returns the daemon's result,
/// the Lua error it reported, or a "daemon did not respond" error on timeout — never a
/// blind ok. Split from `execute_script` (which resolves env) so it is unit-testable.
async fn run_script(
    cmd: &ReaperScriptRequest,
    scripts_dir: &std::path::Path,
    timeout: Duration,
) -> ReaperScriptResult {
    let cmd_path = scripts_dir.join("ai_mesh_cmd.lua");
    let id_path = scripts_dir.join("ai_mesh_id.txt");
    let result_path = scripts_dir.join("ai_mesh_result.txt");

    let err = |message: String| ReaperScriptResult {
        request_id: cmd.request_id.clone(),
        ok: false,
        message,
    };

    // Write the payload first, then the id — the id change is the trigger, so the
    // command file is guaranteed present when the daemon notices it.
    if let Err(e) = tokio::fs::write(&cmd_path, cmd.code.as_bytes()).await {
        return err(format!(
            "failed to write command to {}: {e}",
            cmd_path.display()
        ));
    }
    if let Err(e) = tokio::fs::write(&id_path, cmd.request_id.as_bytes()).await {
        return err(format!("failed to write id to {}: {e}", id_path.display()));
    }

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        // A complete result ends with an ASCII record-separator sentinel (U+001E) the
        // daemon writes last. Until it's present the file is stale/seeded or mid-write
        // (the daemon truncates then fills it in place) — keep polling rather than parse
        // a half-written result. This sentinel replaces the daemon's old temp-file +
        // os.rename, which silently fails on Windows (rename can't overwrite an existing
        // file) and stranded every result in the `.tmp`, making every call hit the timeout.
        //
        // A result is only ours when its leading id matches. Parse the whole body, not
        // just the first line: query tools and Lua errors return multi-line messages. The
        // header is `<id>\t<ok|err>` and everything after the second tab is the message.
        if let Ok(contents) = tokio::fs::read_to_string(&result_path).await
            && let Some(body) = contents.strip_suffix('\u{1e}')
        {
            let mut parts = body.splitn(3, '\t');
            let rid = parts.next().unwrap_or("");
            let status = parts.next().unwrap_or("");
            let message = parts.next().unwrap_or("").trim_end();
            if rid == cmd.request_id && (status == "ok" || status == "err") {
                return ReaperScriptResult {
                    request_id: cmd.request_id.clone(),
                    ok: status == "ok",
                    message: if status == "ok" {
                        // Structured tools return a summary string; relay it. A bare
                        // script returns nothing → empty message → caller shows "ok".
                        message.to_string()
                    } else {
                        format!("REAPER Lua error: {message}")
                    },
                };
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return err(format!(
                "REAPER daemon did not respond within {}s — is REAPER running with the \
                 __startup.lua daemon? Run 'just setup-reaper-daemon' and restart REAPER.",
                timeout.as_secs_f32()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn url_encode(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_maps_to_1013_not_play_stop() {
        // Regression: 1009 is "Transport: Play/stop" and silently plays instead of
        // recording. The record action is 1013.
        assert_eq!(named_action_id("record"), Some(1013));
    }

    #[test]
    fn rewind_maps_to_go_to_start() {
        // Regression: 40113 is not "go to start"; 40042 is.
        assert_eq!(named_action_id("rewind"), Some(40042));
    }

    #[test]
    fn known_transport_actions_resolve() {
        assert_eq!(named_action_id("play"), Some(1008));
        assert_eq!(named_action_id("stop"), Some(1007));
        assert_eq!(named_action_id("pause"), Some(1016));
        assert_eq!(named_action_id("new_project"), Some(40023));
        assert_eq!(named_action_id("save"), Some(40022));
    }

    #[test]
    fn unknown_action_is_none() {
        assert_eq!(named_action_id("frobnicate"), None);
    }

    #[test]
    fn launch_is_not_a_transport_action() {
        // "launch" must fall through named_action_id so it isn't url-encoded into an
        // HTTP action — handle() intercepts it and spawns the app instead.
        assert_eq!(named_action_id("launch"), None);
    }

    #[test]
    fn reaper_exe_path_defaults_per_platform_or_env() {
        // SAFETY: single-threaded test; we set then remove the override.
        unsafe { std::env::set_var("REAPER_EXE", "/custom/reaper") };
        assert_eq!(reaper_exe_path(), "/custom/reaper");
        unsafe { std::env::remove_var("REAPER_EXE") };
        let def = reaper_exe_path();
        if cfg!(target_os = "macos") {
            assert!(def.contains("REAPER.app"), "got: {def}");
        } else {
            assert!(def.ends_with("reaper.exe"), "got: {def}");
        }
    }

    #[test]
    fn default_scripts_dir_targets_reaper_scripts_folder() {
        let dir = default_scripts_dir();
        assert!(dir.ends_with("REAPER/Scripts"), "got: {dir}");
        if cfg!(target_os = "macos") {
            assert!(dir.contains("Library/Application Support"), "got: {dir}");
        } else {
            // WSL2 → the Windows user profile, with the username derived from the
            // environment rather than a hard-coded literal.
            assert!(dir.starts_with("/mnt/c/Users/"), "got: {dir}");
            if let Ok(user) = std::env::var("USER") {
                assert!(
                    dir.contains(&format!("/{user}/")),
                    "should embed $USER, got: {dir}"
                );
            }
        }
    }

    #[test]
    fn url_encode_passes_unreserved_and_escapes_rest() {
        assert_eq!(url_encode("_SWS_ABOUT"), "_SWS_ABOUT");
        assert_eq!(url_encode("40075"), "40075");
        assert_eq!(url_encode("a b"), "a%20b");
    }

    // A stand-in for the in-REAPER daemon: wait for the agent's id file to carry
    // `expect_id`, then write the result file the agent polls for.
    async fn fake_daemon(dir: std::path::PathBuf, expect_id: &str, status: &str, message: &str) {
        let id_path = dir.join("ai_mesh_id.txt");
        let result_path = dir.join("ai_mesh_result.txt");
        // Terminate with the same RS sentinel the real daemon writes, so the agent
        // accepts the result as complete.
        let line = format!("{expect_id}\t{status}\t{message}\u{1e}");
        loop {
            if let Ok(s) = tokio::fs::read_to_string(&id_path).await
                && s.lines().next() == Some(expect_id)
            {
                tokio::fs::write(&result_path, line.as_bytes())
                    .await
                    .unwrap();
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn temp_scripts_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ai_mesh_test_{}_{tag}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        dir
    }

    #[tokio::test]
    async fn run_script_returns_ok_and_writes_command_when_daemon_acks() {
        let dir = temp_scripts_dir("ok").await;
        let daemon = tokio::spawn(fake_daemon(dir.clone(), "req-42", "ok", ""));

        let req = ReaperScriptRequest {
            request_id: "req-42".into(),
            code: "reaper.InsertTrackAtIndex(0, true)".into(),
        };
        let result = run_script(&req, &dir, Duration::from_secs(2)).await;
        daemon.await.unwrap();

        assert!(result.ok, "expected ok, got: {}", result.message);
        let cmd = tokio::fs::read_to_string(dir.join("ai_mesh_cmd.lua"))
            .await
            .unwrap();
        assert_eq!(cmd, "reaper.InsertTrackAtIndex(0, true)");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_script_relays_daemon_success_summary() {
        // Structured tools `return` a summary string; the daemon forwards it as the
        // result message and the agent must surface it verbatim (not a blind "ok").
        let dir = temp_scripts_dir("summary").await;
        let daemon = tokio::spawn(fake_daemon(
            dir.clone(),
            "req-7",
            "ok",
            "Added 'Vocals 2' as track 5 (armed)",
        ));

        let req = ReaperScriptRequest {
            request_id: "req-7".into(),
            code: "return \"x\"".into(),
        };
        let result = run_script(&req, &dir, Duration::from_secs(2)).await;
        daemon.await.unwrap();

        assert!(result.ok);
        assert_eq!(result.message, "Added 'Vocals 2' as track 5 (armed)");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_script_relays_multiline_result() {
        // Query tools (e.g. project info) return a multi-line summary; the agent must
        // surface the whole thing, not just the first line.
        let dir = temp_scripts_dir("multiline").await;
        let body =
            "Project: Song.rpp\nTempo: 120 BPM, 4/4\nTracks: 2\n  1. Vocals [armed]\n  2. Guitar";
        let daemon = tokio::spawn(fake_daemon(dir.clone(), "req-ml", "ok", body));

        let req = ReaperScriptRequest {
            request_id: "req-ml".into(),
            code: "return summary".into(),
        };
        let result = run_script(&req, &dir, Duration::from_secs(2)).await;
        daemon.await.unwrap();

        assert!(result.ok);
        assert_eq!(result.message, body);
        assert!(result.message.contains("2. Guitar"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_script_surfaces_lua_error_from_daemon() {
        let dir = temp_scripts_dir("err").await;
        let daemon = tokio::spawn(fake_daemon(
            dir.clone(),
            "req-7",
            "err",
            "attempt to index a nil value",
        ));

        let req = ReaperScriptRequest {
            request_id: "req-7".into(),
            code: "boom".into(),
        };
        let result = run_script(&req, &dir, Duration::from_secs(2)).await;
        daemon.await.unwrap();

        assert!(!result.ok);
        assert!(
            result.message.contains("attempt to index a nil value"),
            "got: {}",
            result.message
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_script_times_out_when_daemon_silent() {
        let dir = temp_scripts_dir("timeout").await;
        // No daemon writes a result — simulate a dead/missing __startup.lua.
        let req = ReaperScriptRequest {
            request_id: "req-x".into(),
            code: "reaper.InsertTrackAtIndex(0, true)".into(),
        };
        let result = run_script(&req, &dir, Duration::from_millis(250)).await;

        assert!(!result.ok);
        assert!(
            result.message.contains("did not respond"),
            "got: {}",
            result.message
        );
        // A stale result with a different id must not be mistaken for ours.
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_script_ignores_result_without_completion_sentinel() {
        // A result file matching our id but missing the RS sentinel is a mid-write
        // (or a result from the pre-sentinel daemon): it must NOT be accepted, so the
        // agent keeps polling and ultimately times out rather than relaying a partial.
        let dir = temp_scripts_dir("nosentinel").await;
        tokio::fs::write(dir.join("ai_mesh_id.txt"), b"req-ns")
            .await
            .unwrap();
        tokio::fs::write(dir.join("ai_mesh_result.txt"), b"req-ns\tok\tpartial")
            .await
            .unwrap();
        let req = ReaperScriptRequest {
            request_id: "req-ns".into(),
            code: "return \"x\"".into(),
        };
        let result = run_script(&req, &dir, Duration::from_millis(250)).await;

        assert!(!result.ok);
        assert!(
            result.message.contains("did not respond"),
            "got: {}",
            result.message
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
