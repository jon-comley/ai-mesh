//! Online AI gateway config: provider presets, keys, test call.

use axum::{Extension, Json, extract::State, response::IntoResponse};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

use crate::http::state::DashboardState;
use crate::registry::Registry;

use crate::http::auth::Authed;

// ── Cloud gateway (online-AI forwarding) ──────────────────────────────────────

/// Assemble the masked config + cumulative stats for the Gateway tab.
pub fn gateway_snapshot(
    reg: &Registry,
    state: &DashboardState,
) -> crate::http::state::GatewaySnapshot {
    use crate::compress::CompressionEngine;
    let cfg = crate::cloud::GatewayConfig::load(reg);
    let stats = state.get_gateway_stats();
    let engine = match cfg.engine {
        CompressionEngine::Statistical => "statistical",
        CompressionEngine::LocalLlmDistiller => "local_llm_distiller",
        CompressionEngine::Llmlingua2 => "llmlingua2",
    }
    .to_string();
    crate::http::state::GatewaySnapshot {
        enabled: cfg.enabled,
        compress: cfg.compress,
        engine,
        selected_model: cfg.selected_model.clone(),
        base_url: cfg.base_url.clone(),
        key_set: cfg.api_key.as_deref().is_some_and(|k| !k.is_empty()),
        key_hint: cfg.key_hint(),
        available_models: crate::cloud::models_for_base_url(&cfg.base_url),
        presets: crate::cloud::provider_presets()
            .iter()
            .map(|p| crate::http::state::GatewayPreset {
                id: p.id.to_string(),
                label: p.label.to_string(),
                base_url: p.base_url.to_string(),
                models: p.models.iter().map(|s| s.to_string()).collect(),
            })
            .collect(),
        calls: stats.calls,
        tokens_before: stats.tokens_before,
        tokens_after: stats.tokens_after,
        tokens_saved: stats.tokens_saved,
        last_call_at: stats.last_call_at,
        last_error: stats.last_error,
    }
}

pub async fn get_gateway(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    let snap = gateway_snapshot(&registry.lock().unwrap(), &state);
    Json(snap).into_response()
}

#[derive(Deserialize)]
pub struct GatewayConfigBody {
    enabled: Option<bool>,
    compress: Option<bool>,
    engine: Option<String>,
    selected_model: Option<String>,
    base_url: Option<String>,
    /// Persisted only when present; an empty string clears the stored key.
    api_key: Option<String>,
}

pub async fn set_gateway(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Json(body): Json<GatewayConfigBody>,
) -> impl IntoResponse {
    {
        let reg = registry.lock().unwrap();
        if let Some(enabled) = body.enabled {
            crate::cloud::set_gateway_pref(&reg, "enabled", if enabled { "true" } else { "false" });
        }
        if let Some(compress) = body.compress {
            crate::cloud::set_gateway_pref(
                &reg,
                "compress",
                if compress { "true" } else { "false" },
            );
        }
        // Ignore unknown engine ids so a bad value never gets persisted (the
        // snapshot derives the engine from the parsed enum, which would otherwise
        // disagree with the stored string).
        if let Some(engine) = body.engine.as_deref()
            && matches!(engine, "statistical" | "local_llm_distiller" | "llmlingua2")
        {
            crate::cloud::set_gateway_pref(&reg, "engine", engine);
        }
        if let Some(model) = body.selected_model.as_deref() {
            crate::cloud::set_gateway_pref(&reg, "selected_model", model);
        }
        if let Some(base_url) = body.base_url.as_deref() {
            crate::cloud::set_gateway_pref(&reg, "base_url", base_url);
        }
        if let Some(api_key) = body.api_key.as_deref() {
            // Store under the endpoint currently in effect so each provider keeps
            // its own key (base_url, if changing, was written just above).
            let base = crate::cloud::GatewayConfig::load(&reg).base_url;
            crate::cloud::set_gateway_pref(
                &reg,
                &crate::cloud::provider_key_name(&base),
                api_key.trim(),
            );
        }
    }
    let snap = gateway_snapshot(&registry.lock().unwrap(), &state);
    state.push_gateway_update(snap.clone());
    Json(snap).into_response()
}

/// One-shot connectivity check used by the tab's "Test cloud call" button.
pub async fn test_gateway(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    let provider = crate::cloud::GatewayConfig::load(&registry.lock().unwrap()).provider();
    let Some(provider) = provider else {
        return Json(
            serde_json::json!({ "ok": false, "error": "not configured (set a model and API key)" }),
        )
        .into_response();
    };
    match provider
        .complete(
            &[shared::ChatTurn::user("Reply with the single word: pong")],
            0.4,
        )
        .await
    {
        Ok(reply) => Json(serde_json::json!({ "ok": true, "reply": reply.text })).into_response(),
        Err(e) => {
            state.record_gateway_error(e.to_string());
            let snap = gateway_snapshot(&registry.lock().unwrap(), &state);
            state.push_gateway_update(snap);
            Json(serde_json::json!({ "ok": false, "error": e.to_string() })).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;
    use std::sync::Mutex;

    fn gateway_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route("/api/gateway", get(get_gateway).post(set_gateway))
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    #[tokio::test]
    async fn gateway_post_persists_masks_key_and_broadcasts() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let mut rx = state.tx.subscribe();

        let (status, body) = send_with_body(
            gateway_router(state.clone(), registry.clone()),
            "POST",
            "/api/gateway?token=",
            r#"{"enabled":true,"compress":false,"selected_model":"qwen/qwen-2.5-72b-instruct:free","api_key":"sk-secret-1234"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let snap: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(snap["enabled"], true);
        assert_eq!(snap["compress"], false);
        assert_eq!(snap["selected_model"], "qwen/qwen-2.5-72b-instruct:free");
        assert_eq!(snap["key_set"], true);
        assert_eq!(snap["key_hint"], "…1234");
        // The secret must never be serialized back to clients.
        assert!(body.contains("1234"), "hint present");
        assert!(!body.contains("sk-secret"), "raw key must not leak");

        use crate::http::state::DashboardEvent;
        match rx.try_recv().unwrap() {
            DashboardEvent::GatewayUpdate(s) => {
                assert!(s.enabled);
                assert!(!s.compress);
                assert!(s.key_set);
            }
            _ => panic!("expected GatewayUpdate"),
        }

        // GET reflects the persisted config.
        let (status, body) = send_with_body(
            gateway_router(state, registry),
            "GET",
            "/api/gateway?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let snap: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(snap["enabled"], true);
        assert_eq!(snap["key_set"], true);
        assert!(!snap["available_models"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn gateway_get_requires_auth() {
        let registry = make_registry();
        let state = make_state(vec!["secret".into()], empty_connections());
        let (status, _) = send_with_body(
            gateway_router(state, registry),
            "GET",
            "/api/gateway?token=wrong",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
