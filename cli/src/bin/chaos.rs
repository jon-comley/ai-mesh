/// ai-mesh HMAC chaos test
///
/// Fires six attack scenarios at the live coordinator and verifies that each
/// one is correctly rejected. Requires a running, TLS-enabled coordinator.
///
/// Environment variables:
///   MESH_COORDINATOR     host:port (default: 192.168.1.15:9000)
///   MESH_TLS_FINGERPRINT SHA-256 cert fingerprint (required unless MESH_INSECURE=1)
///   MESH_AUTH_TOKEN      the current valid token (required — chaos tests HMAC)
///   MESH_INSECURE        set to "1" to skip TLS cert verification
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme};
use shared::frame::{derive_hmac_key, now_secs, SignedFrame};
use shared::hardware::{HeartbeatPayload, NodeIdentity, NodeRole};
use shared::tls::cert_fingerprint;
use shared::MeshMessage;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

// ── TLS helpers (standalone — no lib.rs in cli crate) ────────────────────────

#[derive(Debug)]
struct FingerprintVerifier {
    expected: String,
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let actual = cert_fingerprint(end_entity);
        if actual == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(format!(
                "TLS fingerprint mismatch: expected {} got {}",
                self.expected, actual
            )))
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

async fn tls_connect(coordinator: &str) -> std::io::Result<TlsStream<TcpStream>> {
    let insecure = std::env::var("MESH_INSECURE").as_deref() == Ok("1");
    let config: ClientConfig = if insecure {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth()
    } else {
        let fp = std::env::var("MESH_TLS_FINGERPRINT").unwrap_or_default();
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(FingerprintVerifier { expected: fp }))
            .with_no_client_auth()
    };
    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from("ai-mesh-coordinator")
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?
        .to_owned();
    let tcp = TcpStream::connect(coordinator).await?;
    connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::ConnectionRefused, e))
}

/// Write a length-prefixed raw byte frame.
async fn write_lv(stream: &mut TlsStream<TcpStream>, bytes: &[u8]) -> std::io::Result<()> {
    let len = (bytes.len() as u32).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(bytes).await?;
    Ok(())
}

/// Try to read one length-prefixed frame. Returns None on EOF or I/O error.
async fn try_read_lv(stream: &mut TlsStream<TcpStream>) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.ok()?;
    let msg_len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf).await.ok()?;
    Some(buf)
}

/// Expect the connection to be rejected: we should get EOF, not a valid frame.
/// Returns true if the connection was rejected (nothing readable within 2s).
async fn expect_rejection(stream: &mut TlsStream<TcpStream>) -> bool {
    match timeout(Duration::from_secs(2), try_read_lv(stream)).await {
        Err(_elapsed) => true,     // timeout — server went quiet, counts as rejection
        Ok(None) => true,          // EOF — server closed the connection
        Ok(Some(_bytes)) => false, // got data — coordinator should NOT have replied
    }
}

/// Expect a valid signed Acknowledge reply (sanity check scenario).
async fn expect_acknowledge(stream: &mut TlsStream<TcpStream>, key: &[u8; 32]) -> bool {
    match timeout(Duration::from_secs(5), try_read_lv(stream)).await {
        Err(_) => {
            println!("      (no reply within 5s)");
            false
        }
        Ok(None) => {
            println!("      (connection closed unexpectedly)");
            false
        }
        Ok(Some(bytes)) => match serde_json::from_slice::<SignedFrame>(&bytes) {
            Err(e) => {
                println!("      (reply is not a SignedFrame: {e})");
                false
            }
            Ok(frame) => match frame.verify(key) {
                Err(e) => {
                    println!("      (reply HMAC invalid: {e})");
                    false
                }
                Ok(payload) => match serde_json::from_slice::<MeshMessage>(payload) {
                    Ok(MeshMessage::Acknowledge) => true,
                    Ok(other) => {
                        println!("      (unexpected reply: {other:?})");
                        false
                    }
                    Err(e) => {
                        println!("      (inner parse error: {e})");
                        false
                    }
                },
            },
        },
    }
}

fn dummy_heartbeat(token: &str) -> MeshMessage {
    MeshMessage::Heartbeat(HeartbeatPayload {
        identity: NodeIdentity {
            id: "chaos-test".into(),
            hostname: "chaos".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        },
        auth_token: token.to_string(),
    })
}

// ── Scenarios ─────────────────────────────────────────────────────────────────

/// Send a plain Heartbeat as the very first frame — no AuthToken prefix.
/// The coordinator expects AuthToken first; anything else should close the connection.
async fn scenario_no_auth(coordinator: &str, token: &str) -> bool {
    let Ok(mut stream) = tls_connect(coordinator).await else {
        println!("      (TLS connect failed)");
        return false;
    };
    let heartbeat = serde_json::to_vec(&dummy_heartbeat(token)).unwrap();
    let _ = write_lv(&mut stream, &heartbeat).await;
    expect_rejection(&mut stream).await
}

