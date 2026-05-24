use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use shared::MeshMessage;
use std::sync::Arc;

use super::state::DashboardState;

#[derive(Deserialize)]
pub struct TokenQuery {
    #[serde(default)]
    token: String,
}

#[derive(Deserialize)]
pub struct SetIntervalBody {
    secs: u64,
}

pub async fn set_heartbeat_interval(
    Path(node_id): Path<String>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetIntervalBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if body.secs == 0 || body.secs > 3600 {
        return (StatusCode::BAD_REQUEST, "secs must be between 1 and 3600").into_response();
    }
    let sent = state.send_to_node(
        &node_id,
        MeshMessage::SetHeartbeatInterval { secs: body.secs },
    );
    if sent {
        StatusCode::OK.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::state::NodeConnections;
    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::routing::post;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    fn make_state(tokens: Vec<String>, connections: NodeConnections) -> Arc<DashboardState> {
        DashboardState::new(Arc::new(tokens), connections)
    }

    fn empty_connections() -> NodeConnections {
        Arc::new(Mutex::new(HashMap::new()))
    }

    async fn post_interval(
        state: Arc<DashboardState>,
        node_id: &str,
        token: &str,
        body: &str,
    ) -> StatusCode {
        let router: Router = Router::new()
            .route(
                "/api/nodes/{id}/heartbeat-interval",
                post(set_heartbeat_interval),
            )
            .with_state(state);

        let uri = format!("/api/nodes/{node_id}/heartbeat-interval?token={token}");
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let _ = to_bytes(resp.into_body(), usize::MAX).await;
        status
    }

    #[tokio::test]
    async fn set_interval_ok_queues_message() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("node1".into(), tx);
        let state = make_state(vec![], connections);

        let status = post_interval(state, "node1", "", r#"{"secs":30}"#).await;
        assert_eq!(status, StatusCode::OK);

        match rx.try_recv().unwrap() {
            MeshMessage::SetHeartbeatInterval { secs } => assert_eq!(secs, 30),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_interval_returns_404_for_unknown_node() {
        let state = make_state(vec![], empty_connections());
        let status = post_interval(state, "ghost-node", "", r#"{"secs":30}"#).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_interval_returns_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = post_interval(state, "node1", "wrong", r#"{"secs":30}"#).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn set_interval_returns_401_for_missing_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = post_interval(state, "node1", "", r#"{"secs":30}"#).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn set_interval_returns_400_for_zero_secs() {
        let state = make_state(vec![], empty_connections());
        let status = post_interval(state, "node1", "", r#"{"secs":0}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn set_interval_returns_400_for_oversized_secs() {
        let state = make_state(vec![], empty_connections());
        let status = post_interval(state, "node1", "", r#"{"secs":3601}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn set_interval_accepts_boundary_values() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("n".into(), tx);
        let state = make_state(vec![], connections);

        // secs=1 (lower boundary)
        assert_eq!(
            post_interval(state.clone(), "n", "", r#"{"secs":1}"#).await,
            StatusCode::OK
        );
        // secs=3600 (upper boundary)
        assert_eq!(
            post_interval(state, "n", "", r#"{"secs":3600}"#).await,
            StatusCode::OK
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            MeshMessage::SetHeartbeatInterval { secs: 1 }
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            MeshMessage::SetHeartbeatInterval { secs: 3600 }
        ));
    }

    #[tokio::test]
    async fn set_interval_returns_422_for_missing_body() {
        let state = make_state(vec![], empty_connections());
        let status = post_interval(state, "node1", "", "").await;
        // axum 0.8 returns 400 Bad Request for an empty/unparseable JSON body
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn set_interval_accepted_with_correct_token() {
        let connections = empty_connections();
        let (tx, _rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("node1".into(), tx);
        let state = make_state(vec!["correct".into()], connections);

        let status = post_interval(state, "node1", "correct", r#"{"secs":10}"#).await;
        assert_eq!(status, StatusCode::OK);
    }
}
