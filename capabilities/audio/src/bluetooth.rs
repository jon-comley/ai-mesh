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
//! Verified live 2026-07-10 against BlueZ 5.x on Debian trixie (pi2) with
//! a Fishman Loudbox amp: `scan()` streams an interactive session (events
//! arrive prompt-redraw-prefixed — see `after_prompt_redraw`), while
//! `pair()` uses one-shot `bluetoothctl <cmd>` invocations and their exit
//! codes, because the interactive session's outcome lines (e.g. "Pairing
//! successful") carry no device MAC and aren't reliably distinguishable
//! from unrelated event noise.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::{Duration, Instant, timeout};
use tracing::warn;

/// How long to allow each one-shot `bluetoothctl` step (`pair`, `trust`,
/// `connect`) to run. Must exceed BlueZ's own internal connect timeout —
/// measured live at ~30s (a failing `connect` against the Fishman Loudbox
/// took 30.3s to report `br-connection-unknown`). Cutting our own timeout
/// shorter than that doesn't speed anything up: killing the `bluetoothctl`
/// CLI process doesn't cancel the in-flight D-Bus call to bluetoothd, so
/// bluetoothd can still complete the connect a few seconds after we've
/// already given up and reported failure to the dashboard — the device
/// briefly shows `Connected: yes` with nobody home to run `trust`/resolve
/// the sink, then it idles out and drops again with no audio ever sent.
const PAIR_STEP_TIMEOUT: Duration = Duration::from_secs(40);

/// `power on`'s confirmation text isn't reliably documented across BlueZ
/// versions either — fire it and wait this long before issuing `scan`/
/// `pair`, same rationale as `TRUST_SETTLE`. Needed because the adapter
/// can start (or come back after a reboot) powered off or rfkill-blocked,
/// which otherwise makes `scan on`/`pair` fail silently.
const POWER_ON_SETTLE: Duration = Duration::from_millis(500);

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
    DiscoveryFailed(String),
    None,
}

/// `bluetoothctl` redraws its prompt in place even when piped, so a
/// `\n`-terminated line arrives as
/// `[bluetoothctl]> \r<blanking spaces>\r[NEW] Device …` — the real event
/// content is only the text after the last carriage return. Lines without
/// `\r` pass through unchanged. (Confirmed against live output on pi2;
/// whole-line prefix matching never fired because of this.)
fn after_prompt_redraw(stripped: &str) -> &str {
    stripped
        .rsplit('\r')
        .map(str::trim)
        .find(|seg| !seg.is_empty())
        .unwrap_or("")
}

/// BlueZ prints RSSI either as a plain decimal (`RSSI: -52`) or, on newer
/// versions, as hex with the decimal in parens (`RSSI: 0xffffffcd (-51)`).
fn parse_rssi(value: &str) -> Option<i32> {
    let value = value.trim();
    if let (Some(open), Some(close)) = (value.find('('), value.rfind(')'))
        && open < close
    {
        return value[open + 1..close].trim().parse().ok();
    }
    value.parse().ok()
}

/// Parses one line of `bluetoothctl` scan output. Recognises:
/// `[NEW] Device AA:BB:CC:DD:EE:FF Some Name` (first appearance, name
/// attached), `[CHG] Device AA:BB:CC:DD:EE:FF RSSI: …` (signal update),
/// `[CHG] Device AA:BB:CC:DD:EE:FF Name: …` (late name resolution — BLE
/// devices often appear MAC-only first), and lines indicating the adapter
/// couldn't even start discovery (powered off/rfkill-blocked) — without
/// that last one, such a failure looks identical to "no devices in range".
/// Other `[CHG]` fields (`Class:`, `Icon:`, `UUIDs:`, `ManufacturerData…`)
/// are metadata, never a device name.
fn parse_scan_line(line: &str) -> ParsedLine {
    let stripped = strip_ansi(line);
    let lower = stripped.to_lowercase();
    if lower.contains("failed to start discovery")
        || lower.contains("no default controller available")
    {
        return ParsedLine::DiscoveryFailed(stripped.trim().to_string());
    }
    let line = after_prompt_redraw(&stripped);
    if let Some(rest) = line.strip_prefix("[NEW] Device ") {
        let mut parts = rest.splitn(2, ' ');
        let mac = parts.next().unwrap_or("").to_string();
        let name = parts.next().unwrap_or("").trim();
        if is_mac(&mac) && !name.is_empty() {
            return ParsedLine::NewOrChanged {
                mac,
                name: name.to_string(),
            };
        }
    } else if let Some(rest) = line.strip_prefix("[CHG] Device ") {
        let mut parts = rest.splitn(2, ' ');
        let mac = parts.next().unwrap_or("").to_string();
        let remainder = parts.next().unwrap_or("").trim();
        if is_mac(&mac) && !remainder.is_empty() {
            if let Some(value) = remainder.strip_prefix("RSSI: ") {
                if let Some(rssi) = parse_rssi(value) {
                    return ParsedLine::Rssi { mac, rssi };
                }
            } else if let Some(name) = remainder.strip_prefix("Name: ") {
                let name = name.trim();
                if !name.is_empty() {
                    return ParsedLine::NewOrChanged {
                        mac,
                        name: name.to_string(),
                    };
                }
            }
        }
    }
    ParsedLine::None
}

