//! Serves synthesized TTS clips to the ESPHome Voice PE puck's
//! media_player. Deliberately unauthenticated (see the registration-site
//! comment in `http/mod.rs`) — the device has no dashboard token, only a
//! URL `capability-voice` handed it in a `tts-end` event.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use std::path::{Path as StdPath, PathBuf};

/// Must match `capabilities/voice/src/tts.rs`'s `cache_dir()` default —
/// `/var/lib/ai-mesh`, not `~`, since this process runs with
/// `ProtectHome=true` (see this unit's own comment on why) and cannot
/// see anything under `/home` at all, including any other user's.
fn cache_dir() -> PathBuf {
    std::env::var("VOICE_TTS_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/ai-mesh/tts-cache"))
}

/// `GET /api/voice/tts/{id}` — `id` is a bare UUID (no extension, no
/// path separators possible in a single Axum path segment), so this
/// can't be used for directory traversal. Best-effort deletes the file
/// after serving: these clips are single-use, played once by the puck.
pub async fn serve_clip(Path(id): Path<String>) -> impl IntoResponse {
    // Belt-and-braces even though Axum path segments can't contain '/':
    // refuse anything that isn't a plain filename component.
    if id.is_empty() || StdPath::new(&id).file_name().map(|n| n.to_str()) != Some(Some(id.as_str()))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let path = cache_dir().join(format!("{id}.wav"));
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let _ = tokio::fs::remove_file(&path).await;
    ([(header::CONTENT_TYPE, "audio/wav")], bytes).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get;
    use tower::ServiceExt;

    // VOICE_TTS_CACHE_DIR is process-global; these tests run in parallel
    // by default (separate OS threads), so setting it in one test can
    // race a concurrent read in another. Serialize with this lock rather
    // than pulling in a new dev-dependency for it.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn router(dir: &std::path::Path) -> Router {
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe { std::env::set_var("VOICE_TTS_CACHE_DIR", dir) };
        Router::new().route("/api/voice/tts/{id}", get(serve_clip))
    }

    #[tokio::test]
    async fn serves_and_deletes_an_existing_clip() {
        let _guard = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let id = "11111111-1111-1111-1111-111111111111";
        std::fs::write(dir.path().join(format!("{id}.wav")), b"RIFF....WAVEfmt ").unwrap();

        let resp = router(dir.path())
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/api/voice/tts/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(!dir.path().join(format!("{id}.wav")).exists());
    }

    #[tokio::test]
    async fn missing_clip_is_404() {
        let _guard = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let resp = router(dir.path())
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/voice/tts/does-not-exist")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn traversal_attempt_is_rejected() {
        let _guard = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let resp = router(dir.path())
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/voice/tts/..%2f..%2f..%2fetc%2fpasswd")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
