//! Live Bluetooth discovery + pairing, driven from the dashboard instead of
//! a manual `bluetoothctl` session on the host — see
//! `plans/audio-output-integration.md` Phase 2's original manual-setup
//! note and the dashboard's "Scan for Bluetooth" button.
//!
//! Shells out to `bluetoothctl` (piped stdin/stdout, one process per scan or
//! pair) rather than binding to BlueZ's D-Bus API directly — no new Cargo
//! dependency, no cross-compile toolchain risk, consistent with how this
//! codebase already shells out to `ss`, `aplay`/`paplay`, `pactl` elsewhere.
//!
//! **Unverified without hardware in hand**: the exact success/failure
//! strings `bluetoothctl` prints for `pair`/`trust`/`connect` are BlueZ
//! version-dependent and were not confirmed against a real device before
//! this shipped. `pair()`'s step-matching is deliberately permissive
//! (case-insensitive substring match, "already exists" treated as success)
//! to tolerate minor wording differences, but a genuinely different BlueZ
//! version may need `PAIR_STEP_TIMEOUT` or the match strings adjusted once
//! tested live.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::{Duration, Instant, timeout};
use tracing::warn;

/// How long to wait for a `bluetoothctl pair`/`connect` step to report
/// success or failure before giving up — a first-time pairing handshake
/// can take a few seconds, but this shouldn't hang the request forever.
const PAIR_STEP_TIMEOUT: Duration = Duration::from_secs(15);

/// `trust` has no reliably-documented confirmation line across BlueZ
/// versions — fire it and wait this long before moving on, rather than
/// blocking on output matching that may never arrive.
const TRUST_SETTLE: Duration = Duration::from_millis(500);

/// How long to keep polling `pactl` for the newly-connected sink to show
/// up — PipeWire/Pulse registers it shortly after BlueZ reports the
/// connection, not necessarily instantly.
const SINK_POLL_TIMEOUT: Duration = Duration::from_secs(5);

pub struct FoundDevice {
    pub mac: String,
    pub name: String,
    pub rssi: Option<i32>,
}

pub struct PairOutcome {
    pub name: String,
    /// Resolved PipeWire/PulseAudio sink name, if `pactl` found one for
    /// this MAC within `SINK_POLL_TIMEOUT`. `None` doesn't necessarily mean
    /// the pair failed — playback falls back to the OS default sink.
    pub sink_name: Option<String>,
}

fn spawn_bluetoothctl() -> Result<Child, String> {
    Command::new("bluetoothctl")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to start bluetoothctl: {e}"))
}

/// Strips ANSI CSI escape sequences (`ESC [ ... letter`) — `bluetoothctl`
/// colors its `[NEW]`/`[CHG]` prefixes, which would otherwise break prefix
/// matching.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn is_mac(s: &str) -> bool {
    s.len() == 17
        && s.split(':').count() == 6
        && s.split(':')
            .all(|g| g.len() == 2 && g.chars().all(|c| c.is_ascii_hexdigit()))
}

enum ParsedLine {
    NewOrChanged { mac: String, name: String },
    Rssi { mac: String, rssi: i32 },
    None,
}

/// Parses one line of `bluetoothctl` scan output. Recognises:
/// `[NEW] Device AA:BB:CC:DD:EE:FF Some Name` and
/// `[CHG] Device AA:BB:CC:DD:EE:FF RSSI: -52`.
fn parse_scan_line(line: &str) -> ParsedLine {
    let line = strip_ansi(line);
    let line = line.trim();
    for prefix in ["[NEW] Device ", "[CHG] Device "] {
        let Some(rest) = line.strip_prefix(prefix) else {
            continue;
        };
        let mut parts = rest.splitn(2, ' ');
        let mac = parts.next().unwrap_or("").to_string();
        let remainder = parts.next().unwrap_or("").trim();
        if !is_mac(&mac) || remainder.is_empty() {
            continue;
        }
        if let Some(rssi_str) = remainder.strip_prefix("RSSI: ") {
            if let Ok(rssi) = rssi_str.trim().parse::<i32>() {
                return ParsedLine::Rssi { mac, rssi };
            }
        } else {
            return ParsedLine::NewOrChanged {
                mac,
                name: remainder.to_string(),
            };
        }
    }
    ParsedLine::None
}

