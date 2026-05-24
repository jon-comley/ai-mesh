use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use shared::frame::{derive_hmac_key, SignedFrame};
use shared::tls::cert_fingerprint;
use shared::MeshMessage;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tracing::warn;

#[derive(Debug)]
struct FingerprintVerifier {
    expected: String,
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let actual = cert_fingerprint(end_entity);
        if actual == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(Error::General(format!(
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
    ) -> Result<HandshakeSignatureValid, Error> {
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
    ) -> Result<HandshakeSignatureValid, Error> {
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
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
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
    ) -> Result<HandshakeSignatureValid, Error> {
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

pub type CoordinatorStream = TlsStream<TcpStream>;

/// Connect to the coordinator with TLS and optional auth token.
/// Reads MESH_TLS_FINGERPRINT, MESH_INSECURE, and MESH_AUTH_TOKEN from env.
pub async fn connect(coordinator: &str) -> std::io::Result<CoordinatorStream> {
    let insecure = std::env::var("MESH_INSECURE").as_deref() == Ok("1");

    if insecure {
        warn!("MESH_INSECURE=1 — TLS certificate verification disabled.");
    }

    let config = if insecure {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth()
    } else {
        let fingerprint = std::env::var("MESH_TLS_FINGERPRINT").unwrap_or_default();
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(FingerprintVerifier {
                expected: fingerprint,
            }))
            .with_no_client_auth()
    };

    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from("ai-mesh-coordinator")
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?
        .to_owned();

    let tcp = TcpStream::connect(coordinator).await?;
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::ConnectionRefused, e))?;

    Ok(tls)
}

/// Send a single framed message and receive a single framed reply.
///
/// When `MESH_AUTH_TOKEN` is set the `AuthToken` first-frame is sent unsigned,
/// then the request and response are HMAC-signed `SignedFrame`s.
pub async fn send_recv(
    stream: &mut CoordinatorStream,
    msg: &MeshMessage,
) -> std::io::Result<MeshMessage> {
    let token = std::env::var("MESH_AUTH_TOKEN").unwrap_or_default();
    let token = token.trim().to_string();
    if !token.is_empty() {
        write_frame(stream, &MeshMessage::AuthToken(token.clone())).await?;
        let key = derive_hmac_key(&token);
        write_signed_frame(stream, msg, &key).await?;
        read_signed_frame(stream, &key).await
    } else {
        write_frame(stream, msg).await?;
        read_frame(stream).await
    }
}

pub async fn write_signed_frame(
    stream: &mut CoordinatorStream,
    msg: &MeshMessage,
    key: &[u8; 32],
) -> std::io::Result<()> {
    let payload = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let frame = SignedFrame::sign(key, payload);
    let data = serde_json::to_vec(&frame)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = (data.len() as u32).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&data).await?;
    Ok(())
}

pub async fn read_signed_frame(
    stream: &mut CoordinatorStream,
    key: &[u8; 32],
) -> std::io::Result<MeshMessage> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let msg_len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf).await?;
    let frame: SignedFrame = serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let payload = frame
        .verify(key)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e.to_string()))?;
    serde_json::from_slice(payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub async fn write_frame(stream: &mut CoordinatorStream, msg: &MeshMessage) -> std::io::Result<()> {
    let data = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = (data.len() as u32).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&data).await?;
    Ok(())
}

pub async fn read_frame(stream: &mut CoordinatorStream) -> std::io::Result<MeshMessage> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let msg_len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use shared::tls::cert_fingerprint;
    use std::time::Duration;

    fn dummy_cert() -> CertificateDer<'static> {
        CertificateDer::from(vec![0xDE, 0xAD, 0xBE, 0xEF])
    }

    fn dummy_server_name() -> ServerName<'static> {
        ServerName::try_from("ai-mesh-coordinator")
            .unwrap()
            .to_owned()
    }

    fn epoch() -> UnixTime {
        UnixTime::since_unix_epoch(Duration::ZERO)
    }

    #[test]
    fn fingerprint_verifier_accepts_matching_cert() {
        let cert = dummy_cert();
        let expected = cert_fingerprint(&cert);
        let verifier = FingerprintVerifier { expected };
        assert!(verifier
            .verify_server_cert(&cert, &[], &dummy_server_name(), &[], epoch())
            .is_ok());
    }

    #[test]
    fn fingerprint_verifier_rejects_wrong_fingerprint() {
        let cert = dummy_cert();
        let verifier = FingerprintVerifier {
            expected: "AA:BB:CC:DD".to_string(),
        };
        assert!(verifier
            .verify_server_cert(&cert, &[], &dummy_server_name(), &[], epoch())
            .is_err());
    }

    #[test]
    fn fingerprint_verifier_error_mentions_mismatch() {
        let cert = dummy_cert();
        let verifier = FingerprintVerifier {
            expected: "XX:YY:ZZ".to_string(),
        };
        let err = verifier
            .verify_server_cert(&cert, &[], &dummy_server_name(), &[], epoch())
            .unwrap_err();
        assert!(err.to_string().contains("mismatch"), "got: {err}");
    }

    #[test]
    fn fingerprint_verifier_rejects_empty_expected() {
        let cert = dummy_cert();
        let verifier = FingerprintVerifier {
            expected: String::new(),
        };
        assert!(verifier
            .verify_server_cert(&cert, &[], &dummy_server_name(), &[], epoch())
            .is_err());
    }

    #[test]
    fn no_verifier_accepts_any_cert() {
        let cert = dummy_cert();
        assert!(NoVerifier
            .verify_server_cert(&cert, &[], &dummy_server_name(), &[], epoch())
            .is_ok());
    }
}