/// Send an AuthToken with a wrong value; the coordinator should close immediately.
async fn scenario_wrong_token(coordinator: &str) -> bool {
    let Ok(mut stream) = tls_connect(coordinator).await else {
        println!("      (TLS connect failed)");
        return false;
    };
    let bad_auth =
        serde_json::to_vec(&MeshMessage::AuthToken("totally-wrong-token".into())).unwrap();
    let _ = write_lv(&mut stream, &bad_auth).await;
    // Send a second frame — it won't be read, but we need something in the pipe
    let heartbeat = serde_json::to_vec(&dummy_heartbeat("totally-wrong-token")).unwrap();
    let _ = write_lv(&mut stream, &heartbeat).await;
    expect_rejection(&mut stream).await
}

/// Valid AuthToken, then a plain unsigned MeshMessage (not wrapped in SignedFrame).
/// The coordinator tries to parse it as SignedFrame, fails, and drops the connection.
async fn scenario_unsigned_frame(coordinator: &str, token: &str) -> bool {
    let Ok(mut stream) = tls_connect(coordinator).await else {
        println!("      (TLS connect failed)");
        return false;
    };
    let auth = serde_json::to_vec(&MeshMessage::AuthToken(token.into())).unwrap();
    let _ = write_lv(&mut stream, &auth).await;
    // Send a raw MeshMessage instead of a SignedFrame
    let plain = serde_json::to_vec(&dummy_heartbeat(token)).unwrap();
    let _ = write_lv(&mut stream, &plain).await;
    expect_rejection(&mut stream).await
}

/// Valid AuthToken, then a SignedFrame with a corrupted HMAC signature.
/// Coordinator should reject: signature mismatch.
async fn scenario_bad_hmac(coordinator: &str, token: &str) -> bool {
    let Ok(mut stream) = tls_connect(coordinator).await else {
        println!("      (TLS connect failed)");
        return false;
    };
    let key = derive_hmac_key(token);
    let auth = serde_json::to_vec(&MeshMessage::AuthToken(token.into())).unwrap();
    let _ = write_lv(&mut stream, &auth).await;
    // Build a valid frame then flip a bit in the signature
    let payload = serde_json::to_vec(&dummy_heartbeat(token)).unwrap();
    let mut frame = SignedFrame::sign(&key, payload);
    frame.sig[0] ^= 0xFF; // corrupt first byte of HMAC
    let frame_bytes = serde_json::to_vec(&frame).unwrap();
    let _ = write_lv(&mut stream, &frame_bytes).await;
    expect_rejection(&mut stream).await
}

/// Valid AuthToken, then a SignedFrame with a timestamp 60 seconds in the past.
/// Coordinator should reject: stale frame (max skew is 30s).
async fn scenario_stale_timestamp(coordinator: &str, token: &str) -> bool {
    let Ok(mut stream) = tls_connect(coordinator).await else {
        println!("      (TLS connect failed)");
        return false;
    };
    let key = derive_hmac_key(token);
    let auth = serde_json::to_vec(&MeshMessage::AuthToken(token.into())).unwrap();
    let _ = write_lv(&mut stream, &auth).await;
    let stale_ts = now_secs().saturating_sub(60);
    let payload = serde_json::to_vec(&dummy_heartbeat(token)).unwrap();
    let frame = SignedFrame::sign_at(&key, stale_ts, payload);
    let frame_bytes = serde_json::to_vec(&frame).unwrap();
    let _ = write_lv(&mut stream, &frame_bytes).await;
    expect_rejection(&mut stream).await
}

/// Valid AuthToken + properly signed Heartbeat → expect a signed Acknowledge reply.
/// This is the sanity-check that we haven't broken normal operation.
async fn scenario_valid_request(coordinator: &str, token: &str) -> bool {
    let Ok(mut stream) = tls_connect(coordinator).await else {
        println!("      (TLS connect failed)");
        return false;
    };
    let key = derive_hmac_key(token);
    let auth = serde_json::to_vec(&MeshMessage::AuthToken(token.into())).unwrap();
    let _ = write_lv(&mut stream, &auth).await;
    let payload = serde_json::to_vec(&dummy_heartbeat(token)).unwrap();
    let frame = SignedFrame::sign(&key, payload);
    let frame_bytes = serde_json::to_vec(&frame).unwrap();
    if write_lv(&mut stream, &frame_bytes).await.is_err() {
        println!("      (write failed)");
        return false;
    }
    expect_acknowledge(&mut stream, &key).await
}

