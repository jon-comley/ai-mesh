mod api;
mod auth;
mod openai;
pub mod state;
mod ws;

use axum::{
    Router,
    http::{StatusCode, header},
    response::Html,
    routing::{delete, get, patch, post, put},
};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tracing::{error, info};

use crate::effects::registry::EffectRegistry;
use crate::registry::Registry;
use state::DashboardState;

const INDEX_HTML: &str = include_str!("static/index.html");
const STYLE_CSS: &str = include_str!("static/style.css");
const DASHBOARD_JS: &str = include_str!("static/dashboard.js");
const TOPOLOGY_JS: &str = include_str!("static/topology.js");
const HEALTH_JS: &str = include_str!("static/health.js");
const MODELS_JS: &str = include_str!("static/models.js");
const ROOMS_JS: &str = include_str!("static/rooms.js");
const DEVICES_JS: &str = include_str!("static/devices.js");
const DEVICEWIDGETS_JS: &str = include_str!("static/devicewidgets.js");
const DRAG_JS: &str = include_str!("static/drag.js");
const COLORMATH_JS: &str = include_str!("static/colormath.js");
const CONTROLS_JS: &str = include_str!("static/controls.js");
const LIGHTCONTROLS_JS: &str = include_str!("static/lightcontrols.js");
const ACTIONS_JS: &str = include_str!("static/actions.js");
const EFFECTS_JS: &str = include_str!("static/effects.js");
const SCENES_JS: &str = include_str!("static/scenes.js");
const ERRORS_JS: &str = include_str!("static/errors.js");
const SECURITY_JS: &str = include_str!("static/security.js");
const CHAT_JS: &str = include_str!("static/chat.js");
const SOLAR_JS: &str = include_str!("static/solar.js");
const LAYOUTSTATE_JS: &str = include_str!("static/layoutstate.js");
const LAYOUT3D_JS: &str = include_str!("static/layout3d.js");
const SUNMODELS_JS: &str = include_str!("static/sunmodels.js");
const UTIL_JS: &str = include_str!("static/util.js");
const API_JS: &str = include_str!("static/api.js");
const STATE_JS: &str = include_str!("static/state.js");
const INDICATORS_JS: &str = include_str!("static/indicators.js");
const LAYOUT_JS: &str = include_str!("static/layout.js");
const PREFS_JS: &str = include_str!("static/prefs.js");
const REAPER_JS: &str = include_str!("static/reaper.js");
const GATEWAY_JS: &str = include_str!("static/gateway.js");
const MANIFEST_JSON: &str = include_str!("static/manifest.json");
const SERVICE_WORKER_JS: &str = include_str!("static/service-worker.js");