/// Parses one line of one-shot `bluetoothctl devices` output:
/// `Device AA:BB:CC:DD:EE:FF Some Name`.
fn parse_device_line(line: &str) -> Option<(String, String)> {
    let stripped = strip_ansi(line);
    let rest = after_prompt_redraw(&stripped).strip_prefix("Device ")?;
    let mut parts = rest.splitn(2, ' ');
    let mac = parts.next().unwrap_or("").to_string();
    let name = parts.next().unwrap_or("").trim();
    (is_mac(&mac) && !name.is_empty()).then(|| (mac, name.to_string()))
}

/// Opens a live discovery window for `seconds`, calling `on_device` once
/// per new device and again each time BlueZ reports an updated RSSI for
/// one already seen.
///
/// Results are seeded from bluetoothd's device cache before discovery
/// starts: a currently-connected device (e.g. an amp already paired to
/// this node) stops advertising entirely and never emits a `[NEW]` line,
/// and neither does anything still cached from a scan moments earlier — so
/// without seeding, the connected device is unselectable and a rescan
/// right after a scan looks empty.
pub async fn scan(seconds: u8, mut on_device: impl FnMut(FoundDevice)) -> Result<(), String> {
    let mut known_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    let (_, cached) = run_oneshot(&["devices"], Duration::from_secs(3)).await;
    for line in cached.lines() {
        if let Some((mac, name)) = parse_device_line(line) {
            known_names.insert(mac.clone(), name.clone());
            on_device(FoundDevice {
                mac,
                name,
                rssi: None,
            });
        }
    }

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
        .write_all(b"power on\n")
        .await
        .map_err(|e| format!("bluetoothctl: failed to power on adapter: {e}"))?;
    tokio::time::sleep(POWER_ON_SETTLE).await;

    stdin
        .write_all(b"scan on\n")
        .await
        .map_err(|e| format!("bluetoothctl: failed to start scan: {e}"))?;

    let mut discovery_failed: Option<String> = None;
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
                ParsedLine::DiscoveryFailed(msg) => {
                    discovery_failed = Some(msg);
                    break;
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
    match discovery_failed {
        Some(msg) => Err(format!("bluetoothctl: failed to start discovery: {msg}")),
        None => Ok(()),
    }
}

/// Runs a single one-shot `bluetoothctl <args>` command, returning whether
/// it exited zero plus its combined stdout+stderr. One-shot invocations
/// print the real outcome lines without the interactive prompt-redraw
/// noise and exit when the underlying D-Bus call completes — far more
/// reliable than string-matching an interactive session, whose "Pairing
/// successful" line never carries the device MAC (verified live against
/// the Fishman Loudbox, 2026-07-10).
async fn run_oneshot(args: &[&str], limit: Duration) -> (bool, String) {
    let mut cmd = Command::new("bluetoothctl");
    cmd.args(args).kill_on_drop(true);
    match timeout(limit, cmd.output()).await {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), text)
        }
        Ok(Err(e)) => (false, format!("failed to run bluetoothctl: {e}")),
        Err(_) => (false, format!("bluetoothctl {} timed out", args.join(" "))),
    }
}

/// Picks the most useful error line out of one-shot `connect` output — the
/// `Failed to connect: org.bluez.Error…` line when present (it names the
/// actual reason, e.g. `br-connection-profile-unavailable`), otherwise the
/// last non-empty line.
fn connect_failure_reason(output: &str) -> String {
    let cleaned: Vec<String> = output
        .lines()
        .map(|l| {
            let stripped = strip_ansi(l);
            after_prompt_redraw(&stripped).to_string()
        })
        .filter(|l| !l.is_empty())
        .collect();
    cleaned
        .iter()
        .find(|l| l.to_lowercase().contains("failed to connect"))
        .or_else(|| cleaned.last())
        .cloned()
        .unwrap_or_else(|| "no output from bluetoothctl connect".to_string())
}

/// Extracts the value of a `bluetoothctl info` "Name: <value>" line, if
/// this is one. Factored out from `resolve_name` so the string-matching
/// logic is unit-testable without spawning a process.
fn extract_name_field(line: &str) -> Option<String> {
    let stripped = strip_ansi(line);
    after_prompt_redraw(&stripped)
        .strip_prefix("Name: ")
        .map(str::to_string)
}

