//! Zigbee bridge administration — bridge-wide operations that belong to no
//! single device domain (pairing is not device-type specific: permit-join
//! accepts whatever announces). Commands route to the node that owns the
//! bridge (tracked via its DeviceList/ZigbeeStatus reports).

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use shared::{MeshMessage, PermitJoinRequest};
use std::sync::Arc;

use super::gen_request_id;
use crate::http::auth::Authed;
use crate::http::state::DashboardState;

#[derive(Deserialize, Default)]
pub struct PermitJoinBody {
    /// Pairing-window length; z2m caps at 254 s (also the default).
    #[serde(default)]
    seconds: Option<u16>,
}

/// POST /api/zigbee/permit-join — open the bridge-wide pairing window.
/// Feedback streams to the dashboard as ZigbeeJoinEvent WS events.
/// Body is optional (same lenient pattern as scene recall).
pub async fn permit_join(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let body: PermitJoinBody = {
        let bytes = axum::body::to_bytes(req.into_body(), 4096)
            .await
            .unwrap_or_default();
        if bytes.is_empty() {
            PermitJoinBody::default()
        } else {
            serde_json::from_slice(&bytes).unwrap_or_default()
        }
    };
    let Some(node_id) = state.get_zigbee_node() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no zigbee node connected").into_response();
    };
    let seconds = body.seconds.unwrap_or(254).min(254) as u8;
    let req = PermitJoinRequest {
        request_id: gen_request_id(),
        seconds,
    };
    if state.send_to_node(&node_id, MeshMessage::PermitJoin(req)) {
        Json(serde_json::json!({ "seconds": seconds })).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "zigbee node not reachable").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::post;
    use tokio::sync::mpsc;

    fn zigbee_router(state: Arc<DashboardState>) -> Router {
        Router::new()
            .route("/api/zigbee/permit-join", post(permit_join))
            .with_state(state)
    }

    #[tokio::test]
    async fn permit_join_sends_to_zigbee_node() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        state.set_zigbee_node("pi1");

        let status = send(zigbee_router(state), "POST", "/api/zigbee/permit-join", "").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        match rx.try_recv().unwrap() {
            MeshMessage::PermitJoin(req) => assert_eq!(req.seconds, 254),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn permit_join_clamps_seconds() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        state.set_zigbee_node("pi1");

        let status = send(
            zigbee_router(state),
            "POST",
            "/api/zigbee/permit-join",
            r#"{"seconds":9999}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        match rx.try_recv().unwrap() {
            MeshMessage::PermitJoin(req) => assert_eq!(req.seconds, 254),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn permit_join_503_without_zigbee_node() {
        let state = make_state(vec![], empty_connections());
        let status = send(zigbee_router(state), "POST", "/api/zigbee/permit-join", "").await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn permit_join_503_when_tracked_node_disconnected() {
        // A node reported the bridge earlier but its connection is gone.
        let state = make_state(vec![], empty_connections());
        state.set_zigbee_node("pi1");
        let status = send(zigbee_router(state), "POST", "/api/zigbee/permit-join", "").await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn permit_join_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        state.set_zigbee_node("pi1");
        let status = send(
            zigbee_router(state),
            "POST",
            "/api/zigbee/permit-join?token=wrong",
            "",
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    }
}
