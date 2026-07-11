//! Lighting — the first *device domain* module. Device-level command,
//! naming, and position handlers plus the lighting command primitives
//! (`LightCommandBody`, `build_light_action`) that rooms and scenes fan
//! out through. Future device domains (aircon, blinds, sensors) get
//! sibling modules shaped like this one.

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use shared::{LightAction, LightCommandRequest, LightStateReport, LightTarget, MeshMessage};
use std::sync::{Arc, Mutex};

use crate::http::state::{DashboardState, RoomInfo};
use crate::registry::Registry;

use super::gen_request_id;
use crate::http::auth::Authed;

#[derive(Deserialize)]
pub struct LightCommandBody {
    action: String,
    #[serde(default)]
    value: Option<f64>,
    #[serde(default)]
    x: Option<f32>,
    #[serde(default)]
    y: Option<f32>,
    #[serde(default)]
    transition_secs: Option<f32>,
}

// Transition-aware action constructors — the lighting-domain primitives.
// Every caller that turns typed values into a `LightAction` (the HTTP command
// body below, scene recall) goes through these, so the `t > 0.0` transition
// dispatch has exactly one owner.

pub(crate) fn brightness_action(value: u8, transition_secs: Option<f32>) -> LightAction {
    match transition_secs {
        Some(t) if t > 0.0 => LightAction::BrightnessTransition {
            value,
            transition_secs: t,
        },
        _ => LightAction::Brightness(value),
    }
}

pub(crate) fn color_temp_action(value: u16, transition_secs: Option<f32>) -> LightAction {
    match transition_secs {
        Some(t) if t > 0.0 => LightAction::ColorTempTransition {
            value,
            transition_secs: t,
        },
        _ => LightAction::ColorTemp(value),
    }
}

pub(crate) fn color_xy_action(x: f32, y: f32, transition_secs: Option<f32>) -> LightAction {
    match transition_secs {
        Some(t) if t > 0.0 => LightAction::ColorXYTransition {
            x,
            y,
            transition_secs: t,
        },
        _ => LightAction::ColorXY { x, y },
    }
}

/// Is `target` a device we know to be offline (Zigbee-level, not just the
/// node's mesh connection — see `LightStateReport::online`'s doc comment)?
/// An unknown target (never reported) is treated as online — let it
/// through and let the lighting node decide if it actually exists, rather
/// than guessing.
///
/// Shared by every light-command dispatch path — `intent.rs`'s voice/chat
/// tool already had this check; the single-device HTTP endpoint below
/// didn't, which meant clicking a light in the dashboard while its bulb was
/// offline silently "succeeded" (204, optimistic snapshot update) with no
/// feedback, while asking the assistant to do the same thing got a clear
/// "device is offline" answer. One shared implementation, not two that can
/// drift apart (same reasoning as `effects::exclude_device_from_its_active_effect`).
pub(crate) fn device_is_offline(target: &str, states: &[LightStateReport]) -> bool {
    states
        .iter()
        .find(|s| s.device_id == target)
        .is_some_and(|s| !s.online)
}

pub(crate) fn build_light_action(body: &LightCommandBody) -> Option<LightAction> {
    match body.action.as_str() {
        "on" => Some(LightAction::On),
        "off" => Some(LightAction::Off),
        "toggle" => Some(LightAction::Toggle),
        "brightness" => body
            .value
            .map(|v| brightness_action(v.clamp(0.0, 255.0) as u8, body.transition_secs)),
        "color_temp" => body
            .value
            .map(|v| color_temp_action(v.clamp(1.0, 65535.0) as u16, body.transition_secs)),
        "color_xy" => match (body.x, body.y) {
            (Some(x), Some(y)) => Some(color_xy_action(
                x.clamp(0.0, 1.0),
                y.clamp(0.0, 1.0),
                body.transition_secs,
            )),
            _ => None,
        },
        _ => None,
    }
}

