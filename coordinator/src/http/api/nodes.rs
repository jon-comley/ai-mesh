//! Node/agent control: heartbeat cadence, model load/unload, REAPER status.

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use shared::{MeshMessage, ModelLoadRequest, ModelUnloadRequest, WIRE_VERSION};
use std::sync::{Arc, Mutex};

use crate::http::state::DashboardState;
use crate::registry::Registry;

use super::gen_request_id;
use crate::http::auth::Authed;

#[derive(Deserialize)]
pub struct SetIntervalBody {
    secs: u64,
}

pub async fn set_heartbeat_interval(
    Path(node_id): Path<String>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetIntervalBody>,
) -> impl IntoResponse {
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

#[derive(Deserialize)]
pub struct LoadModelBody {
    node_id: String,
    model_name: String,
    size_mb: u64,
}

#[derive(Deserialize)]
pub struct UnloadModelBody {
    node_id: String,
    model_name: String,
}

pub async fn load_model(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<LoadModelBody>,
) -> impl IntoResponse {
    if body.node_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "node_id must not be empty").into_response();
    }
    if body.model_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "model_name must not be empty").into_response();
    }
    if body.size_mb == 0 {
        return (StatusCode::BAD_REQUEST, "size_mb must be greater than 0").into_response();
    }
    let LoadModelBody {
        node_id,
        model_name,
        size_mb,
    } = body;
    let req = ModelLoadRequest {
        request_id: gen_request_id(),
        node_id: Some(node_id.clone()),
        model_name,
        model_size_mb: size_mb,
        wire_version: WIRE_VERSION,
    };
    if state.send_to_node(&node_id, MeshMessage::ModelLoad(req)) {
        StatusCode::OK.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

pub async fn unload_model(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<UnloadModelBody>,
) -> impl IntoResponse {
    if body.node_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "node_id must not be empty").into_response();
    }
    if body.model_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "model_name must not be empty").into_response();
    }
    let UnloadModelBody {
        node_id,
        model_name,
    } = body;
    let req = ModelUnloadRequest {
        request_id: gen_request_id(),
        node_id: node_id.clone(),
        model_name,
        wire_version: WIRE_VERSION,
    };
    if state.send_to_node(&node_id, MeshMessage::ModelUnload(req)) {
        StatusCode::OK.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Remove a dead node from the registry. Nodes never expire on their own, so
/// a decommissioned or renamed node sits in the dashboard forever without
/// this. Refuses (409) while the node's TCP connection is live — stop the
/// agent first; a connected node would just re-register on its next
/// heartbeat anyway.
pub async fn remove_node(
    Path(node_id): Path<String>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    if state.connections.lock().unwrap().contains_key(&node_id) {
        return (
            StatusCode::CONFLICT,
            "node is currently connected — stop its agent before removing",
        )
            .into_response();
    }
    let (removed, nodes) = {
        let mut reg = registry.lock().unwrap();
        (reg.remove_node(&node_id), reg.list_nodes())
    };
    if !removed {
        return StatusCode::NOT_FOUND.into_response();
    }
    tracing::info!(node_id = %node_id, "node removed from registry");
    state.push_topology(&nodes);
    StatusCode::NO_CONTENT.into_response()
}

pub async fn get_reaper_state(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    match state.get_reaper_snapshot() {
        Some(snap) => Json(serde_json::json!({
            "reaper_online": snap.reaper_online,
            "play_state": snap.play_state,
            "position": snap.position,
            "tempo": snap.tempo,
            "ts_num": snap.ts_num,
            "ts_denom": snap.ts_denom,
        }))
        .into_response(),
        None => Json(serde_json::json!({ "reaper_online": false })).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::post;
    use tokio::sync::mpsc;

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
        send(router, "POST", &uri, body).await
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

    // ── load_model / unload_model ─────────────────────────────────────────────

    async fn post_load(state: Arc<DashboardState>, token: &str, body: &str) -> StatusCode {
        let router: Router = Router::new()
            .route("/api/models/load", post(load_model))
            .with_state(state);
        send(
            router,
            "POST",
            &format!("/api/models/load?token={token}"),
            body,
        )
        .await
    }

    async fn post_unload(state: Arc<DashboardState>, token: &str, body: &str) -> StatusCode {
        let router: Router = Router::new()
            .route("/api/models/unload", post(unload_model))
            .with_state(state);
        send(
            router,
            "POST",
            &format!("/api/models/unload?token={token}"),
            body,
        )
        .await
    }

    #[tokio::test]
    async fn load_model_ok_queues_message() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("node1".into(), tx);
        let state = make_state(vec![], connections);

        let status = post_load(
            state,
            "",
            r#"{"node_id":"node1","model_name":"qwen2.5:7b","size_mb":4000}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        match rx.try_recv().unwrap() {
            MeshMessage::ModelLoad(req) => {
                assert_eq!(req.model_name, "qwen2.5:7b");
                assert_eq!(req.model_size_mb, 4000);
                assert_eq!(req.node_id, Some("node1".into()));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_model_returns_404_for_unknown_node() {
        let state = make_state(vec![], empty_connections());
        let status = post_load(
            state,
            "",
            r#"{"node_id":"ghost","model_name":"qwen2.5:7b","size_mb":4000}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn load_model_returns_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = post_load(
            state,
            "wrong",
            r#"{"node_id":"node1","model_name":"qwen2.5:7b","size_mb":4000}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn load_model_returns_400_for_missing_body() {
        let state = make_state(vec![], empty_connections());
        let status = post_load(state, "", "").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn load_model_returns_400_for_zero_size() {
        let state = make_state(vec![], empty_connections());
        let status = post_load(
            state,
            "",
            r#"{"node_id":"node1","model_name":"qwen2.5:7b","size_mb":0}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn load_model_returns_400_for_empty_node_id() {
        let state = make_state(vec![], empty_connections());
        let status = post_load(
            state,
            "",
            r#"{"node_id":"","model_name":"qwen2.5:7b","size_mb":4000}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unload_model_returns_400_for_empty_node_id() {
        let state = make_state(vec![], empty_connections());
        let status = post_unload(state, "", r#"{"node_id":"","model_name":"qwen2.5:7b"}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unload_model_ok_queues_message() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("node1".into(), tx);
        let state = make_state(vec![], connections);

        let status = post_unload(
            state,
            "",
            r#"{"node_id":"node1","model_name":"qwen2.5:7b"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        match rx.try_recv().unwrap() {
            MeshMessage::ModelUnload(req) => assert_eq!(req.model_name, "qwen2.5:7b"),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn unload_model_returns_404_for_unknown_node() {
        let state = make_state(vec![], empty_connections());
        let status = post_unload(
            state,
            "",
            r#"{"node_id":"ghost","model_name":"qwen2.5:7b"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unload_model_returns_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = post_unload(
            state,
            "wrong",
            r#"{"node_id":"node1","model_name":"qwen2.5:7b"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // ── remove_node ───────────────────────────────────────────────────────────

    fn remove_router(
        state: Arc<DashboardState>,
        registry: Arc<Mutex<crate::registry::Registry>>,
    ) -> Router {
        Router::new()
            .route("/api/nodes/{id}", axum::routing::delete(remove_node))
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    #[tokio::test]
    async fn remove_node_deletes_disconnected_node() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        registry
            .lock()
            .unwrap()
            .update_heartbeat(shared::NodeIdentity {
                id: "dead-node".into(),
                hostname: "old-host".into(),
                ip: "10.0.0.18".into(),
                role: shared::NodeRole::Compute,
            });

        let status = send(
            remove_router(state, registry.clone()),
            "DELETE",
            "/api/nodes/dead-node",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(
            registry
                .lock()
                .unwrap()
                .get_node_full("dead-node")
                .is_none()
        );
    }

    #[tokio::test]
    async fn remove_node_unknown_returns_404() {
        let state = make_state(vec![], empty_connections());
        let status = send(
            remove_router(state, make_registry()),
            "DELETE",
            "/api/nodes/ghost",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn remove_node_connected_returns_409() {
        let connections = empty_connections();
        let (tx, _rx) = mpsc::channel::<MeshMessage>(1);
        connections.lock().unwrap().insert("live-node".into(), tx);
        let state = make_state(vec![], connections);
        let registry = make_registry();
        registry
            .lock()
            .unwrap()
            .update_heartbeat(shared::NodeIdentity {
                id: "live-node".into(),
                hostname: "host".into(),
                ip: "10.0.0.1".into(),
                role: shared::NodeRole::Compute,
            });

        let status = send(
            remove_router(state, registry.clone()),
            "DELETE",
            "/api/nodes/live-node",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            registry
                .lock()
                .unwrap()
                .get_node_full("live-node")
                .is_some()
        );
    }

    #[tokio::test]
    async fn remove_node_requires_auth() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            remove_router(state, make_registry()),
            "DELETE",
            "/api/nodes/x",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
