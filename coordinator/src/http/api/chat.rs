//! Dashboard chat: the tool-calling intent pipeline over HTTP.

use axum::{Extension, Json, extract::State, response::IntoResponse};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

use crate::http::state::DashboardState;
use crate::registry::Registry;

use super::gateway::gateway_snapshot;
use super::gen_request_id;
use crate::http::auth::Authed;

// ── Chat / inference ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ChatRequest {
    pub text: String,
    pub model_name: Option<String>,
    #[serde(default)]
    pub context: Vec<shared::IntentTurn>,
}

pub async fn chat(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Json(body): Json<ChatRequest>,
) -> impl IntoResponse {
    let request_id = gen_request_id();
    let req = shared::IntentRequest {
        request_id,
        text: body.text,
        model_name: body.model_name,
        context: body.context,
    };

    // Build the cloud gateway invocation when enabled + fully configured.
    let gateway = {
        let cfg = crate::cloud::GatewayConfig::load(&registry.lock().unwrap());
        match cfg.provider() {
            Some(provider) if cfg.enabled => Some(crate::cloud::GatewayInvocation {
                provider,
                engine: cfg.engine,
                compress: cfg.compress,
                state: state.clone(),
            }),
            _ => {
                if cfg.enabled {
                    tracing::warn!(
                        "online AI is enabled but not fully configured (missing API key or model); using local inference"
                    );
                }
                None
            }
        }
    };
    let used_gateway = gateway.is_some();

    let reaper_online = state.get_reaper_snapshot().is_some_and(|s| s.reaper_online);
    let resp = crate::intent::handle_intent(
        req,
        registry.clone(),
        state.connections.clone(),
        state.pending_inferences.clone(),
        state.pending_intents.clone(),
        state.get_light_snapshot(),
        state.get_sensor_snapshot(),
        reaper_online,
        gateway,
    )
    .await;

    // Reflect updated cumulative stats / last-error on the Gateway tab.
    if used_gateway {
        let snap = gateway_snapshot(&registry.lock().unwrap(), &state);
        state.push_gateway_update(snap);
    }

    Json(resp).into_response()
}
