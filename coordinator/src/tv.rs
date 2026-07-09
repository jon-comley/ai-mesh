//! Samsung Tizen TV local control (Phase 5 of
//! `plans/audio-output-integration.md`). Like the soundbar (`soundbar.rs`),
//! the TV is a direct LAN device — no mesh node involved, so this is a
//! coordinator module, not an agent capability.
//!
//! Protocol: Tizen's local remote-control websocket API, confirmed from
//! community reverse-engineering (the shape used by tools like
//! `samsungtvws`), not Samsung's own documentation:
//! `wss://<ip>:8002/api/v2/channels/samsung.remote.control?name=<base64 app
//! name>&token=<token>`. The endpoint serves a self-signed certificate —
//! this module accepts it unconditionally via a no-op `ServerCertVerifier`
//! (there's no other way to reach a LAN-local Tizen TV over TLS, and
//! nothing sensitive rides on a key-press command).
//!
//! **First connection is a pairing handshake**: connecting without a
//! token pops an on-screen "Allow ai-mesh to control this TV?" prompt on
//! the TV itself; once approved, the TV's own connect-ack message carries
//! a token this module persists (`tv-token` preference) and reuses on
//! every later connection. Until approved on the physical remote, every
//! command here will time out waiting on a prompt nobody's answered —
//! that's a live-test-only failure mode, not something to code around
//! blind.
//!
//! **Audio-output routing (soundbar <-> Bluetooth speaker) is NOT
//! available through this API.** Confirmed during Phase 5 research:
//! Samsung only exposes that switch through the SmartThings cloud API,
//! never the local remote-control websocket. `tv_audio_output` is
//! implemented as an explicit "not supported locally" error rather than
//! a silent no-op, so asking for it produces an honest answer instead of
//! nothing happening — this is the concrete reason Phase 5's original
//! "switch TV audio to the Bluetooth speaker" goal is not achievable
//! end-to-end (see the assumptions list).
//!
//! Wake-on-LAN requires the TV's Ethernet connection (Tizen does not
//! support WoL over Wi-Fi) and a MAC address the user must supply — no
//! way to discover it automatically without the TV already being awake
//! and queryable, so `tv-mac` is a manually-set preference, not
//! auto-detected.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::UdpSocket;
use tokio_tungstenite::Connector;
use tokio_tungstenite::tungstenite::Message;

use crate::http::api::prefs::PREF_USER_ID;
use crate::registry::Registry;

const TV_PORT: u16 = 8002;
const WOL_PORT: u16 = 9;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const APP_NAME: &str = "ai-mesh";

const TV_IP_PREF: &str = "tv-ip";
const TV_TOKEN_PREF: &str = "tv-token";
const TV_MAC_PREF: &str = "tv-mac";

/// The Tizen remote-control key names this module knows how to send.
/// Not exhaustive — the full `KEY_*` set is large; these cover the
/// commands `tv_key`'s schema exposes to the model.
pub const KNOWN_KEYS: &[&str] = &[
    "KEY_POWER",
    "KEY_VOLUP",
    "KEY_VOLDOWN",
    "KEY_MUTE",
    "KEY_HOME",
    "KEY_SOURCE",
    "KEY_UP",
    "KEY_DOWN",
    "KEY_LEFT",
    "KEY_RIGHT",
    "KEY_ENTER",
    "KEY_RETURN",
];

fn configured(registry: &Arc<Mutex<Registry>>, key: &str) -> Option<String> {
    registry.lock().unwrap().get_preference(PREF_USER_ID, key)
}

