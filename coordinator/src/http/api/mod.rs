//! Dashboard/control HTTP API, split by domain.
//!
//! **Adding a device domain** (aircon, blinds, sensors, …): create a new
//! sibling module parallel to [`lights`] with its own command primitives and
//! handlers, register its routes in `http/mod.rs`, and attach devices to
//! rooms through the existing room-membership APIs. [`rooms`] is the
//! domain-agnostic spatial container and must never gain device-specific
//! logic; [`scenes`] and [`effects`] compose devices across domains.
//!
//! Every handler takes `_: Authed` (see `http/auth.rs`) — auth is part of
//! the handler's type. The deliberately public routes are
//! `rooms::solar_config` and `voice::serve_clip`, each justified at its
//! registration site.

pub mod art;
pub mod chat;
pub mod effects;
pub mod gateway;
pub mod lights;
pub mod model_search;
pub mod nodes;
pub mod prefs;
pub mod rooms;
pub mod scenes;
pub mod sensors;
pub mod switch_bindings;
pub mod voice;
pub mod zigbee;

fn gen_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
pub(crate) mod test_util {
    use crate::http::state::{DashboardState, NodeConnections};
    use crate::registry::Registry;
    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use shared::LightStateReport;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    pub(crate) fn make_state(
        tokens: Vec<String>,
        connections: NodeConnections,
    ) -> Arc<DashboardState> {
        DashboardState::new(Arc::new(tokens), connections)
    }

    pub(crate) fn empty_connections() -> NodeConnections {
        Arc::new(Mutex::new(HashMap::new()))
    }

    pub(crate) fn make_registry() -> Arc<Mutex<Registry>> {
        Arc::new(Mutex::new(Registry::new()))
    }

    pub(crate) fn make_room(registry: &Arc<Mutex<Registry>>, name: &str) -> String {
        registry.lock().unwrap().create_room(name).id
    }

    /// Record a light state report so a device exists in the dashboard snapshot.
    pub(crate) fn seed_light(state: &Arc<DashboardState>, device_id: &str, node_id: &str) {
        state.push_lighting_update(LightStateReport {
            node_id: node_id.into(),
            device_id: device_id.into(),
            on: false,
            brightness: Some(200),
            color_xy: None,
            color_temp: Some(370),
            online: true,
        });
    }

    pub(crate) async fn send(router: Router, method: &str, uri: &str, body: &str) -> StatusCode {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let _ = to_bytes(resp.into_body(), usize::MAX).await;
        status
    }

    pub(crate) async fn send_with_body(
        router: Router,
        method: &str,
        uri: &str,
        body: &str,
    ) -> (StatusCode, String) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }
}