/// BlueZ remembers a device's friendly name from whichever earlier session
/// discovered it — `info <mac>` reads it back from bluetoothd.
async fn resolve_name(mac: &str) -> Option<String> {
    let (_, output) = run_oneshot(&["info", mac], Duration::from_secs(3)).await;
    output.lines().find_map(extract_name_field)
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

fn connect_succeeded(ok: bool, output: &str) -> bool {
    ok && !output.to_lowercase().contains("failed to connect")
}

/// A2DP Audio Sink service class UUID. Passing it to `bluetoothctl
/// connect <mac> <uuid>` forces the connection onto classic BR/EDR — the
/// only transport A2DP exists on.
const A2DP_SINK_UUID: &str = "0000110b-0000-1000-8000-00805f9b34fb";

/// Connects `mac` via one-shot `bluetoothctl` commands, then resolves its
/// friendly name and the resulting PipeWire/Pulse sink name. `trust` is
/// applied *after* a successful connect (best-effort, for auto-reconnect
/// persistence) rather than before it.
///
/// Connect is profile-targeted (`connect <mac> <A2DP_SINK_UUID>`), never
/// bare: a dual-mode speaker (the Fishman Loudbox exposes BLE battery/
/// proximity services next to classic A2DP) leaves the stored record's
/// `PreferredBearer=last-seen` pointing at LE after any scan — its BLE
/// beacons are always the most recently seen — so a bare `connect` tries
/// an LE connection that the device doesn't accept and hangs to BlueZ's
/// ~30s timeout. Confirmed via btmon 2026-07-11: bare connect issued only
/// LE scan commands, never a BR/EDR page; the UUID form connected
/// instantly. (Pinning `PreferredBearer` needs bluetoothd's experimental
/// flag, so the UUID form is the portable fix.)
///
/// No `trust` or explicit `pair` before the first connect: BlueZ pairs
/// implicitly during `connect` ("just works" SSP), and running `trust`
/// first (tried, reverted 2026-07-10) reliably made `connect` hang.
/// Explicit `pair` runs only as a fallback after a failed connect.
pub async fn pair(mac: &str) -> Result<PairOutcome, String> {
    let _ = run_oneshot(&["power", "on"], PAIR_STEP_TIMEOUT).await;

    let (ok, output) = run_oneshot(&["connect", mac, A2DP_SINK_UUID], PAIR_STEP_TIMEOUT).await;
    if !connect_succeeded(ok, &output) {
        let first_failure = connect_failure_reason(&output);
        warn!(mac, error = %first_failure, "bluetooth: connect failed — trying explicit pair then reconnect");

        let (ok, output) = run_oneshot(&["pair", mac], PAIR_STEP_TIMEOUT).await;
        if !ok && !output.to_lowercase().contains("already exists") {
            warn!(mac, output = %output.trim(), "bluetooth: explicit pair also reported failure");
        }
        let (ok, output) = run_oneshot(&["connect", mac, A2DP_SINK_UUID], PAIR_STEP_TIMEOUT).await;
        if !connect_succeeded(ok, &output) {
            // Report the first failure's reason: the retry's error is
            // usually secondary wreckage (e.g. device wedged by pair).
            return Err(first_failure);
        }
    }

    let (ok, output) = run_oneshot(&["trust", mac], PAIR_STEP_TIMEOUT).await;
    if !ok {
        warn!(mac, output = %output.trim(), "bluetooth: trust step reported failure (device stays connected regardless)");
    }

    let name = resolve_name(mac).await.unwrap_or_else(|| mac.to_string());
    let sink_name = resolve_sink_name(mac).await;
    Ok(PairOutcome { name, sink_name })
}

/// Forgets every cached device bluetoothd knows about (`bluetoothctl
/// remove <mac>`) except whichever one is currently connected — needed
/// because `scan()` seeds its results from that same cache (see its doc
/// comment), so anything left behind (an amp that fell out of range, a
/// neighbour's phone from months ago) keeps reappearing in the dashboard's
/// list indistinguishable from something actually live right now. Returns
/// the number of devices removed.
pub async fn clear_cache() -> Result<usize, String> {
    let (ok, output) = run_oneshot(&["devices"], Duration::from_secs(5)).await;
    if !ok {
        return Err(format!("bluetoothctl devices failed: {}", output.trim()));
    }

    let mut cleared = 0usize;
    for line in output.lines() {
        let Some((mac, _)) = parse_device_line(line) else {
            continue;
        };
        let (_, info) = run_oneshot(&["info", &mac], Duration::from_secs(3)).await;
        if info.lines().any(|l| l.trim() == "Connected: yes") {
            continue; // never rip out the device someone's actively using
        }
        let (ok, output) = run_oneshot(&["remove", &mac], Duration::from_secs(5)).await;
        if ok {
            cleared += 1;
        } else {
            warn!(mac, output = %output.trim(), "bluetooth: failed to remove cached device");
        }
    }
    Ok(cleared)
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
    fn parse_scan_line_handles_prompt_redraw_prefix() {
        // Real line captured from bluetoothctl piped on pi2: the prompt is
        // redrawn in place before the event, all on one \n-terminated line.
        let line = "\u{1b}[0;94m[bluetoothctl]> \u{1b}[0m\r                    \r[\u{1b}[0;92mNEW\u{1b}[0m] Device AA:BB:CC:DD:EE:02 Kitchen tv";
        match parse_scan_line(line) {
            ParsedLine::NewOrChanged { mac, name } => {
                assert_eq!(mac, "AA:BB:CC:DD:EE:02");
                assert_eq!(name, "Kitchen tv");
            }
            _ => panic!("expected NewOrChanged"),
        }
    }

    #[test]
    fn parse_scan_line_parses_hex_rssi_with_parenthesised_decimal() {
        // Newer BlueZ prints RSSI as hex plus the decimal in parens.
        match parse_scan_line("[CHG] Device AA:BB:CC:DD:EE:02 RSSI: 0xffffffcd (-51)") {
            ParsedLine::Rssi { mac, rssi } => {
                assert_eq!(mac, "AA:BB:CC:DD:EE:02");
                assert_eq!(rssi, -51);
            }
            _ => panic!("expected Rssi"),
        }
    }

    #[test]
    fn parse_scan_line_treats_chg_name_as_name_update() {
        match parse_scan_line("[CHG] Device AA:BB:CC:DD:EE:02 Name: Kitchen tv") {
            ParsedLine::NewOrChanged { mac, name } => {
                assert_eq!(mac, "AA:BB:CC:DD:EE:02");
                assert_eq!(name, "Kitchen tv");
            }
            _ => panic!("expected NewOrChanged"),
        }
    }

    #[test]
    fn parse_scan_line_ignores_chg_metadata_fields() {
        for line in [
            "[CHG] Device AA:BB:CC:DD:EE:02 Class: 0x000c043c (787516)",
            "[CHG] Device AA:BB:CC:DD:EE:02 Icon: audio-card",
            "[CHG] Device AA:BB:CC:DD:EE:02 UUIDs: 0000110a-0000-1000-8000-00805f9b34fb",
            "[CHG] Device AA:BB:CC:DD:EE:02 ManufacturerData.Key: 0x0075 (117)",
        ] {
            assert!(
                matches!(parse_scan_line(line), ParsedLine::None),
                "should ignore: {line}"
            );
        }
    }

    #[test]
    fn parse_scan_line_detects_discovery_failure() {
        assert!(matches!(
            parse_scan_line("Failed to start discovery: org.bluez.Error.NotReady"),
            ParsedLine::DiscoveryFailed(_)
        ));
    }

    #[test]
    fn parse_scan_line_detects_missing_controller() {
        assert!(matches!(
            parse_scan_line("No default controller available"),
            ParsedLine::DiscoveryFailed(_)
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

    #[test]
    fn parse_device_line_reads_cached_device() {
        assert_eq!(
            parse_device_line("Device AA:BB:CC:DD:EE:01 Fishman Loudbox"),
            Some((
                "AA:BB:CC:DD:EE:01".to_string(),
                "Fishman Loudbox".to_string()
            ))
        );
    }

    #[test]
    fn parse_device_line_rejects_non_device_lines() {
        assert_eq!(parse_device_line("Controller AA:BB:CC:DD:EE:03 pi2"), None);
        assert_eq!(parse_device_line("Device not-a-mac Name"), None);
    }

    #[test]
    fn connect_failure_reason_picks_the_bluez_error_line() {
        // Real output captured from a one-shot connect on pi2.
        let output = "Attempting to connect to AA:BB:CC:DD:EE:01\n[CHG] Device AA:BB:CC:DD:EE:01 Connected: yes\nFailed to connect: org.bluez.Error.Failed br-connection-profile-unavailable\n";
        assert_eq!(
            connect_failure_reason(output),
            "Failed to connect: org.bluez.Error.Failed br-connection-profile-unavailable"
        );
    }

    #[test]
    fn connect_failure_reason_falls_back_to_last_line() {
        assert_eq!(
            connect_failure_reason("Attempting to connect to AA:BB:CC:DD:EE:FF\n"),
            "Attempting to connect to AA:BB:CC:DD:EE:FF"
        );
        assert_eq!(
            connect_failure_reason(""),
            "no output from bluetoothctl connect"
        );
    }
}
