use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use shared::tls::cert_fingerprint;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;
use tracing::info;

pub fn cert_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ai-mesh")
}

/// Load an existing cert+key from PEM files, or generate and persist new ones.
/// Returns (cert_der, key_der).
pub fn load_or_generate(cert_path: &Path, key_path: &Path) -> (Vec<u8>, Vec<u8>) {
    if cert_path.exists() && key_path.exists() {
        let cert_pem = fs::read(cert_path).expect("failed to read coordinator cert");
        let key_pem = fs::read(key_path).expect("failed to read coordinator key");
        let cert_der = pem_to_der(&cert_pem, "CERTIFICATE");
        let key_der = pem_to_der(&key_pem, "PRIVATE KEY");
        return (cert_der, key_der);
    }

    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["ai-mesh-coordinator".to_string()])
            .expect("failed to generate self-signed cert");

    let cert_der = cert.der().to_vec();
    let key_der = key_pair.serialize_der();

    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent).expect("failed to create cert dir");
    }
    fs::write(cert_path, cert.pem()).expect("failed to write coordinator cert");
    fs::write(key_path, key_pair.serialize_pem()).expect("failed to write coordinator key");

    info!(
        "generated new TLS certificate — stored at {}",
        cert_path.display()
    );
    (cert_der, key_der)
}

pub fn make_acceptor(cert_der: Vec<u8>, key_der: Vec<u8>) -> TlsAcceptor {
    let cert = CertificateDer::from(cert_der);
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("invalid TLS cert/key");
    TlsAcceptor::from(Arc::new(config))
}

/// Print the coordinator's fingerprint on startup.
pub fn log_fingerprint(cert_der: &[u8]) {
    let fp = cert_fingerprint(cert_der);
    info!("coordinator TLS fingerprint: {}", fp);
    info!("  (copy this to MESH_TLS_FINGERPRINT in each node's .env)");
}

fn pem_to_der(pem: &[u8], label: &str) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(pem);
    rustls_pemfile::read_all(&mut cursor)
        .filter_map(|item| {
            item.ok().and_then(|i| match i {
                rustls_pemfile::Item::X509Certificate(der) if label == "CERTIFICATE" => {
                    Some(der.to_vec())
                }
                rustls_pemfile::Item::Pkcs8Key(der) if label == "PRIVATE KEY" => {
                    Some(der.secret_pkcs8_der().to_vec())
                }
                _ => None,
            })
        })
        .next()
        .unwrap_or_else(|| panic!("no {} found in PEM file", label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generates_cert_and_persists() {
        let dir = TempDir::new().unwrap();
        let cert_path = dir.path().join("coordinator.crt");
        let key_path = dir.path().join("coordinator.key");

        let (cert_der, key_der) = load_or_generate(&cert_path, &key_path);
        assert!(!cert_der.is_empty());
        assert!(!key_der.is_empty());
        assert!(cert_path.exists());
        assert!(key_path.exists());
    }

    #[test]
    fn load_returns_same_cert_on_second_call() {
        let dir = TempDir::new().unwrap();
        let cert_path = dir.path().join("coordinator.crt");
        let key_path = dir.path().join("coordinator.key");

        let (cert_der1, _) = load_or_generate(&cert_path, &key_path);
        let (cert_der2, _) = load_or_generate(&cert_path, &key_path);
        assert_eq!(cert_der1, cert_der2);
    }

    #[test]
    fn make_acceptor_does_not_panic() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = TempDir::new().unwrap();
        let cert_path = dir.path().join("coordinator.crt");
        let key_path = dir.path().join("coordinator.key");
        let (cert_der, key_der) = load_or_generate(&cert_path, &key_path);
        let _ = make_acceptor(cert_der, key_der);
    }
}
