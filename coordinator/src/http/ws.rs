use axum::extract::ws::{Message, WebSocket};
use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tracing::debug;

use super::state::{DashboardEvent, DashboardState, SecurityEvent, SecurityEventKind};

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
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let auth_detail = if state.auth_tokens.is_empty() {
        "no auth (dev)".into()
    } else {
        "token auth".into()
    };
    state.push_security(SecurityEvent {
        ts_ms,
        kind: SecurityEventKind::DashboardConnect,
        source: "dashboard".into(),
        detail: auth_detail,
    });
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
    let light_groups = state.get_group_snapshot();
    if !light_devices.is_empty() || !light_groups.is_empty() {
        let evt = DashboardEvent::LightingUpdate {
            devices: light_devices,
            groups: light_groups,
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

    // Push current sensor snapshot so sensor readouts populate immediately on connect.
    let sensors = state.get_sensor_snapshot();
    if !sensors.is_empty() {
        let evt = DashboardEvent::SensorUpdate { sensors };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot SensorUpdate: {e}"),
        }
    }

    // Push current rooms snapshot so the Rooms panel populates immediately on connect.
    let rooms = state.get_room_snapshot();
    if !rooms.is_empty() {
        let evt = DashboardEvent::RoomsUpdate {
            rooms,
            device_names: std::collections::HashMap::new(),
        };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot RoomsUpdate: {e}"),
        }
    }

    // Push current scenes snapshot so the Scenes sections populate immediately on connect.
    let scenes = state.get_scene_snapshot();
    if !scenes.is_empty() {
        let evt = DashboardEvent::ScenesUpdate { scenes };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot ScenesUpdate: {e}"),
        }
    }

    // Push current per-room active effect so the effect badge UX renders
    // immediately on connect instead of waiting for the next runner tick.
    for info in state.get_effect_snapshot() {
        let evt = DashboardEvent::EffectUpdate {
            room_id: info.room_id,
            effect_id: info.effect_id,
            params: info.params,
            overrides: info.overrides,
        };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot EffectUpdate: {e}"),
        }
    }

    // Push the current zigbee bridge status so a dashboard that connects while
    // the bridge is already down shows the offline banner immediately — the
    // ZigbeeStatus event is otherwise only broadcast on change.
    {
        let evt = DashboardEvent::ZigbeeStatus {
            online: state.get_zigbee_status(),
        };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot ZigbeeStatus: {e}"),
        }
    }

    // Push error log snapshot so the Errors panel populates immediately on connect.
    let errors = state.get_error_snapshot();
    if !errors.is_empty() {
        let evt = DashboardEvent::ErrorUpdate { errors };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot ErrorUpdate: {e}"),
        }
    }

    // Push security log snapshot so the Security panel populates immediately on connect.
    let events = state.get_security_snapshot();
    if !events.is_empty() {
        let evt = DashboardEvent::SecurityUpdate { events };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot SecurityUpdate: {e}"),
        }
    }

    // Push REAPER status snapshot so the REAPER tab populates immediately on connect.
    // Only push when online — avoids misleading the client with a stale offline state.
    if let Some(snap) = state.get_reaper_snapshot()
        && snap.reaper_online
    {
        let evt = DashboardEvent::ReaperUpdate {
            online: snap.reaper_online,
            play_state: snap.play_state,
            position: snap.position,
            tempo: snap.tempo,
            ts_num: snap.ts_num,
            ts_denom: snap.ts_denom,
            last_command: None,
        };
        match serde_json::to_string(&evt) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(e) => debug!("failed to serialise snapshot ReaperUpdate: {e}"),
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
                // A persistent recv error returns immediately every poll; without
                // breaking, the select! loop would spin at 100% CPU. Treat it as a
                // disconnect.
                Some(Err(e)) => {
                    debug!("dashboard WS read error: {e}");
                    break;
                }
                Some(Ok(_)) => {}
            }
        }
    }

    debug!("dashboard WebSocket client disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn make_state(tokens: Vec<String>) -> Arc<DashboardState> {
        DashboardState::new(
            Arc::new(tokens),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        )
    }

    /// Bind an ephemeral server, run `f` against its address, then return the
    /// raw HTTP response status line (e.g. "HTTP/1.1 401 Unauthorized").
    async fn ws_status(tokens: Vec<String>, path_and_query: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let state = make_state(tokens);
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(state);

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET {path_and_query} HTTP/1.1\r\n\
             Host: localhost\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 512];
        let n = stream.read(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf[..n]);
        response.lines().next().unwrap_or("").to_string()
    }

    /// A wrong dashboard token must return 401 before the WS upgrade happens.
    #[tokio::test]
    async fn ws_handler_rejects_wrong_token() {
        let status = ws_status(vec!["secret".into()], "/ws?token=definitely-wrong").await;
        assert!(status.contains("401"), "expected 401, got: {status}");
    }

    /// During token rotation both old and new tokens must be accepted.
    /// A client that connects with the old token gets a 101 upgrade, not 401.
    #[tokio::test]
    async fn ws_handler_accepts_both_tokens_during_rotation() {
        let tokens = vec!["old-token".into(), "new-token".into()];

        let status_old = ws_status(tokens.clone(), "/ws?token=old-token").await;
        assert!(
            status_old.contains("101"),
            "old token: expected 101, got: {status_old}"
        );

        let status_new = ws_status(tokens, "/ws?token=new-token").await;
        assert!(
            status_new.contains("101"),
            "new token: expected 101, got: {status_new}"
        );
    }

    /// An expired token (not in the rotation window) is rejected even when a
    /// rotation is active.
    #[tokio::test]
    async fn ws_handler_rejects_expired_token_during_rotation() {
        let tokens = vec!["old-token".into(), "new-token".into()];
        let status = ws_status(tokens, "/ws?token=expired-token").await;
        assert!(status.contains("401"), "expected 401, got: {status}");
    }
}
