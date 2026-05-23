use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use shared::tls::cert_fingerprint;
use std::sync::Arc;
use tokio_rustls::TlsConnector;
use tracing::warn;

/// Verifies the coordinator cert matches a known SHA-256 fingerprint (TOFU).
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
            warn!(
                "TLS fingerprint mismatch — expected {} got {} (possible coordinator rekey or MITM)",
                self.expected, actual
            );
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

/// Accepts any certificate without verification — for MESH_INSECURE=1 only.
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

/// Build a TLS connector from env vars:
/// - MESH_TLS_FINGERPRINT — expected coordinator cert fingerprint (required unless MESH_INSECURE=1)
/// - MESH_INSECURE=1      — skip cert verification (dev/test only, logs a loud warning)
pub fn make_connector() -> TlsConnector {
    let insecure = std::env::var("MESH_INSECURE").as_deref() == Ok("1");

    let config = if insecure {
        warn!("MESH_INSECURE=1 — TLS certificate verification disabled. Do not use in production.");
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth()
    } else {
        let fingerprint = std::env::var("MESH_TLS_FINGERPRINT").unwrap_or_else(|_| {
            warn!(
                "MESH_TLS_FINGERPRINT not set and MESH_INSECURE != 1 — TLS connection will fail. \
                 Run coordinator and copy the printed fingerprint to MESH_TLS_FINGERPRINT."
            );
            String::new()
        });
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(FingerprintVerifier {
                expected: fingerprint,
            }))
            .with_no_client_auth()
    };

    TlsConnector::from(Arc::new(config))
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
        assert!(
            verifier
                .verify_server_cert(&cert, &[], &dummy_server_name(), &[], epoch())
                .is_ok()
        );
    }

    #[test]
    fn fingerprint_verifier_rejects_wrong_fingerprint() {
        let cert = dummy_cert();
        let verifier = FingerprintVerifier {
            expected: "AA:BB:CC:DD".to_string(),
        };
        assert!(
            verifier
                .verify_server_cert(&cert, &[], &dummy_server_name(), &[], epoch())
                .is_err()
        );
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
        assert!(
            verifier
                .verify_server_cert(&cert, &[], &dummy_server_name(), &[], epoch())
                .is_err()
        );
    }

    #[test]
    fn no_verifier_accepts_any_cert() {
        let cert = dummy_cert();
        assert!(
            NoVerifier
                .verify_server_cert(&cert, &[], &dummy_server_name(), &[], epoch())
                .is_ok()
        );
    }
}
