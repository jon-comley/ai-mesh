use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt};

type HmacSha256 = Hmac<Sha256>;

/// Maximum accepted length (bytes) of a single length-prefixed wire frame.
///
/// Every framed read takes a 4-byte little-endian length prefix off the socket
/// and then allocates that many bytes. Without a ceiling a peer can advertise
/// `u32::MAX` (~4 GiB) and force an out-of-memory crash *before* the payload —
/// or any authentication — is read. Real frames (heartbeats, model status,
/// inference results, lighting commands) are kilobytes; 64 MiB is comfortably
/// above any legitimate message while bounding a hostile allocation.
pub const MAX_FRAME_LEN: usize = 64 * 1024 * 1024;

/// Why a [`read_bounded_frame`] call did not yield a frame.
#[derive(Debug)]
pub enum FrameReadError {
    /// The stream hit EOF or an I/O error — the peer is gone. Normal disconnect.
    Closed,
    /// The advertised length exceeds [`MAX_FRAME_LEN`]; nothing was allocated.
    /// The caller should drop the connection (a peer this far out of spec is
    /// hostile or badly broken).
    TooLarge(usize),
}

/// Read one length-prefixed wire frame: a 4-byte little-endian length, then that
/// many payload bytes. The length is validated against [`MAX_FRAME_LEN`] **before**
/// the buffer is allocated, so a hostile length prefix can't force an unbounded
/// allocation (the OOM-DoS this guards against). Returns the raw payload bytes.
///
/// This is the single framed-read path for both the coordinator and the agent —
/// route every socket read through it so the bound can never be forgotten.
pub async fn read_bounded_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, FrameReadError> {
    let mut len_buf = [0u8; 4];
    if reader.read_exact(&mut len_buf).await.is_err() {
        return Err(FrameReadError::Closed);
    }
    let msg_len = u32::from_le_bytes(len_buf) as usize;
    if msg_len > MAX_FRAME_LEN {
        return Err(FrameReadError::TooLarge(msg_len));
    }
    let mut buf = vec![0u8; msg_len];
    if reader.read_exact(&mut buf).await.is_err() {
        return Err(FrameReadError::Closed);
    }
    Ok(buf)
}

/// Wire frame used when HMAC signing is active (i.e. when an auth token is configured).
///
/// `payload` holds the JSON-encoded `MeshMessage` bytes.
/// Sent on all connections after the initial unsigned `AuthToken` first-frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedFrame {
    pub ts: u64,
    pub payload: Vec<u8>,
    /// HMAC-SHA256 over `ts_le_bytes || payload`. Always 32 bytes.
    pub sig: Vec<u8>,
}