/// Opens a live discovery window for `seconds`, calling `on_device` once
/// per new device and again each time BlueZ reports an updated RSSI for
/// one already seen. Devices with an RSSI update but no prior name (a
/// device already known to BlueZ from a previous session that never gets a
/// `[NEW]` line this run) are skipped rather than surfaced nameless.
pub async fn scan(seconds: u8, mut on_device: impl FnMut(FoundDevice)) -> Result<(), String> {
    let mut child = spawn_bluetoothctl()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("bluetoothctl: failed to open stdin")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("bluetoothctl: failed to open stdout")?;
    let mut lines = BufReader::new(stdout).lines();

    stdin
        .write_all(b"scan on\n")
        .await
        .map_err(|e| format!("bluetoothctl: failed to start scan: {e}"))?;

    let mut known_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(seconds as u64);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, lines.next_line()).await {
            Ok(Ok(Some(line))) => match parse_scan_line(&line) {
                ParsedLine::NewOrChanged { mac, name } => {
                    known_names.insert(mac.clone(), name.clone());
                    on_device(FoundDevice {
                        mac,
                        name,
                        rssi: None,
                    });
                }
                ParsedLine::Rssi { mac, rssi } => {
                    if let Some(name) = known_names.get(&mac) {
                        on_device(FoundDevice {
                            mac,
                            name: name.clone(),
                            rssi: Some(rssi),
                        });
                    }
                }
                ParsedLine::None => {}
            },
            Ok(Ok(None)) => break, // bluetoothctl exited early
            Ok(Err(e)) => {
                warn!(error = %e, "bluetoothctl: error reading scan output");
                break;
            }
            Err(_) => break, // window elapsed
        }
    }

    let _ = stdin.write_all(b"scan off\nexit\n").await;
    let _ = child.kill().await;
    Ok(())
}

/// Reads lines until one contains `success_needle` or `fail_needle`
/// (case-insensitive), both scoped to `mac`. "Already exists"-type
/// failures are treated as success — pairing/trusting an already-known
/// device isn't an error from the dashboard's point of view.
async fn wait_for_step(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    mac: &str,
    success_needle: &str,
    fail_needle: &str,
) -> Result<(), String> {
    let mac_lower = mac.to_lowercase();
    let deadline = Instant::now() + PAIR_STEP_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "{success_needle}: timed out waiting for a response"
            ));
        }
        let Ok(Ok(Some(line))) = timeout(remaining, lines.next_line()).await else {
            return Err(format!(
                "{success_needle}: bluetoothctl closed unexpectedly"
            ));
        };
        let clean = strip_ansi(&line);
        let lower = clean.to_lowercase();
        if !lower.contains(&mac_lower) {
            continue;
        }
        if lower.contains(&success_needle.to_lowercase()) {
            return Ok(());
        }
        if lower.contains(&fail_needle.to_lowercase()) {
            if lower.contains("already exists") || lower.contains("already connected") {
                return Ok(());
            }
            return Err(clean.trim().to_string());
        }
    }
}

/// Extracts the value of a `bluetoothctl info` "Name: <value>" line, if
/// this is one. Factored out from `resolve_name` so the string-matching
/// logic is unit-testable without spawning a process.
fn extract_name_field(line: &str) -> Option<String> {
    strip_ansi(line)
        .trim()
        .strip_prefix("Name: ")
        .map(str::to_string)
}

/// `pair()`'s own bluetoothctl session has no scan history, but BlueZ
/// itself remembers this device's friendly name from whichever earlier
/// session discovered it — `info <mac>` reads it back from bluetoothd.
async fn resolve_name(mac: &str) -> Option<String> {
    let mut child = spawn_bluetoothctl().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let mut lines = BufReader::new(stdout).lines();
    stdin
        .write_all(format!("info {mac}\n").as_bytes())
        .await
        .ok()?;

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut name = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                if let Some(found) = extract_name_field(&line) {
                    name = Some(found);
                    break;
                }
            }
            _ => break,
        }
    }
    let _ = stdin.write_all(b"exit\n").await;
    let _ = child.kill().await;
    name
}