pub fn router(
    dashboard: Arc<DashboardState>,
    registry: Arc<Mutex<Registry>>,
    effects: Arc<EffectRegistry>,
) -> Router {
    Router::new()
        .route("/ws", get(ws::ws_handler))
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .merge(static_asset_routes())
        .route(
            "/api/nodes/{id}/heartbeat-interval",
            post(api::nodes::set_heartbeat_interval),
        )
        .route("/api/models/load", post(api::nodes::load_model))
        .route("/api/models/unload", post(api::nodes::unload_model))
        .route("/api/nodes/{id}", delete(api::nodes::remove_node))
        .route("/api/lights/names", get(api::lights::get_device_names))
        .route("/api/sensors", get(api::sensors::list_sensors))
        .route("/api/zigbee/permit-join", post(api::zigbee::permit_join))
        .route(
            "/api/lights/{device}/command",
            post(api::lights::light_command),
        )
        .route(
            "/api/lights/{device}/name",
            patch(api::lights::rename_device),
        )
        .route("/api/lights/{device}", delete(api::lights::delete_device))
        .route(
            "/api/lights/{device}/position",
            get(api::lights::get_light_position).post(api::lights::update_light_position),
        )
        .route(
            "/api/lights/group/{group}/command",
            post(api::lights::group_light_command),
        )
        .route("/api/rooms", post(api::rooms::create_room))
        .route("/api/rooms/reorder", post(api::rooms::reorder_rooms))
        .route("/api/rooms/{id}", delete(api::rooms::delete_room))
        .route("/api/rooms/{id}/name", patch(api::rooms::rename_room))
        .route(
            "/api/rooms/{id}/devices",
            patch(api::rooms::modify_room_devices),
        )
        .route(
            "/api/rooms/{id}/devices/reorder",
            post(api::rooms::reorder_room_devices),
        )
        .route(
            "/api/rooms/{id}/positions",
            get(api::rooms::get_room_positions),
        )
        .route(
            "/api/rooms/{id}/openings",
            get(api::rooms::list_openings).post(api::rooms::create_opening),
        )
        .route(
            "/api/rooms/{id}/openings/{oid}",
            patch(api::rooms::update_opening).delete(api::rooms::delete_opening),
        )
        .route("/api/effects", get(api::effects::list_effects))
        .route(
            "/api/rooms/{id}/effect",
            post(api::effects::set_room_effect).delete(api::effects::clear_room_effect),
        )
        .route(
            "/api/rooms/{id}/effect/override",
            patch(api::effects::patch_effect_override),
        )
        .route(
            "/api/rooms/{id}/orientation",
            patch(api::rooms::set_room_orientation),
        )
        .route("/api/rooms/{id}/origin", patch(api::rooms::set_room_origin))
        .route(
            "/api/rooms/{id}/dimensions",
            patch(api::rooms::set_room_dimensions),
        )
        .route("/api/rooms/{id}/command", post(api::rooms::room_command))
        .route(
            "/api/rooms/{id}/groups",
            post(api::rooms::create_room_group),
        )
        .route(
            "/api/rooms/{id}/groups/{gid}/name",
            patch(api::rooms::rename_room_group),
        )
        .route(
            "/api/rooms/{id}/groups/{gid}",
            delete(api::rooms::delete_room_group),
        )
        .route(
            "/api/rooms/{id}/groups/{gid}/command",
            post(api::rooms::group_command),
        )
        .route(
            "/api/rooms/{id}/devices/{did}/group",
            patch(api::rooms::set_device_group),
        )
        // PUBLIC — no Authed: the dashboard's solar calculator fetches this
        // before any token is entered; it exposes only lat/lon. Every other
        // /api/* handler must take `_: Authed` (see http/auth.rs).
        .route("/api/solar/config", get(api::rooms::solar_config))
        .route("/api/scenes", post(api::scenes::save_scene))
        .route("/api/scenes/reorder", post(api::scenes::reorder_scenes))
        .route("/api/scenes/{id}/recall", post(api::scenes::recall_scene))
        .route("/api/scenes/{id}", delete(api::scenes::delete_scene))
        .route("/api/chat", post(api::chat::chat))
        .route("/v1/chat/completions", post(openai::chat_completions))
        .route("/v1/models", get(openai::list_models))
        .route(
            "/api/gateway",
            get(api::gateway::get_gateway).post(api::gateway::set_gateway),
        )
        .route("/api/gateway/test", post(api::gateway::test_gateway))
        .route("/api/preferences", get(api::prefs::get_preferences))
        .route(
            "/api/preferences/{key}",
            put(api::prefs::set_preference).delete(api::prefs::delete_preference),
        )
        .route("/api/reaper/state", get(api::nodes::get_reaper_state))
        .layer(axum::Extension(registry))
        .layer(axum::Extension(effects))
        .with_state(dashboard)
}

// All embedded static assets, served with their MIME type and a no-cache header.
// One table instead of a dozen near-identical route closures — add a new asset
// by adding a row. (Kept generic over the router state so it merges into router().)
fn static_asset_routes() -> Router<Arc<DashboardState>> {
    const JS: &str = "application/javascript; charset=utf-8";
    const ASSETS: &[(&str, &str, &str)] = &[
        ("/static/style.css", STYLE_CSS, "text/css; charset=utf-8"),
        ("/static/dashboard.js", DASHBOARD_JS, JS),
        ("/static/topology.js", TOPOLOGY_JS, JS),
        ("/static/health.js", HEALTH_JS, JS),
        ("/static/models.js", MODELS_JS, JS),
        ("/static/rooms.js", ROOMS_JS, JS),
        ("/static/devices.js", DEVICES_JS, JS),
        ("/static/devicewidgets.js", DEVICEWIDGETS_JS, JS),
        ("/static/drag.js", DRAG_JS, JS),
        ("/static/colormath.js", COLORMATH_JS, JS),
        ("/static/controls.js", CONTROLS_JS, JS),
        ("/static/lightcontrols.js", LIGHTCONTROLS_JS, JS),
        ("/static/actions.js", ACTIONS_JS, JS),
        ("/static/effects.js", EFFECTS_JS, JS),
        ("/static/scenes.js", SCENES_JS, JS),
        ("/static/errors.js", ERRORS_JS, JS),
        ("/static/security.js", SECURITY_JS, JS),
        ("/static/chat.js", CHAT_JS, JS),
        ("/static/solar.js", SOLAR_JS, JS),
        ("/static/layoutstate.js", LAYOUTSTATE_JS, JS),
        ("/static/layout3d.js", LAYOUT3D_JS, JS),
        ("/static/sunmodels.js", SUNMODELS_JS, JS),
        ("/static/util.js", UTIL_JS, JS),
        ("/static/api.js", API_JS, JS),
        ("/static/state.js", STATE_JS, JS),
        ("/static/indicators.js", INDICATORS_JS, JS),
        ("/static/layout.js", LAYOUT_JS, JS),
        ("/static/prefs.js", PREFS_JS, JS),
        ("/static/reaper.js", REAPER_JS, JS),
        ("/static/gateway.js", GATEWAY_JS, JS),
        ("/manifest.json", MANIFEST_JSON, "application/manifest+json"),
        ("/service-worker.js", SERVICE_WORKER_JS, JS),
    ];
    let mut r = Router::new();
    for &(path, body, mime) in ASSETS {
        r = r.route(
            path,
            get(move || async move {
                (
                    [
                        (header::CONTENT_TYPE, mime),
                        (header::CACHE_CONTROL, "no-cache"),
                    ],
                    body,
                )
            }),
        );
    }
    r
}