/// Derive a 32-byte HMAC key from an auth token using HKDF-SHA256.
/// The coordinator and client both call this with the same token to get the same key.
pub fn derive_hmac_key(token: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, token.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(b"ai-mesh-hmac-v1", &mut key)
        .expect("32 bytes is a valid HKDF output length");
    key
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn compute_sig(key: &[u8; 32], ts: u64, payload: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key size");
    mac.update(&ts.to_le_bytes());
    mac.update(payload);
    mac.finalize().into_bytes().to_vec()
}

impl SignedFrame {
    /// Build and sign a frame from a raw payload (JSON bytes of `MeshMessage`).
    pub fn sign(key: &[u8; 32], payload: Vec<u8>) -> Self {
        let ts = now_secs();
        let sig = compute_sig(key, ts, &payload);
        Self { ts, payload, sig }
    }

    /// Build a signed frame with an explicit timestamp. Useful for chaos/security testing
    /// (e.g., deliberately crafting a stale frame to verify rejection).
    pub fn sign_at(key: &[u8; 32], ts: u64, payload: Vec<u8>) -> Self {
        let sig = compute_sig(key, ts, &payload);
        Self { ts, payload, sig }
    }

    /// Verify the HMAC signature and timestamp. Returns the payload on success.
    /// Rejects frames whose timestamp differs from `now` by more than 30 seconds.
    pub fn verify(&self, key: &[u8; 32]) -> Result<&[u8], FrameVerifyError> {
        let now = now_secs();
        let skew = (self.ts as i64 - now as i64).unsigned_abs();
        if skew > 30 {
            return Err(FrameVerifyError::Stale { ts: self.ts, now });
        }
        let expected = compute_sig(key, self.ts, &self.payload);
        if expected != self.sig {
            return Err(FrameVerifyError::InvalidSignature);
        }
        Ok(&self.payload)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameVerifyError {
    #[error("frame timestamp {ts} is stale (now={now}, max skew=30s)")]
    Stale { ts: u64, now: u64 },
    #[error("HMAC signature mismatch — possible forgery or key mismatch")]
    InvalidSignature,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip() {
        let key = derive_hmac_key("test-token");
        let payload = b"hello world".to_vec();
        let frame = SignedFrame::sign(&key, payload.clone());
        let out = frame.verify(&key).unwrap();
        assert_eq!(out, payload.as_slice());
    }

    #[test]
    fn wrong_key_fails_verification() {
        let key = derive_hmac_key("token-a");
        let wrong_key = derive_hmac_key("token-b");
        let frame = SignedFrame::sign(&key, b"data".to_vec());
        assert!(frame.verify(&wrong_key).is_err());
    }

    #[test]
    fn tampered_payload_fails_verification() {
        let key = derive_hmac_key("token");
        let mut frame = SignedFrame::sign(&key, b"original".to_vec());
        frame.payload = b"tampered".to_vec();
        assert!(matches!(
            frame.verify(&key),
            Err(FrameVerifyError::InvalidSignature)
        ));
    }

    #[test]
    fn stale_timestamp_fails_verification() {
        let key = derive_hmac_key("token");
        let frame = SignedFrame::sign_at(&key, now_secs().saturating_sub(60), b"data".to_vec());
        assert!(matches!(
            frame.verify(&key),
            Err(FrameVerifyError::Stale { .. })
        ));
    }

    #[test]
    fn different_tokens_produce_different_keys() {
        let k1 = derive_hmac_key("token1");
        let k2 = derive_hmac_key("token2");
        assert_ne!(k1, k2);
    }

    #[test]
    fn derive_key_is_deterministic() {
        let k1 = derive_hmac_key("same-token");
        let k2 = derive_hmac_key("same-token");
        assert_eq!(k1, k2);
    }

    fn framed(payload: &[u8]) -> Vec<u8> {
        let mut v = (payload.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(payload);
        v
    }

    #[tokio::test]
    async fn read_bounded_frame_roundtrips_payload() {
        let mut cur = std::io::Cursor::new(framed(b"hello world"));
        let out = read_bounded_frame(&mut cur).await.unwrap();
        assert_eq!(out, b"hello world");
    }

    #[tokio::test]
    async fn read_bounded_frame_rejects_oversize_without_allocating() {
        // Advertise > MAX_FRAME_LEN, supply no payload: must reject on the length
        // alone (no attempt to read/allocate the absurd buffer).
        let bytes = ((MAX_FRAME_LEN + 1) as u32).to_le_bytes().to_vec();
        let mut cur = std::io::Cursor::new(bytes);
        assert!(matches!(
            read_bounded_frame(&mut cur).await,
            Err(FrameReadError::TooLarge(n)) if n == MAX_FRAME_LEN + 1
        ));
    }

    #[tokio::test]
    async fn read_bounded_frame_reports_closed_on_eof() {
        let mut cur = std::io::Cursor::new(Vec::new());
        assert!(matches!(
            read_bounded_frame(&mut cur).await,
            Err(FrameReadError::Closed)
        ));
    }

    #[tokio::test]
    async fn read_bounded_frame_reports_closed_on_truncated_payload() {
        // Length says 8 bytes but only 3 follow → mid-frame EOF.
        let mut bytes = 8u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"abc");
        let mut cur = std::io::Cursor::new(bytes);
        assert!(matches!(
            read_bounded_frame(&mut cur).await,
            Err(FrameReadError::Closed)
        ));
    }
}