/// HTTP dashboard: a wrong token on /ws must return 401 Unauthorized.
/// Connects via plain TCP (the dashboard has no TLS) and sends a proper
/// WebSocket upgrade request with a bogus token. Auth is checked before the
/// upgrade, so the response must be 401, not 400.
async fn scenario_dashboard_auth(dashboard: &str) -> bool {
    let Ok(Ok(mut stream)) = timeout(Duration::from_secs(5), TcpStream::connect(dashboard)).await
    else {
        println!("      (TCP connect to {dashboard} failed/timed out — is the dashboard running?)");
        return false;
    };
    let req = format!(
        "GET /ws?token=definitely-wrong-token HTTP/1.1\r\n\
         Host: {dashboard}\r\n\
         Connection: Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).await.is_err() {
        println!("      (write failed)");
        return false;
    }
    let mut buf = vec![0u8; 512];
    match timeout(Duration::from_secs(3), stream.read(&mut buf)).await {
        Err(_) => {
            println!("      (no response within 3s)");
            false
        }
        Ok(Err(_)) => {
            println!("      (read error)");
            false
        }
        Ok(Ok(n)) => {
            let response = String::from_utf8_lossy(&buf[..n]);
            let got_401 = response.contains("401");
            if !got_401 {
                let first_line = response.lines().next().unwrap_or("(empty)");
                println!("      (unexpected response: {first_line})");
            }
            got_401
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    default_provider()
        .install_default()
        .expect("failed to install ring crypto provider");

    let coordinator =
        std::env::var("MESH_COORDINATOR").unwrap_or_else(|_| "192.168.1.15:9000".to_string());
    let token = std::env::var("MESH_AUTH_TOKEN").unwrap_or_default();
    let token = token.trim().to_string();
    let http_port = std::env::var("MESH_HTTP_PORT").unwrap_or_else(|_| "9001".to_string());
    // MESH_DASHBOARD_HOST lets the caller override the dashboard host independently of
    // MESH_COORDINATOR — needed on WSL2 where only port 9000 has a portproxy.
    let dashboard_host = std::env::var("MESH_DASHBOARD_HOST").unwrap_or_else(|_| {
        coordinator
            .rsplit_once(':')
            .map(|(h, _)| h.to_string())
            .unwrap_or_else(|| coordinator.clone())
    });
    let dashboard = format!("{dashboard_host}:{http_port}");

    println!("=== ai-mesh HMAC chaos test ===");
    println!("Coordinator: {coordinator}");
    println!("Dashboard:   {dashboard}");
    if token.is_empty() {
        eprintln!("\nERROR: MESH_AUTH_TOKEN is not set.");
        eprintln!("The chaos test requires a coordinator with HMAC auth enabled.");
        eprintln!("Source coordinator.state first:  source ~/.config/ai-mesh/coordinator.state");
        std::process::exit(2);
    }
    println!();

    let scenarios: &[(&str, &str, bool)] = &[
        (
            "No AuthToken — plain Heartbeat as the very first frame",
            "Coordinator requires AuthToken as first frame; anything else must be rejected.",
            false,
        ),
        (
            "Wrong auth token (\"totally-wrong-token\")",
            "Coordinator must reject connections with an unrecognised token.",
            false,
        ),
        (
            "Valid AuthToken → unsigned plain frame (no SignedFrame wrapper)",
            "After auth, coordinator expects SignedFrame JSON; a plain MeshMessage must be rejected.",
            false,
        ),
        (
            "Valid AuthToken → SignedFrame with corrupted HMAC (first byte flipped)",
            "Coordinator must reject frames whose signature does not match.",
            false,
        ),
        (
            "Valid AuthToken → SignedFrame with stale timestamp (ts − 60s)",
            "Coordinator must reject frames older than 30 seconds.",
            false,
        ),
        (
            "Valid AuthToken → properly signed Heartbeat (sanity check)",
            "A legitimate request must succeed and return a signed Acknowledge.",
            true,  // this one expects success
        ),
        (
            "Dashboard HTTP: wrong token on /ws must return 401",
            "The dashboard WebSocket endpoint must reject unauthenticated connections with 401.",
            false,
        ),
    ];

    let mut passed = 0usize;
    let mut failed = 0usize;

    for (i, (name, desc, expect_success)) in scenarios.iter().enumerate() {
        println!("[{}/{}] {name}", i + 1, scenarios.len());
        println!("      {desc}");

        let result = match i {
            0 => scenario_no_auth(&coordinator, &token).await,
            1 => scenario_wrong_token(&coordinator).await,
            2 => scenario_unsigned_frame(&coordinator, &token).await,
            3 => scenario_bad_hmac(&coordinator, &token).await,
            4 => scenario_stale_timestamp(&coordinator, &token).await,
            5 => scenario_valid_request(&coordinator, &token).await,
            6 => scenario_dashboard_auth(&dashboard).await,
            _ => unreachable!(),
        };

        // All scenario functions return true when the expected outcome occurred.
        // Rejection scenarios: true = coordinator correctly closed the connection.
        // Success scenario:    true = coordinator replied with a signed Acknowledge.
        let pass = result;

        if pass {
            println!("      Result: PASS");
            passed += 1;
        } else {
            let detail = if *expect_success {
                "coordinator rejected a valid request — is auth configured correctly?"
            } else {
                "coordinator accepted an attack frame — HMAC security may be broken!"
            };
            println!("      Result: FAIL  ← {detail}");
            failed += 1;
        }
        println!();
    }

    println!("─────────────────────────────────────────");
    if failed == 0 {
        println!("All {passed} scenarios passed. HMAC security verified.");
    } else {
        println!("{passed} passed, {failed} FAILED.");
        std::process::exit(1);
    }
}
