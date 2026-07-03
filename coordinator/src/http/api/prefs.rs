//! Dashboard preferences: per-user K/V store.

use axum::{Extension, Json, extract::Path, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

use crate::registry::Registry;

use crate::http::auth::Authed;

// ── Dashboard preferences ─────────────────────────────────────────────────────

const PREF_USER_ID: &str = "default";

#[derive(Deserialize)]
pub struct PrefBody {
    value: String,
}

pub async fn get_preferences(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    let pairs = registry.lock().unwrap().get_all_preferences(PREF_USER_ID);
    let map: std::collections::HashMap<String, String> = pairs.into_iter().collect();
    Json(map).into_response()
}

pub async fn set_preference(
    Path(key): Path<String>,
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Json(body): Json<PrefBody>,
) -> impl IntoResponse {
    if key.is_empty() {
        return (StatusCode::BAD_REQUEST, "key must not be empty").into_response();
    }
    let reg = registry.lock().unwrap();
    reg.set_preference(PREF_USER_ID, &key, &body.value);
    let map: std::collections::HashMap<String, String> =
        reg.get_all_preferences(PREF_USER_ID).into_iter().collect();
    Json(map).into_response()
}

pub async fn delete_preference(
    Path(key): Path<String>,
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    let reg = registry.lock().unwrap();
    if !reg.delete_preference(PREF_USER_ID, &key) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let map: std::collections::HashMap<String, String> =
        reg.get_all_preferences(PREF_USER_ID).into_iter().collect();
    Json(map).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use crate::http::state::DashboardState;
    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::routing::get;
    use std::sync::Mutex;
    use tower::ServiceExt;

    // ── preferences ──────────────────────────────────────────────────────────

    fn make_pref_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        use axum::routing::put;
        Router::new()
            .route("/api/preferences", get(get_preferences))
            .route(
                "/api/preferences/{key}",
                put(set_preference).delete(delete_preference),
            )
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    async fn get_prefs(
        state: Arc<DashboardState>,
        registry: Arc<Mutex<Registry>>,
        token: &str,
    ) -> (StatusCode, std::collections::HashMap<String, String>) {
        let router = make_pref_router(state, registry);
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/preferences?token={token}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let map = serde_json::from_slice(&bytes).unwrap_or_default();
        (status, map)
    }

    async fn put_pref(
        state: Arc<DashboardState>,
        registry: Arc<Mutex<Registry>>,
        token: &str,
        key: &str,
        value: &str,
    ) -> (StatusCode, std::collections::HashMap<String, String>) {
        let router = make_pref_router(state, registry);
        let body = format!(r#"{{"value":{value:?}}}"#);
        let req = Request::builder()
            .method("PUT")
            .uri(format!("/api/preferences/{key}?token={token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let map = serde_json::from_slice(&bytes).unwrap_or_default();
        (status, map)
    }

    async fn delete_pref(
        state: Arc<DashboardState>,
        registry: Arc<Mutex<Registry>>,
        token: &str,
        key: &str,
    ) -> (StatusCode, std::collections::HashMap<String, String>) {
        let router = make_pref_router(state, registry);
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/api/preferences/{key}?token={token}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let map = serde_json::from_slice(&bytes).unwrap_or_default();
        (status, map)
    }

    #[tokio::test]
    async fn get_preferences_empty() {
        let (state, reg) = (
            make_state(vec![], empty_connections()),
            Arc::new(Mutex::new(Registry::new())),
        );
        let (status, map) = get_prefs(state, reg, "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn set_preference_then_get() {
        let state = make_state(vec![], empty_connections());
        let reg = Arc::new(Mutex::new(Registry::new()));

        let (status, map) = put_pref(
            state.clone(),
            Arc::clone(&reg),
            "",
            "meshNodeOrder",
            r#"["n1","n2"]"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            map.get("meshNodeOrder").map(String::as_str),
            Some(r#"["n1","n2"]"#),
            "PUT should return the updated map"
        );

        let (get_status, get_map) = get_prefs(state, reg, "").await;
        assert_eq!(get_status, StatusCode::OK);
        assert_eq!(
            get_map.get("meshNodeOrder").map(String::as_str),
            Some(r#"["n1","n2"]"#)
        );
    }

    #[tokio::test]
    async fn set_preference_upsert() {
        let state = make_state(vec![], empty_connections());
        let reg = Arc::new(Mutex::new(Registry::new()));

        put_pref(state.clone(), Arc::clone(&reg), "", "k", "v1").await;
        let (_, map) = put_pref(state.clone(), Arc::clone(&reg), "", "k", "v2").await;
        assert_eq!(map.get("k").map(String::as_str), Some("v2"));

        let (_, get_map) = get_prefs(state, reg, "").await;
        assert_eq!(get_map.get("k").map(String::as_str), Some("v2"));
    }

    #[tokio::test]
    async fn preferences_returns_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let reg = Arc::new(Mutex::new(Registry::new()));
        let (status, _) = get_prefs(state.clone(), Arc::clone(&reg), "wrong").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (s, _) = put_pref(state.clone(), Arc::clone(&reg), "wrong", "k", "v").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (ds, _) = delete_pref(state, reg, "wrong", "k").await;
        assert_eq!(ds, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delete_preference_removes_key() {
        let state = make_state(vec![], empty_connections());
        let reg = Arc::new(Mutex::new(Registry::new()));

        put_pref(state.clone(), Arc::clone(&reg), "", "k", "v").await;
        let (status, map) = delete_pref(state.clone(), Arc::clone(&reg), "", "k").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !map.contains_key("k"),
            "deleted key should not appear in response map"
        );

        let (_, get_map) = get_prefs(state, reg, "").await;
        assert!(!get_map.contains_key("k"));
    }

    #[tokio::test]
    async fn delete_nonexistent_preference_returns_404() {
        let state = make_state(vec![], empty_connections());
        let reg = Arc::new(Mutex::new(Registry::new()));
        let (status, _) = delete_pref(state, reg, "", "no-such-key").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_preference_empty_body_returns_400() {
        let state = make_state(vec![], empty_connections());
        let reg = Arc::new(Mutex::new(Registry::new()));
        let router = make_pref_router(state, reg);
        let req = Request::builder()
            .method("PUT")
            .uri("/api/preferences/k?token=")
            .header("content-type", "application/json")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