pub async fn light_command(
    Path(device): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<LightCommandBody>,
) -> impl IntoResponse {
    let Some(node_id) = state.get_node_for_device(&device) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(command) = build_light_action(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            "unknown action or missing required fields",
        )
            .into_response();
    };
    if device_is_offline(&device, &state.get_light_snapshot()) {
        return (
            StatusCode::CONFLICT,
            format!("device '{device}' is currently offline"),
        )
            .into_response();
    }

    let cmd = LightCommandRequest {
        request_id: gen_request_id(),
        target: LightTarget::Device(device.clone()),
        command: command.clone(),
    };
    let sent = state.send_to_node(&node_id, MeshMessage::LightCommand(cmd));
    if sent {
        // Optimistically update the snapshot only after a successful send, so
        // subsequent broadcasts (triggered by any other device's status report)
        // carry the intended value rather than snapping UI sliders back — but a
        // failed send leaves no phantom value for a later broadcast to push out.
        state.apply_command_to_snapshot(&device, &command);
        // A manual command on a bulb its room's effect still owns would
        // otherwise get silently reverted on the effect's next tick — this
        // is the server-side equivalent of rooms.js's own excludeFromEffect()
        // call on a dashboard click (see the same call in intent.rs's
        // dispatch_light_command for the voice/chat tool's copy of this).
        super::effects::exclude_device_from_its_active_effect(&registry, Some(&state), &device);
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}

pub async fn group_light_command(
    Path(group): Path<String>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<LightCommandBody>,
) -> impl IntoResponse {
    let Some(node_id) = state.get_node_for_group(&group) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(command) = build_light_action(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            "unknown action or missing required fields",
        )
            .into_response();
    };
    let cmd = LightCommandRequest {
        request_id: gen_request_id(),
        target: LightTarget::Group(group),
        command,
    };
    if state.send_to_node(&node_id, MeshMessage::LightCommand(cmd)) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}

/// DELETE /api/lights/{id} — delete a device completely from the system.
pub async fn delete_device(
    Path(device_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    // Ask the bridge to unpair the device first — registry deletion alone is
    // cosmetic: a still-paired device re-announces and reappears on the next
    // bridge/devices publish. Best-effort: with the node offline we still
    // clean local records (the device returns when the node does, and the
    // user can delete again).
    let node = state
        .get_node_for_device(&device_id)
        .or_else(|| state.get_zigbee_node());
    match node {
        Some(node_id) => {
            let req = shared::DeviceRemoveRequest {
                request_id: gen_request_id(),
                device_id: device_id.clone(),
            };
            if !state.send_to_node(&node_id, MeshMessage::DeviceRemove(req)) {
                tracing::warn!(%device_id, "delete_device: zigbee node unreachable — removed locally only");
            }
        }
        None => {
            tracing::warn!(%device_id, "delete_device: no zigbee node known — removed locally only");
        }
    }
    registry.lock().unwrap().delete_device(&device_id);
    // Registry deletion alone is invisible to already-connected clients —
    // without this, the device keeps showing "Unassigned" forever (see
    // DashboardState::remove_device).
    state.remove_device(&device_id);
    StatusCode::NO_CONTENT.into_response()
}

// ── Device names ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RenameDeviceBody {
    name: String,
}

pub async fn rename_device(
    Path(device_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<RenameDeviceBody>,
) -> impl IntoResponse {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name must not be empty").into_response();
    }
    // One lock scope for the write and both reads, so the pushed snapshot is a
    // single consistent view — a concurrent handler can't interleave between the
    // rename and the names/rooms it's bundled with.
    let (names, rooms) = {
        let mut reg = registry.lock().unwrap();
        reg.set_device_name(&device_id, &name);
        let names = reg.get_all_device_names();
        let rooms: Vec<RoomInfo> = reg.list_rooms().into_iter().map(RoomInfo::from).collect();
        (names, rooms)
    };
    state.push_rooms_update_with_names(rooms, names);
    StatusCode::NO_CONTENT.into_response()
}

pub async fn get_device_names(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
) -> impl IntoResponse {
    let names = registry.lock().unwrap().get_all_device_names();
    Json(names).into_response()
}

// ── Spatial ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SetPositionBody {
    x: f32,
    y: f32,
    z: f32,
    room_id: Option<String>,
    fixture_type: Option<String>,
}