async fn resolve_sink_name(mac: &str) -> Option<String> {
    let needle = mac.replace(':', "_");
    let deadline = Instant::now() + SINK_POLL_TIMEOUT;
    loop {
        if let Ok(output) = Command::new("pactl")
            .args(["list", "short", "sinks"])
            .output()
            .await
        {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(name) = text.lines().find_map(|line| {
                let name = line.split_whitespace().nth(1)?;
                name.contains(&needle).then(|| name.to_string())
            }) {
                return Some(name);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Pairs, trusts, and connects `mac`, then resolves its friendly name and
/// the resulting PipeWire/Pulse sink name.
pub async fn pair(mac: &str) -> Result<PairOutcome, String> {
    let mut child = spawn_bluetoothctl()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("bluetoothctl: failed to open stdin")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("bluetoothctl: failed to open stdout")?;
    let mut lines = BufReader::new(stdout).lines();

    stdin
        .write_all(format!("pair {mac}\n").as_bytes())
        .await
        .map_err(|e| format!("bluetoothctl: failed to send pair: {e}"))?;
    wait_for_step(&mut lines, mac, "Pairing successful", "Failed to pair").await?;

    stdin
        .write_all(format!("trust {mac}\n").as_bytes())
        .await
        .map_err(|e| format!("bluetoothctl: failed to send trust: {e}"))?;
    tokio::time::sleep(TRUST_SETTLE).await;

    stdin
        .write_all(format!("connect {mac}\n").as_bytes())
        .await
        .map_err(|e| format!("bluetoothctl: failed to send connect: {e}"))?;
    wait_for_step(
        &mut lines,
        mac,
        "Connection successful",
        "Failed to connect",
    )
    .await?;

    let _ = stdin.write_all(b"exit\n").await;
    let _ = child.kill().await;

    let name = resolve_name(mac).await.unwrap_or_else(|| mac.to_string());
    let sink_name = resolve_sink_name(mac).await;
    Ok(PairOutcome { name, sink_name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_color_codes() {
        assert_eq!(
            strip_ansi("\u{1b}[0;93m[NEW]\u{1b}[0m Device"),
            "[NEW] Device"
        );
    }

    #[test]
    fn strip_ansi_leaves_plain_text_untouched() {
        assert_eq!(
            strip_ansi("plain line, no escapes"),
            "plain line, no escapes"
        );
    }

    #[test]
    fn is_mac_accepts_valid_address() {
        assert!(is_mac("AA:BB:CC:DD:EE:FF"));
        assert!(is_mac("00:11:22:33:44:55"));
    }

    #[test]
    fn is_mac_rejects_malformed_input() {
        assert!(!is_mac("AA:BB:CC:DD:EE"));
        assert!(!is_mac("not-a-mac-address"));
        assert!(!is_mac("AA:BB:CC:DD:EE:GG"));
    }

    #[test]
    fn parse_scan_line_extracts_new_device() {
        match parse_scan_line("[NEW] Device AA:BB:CC:DD:EE:FF Fishman PA") {
            ParsedLine::NewOrChanged { mac, name } => {
                assert_eq!(mac, "AA:BB:CC:DD:EE:FF");
                assert_eq!(name, "Fishman PA");
            }
            _ => panic!("expected NewOrChanged"),
        }
    }

    #[test]
    fn parse_scan_line_extracts_rssi_update() {
        match parse_scan_line("[CHG] Device AA:BB:CC:DD:EE:FF RSSI: -52") {
            ParsedLine::Rssi { mac, rssi } => {
                assert_eq!(mac, "AA:BB:CC:DD:EE:FF");
                assert_eq!(rssi, -52);
            }
            _ => panic!("expected Rssi"),
        }
    }

    #[test]
    fn parse_scan_line_handles_ansi_colored_prefix() {
        match parse_scan_line("\u{1b}[0;93m[NEW]\u{1b}[0m Device AA:BB:CC:DD:EE:FF Speaker") {
            ParsedLine::NewOrChanged { mac, name } => {
                assert_eq!(mac, "AA:BB:CC:DD:EE:FF");
                assert_eq!(name, "Speaker");
            }
            _ => panic!("expected NewOrChanged"),
        }
    }

    #[test]
    fn parse_scan_line_ignores_unrelated_lines() {
        assert!(matches!(
            parse_scan_line("[bluetoothctl]# scan on"),
            ParsedLine::None
        ));
        assert!(matches!(
            parse_scan_line("Discovery started"),
            ParsedLine::None
        ));
    }

    #[test]
    fn parse_scan_line_rejects_malformed_mac_in_device_line() {
        assert!(matches!(
            parse_scan_line("[NEW] Device not-a-mac Name"),
            ParsedLine::None
        ));
    }

    #[test]
    fn extract_name_field_reads_info_output() {
        assert_eq!(
            extract_name_field("\tName: Fishman PA"),
            Some("Fishman PA".to_string())
        );
    }

    #[test]
    fn extract_name_field_ignores_unrelated_lines() {
        assert_eq!(extract_name_field("\tPaired: yes"), None);
    }
}
