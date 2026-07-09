//! Samsung soundbar local control (Phase 4 of
//! `plans/audio-output-integration.md`): the S701D is a direct LAN device,
//! not a mesh node — nothing here runs on a Pi, so this is a coordinator
//! module (reqwest straight to the soundbar's IP), the same shape as
//! `audio.rs`'s room/broadcast resolution being coordinator-side rather
//! than a new agent capability.
//!
//! Protocol: Samsung's WAM/UIC API, used across their MultiRoom/soundbar
//! line — unauthenticated `GET http://<ip>:56001/UIC?cmd=<url-encoded-XML>`,
//! response is XML back on the same connection. This is reverse-engineered
//! community knowledge (no official docs), confirmed against sibling
//! Samsung soundbar/MultiRoom models, **not verified against this exact
//! S701D unit** — treat every command here as a best-effort guess pending
//! a live test, same posture as `capability-audio`'s `paplay`/`aplay`
//! commands. `SOUNDBAR_PORT` and the command XML shapes are the part most
//! likely to need adjusting once real hardware is available.
//!
//! Power-on is deliberately not implemented: the UIC API only reaches a
//! device that's already awake and on the network (no WoL-equivalent is
//! documented for this product line) — treat the soundbar as normally-on
//! or wake it by other means (remote, HDMI-CEC via the TV), not something
//! ai-mesh can do from cold.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::warn;

use crate::http::api::prefs::PREF_USER_ID;
use crate::registry::Registry;

const SOUNDBAR_PORT: u16 = 56001;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const SOUNDBAR_IP_PREF: &str = "soundbar-ip";

/// The configured soundbar's LAN IP, if one's been set via the same
/// preferences store `room-audio-sink:*`/`tts-voice` already use — no new
/// schema. No dashboard control exists for this yet (see the assumptions
/// list); set directly via `PUT /api/preferences/soundbar-ip` until one
/// does.
pub fn configured_ip(registry: &Arc<Mutex<Registry>>) -> Option<String> {
    registry
        .lock()
        .unwrap()
        .get_preference(PREF_USER_ID, SOUNDBAR_IP_PREF)
}

fn uic_url(ip: &str, xml: &str) -> String {
    format!(
        "http://{ip}:{SOUNDBAR_PORT}/UIC?cmd={}",
        urlencoding_encode(xml)
    )
}

/// Minimal percent-encoding for the one query param this module ever
/// builds — avoids pulling in a dedicated crate for a single call site.
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn send_uic_command(ip: &str, xml: &str) -> Result<String, String> {
    let url = uic_url(ip, xml);
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("soundbar unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("soundbar returned HTTP {}", resp.status()));
    }
    resp.text()
        .await
        .map_err(|e| format!("failed to read soundbar response: {e}"))
}