pub async fn start(
    port: u16,
    dashboard: Arc<DashboardState>,
    registry: Arc<Mutex<Registry>>,
    effects: Arc<EffectRegistry>,
) {
    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("failed to bind dashboard HTTP server to {addr}: {e}");
            return;
        }
    };
    info!("Dashboard available at http://localhost:{port}");
    if let Err(e) = axum::serve(listener, router(dashboard, registry, effects)).await {
        error!("Dashboard HTTP server error: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn get(uri: &str) -> (StatusCode, String) {
        let dashboard = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let registry = Arc::new(std::sync::Mutex::new(crate::registry::Registry::new()));
        let effects = Arc::new(EffectRegistry::default());
        let resp = router(dashboard, registry, effects)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        // Drain the body so the connection closes cleanly.
        let _ = to_bytes(resp.into_body(), usize::MAX).await;
        (status, ct)
    }

    #[tokio::test]
    async fn index_returns_200() {
        let (status, ct) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("text/html"), "expected text/html, got {ct}");
    }

    #[tokio::test]
    async fn css_returns_correct_content_type() {
        let (status, ct) = get("/static/style.css").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("text/css"), "expected text/css, got {ct}");
    }

    #[tokio::test]
    async fn dashboard_js_returns_correct_content_type() {
        let (status, ct) = get("/static/dashboard.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("javascript"),
            "expected application/javascript, got {ct}"
        );
    }

    #[tokio::test]
    async fn health_js_returns_correct_content_type() {
        let (status, ct) = get("/static/health.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("javascript"),
            "expected application/javascript, got {ct}"
        );
    }

    #[tokio::test]
    async fn models_js_returns_correct_content_type() {
        let (status, ct) = get("/static/models.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("javascript"),
            "expected application/javascript, got {ct}"
        );
    }

    #[tokio::test]
    async fn devices_js_returns_correct_content_type() {
        let (status, ct) = get("/static/devices.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("javascript"),
            "expected application/javascript, got {ct}"
        );
    }

    #[tokio::test]
    async fn rooms_js_returns_correct_content_type() {
        let (status, ct) = get("/static/rooms.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("javascript"),
            "expected application/javascript, got {ct}"
        );
    }

    #[tokio::test]
    async fn layout_js_returns_correct_content_type() {
        let (status, ct) = get("/static/layout.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("javascript"),
            "expected application/javascript, got {ct}"
        );
    }

    #[tokio::test]
    async fn manifest_returns_correct_content_type() {
        let (status, ct) = get("/manifest.json").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("manifest+json"),
            "expected application/manifest+json, got {ct}"
        );
    }

    #[tokio::test]
    async fn service_worker_returns_correct_content_type() {
        // Browsers silently reject service workers with a non-JS content-type.
        let (status, ct) = get("/service-worker.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("javascript"),
            "expected application/javascript, got {ct}"
        );
    }

    // ── WebSocket endpoint ────────────────────────────────────────────────────
    // Token auth logic is unit-tested via DashboardState::auth_ok() in state::tests.
    // A 401 rejection test would require a real HTTP/1.1 connection (axum's
    // WebSocketUpgrade extractor returns 426 in oneshot — no upgradeable conn).
    // Live auth rejection is verified by `just chaos` (scenario 7).

    #[tokio::test]
    async fn ws_endpoint_rejects_non_upgrade_request() {
        // Without WS upgrade headers the extractor returns 400 Bad Request.
        let (status, _) = get("/ws").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