/// Minimal base64 (standard alphabet, padded) — the one call site here is
/// encoding a short fixed app name, not worth a dependency.
fn base64_encode(input: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        out.push(if let Some(b1) = b1 {
            ALPHABET[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if let Some(b2) = b2 {
            ALPHABET[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn ws_url(ip: &str, token: Option<&str>) -> String {
    let name = base64_encode(APP_NAME);
    match token {
        Some(t) => format!(
            "wss://{ip}:{TV_PORT}/api/v2/channels/samsung.remote.control?name={name}&token={t}"
        ),
        None => format!("wss://{ip}:{TV_PORT}/api/v2/channels/samsung.remote.control?name={name}"),
    }
}

#[derive(Debug)]
struct NoCertVerification;

impl rustls::client::danger::ServerCertVerifier for NoCertVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

fn tls_connector() -> Connector {
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoCertVerification))
        .with_no_client_auth();
    Connector::Rustls(Arc::new(config))
}

/// Pull `data.token` out of the TV's connect-ack JSON, if present (only
/// sent on a fresh pairing approval — an already-paired reconnect omits
/// it).
fn extract_token(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    value["data"]["token"].as_str().map(str::to_string)
}

/// Send a single Tizen remote key press. Handles the pairing dance
/// transparently: if no token is stored yet, connects without one (which
/// pops the on-screen approval prompt) and persists whatever token the
/// TV's connect-ack hands back.
pub async fn send_key(key: &str, registry: &Arc<Mutex<Registry>>) -> Result<String, String> {
    let Some(ip) = configured(registry, TV_IP_PREF) else {
        return Err("no TV configured (set the 'tv-ip' preference)".into());
    };
    let token = configured(registry, TV_TOKEN_PREF);
    let url = ws_url(&ip, token.as_deref());

    let (mut ws, _) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_tls_with_config(&url, None, false, Some(tls_connector())),
    )
    .await
    .map_err(|_| "TV connection timed out".to_string())?
    .map_err(|e| format!("TV unreachable: {e}"))?;

    // First frame is the connect ack — on a fresh pairing it carries the
    // token to persist; on an already-paired reconnect it's just a
    // confirmation.
    if let Ok(Some(Ok(Message::Text(text)))) =
        tokio::time::timeout(CONNECT_TIMEOUT, ws.next()).await
        && let Some(new_token) = extract_token(&text)
    {
        registry
            .lock()
            .unwrap()
            .set_preference(PREF_USER_ID, TV_TOKEN_PREF, &new_token);
    }

    let cmd = serde_json::json!({
        "method": "ms.remote.control",
        "params": {
            "Cmd": "Click",
            "DataOfCmd": key,
            "Option": "false",
            "TypeOfRemote": "SendRemoteKey"
        }
    });
    ws.send(Message::Text(cmd.to_string().into()))
        .await
        .map_err(|e| format!("failed to send key: {e}"))?;
    let _ = ws.close(None).await;
    Ok(format!("sent {key}"))
}

/// Broadcast a Wake-on-LAN magic packet to `tv-mac`. Only works if the TV
/// is wired (Tizen doesn't support WoL over Wi-Fi) and the feature is
/// enabled in the TV's own network settings — neither is verifiable from
/// here.
pub async fn wake(registry: &Arc<Mutex<Registry>>) -> Result<String, String> {
    let Some(mac) = configured(registry, TV_MAC_PREF) else {
        return Err("no TV MAC address configured (set the 'tv-mac' preference)".into());
    };
    let mac_bytes = parse_mac(&mac)?;

    let mut packet = vec![0xFFu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac_bytes);
    }

    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| format!("failed to open UDP socket: {e}"))?;
    socket
        .set_broadcast(true)
        .map_err(|e| format!("failed to enable broadcast: {e}"))?;
    socket
        .send_to(&packet, ("255.255.255.255", WOL_PORT))
        .await
        .map_err(|e| format!("failed to send WoL packet: {e}"))?;
    Ok("wake-on-LAN packet sent".into())
}

fn parse_mac(mac: &str) -> Result<[u8; 6], String> {
    let parts: Vec<&str> = mac.split([':', '-']).collect();
    if parts.len() != 6 {
        return Err(format!("'{mac}' is not a valid MAC address"));
    }
    let mut bytes = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        bytes[i] = u8::from_str_radix(part, 16)
            .map_err(|_| format!("'{mac}' is not a valid MAC address"))?;
    }
    Ok(bytes)
}

/// Explicit "not supported" rather than a silent no-op — see the module
/// doc comment.
pub fn audio_output_unsupported() -> String {
    "switching the TV's audio output between the soundbar and the \
     Bluetooth speaker isn't possible over the local API — Samsung only \
     exposes that control through the SmartThings cloud, which ai-mesh \
     deliberately doesn't use"
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_reads_the_preference() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        assert_eq!(configured(&registry, TV_IP_PREF), None);
        registry
            .lock()
            .unwrap()
            .set_preference(PREF_USER_ID, TV_IP_PREF, "10.0.0.21");
        assert_eq!(configured(&registry, TV_IP_PREF), Some("10.0.0.21".into()));
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        assert_eq!(base64_encode("ai-mesh"), "YWktbWVzaA==");
        assert_eq!(base64_encode(""), "");
        assert_eq!(base64_encode("f"), "Zg==");
        assert_eq!(base64_encode("fo"), "Zm8=");
        assert_eq!(base64_encode("foo"), "Zm9v");
    }

    #[test]
    fn ws_url_without_token_omits_token_param() {
        let url = ws_url("10.0.0.21", None);
        assert!(
            url.starts_with("wss://10.0.0.21:8002/api/v2/channels/samsung.remote.control?name=")
        );
        assert!(!url.contains("&token="));
    }

    #[test]
    fn ws_url_with_token_includes_it() {
        let url = ws_url("10.0.0.21", Some("abc123"));
        assert!(url.ends_with("&token=abc123"));
    }

    #[test]
    fn extract_token_reads_data_token() {
        let text = r#"{"event":"ms.channel.connect","data":{"token":"abc123"}}"#;
        assert_eq!(extract_token(text), Some("abc123".into()));
    }

    #[test]
    fn extract_token_missing_returns_none() {
        let text = r#"{"event":"ms.channel.connect","data":{}}"#;
        assert_eq!(extract_token(text), None);
    }

    #[test]
    fn parse_mac_accepts_colon_and_dash_separators() {
        assert_eq!(
            parse_mac("AA:BB:CC:DD:EE:FF").unwrap(),
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]
        );
        assert_eq!(
            parse_mac("aa-bb-cc-dd-ee-ff").unwrap(),
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]
        );
    }

    #[test]
    fn parse_mac_rejects_malformed_input() {
        assert!(parse_mac("not-a-mac").is_err());
        assert!(parse_mac("AA:BB:CC").is_err());
    }

    #[tokio::test]
    async fn send_key_without_configured_ip_errors() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let result = send_key("KEY_VOLUP", &registry).await;
        assert!(result.unwrap_err().contains("no TV configured"));
    }

    #[tokio::test]
    async fn wake_without_configured_mac_errors() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let result = wake(&registry).await;
        assert!(result.unwrap_err().contains("no TV MAC address configured"));
    }

    #[test]
    fn audio_output_unsupported_mentions_smartthings() {
        assert!(audio_output_unsupported().contains("SmartThings"));
    }
}