/// Pull `val="..."` out of a `<name>{field}</name>` sibling `<p ... val="X"/>`
/// pair in the UIC XML reply. String-scan rather than a real XML parser —
/// the response shape is small and fixed, not worth a new dependency.
fn extract_val(xml: &str, field: &str) -> Option<String> {
    let name_tag = format!("name=\"{field}\"");
    let idx = xml.find(&name_tag)?;
    let after = &xml[idx..];
    let val_idx = after.find("val=\"")? + 5;
    let rest = &after[val_idx..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// `<name>SetVolume</name><p type="dec" name="volume" val="{vol}"/>` —
/// 0-100 per the WAM API convention (not this unit's actual usable range,
/// unverified).
pub async fn set_volume(vol: u8, registry: &Arc<Mutex<Registry>>) -> Result<String, String> {
    let Some(ip) = configured_ip(registry) else {
        return Err("no soundbar configured (set the 'soundbar-ip' preference)".into());
    };
    let xml = format!("<name>SetVolume</name><p type=\"dec\" name=\"volume\" val=\"{vol}\"/>");
    send_uic_command(&ip, &xml).await?;
    Ok(format!("soundbar volume set to {vol}"))
}

/// `<name>GetVolume</name>` — returns the current 0-100 volume level.
pub async fn get_volume(registry: &Arc<Mutex<Registry>>) -> Result<u8, String> {
    let Some(ip) = configured_ip(registry) else {
        return Err("no soundbar configured (set the 'soundbar-ip' preference)".into());
    };
    let xml = "<name>GetVolume</name>";
    let body = send_uic_command(&ip, xml).await?;
    extract_val(&body, "volume")
        .and_then(|v| v.parse::<u8>().ok())
        .ok_or_else(|| "soundbar did not report a volume".into())
}

/// `<name>SetMute</name><p type="dec" name="mute" val="on|off"/>`.
pub async fn set_mute(mute: bool, registry: &Arc<Mutex<Registry>>) -> Result<String, String> {
    let Some(ip) = configured_ip(registry) else {
        return Err("no soundbar configured (set the 'soundbar-ip' preference)".into());
    };
    let val = if mute { "on" } else { "off" };
    let xml = format!("<name>SetMute</name><p type=\"str\" name=\"mute\" val=\"{val}\"/>");
    send_uic_command(&ip, &xml).await?;
    Ok(if mute {
        "soundbar muted".into()
    } else {
        "soundbar unmuted".into()
    })
}

/// `<name>SetPowerStatus</name><p type="dec" name="powerstatus" val="0"/>` —
/// standby. No corresponding power-on: see the module doc comment.
pub async fn power_off(registry: &Arc<Mutex<Registry>>) -> Result<String, String> {
    let Some(ip) = configured_ip(registry) else {
        return Err("no soundbar configured (set the 'soundbar-ip' preference)".into());
    };
    let xml = "<name>SetPowerStatus</name><p type=\"dec\" name=\"powerstatus\" val=\"0\"/>";
    send_uic_command(&ip, xml).await?;
    warn!("soundbar power-off sent — this command is unverified against real hardware");
    Ok("soundbar powered off".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_ip_reads_the_preference() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        assert_eq!(configured_ip(&registry), None);
        registry
            .lock()
            .unwrap()
            .set_preference(PREF_USER_ID, SOUNDBAR_IP_PREF, "10.0.0.20");
        assert_eq!(configured_ip(&registry), Some("10.0.0.20".into()));
    }

    #[test]
    fn urlencoding_encode_escapes_reserved_chars() {
        let encoded = urlencoding_encode("<name>SetVolume</name>");
        assert!(!encoded.contains('<'));
        assert!(!encoded.contains('>'));
        assert!(encoded.contains("%3C"));
    }

    #[test]
    fn uic_url_embeds_ip_port_and_encoded_command() {
        let url = uic_url("10.0.0.20", "<name>GetVolume</name>");
        assert_eq!(
            url,
            "http://10.0.0.20:56001/UIC?cmd=%3Cname%3EGetVolume%3C%2Fname%3E"
        );
    }

    #[test]
    fn extract_val_reads_the_named_field() {
        let xml = r#"<UIC><response><volume><name>volume</name><p type="dec" name="volume" val="17"/></volume></response></UIC>"#;
        assert_eq!(extract_val(xml, "volume"), Some("17".into()));
    }

    #[test]
    fn extract_val_missing_field_returns_none() {
        let xml = r#"<UIC><response></response></UIC>"#;
        assert_eq!(extract_val(xml, "volume"), None);
    }

    #[tokio::test]
    async fn set_volume_without_configured_ip_errors() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let result = set_volume(20, &registry).await;
        assert!(result.unwrap_err().contains("no soundbar configured"));
    }

    #[tokio::test]
    async fn get_volume_without_configured_ip_errors() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let result = get_volume(&registry).await;
        assert!(result.unwrap_err().contains("no soundbar configured"));
    }

    #[tokio::test]
    async fn set_mute_without_configured_ip_errors() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let result = set_mute(true, &registry).await;
        assert!(result.unwrap_err().contains("no soundbar configured"));
    }

    #[tokio::test]
    async fn power_off_without_configured_ip_errors() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let result = power_off(&registry).await;
        assert!(result.unwrap_err().contains("no soundbar configured"));
    }
}
