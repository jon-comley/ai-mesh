use sha2::{Digest, Sha256};

/// SHA-256 fingerprint of a DER-encoded certificate, formatted as colon-separated
/// uppercase hex (e.g. `AA:BB:CC:...`). Used for TOFU fingerprint verification.
pub fn cert_fingerprint(cert_der: &[u8]) -> String {
    let hash = Sha256::digest(cert_der);
    hash.iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_64_hex_chars_with_colons() {
        let cert_der = vec![0u8; 32];
        let fp = cert_fingerprint(&cert_der);
        let parts: Vec<&str> = fp.split(':').collect();
        assert_eq!(parts.len(), 32);
        for part in &parts {
            assert_eq!(part.len(), 2);
            assert!(part.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let cert_der = b"hello world";
        assert_eq!(cert_fingerprint(cert_der), cert_fingerprint(cert_der));
    }

    #[test]
    fn different_bytes_give_different_fingerprint() {
        assert_ne!(cert_fingerprint(b"aaa"), cert_fingerprint(b"bbb"));
    }
}
