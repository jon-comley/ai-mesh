//! HTTP auth: one token-extraction helper and the [`Authed`] extractor.
//!
//! Every `/api/*` handler takes `_: Authed` as a parameter, which makes the
//! token check part of the handler's type — a new route cannot compile
//! without deciding its auth story. The only deliberately public route is
//! `solar_config` (see the registration-site comment in `http/mod.rs`).
//!
//! Tokens are accepted from `Authorization: Bearer <token>` (what OpenAI
//! SDKs and most HTTP clients send) with the `?token=` query param as a
//! fallback (what the dashboard and `just` recipes use). The header wins
//! when both are present.

use super::state::DashboardState;
use axum::extract::FromRequestParts;
use axum::http::{HeaderMap, StatusCode, header, request::Parts};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct TokenQuery {
    #[serde(default)]
    pub(crate) token: String,
}

/// Token from an `Authorization: Bearer …` header, if present and non-empty.
pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Pull the auth token out of request parts: Bearer header first, `?token=`
/// query param as fallback. Returns an empty string when neither is present
/// (which `auth_ok` accepts only in dev mode — empty token list).
pub(crate) fn token_from_parts(parts: &Parts) -> String {
    if let Some(token) = bearer_token(&parts.headers) {
        return token;
    }
    parts
        .uri
        .query()
        .and_then(|q| serde_urlencoded::from_str::<TokenQuery>(q).ok())
        .map(|q| q.token)
        .unwrap_or_default()
}

/// Proof-of-auth extractor. Handlers list `_: Authed` to require a valid
/// mesh token; rejection is a plain 401 (the shape every `/api/*` handler
/// returned before this extractor existed).
pub struct Authed;

impl FromRequestParts<Arc<DashboardState>> for Authed {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<DashboardState>,
    ) -> Result<Self, Self::Rejection> {
        if state.auth_ok(&token_from_parts(parts)) {
            Ok(Authed)
        } else {
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    fn parts_for(uri: &str, bearer: Option<&str>) -> Parts {
        let mut builder = Request::builder().uri(uri);
        if let Some(t) = bearer {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        builder.body(Body::empty()).unwrap().into_parts().0
    }

    #[test]
    fn token_from_query() {
        let parts = parts_for("/api/x?token=abc", None);
        assert_eq!(token_from_parts(&parts), "abc");
    }

    #[test]
    fn token_from_bearer() {
        let parts = parts_for("/api/x", Some("abc"));
        assert_eq!(token_from_parts(&parts), "abc");
    }

    #[test]
    fn bearer_wins_over_query() {
        let parts = parts_for("/api/x?token=query-one", Some("header-one"));
        assert_eq!(token_from_parts(&parts), "header-one");
    }

    #[test]
    fn missing_both_is_empty() {
        let parts = parts_for("/api/x", None);
        assert_eq!(token_from_parts(&parts), "");
    }

    #[tokio::test]
    async fn extractor_accepts_valid_token_and_rejects_wrong() {
        use std::collections::HashMap;
        use std::sync::Mutex;
        let state = DashboardState::new(
            Arc::new(vec!["secret".to_string()]),
            Arc::new(Mutex::new(HashMap::new())),
        );

        let mut ok = parts_for("/api/x?token=secret", None);
        assert!(
            Authed::from_request_parts(&mut ok, &state).await.is_ok(),
            "valid query token accepted"
        );

        let mut ok_bearer = parts_for("/api/x", Some("secret"));
        assert!(
            Authed::from_request_parts(&mut ok_bearer, &state)
                .await
                .is_ok(),
            "valid bearer accepted"
        );

        let mut bad = parts_for("/api/x?token=wrong", None);
        assert_eq!(
            Authed::from_request_parts(&mut bad, &state)
                .await
                .err()
                .unwrap(),
            StatusCode::UNAUTHORIZED
        );

        let mut missing = parts_for("/api/x", None);
        assert!(
            Authed::from_request_parts(&mut missing, &state)
                .await
                .is_err(),
            "missing token rejected when tokens configured"
        );
    }

    #[tokio::test]
    async fn extractor_dev_mode_accepts_anything() {
        use std::collections::HashMap;
        use std::sync::Mutex;
        let state = DashboardState::new(Arc::new(vec![]), Arc::new(Mutex::new(HashMap::new())));
        let mut parts = parts_for("/api/x", None);
        assert!(Authed::from_request_parts(&mut parts, &state).await.is_ok());
    }
}