pub async fn get_light_position(
    Path(device_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
) -> impl IntoResponse {
    let pos = registry.lock().unwrap().get_light_position(&device_id);
    match pos {
        Some(p) => Json(serde_json::json!({
            "x": p.x,
            "y": p.y,
            "z": p.z,
            "room_id": p.room_id,
            "fixture_type": p.fixture_type,
        }))
        .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn update_light_position(
    Path(device_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    Json(body): Json<SetPositionBody>,
) -> impl IntoResponse {
    registry.lock().unwrap().update_light_position(
        &device_id,
        body.x,
        body.y,
        body.z,
        body.room_id,
        body.fixture_type,
    );
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::{get, post};
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    // ── action constructors ───────────────────────────────────────────────────

    #[test]
    fn action_constructors_dispatch_on_transition() {
        // No transition, zero, and negative all mean "immediate".
        for t in [None, Some(0.0), Some(-1.0)] {
            assert!(matches!(
                brightness_action(200, t),
                LightAction::Brightness(200)
            ));
            assert!(matches!(
                color_temp_action(370, t),
                LightAction::ColorTemp(370)
            ));
            assert!(matches!(
                color_xy_action(0.3, 0.4, t),
                LightAction::ColorXY { .. }
            ));
        }
        assert!(matches!(
            brightness_action(200, Some(1.5)),
            LightAction::BrightnessTransition {
                value: 200,
                transition_secs: t
            } if t == 1.5
        ));
        assert!(matches!(
            color_temp_action(370, Some(1.5)),
            LightAction::ColorTempTransition { value: 370, .. }
        ));
        assert!(matches!(
            color_xy_action(0.3, 0.4, Some(1.5)),
            LightAction::ColorXYTransition { .. }
        ));
    }

    // ── device_is_offline ────────────────────────────────────────────────────
    // Moved here from intent.rs: this check is shared by every light-command
    // dispatch path, not just the voice/chat tool (see the shared
    // `device_is_offline`'s doc comment).

    #[test]
    fn device_is_offline_returns_true_for_offline_device() {
        let states = vec![
            LightStateReport {
                node_id: "n1".into(),
                device_id: "bulb_a".into(),
                on: false,
                brightness: None,
                color_xy: None,
                color_temp: None,
                online: false,
            },
            LightStateReport {
                node_id: "n1".into(),
                device_id: "bulb_b".into(),
                on: true,
                brightness: None,
                color_xy: None,
                color_temp: None,
                online: true,
            },
        ];
        assert!(device_is_offline("bulb_a", &states));
        assert!(!device_is_offline("bulb_b", &states));
    }

    #[test]
    fn device_is_offline_unknown_device_is_not_offline() {
        let states = vec![LightStateReport {
            node_id: "n1".into(),
            device_id: "bulb_a".into(),
            on: true,
            brightness: None,
            color_xy: None,
            color_temp: None,
            online: true,
        }];
        // Unknown target → not in states → treat as online (let it through;
        // the lighting node decides if it actually exists).
        assert!(!device_is_offline("unknown_bulb", &states));
    }

    #[test]
    fn build_light_action_clamps_out_of_range_values() {
        let brightness = LightCommandBody {
            action: "brightness".into(),
            value: Some(9000.0),
            x: None,
            y: None,
            transition_secs: None,
        };
        assert!(matches!(
            build_light_action(&brightness),
            Some(LightAction::Brightness(255))
        ));
        let xy = LightCommandBody {
            action: "color_xy".into(),
            value: None,
            x: Some(-0.5),
            y: Some(1.5),
            transition_secs: None,
        };
        assert!(matches!(
            build_light_action(&xy),
            Some(LightAction::ColorXY { x: 0.0, y: 1.0 })
        ));
    }

    // ── light_command ─────────────────────────────────────────────────────────

    async fn post_light_cmd(
        state: Arc<DashboardState>,
        device: &str,
        token: &str,
        body: &str,
    ) -> StatusCode {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let router: Router = Router::new()
            .route("/api/lights/{device}/command", post(light_command))
            .layer(axum::Extension(registry))
            .with_state(state);
        let uri = format!("/api/lights/{device}/command?token={token}");
        send(router, "POST", &uri, body).await
    }

    #[tokio::test]
    async fn light_command_ok_queues_message() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "kitchen_bulb", "pi1");

        let status = post_light_cmd(state, "kitchen_bulb", "", r#"{"action":"toggle"}"#).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        match rx.try_recv().unwrap() {
            MeshMessage::LightCommand(req) => {
                assert!(matches!(req.command, LightAction::Toggle));
                assert!(matches!(req.target, LightTarget::Device(d) if d == "kitchen_bulb"));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn light_command_returns_409_for_offline_device() {
        // Parity with intent.rs's voice/chat tool, which already refused an
        // offline device before this fix — the dashboard's direct click
        // path used to silently 204 here instead.
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        state.push_lighting_update(LightStateReport {
            node_id: "pi1".into(),
            device_id: "bulb1".into(),
            on: false,
            brightness: None,
            color_xy: None,
            color_temp: None,
            online: false,
        });

        let status = post_light_cmd(state, "bulb1", "", r#"{"action":"on"}"#).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(rx.try_recv().is_err(), "no command should have been sent");
    }

    #[tokio::test]
    async fn light_command_excludes_device_from_its_active_effect() {
        // A manual command on a bulb its room's effect still owns must not
        // get silently reverted on the effect's next tick — the server-side
        // equivalent of rooms.js's own excludeFromEffect() on a dashboard click.
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "bulb1");
        registry
            .lock()
            .unwrap()
            .set_active_effect(&room_id, "aurora", r#"{"speed":1}"#, None, 0)
            .unwrap();

        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "bulb1", "pi1");

        let router: Router = Router::new()
            .route("/api/lights/{device}/command", post(light_command))
            .layer(axum::Extension(Arc::clone(&registry)))
            .with_state(state);
        let status = send(
            router,
            "POST",
            "/api/lights/bulb1/command?token=",
            r#"{"action":"toggle"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let active = registry
            .lock()
            .unwrap()
            .get_active_effect(&room_id)
            .unwrap();
        let overrides: Vec<String> = serde_json::from_str(&active.overrides_json).unwrap();
        assert_eq!(overrides, vec!["bulb1".to_string()]);
        let _ = rx.try_recv(); // drain the light command, not under test here
    }

    #[tokio::test]
    async fn light_command_returns_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        seed_light(&state, "bulb1", "pi1");
        let status = post_light_cmd(state, "bulb1", "wrong", r#"{"action":"on"}"#).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn light_command_returns_404_for_unknown_device() {
        let state = make_state(vec![], empty_connections());
        let status = post_light_cmd(state, "ghost_bulb", "", r#"{"action":"on"}"#).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn light_command_returns_400_for_unknown_action() {
        let connections = empty_connections();
        let (tx, _rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "bulb1", "pi1");
        let status = post_light_cmd(state, "bulb1", "", r#"{"action":"dance"}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn light_command_brightness_requires_value() {
        let connections = empty_connections();
        let (tx, _rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "bulb1", "pi1");
        let status = post_light_cmd(state, "bulb1", "", r#"{"action":"brightness"}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn light_command_color_xy_requires_both_coords() {
        let connections = empty_connections();
        let (tx, _rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "bulb1", "pi1");
        let status = post_light_cmd(state, "bulb1", "", r#"{"action":"color_xy","x":0.3}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ── delete_device ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_device_requests_network_removal() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "old_bulb", "pi1");
        let registry = Arc::new(Mutex::new(Registry::new()));
        let router: Router = Router::new()
            .route("/api/lights/{device}", axum::routing::delete(delete_device))
            .layer(axum::Extension(registry))
            .with_state(state);
        let status = send(router, "DELETE", "/api/lights/old_bulb", "").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        match rx.try_recv().unwrap() {
            MeshMessage::DeviceRemove(req) => assert_eq!(req.device_id, "old_bulb"),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_device_purges_the_dashboard_snapshot() {
        // Registry deletion alone doesn't tell already-connected clients
        // anything — without state.remove_device the device just sits
        // showing "Unassigned" forever (see DashboardState::remove_device).
        let connections = empty_connections();
        let (tx, _rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "old_bulb", "pi1");
        let state_check = state.clone();
        let registry = Arc::new(Mutex::new(Registry::new()));
        let router: Router = Router::new()
            .route("/api/lights/{device}", axum::routing::delete(delete_device))
            .layer(axum::Extension(registry))
            .with_state(state);
        let status = send(router, "DELETE", "/api/lights/old_bulb", "").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(
            !state_check
                .get_light_snapshot()
                .iter()
                .any(|l| l.device_id == "old_bulb"),
            "deleted device must be gone from the dashboard snapshot, not just the registry"
        );
    }

    #[tokio::test]
    async fn delete_device_without_node_still_cleans_registry() {
        let state = make_state(vec![], empty_connections());
        let registry = Arc::new(Mutex::new(Registry::new()));
        let router: Router = Router::new()
            .route("/api/lights/{device}", axum::routing::delete(delete_device))
            .layer(axum::Extension(registry))
            .with_state(state);
        let status = send(router, "DELETE", "/api/lights/ghost", "").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    // ── group_light_command ───────────────────────────────────────────────────

    fn seed_group(state: &Arc<DashboardState>, group: &str, node_id: &str) {
        state.push_group_update(node_id, vec![group.into()]);
    }

    async fn post_group_cmd(
        state: Arc<DashboardState>,
        group: &str,
        token: &str,
        body: &str,
    ) -> StatusCode {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let router: Router = Router::new()
            .route(
                "/api/lights/group/{group}/command",
                post(group_light_command),
            )
            .layer(axum::Extension(registry))
            .with_state(state);
        let uri = format!("/api/lights/group/{group}/command?token={token}");
        send(router, "POST", &uri, body).await
    }

    #[tokio::test]
    async fn group_light_command_ok_queues_message() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_group(&state, "all", "pi1");

        let status = post_group_cmd(state, "all", "", r#"{"action":"off"}"#).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        match rx.try_recv().unwrap() {
            MeshMessage::LightCommand(req) => {
                assert!(matches!(req.command, LightAction::Off));
                assert!(matches!(req.target, LightTarget::Group(ref g) if g == "all"));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn group_light_command_returns_404_for_unknown_group() {
        let state = make_state(vec![], empty_connections());
        let status = post_group_cmd(state, "ghost_group", "", r#"{"action":"on"}"#).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn group_light_command_returns_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        seed_group(&state, "all", "pi1");
        let status = post_group_cmd(state, "all", "wrong", r#"{"action":"on"}"#).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn group_light_command_returns_400_for_unknown_action() {
        let connections = empty_connections();
        let (tx, _rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_group(&state, "all", "pi1");
        let status = post_group_cmd(state, "all", "", r#"{"action":"dance"}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn build_light_action_maps_all_variants() {
        let mk = |action: &str, value: Option<f64>, x: Option<f32>, y: Option<f32>| {
            build_light_action(&LightCommandBody {
                action: action.into(),
                value,
                x,
                y,
                transition_secs: None,
            })
        };
        assert!(matches!(mk("on", None, None, None), Some(LightAction::On)));
        assert!(matches!(
            mk("off", None, None, None),
            Some(LightAction::Off)
        ));
        assert!(matches!(
            mk("toggle", None, None, None),
            Some(LightAction::Toggle)
        ));
        assert!(matches!(
            mk("brightness", Some(128.0), None, None),
            Some(LightAction::Brightness(128))
        ));
        assert!(matches!(
            mk("color_temp", Some(370.0), None, None),
            Some(LightAction::ColorTemp(370))
        ));
        assert!(matches!(
            mk("color_xy", None, Some(0.3), Some(0.3)),
            Some(LightAction::ColorXY { x, y }) if (x - 0.3).abs() < 1e-4 && (y - 0.3).abs() < 1e-4
        ));
        assert!(mk("unknown", None, None, None).is_none());
        assert!(mk("color_xy", None, Some(0.3), None).is_none());
    }

    // ── light position endpoints ──────────────────────────────────────────────

    fn position_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route(
                "/api/lights/{device}/position",
                get(get_light_position).post(update_light_position),
            )
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    #[tokio::test]
    async fn get_light_position_returns_404_when_unset() {
        let router = position_router(make_state(vec![], empty_connections()), make_registry());
        let status = send(router, "GET", "/api/lights/bulb1/position?token=", "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_then_get_light_position_round_trips() {
        let registry = make_registry();
        let router = position_router(make_state(vec![], empty_connections()), registry.clone());

        let set_status = send(
            router,
            "POST",
            "/api/lights/bulb1/position?token=",
            r#"{"x":1.5,"y":2.5,"z":0.0}"#,
        )
        .await;
        assert_eq!(set_status, StatusCode::NO_CONTENT);

        let pos = registry.lock().unwrap().get_light_position("bulb1");
        assert!(pos.is_some());
        let p = pos.unwrap();
        assert!((p.x - 1.5).abs() < 1e-4);
        assert!((p.y - 2.5).abs() < 1e-4);
        assert!((p.z - 0.0).abs() < 1e-4);
    }

    #[tokio::test]
    async fn get_light_position_returns_401_for_wrong_token() {
        let router = position_router(
            make_state(vec!["secret".into()], empty_connections()),
            make_registry(),
        );
        let status = send(router, "GET", "/api/lights/bulb1/position?token=wrong", "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
