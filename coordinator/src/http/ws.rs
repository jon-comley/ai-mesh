use axum::extract::ws::{Message, WebSocket};
use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::debug;

use super::state::{DashboardEvent, DashboardState};

#[derive(Deserialize)]
pub struct WsQuery {
    #[serde(default)]
    token: String,
}

pub async fn ws_handler(
    Query(q): Query<WsQuery>,
    State(state): State<Arc<DashboardState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

async fn handle_socket(mut socket: WebSocket, state: Arc<DashboardState>) {
    let mut rx: broadcast::Receiver<DashboardEvent> = state.tx.subscribe();
    debug!("dashboard WebSocket client connected");

    // Push current health windows so sparklines populate immediately on connect.
    for (node_id, samples) in state.get_all_health_snapshots() {
        if samples.is_empty() {
            continue;
        }
        let evt = DashboardEvent::HealthUpdate { node_id, samples };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot HealthUpdate: {e}"),
        }
    }

    // Push current model snapshot so the Models panel populates immediately on connect.
    let model_nodes = state.get_model_snapshot();
    if !model_nodes.is_empty() {
        let evt = DashboardEvent::ModelUpdate { nodes: model_nodes };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot ModelUpdate: {e}"),
        }
    }

    // Push current lighting snapshot so the Lighting panel populates immediately on connect.
    let light_devices = state.get_light_snapshot();
    if !light_devices.is_empty() {
        let evt = DashboardEvent::LightingUpdate {
            devices: light_devices,
        };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot LightingUpdate: {e}"),
        }
    }

    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(evt) => {
                    match serde_json::to_string(&evt) {
                        Ok(json) => {
                            if socket.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            debug!("failed to serialise DashboardEvent: {e}");
                            continue;
                        }
                    }
                }
                // Receiver fell behind — skip missed events and continue.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    debug!("dashboard WS receiver lagged by {n} events");
                    continue;
                }
                // Channel closed — coordinator shutting down.
                Err(_) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None => break,
                _ => {}
            }
        }
    }

    debug!("dashboard WebSocket client disconnected");
}
