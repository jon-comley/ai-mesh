use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use shared::{
    LightAction, LightCommandRequest, LightStateReport, LightTarget, MeshMessage, ModelLoadRequest,
    ModelUnloadRequest, WIRE_VERSION,
};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use super::state::{DashboardState, RoomInfo, SceneInfo};
use crate::effects::registry::EffectRegistry;
use crate::registry::{DeviceSnapshot, Opening, Registry};

fn gen_request_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("http-{ms}")
}

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
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<LoadModelBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<UnloadModelBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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

fn build_light_action(body: &LightCommandBody) -> Option<LightAction> {
    match body.action.as_str() {
        "on" => Some(LightAction::On),
        "off" => Some(LightAction::Off),
        "toggle" => Some(LightAction::Toggle),
        "brightness" => body.value.map(|v| {
            let value = v.clamp(0.0, 255.0) as u8;
            match body.transition_secs {
                Some(t) if t > 0.0 => LightAction::BrightnessTransition {
                    value,
                    transition_secs: t,
                },
                _ => LightAction::Brightness(value),
            }
        }),
        "color_temp" => body.value.map(|v| {
            let value = v.clamp(1.0, 65535.0) as u16;
            match body.transition_secs {
                Some(t) if t > 0.0 => LightAction::ColorTempTransition {
                    value,
                    transition_secs: t,
                },
                _ => LightAction::ColorTemp(value),
            }
        }),
        "color_xy" => match (body.x, body.y) {
            (Some(x), Some(y)) => match body.transition_secs {
                Some(t) if t > 0.0 => Some(LightAction::ColorXYTransition {
                    x,
                    y,
                    transition_secs: t,
                }),
                _ => Some(LightAction::ColorXY { x, y }),
            },
            _ => None,
        },
        _ => None,
    }
}

pub async fn light_command(
    Path(device): Path<String>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<LightCommandBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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

    // Optimistically update the snapshot so subsequent broadcasts (triggered by
    // any other device's status report) carry the intended value, not the stale
    // pre-command value that would otherwise snap UI sliders back.
    state.apply_command_to_snapshot(&device, &command);
    let cmd = LightCommandRequest {
        request_id: gen_request_id(),
        target: LightTarget::Device(device.clone()),
        command,
    };
    let sent = state.send_to_node(&node_id, MeshMessage::LightCommand(cmd));
    if sent {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}

pub async fn group_light_command(
    Path(group): Path<String>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<LightCommandBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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

// ── Rooms ─────────────────────────────────────────────────────────────────────

fn rooms_from_registry(registry: &Arc<Mutex<Registry>>) -> Vec<RoomInfo> {
    registry
        .lock()
        .unwrap()
        .list_rooms()
        .into_iter()
        .map(RoomInfo::from)
        .collect()
}

#[derive(Deserialize)]
pub struct ReorderRoomsBody {
    ids: Vec<String>,
}

pub async fn reorder_rooms(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<ReorderRoomsBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    {
        let mut reg = registry.lock().unwrap();
        let refs: Vec<&str> = body.ids.iter().map(|s| s.as_str()).collect();
        reg.set_room_positions(&refs);
    }
    state.push_rooms_update(rooms_from_registry(&registry));
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct CreateRoomBody {
    name: String,
}

pub async fn create_room(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<CreateRoomBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name must not be empty").into_response();
    }
    let room_id = registry.lock().unwrap().create_room(&name).id;
    state.push_rooms_update(rooms_from_registry(&registry));
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": room_id })),
    )
        .into_response()
}

pub async fn delete_room(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        reg.delete_room(&room_id);
    }
    state.push_rooms_update(rooms_from_registry(&registry));
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct RenameRoomBody {
    name: String,
}

pub async fn rename_room(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<RenameRoomBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name must not be empty").into_response();
    }
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        reg.rename_room(&room_id, &name);
    }
    state.push_rooms_update(rooms_from_registry(&registry));
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct ModifyRoomDevicesBody {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

pub async fn modify_room_devices(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<ModifyRoomDevicesBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        for device_id in &body.remove {
            reg.remove_device_from_room(&room_id, device_id);
        }
        for device_id in &body.add {
            reg.add_device_to_room(&room_id, device_id);
        }
    }
    state.push_rooms_update(rooms_from_registry(&registry));
    StatusCode::NO_CONTENT.into_response()
}

pub async fn room_command(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<LightCommandBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let device_ids: Vec<String> = {
        let reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        reg.list_rooms()
            .into_iter()
            .find(|r| r.id == room_id)
            .map(|r| r.device_ids)
            .unwrap_or_default()
    };
    let Some(command) = build_light_action(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            "unknown action or missing required fields",
        )
            .into_response();
    };
    let mut any_unavailable = false;
    for device_id in &device_ids {
        let Some(node_id) = state.get_node_for_device(device_id) else {
            any_unavailable = true;
            continue;
        };
        state.apply_command_to_snapshot(device_id, &command);
        let cmd = LightCommandRequest {
            request_id: gen_request_id(),
            target: LightTarget::Device(device_id.clone()),
            command: command.clone(),
        };
        if !state.send_to_node(&node_id, MeshMessage::LightCommand(cmd)) {
            any_unavailable = true;
        }
    }
    if any_unavailable {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    } else {
        StatusCode::NO_CONTENT.into_response()
    }
}

// ── Solar config ──────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct SolarConfigResponse {
    lat: f64,
    lon: f64,
}

pub async fn solar_config(State(state): State<Arc<DashboardState>>) -> impl IntoResponse {
    Json(SolarConfigResponse {
        lat: state.lat,
        lon: state.lon,
    })
}

// ── Room orientation ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SetOrientationBody {
    orientation_degrees: f32,
}

pub async fn set_room_orientation(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetOrientationBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !body.orientation_degrees.is_finite() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        reg.set_room_orientation(&room_id, body.orientation_degrees);
    }
    state.push_rooms_update(rooms_from_registry(&registry));
    state.solar_sweep_notify.notify_one();
    StatusCode::NO_CONTENT.into_response()
}

// ── Room origin + dimensions ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SetOriginBody {
    origin_x: f64,
    origin_y: f64,
}

pub async fn set_room_origin(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetOriginBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        reg.set_room_origin(&room_id, body.origin_x, body.origin_y);
    }
    state.push_rooms_update(rooms_from_registry(&registry));
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct SetDimensionsBody {
    width_m: Option<f64>,
    depth_m: Option<f64>,
    height_m: Option<f64>,
}

pub async fn set_room_dimensions(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetDimensionsBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let (width_m, depth_m, height_m) = {
        let reg = registry.lock().unwrap();
        let room = match reg.get_room(&room_id) {
            Some(r) => r,
            None => return StatusCode::NOT_FOUND.into_response(),
        };
        (
            body.width_m.unwrap_or(room.width_m),
            body.depth_m.unwrap_or(room.depth_m),
            body.height_m.unwrap_or(room.height_m),
        )
    };
    registry
        .lock()
        .unwrap()
        .set_room_dimensions(&room_id, width_m, depth_m, height_m);
    state.push_rooms_update(rooms_from_registry(&registry));
    StatusCode::NO_CONTENT.into_response()
}

// ── Scenes ────────────────────────────────────────────────────────────────────

fn scenes_from_registry(registry: &Arc<Mutex<Registry>>) -> Vec<SceneInfo> {
    registry
        .lock()
        .unwrap()
        .list_scenes()
        .into_iter()
        .map(|s| {
            let preview_color = s.preview_color();
            SceneInfo {
                id: s.id,
                name: s.name,
                room_id: s.room_id,
                created_at: s.created_at,
                position: s.position,
                preview_color,
            }
        })
        .collect()
}

#[derive(Deserialize)]
pub struct ReorderScenesBody {
    ids: Vec<String>,
}

pub async fn reorder_scenes(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<ReorderScenesBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    registry.lock().unwrap().reorder_scenes(&body.ids);
    state.push_scenes_update(scenes_from_registry(&registry));
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct SaveSceneBody {
    name: String,
    #[serde(default)]
    room_id: Option<String>,
}

pub async fn save_scene(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SaveSceneBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name must not be empty").into_response();
    }
    let all_states = state.get_light_snapshot();
    let device_states: Vec<DeviceSnapshot> = if let Some(ref rid) = body.room_id {
        let room_device_ids: Vec<String> = registry
            .lock()
            .unwrap()
            .list_rooms()
            .into_iter()
            .find(|r| &r.id == rid)
            .map(|r| r.device_ids)
            .unwrap_or_default();
        all_states
            .into_iter()
            .filter(|s| room_device_ids.contains(&s.device_id))
            .map(|s| DeviceSnapshot {
                device_id: s.device_id.clone(),
                node_id: s.node_id.clone(),
                on: s.on,
                brightness: s.brightness,
                color_xy: s.color_xy,
                color_temp: s.color_temp,
            })
            .collect()
    } else {
        all_states
            .into_iter()
            .map(|s| DeviceSnapshot {
                device_id: s.device_id.clone(),
                node_id: s.node_id.clone(),
                on: s.on,
                brightness: s.brightness,
                color_xy: s.color_xy,
                color_temp: s.color_temp,
            })
            .collect()
    };
    let scene_id = {
        let mut reg = registry.lock().unwrap();
        reg.save_scene(&name, body.room_id.as_deref(), device_states)
            .id
    };
    state.push_scenes_update(scenes_from_registry(&registry));
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": scene_id })),
    )
        .into_response()
}

#[derive(Deserialize, Default)]
pub struct RecallBody {
    #[serde(default)]
    transition_secs: Option<f32>,
}

pub async fn recall_scene(
    Path(scene_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let transition_secs = {
        let bytes = axum::body::to_bytes(req.into_body(), 4096)
            .await
            .unwrap_or_default();
        if bytes.is_empty() {
            None
        } else {
            serde_json::from_slice::<RecallBody>(&bytes)
                .ok()
                .and_then(|b| b.transition_secs)
        }
    };
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let scene = registry.lock().unwrap().get_scene(&scene_id);
    let Some(scene) = scene else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut any_unavailable = false;
    for snap in &scene.states {
        let node_id = match state.get_node_for_device(&snap.device_id) {
            Some(n) => n,
            None => {
                any_unavailable = true;
                continue;
            }
        };
        if let Some((x, y)) = snap.color_xy {
            let command = match transition_secs {
                Some(t) if t > 0.0 => LightAction::ColorXYTransition {
                    x,
                    y,
                    transition_secs: t,
                },
                _ => LightAction::ColorXY { x, y },
            };
            let cmd = LightCommandRequest {
                request_id: gen_request_id(),
                target: LightTarget::Device(snap.device_id.clone()),
                command,
            };
            let _ = state.send_to_node(&node_id, MeshMessage::LightCommand(cmd));
        } else if let Some(ct) = snap.color_temp {
            let command = match transition_secs {
                Some(t) if t > 0.0 => LightAction::ColorTempTransition {
                    value: ct,
                    transition_secs: t,
                },
                _ => LightAction::ColorTemp(ct),
            };
            let cmd = LightCommandRequest {
                request_id: gen_request_id(),
                target: LightTarget::Device(snap.device_id.clone()),
                command,
            };
            let _ = state.send_to_node(&node_id, MeshMessage::LightCommand(cmd));
        }
        if let Some(brightness) = snap.brightness {
            let command = match transition_secs {
                Some(t) if t > 0.0 => LightAction::BrightnessTransition {
                    value: brightness,
                    transition_secs: t,
                },
                _ => LightAction::Brightness(brightness),
            };
            let cmd = LightCommandRequest {
                request_id: gen_request_id(),
                target: LightTarget::Device(snap.device_id.clone()),
                command,
            };
            let _ = state.send_to_node(&node_id, MeshMessage::LightCommand(cmd));
        }
        let on_off = if snap.on {
            LightAction::On
        } else {
            LightAction::Off
        };
        let cmd = LightCommandRequest {
            request_id: gen_request_id(),
            target: LightTarget::Device(snap.device_id.clone()),
            command: on_off,
        };
        if !state.send_to_node(&node_id, MeshMessage::LightCommand(cmd)) {
            any_unavailable = true;
        }

        // Update the dashboard snapshot so the UI sees the state from the scene.
        state.push_lighting_update(LightStateReport {
            node_id: node_id.clone(),
            device_id: snap.device_id.clone(),
            on: snap.on,
            brightness: snap.brightness,
            color_xy: snap.color_xy,
            color_temp: snap.color_temp,
        });
    }
    if any_unavailable {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    } else {
        StatusCode::NO_CONTENT.into_response()
    }
}

pub async fn delete_scene(
    Path(scene_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    {
        let mut reg = registry.lock().unwrap();
        if !reg.scene_exists(&scene_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        reg.delete_scene(&scene_id);
    }
    state.push_scenes_update(scenes_from_registry(&registry));
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
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<RenameDeviceBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name must not be empty").into_response();
    }
    registry.lock().unwrap().set_device_name(&device_id, &name);
    let names = registry.lock().unwrap().get_all_device_names();
    let rooms = rooms_from_registry(&registry);
    state.push_rooms_update_with_names(rooms, names);
    StatusCode::NO_CONTENT.into_response()
}

pub async fn get_device_names(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetPositionBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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

pub async fn get_room_positions(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let positions = registry.lock().unwrap().get_positions_for_room(&room_id);
    let body: Vec<_> = positions
        .into_iter()
        .map(|(device_id, p)| {
            serde_json::json!({
                "device_id": device_id,
                "x": p.x,
                "y": p.y,
                "z": p.z,
                "fixture_type": p.fixture_type,
            })
        })
        .collect();
    Json(body).into_response()
}

// ── Openings CRUD ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateOpeningBody {
    opening_type: String,
    wall_edge: String,
    x_norm: f32,
    width_norm: f32,
    transmission: Option<f32>,
}

#[derive(Deserialize)]
pub struct UpdateOpeningBody {
    x_norm: Option<f32>,
    width_norm: Option<f32>,
    transmission: Option<f32>,
    wall_edge: Option<String>,
}

const VALID_WALL_EDGES: &[&str] = &["N", "S", "E", "W"];

pub async fn list_openings(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let openings: Vec<Opening> = registry.lock().unwrap().get_openings_for_room(&room_id);
    Json(openings).into_response()
}

pub async fn create_opening(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<CreateOpeningBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let opening_type = body.opening_type.as_str();
    if opening_type != "window" && opening_type != "door" {
        return (
            StatusCode::BAD_REQUEST,
            "opening_type must be 'window' or 'door'",
        )
            .into_response();
    }
    if !VALID_WALL_EDGES.contains(&body.wall_edge.as_str()) {
        return (StatusCode::BAD_REQUEST, "wall_edge must be N, S, E, or W").into_response();
    }
    {
        let reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
    let default_transmission = if opening_type == "window" { 1.0 } else { 0.1 };
    let transmission = body
        .transmission
        .unwrap_or(default_transmission)
        .clamp(0.0, 1.0);
    let opening = registry.lock().unwrap().create_opening(
        &room_id,
        opening_type,
        &body.wall_edge,
        body.x_norm.clamp(0.0, 1.0),
        body.width_norm.clamp(0.01, 1.0),
        transmission,
    );
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": opening.id })),
    )
        .into_response()
}

pub async fn update_opening(
    Path((room_id, opening_id)): Path<(String, String)>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<UpdateOpeningBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let _ = room_id; // validated implicitly via FK; opening_id is the authority
    {
        let reg = registry.lock().unwrap();
        if !reg.opening_exists(&opening_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
    if let Some(ref we) = body.wall_edge
        && !VALID_WALL_EDGES.contains(&we.as_str())
    {
        return (StatusCode::BAD_REQUEST, "wall_edge must be N, S, E, or W").into_response();
    }
    registry.lock().unwrap().update_opening(
        &opening_id,
        body.x_norm.map(|v| v.clamp(0.0, 1.0)),
        body.width_norm.map(|v| v.clamp(0.01, 1.0)),
        body.transmission.map(|v| v.clamp(0.0, 1.0)),
        body.wall_edge.as_deref(),
    );
    StatusCode::NO_CONTENT.into_response()
}

pub async fn delete_opening(
    Path((room_id, opening_id)): Path<(String, String)>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let _ = room_id;
    let mut reg = registry.lock().unwrap();
    if !reg.opening_exists(&opening_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    reg.delete_opening(&opening_id);
    StatusCode::NO_CONTENT.into_response()
}

// ── Effects (F-Effects-2.2) ──────────────────────────────────────────────────

/// GET /api/effects — list every registered effect's metadata.
/// Cacheable: called once on dashboard load.
pub async fn list_effects(
    Extension(effects): Extension<Arc<EffectRegistry>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(effects.list_metadata().to_vec()).into_response()
}

#[derive(Deserialize)]
pub struct SetEffectBody {
    pub effect_id: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// POST /api/rooms/{id}/effect — set the active effect for the room.
/// Validates `effect_id` against the registry and `params` against the
/// effect's JSON Schema. 204 on success.
pub async fn set_room_effect(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Extension(effects): Extension<Arc<EffectRegistry>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetEffectBody>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Look up the effect's metadata so we can validate params and fall back to
    // defaults if none were supplied.
    let metadata = match effects
        .list_metadata()
        .iter()
        .find(|m| m.id == body.effect_id)
        .cloned()
    {
        Some(m) => m,
        None => return (StatusCode::BAD_REQUEST, "unknown effect_id").into_response(),
    };

    // Merge any caller-supplied params on top of the effect's defaults so the
    // stored row is always fully filled. A partial body like
    // {"duration_secs":600} for Sunset keeps the default peak_warmth +
    // start_at without forcing the effect tick to handle missing keys.
    let params = merge_with_defaults(body.params, &metadata.default_params);

    // Use the pre-compiled validator from the registry — compiled once at
    // startup, reused across requests. `None` here is impossible (we already
    // matched the metadata above) but we degrade gracefully if some future
    // code path registers without compiling.
    if let Some(schema) = effects.compiled_schema(&body.effect_id)
        && let Err(errors) = schema.validate(&params)
    {
        let msg = errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; ");
        return (StatusCode::BAD_REQUEST, format!("invalid params: {msg}")).into_response();
    }

    persist_active_effect(&registry, &state, &room_id, &body.effect_id, &params)
}

/// Returns a JSON object: the effect's `defaults`, shallow-overlaid with
/// whatever the caller supplied in `body`. When `body` is `None` the defaults
/// are returned as-is. When either side isn't a JSON object the caller's value
/// wins outright (passthrough; the schema validator will reject anything
/// that's not the right shape).
fn merge_with_defaults(
    body: Option<serde_json::Value>,
    defaults: &serde_json::Value,
) -> serde_json::Value {
    let Some(body_val) = body else {
        return defaults.clone();
    };
    let (Some(body_obj), Some(default_obj)) = (body_val.as_object(), defaults.as_object()) else {
        return body_val;
    };
    let mut merged = serde_json::Map::new();
    for (k, v) in default_obj {
        merged.insert(k.clone(), v.clone());
    }
    for (k, v) in body_obj {
        merged.insert(k.clone(), v.clone());
    }
    serde_json::Value::Object(merged)
}

fn persist_active_effect(
    registry: &Arc<Mutex<Registry>>,
    state: &Arc<DashboardState>,
    room_id: &str,
    effect_id: &str,
    params: &serde_json::Value,
) -> axum::response::Response {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let params_json = params.to_string();
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        if let Err(e) = reg.set_active_effect(room_id, effect_id, &params_json, None, now_ms) {
            tracing::warn!(error = %e, "set_active_effect failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    state.push_effect_update(
        room_id.to_string(),
        Some(effect_id.to_string()),
        params.clone(),
    );
    state.solar_sweep_notify.notify_one();
    StatusCode::NO_CONTENT.into_response()
}

/// DELETE /api/rooms/{id}/effect — clear the active effect.
pub async fn clear_room_effect(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    if !state.auth_ok(&q.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        if let Err(e) = reg.disable_active_effect(&room_id) {
            tracing::warn!(error = %e, "disable_active_effect failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    state.push_effect_update(room_id.clone(), None, serde_json::json!({}));
    state.solar_sweep_notify.notify_one();
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::state::NodeConnections;
    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::routing::{get, post};
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

    // ── load_model / unload_model ─────────────────────────────────────────────

    async fn post_load(state: Arc<DashboardState>, token: &str, body: &str) -> StatusCode {
        let router: Router = Router::new()
            .route("/api/models/load", post(load_model))
            .with_state(state);
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/models/load?token={token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let _ = to_bytes(resp.into_body(), usize::MAX).await;
        status
    }

    async fn post_unload(state: Arc<DashboardState>, token: &str, body: &str) -> StatusCode {
        let router: Router = Router::new()
            .route("/api/models/unload", post(unload_model))
            .with_state(state);
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/models/unload?token={token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let _ = to_bytes(resp.into_body(), usize::MAX).await;
        status
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

    // ── light_command ─────────────────────────────────────────────────────────

    use shared::messages::LightStateReport;

    fn seed_light(state: &Arc<DashboardState>, device_id: &str, node_id: &str) {
        state.push_lighting_update(LightStateReport {
            node_id: node_id.into(),
            device_id: device_id.into(),
            on: false,
            brightness: Some(200),
            color_xy: None,
            color_temp: Some(370),
        });
    }

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
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/lights/{device}/command?token={token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let _ = to_bytes(resp.into_body(), usize::MAX).await;
        status
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
        let router: Router = Router::new()
            .route(
                "/api/lights/group/{group}/command",
                post(group_light_command),
            )
            .with_state(state);
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/lights/group/{group}/command?token={token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let _ = to_bytes(resp.into_body(), usize::MAX).await;
        status
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

    // ── rooms ─────────────────────────────────────────────────────────────────

    use crate::registry::Registry;

    fn make_registry() -> Arc<Mutex<Registry>> {
        Arc::new(Mutex::new(Registry::new()))
    }

    fn make_room(registry: &Arc<Mutex<Registry>>, name: &str) -> String {
        registry.lock().unwrap().create_room(name).id
    }

    fn rooms_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route("/api/rooms", post(create_room))
            .route("/api/rooms/reorder", post(reorder_rooms))
            .route("/api/rooms/{id}", axum::routing::delete(delete_room))
            .route("/api/rooms/{id}/name", axum::routing::patch(rename_room))
            .route(
                "/api/rooms/{id}/devices",
                axum::routing::patch(modify_room_devices),
            )
            .route("/api/rooms/{id}/command", post(room_command))
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    async fn send(router: Router, method: &str, uri: &str, body: &str) -> StatusCode {
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

    async fn send_with_body(
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

    // ── create_room ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_room_returns_201_with_id() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let (status, body) = send_with_body(
            rooms_router(state, registry),
            "POST",
            "/api/rooms?token=",
            r#"{"name":"Living Room"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(
            body.contains("\"id\""),
            "response should contain id: {body}"
        );
    }

    #[tokio::test]
    async fn create_room_returns_400_for_empty_name() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "POST",
            "/api/rooms?token=",
            r#"{"name":"  "}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_room_returns_401_for_wrong_token() {
        let registry = make_registry();
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "POST",
            "/api/rooms?token=wrong",
            r#"{"name":"Hall"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_room_broadcasts_rooms_update() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let mut rx = state.tx.subscribe();
        send(
            rooms_router(state, registry),
            "POST",
            "/api/rooms?token=",
            r#"{"name":"Bedroom"}"#,
        )
        .await;
        use super::super::state::DashboardEvent;
        match rx.try_recv().unwrap() {
            DashboardEvent::RoomsUpdate { rooms, .. } => {
                assert_eq!(rooms.len(), 1);
                assert_eq!(rooms[0].name, "Bedroom");
            }
            _ => panic!("expected RoomsUpdate"),
        }
    }

    // ── delete_room ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_room_returns_204() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "DELETE",
            &format!("/api/rooms/{room_id}?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_room_returns_404_for_unknown_id() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "DELETE",
            "/api/rooms/nonexistent-id?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_room_returns_401_for_wrong_token() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Test");
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "DELETE",
            &format!("/api/rooms/{room_id}?token=wrong"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // ── rename_room ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn rename_room_returns_204() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Old Name");
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry.clone()),
            "PATCH",
            &format!("/api/rooms/{room_id}/name?token="),
            r#"{"name":"New Name"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let rooms = registry.lock().unwrap().list_rooms();
        assert_eq!(rooms[0].name, "New Name");
    }

    #[tokio::test]
    async fn rename_room_returns_404_for_unknown_id() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "PATCH",
            "/api/rooms/ghost-id/name?token=",
            r#"{"name":"Whatever"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rename_room_returns_400_for_empty_name() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Room");
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "PATCH",
            &format!("/api/rooms/{room_id}/name?token="),
            r#"{"name":""}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ── modify_room_devices ───────────────────────────────────────────────────

    #[tokio::test]
    async fn modify_devices_add_returns_204() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry.clone()),
            "PATCH",
            &format!("/api/rooms/{room_id}/devices?token="),
            r#"{"add":["bulb1","bulb2"]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let rooms = registry.lock().unwrap().list_rooms();
        assert!(rooms[0].device_ids.contains(&"bulb1".to_string()));
        assert!(rooms[0].device_ids.contains(&"bulb2".to_string()));
    }

    #[tokio::test]
    async fn modify_devices_remove_returns_204() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Office");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "desk_lamp");
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry.clone()),
            "PATCH",
            &format!("/api/rooms/{room_id}/devices?token="),
            r#"{"remove":["desk_lamp"]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let rooms = registry.lock().unwrap().list_rooms();
        assert!(rooms[0].device_ids.is_empty());
    }

    #[tokio::test]
    async fn modify_devices_returns_404_for_unknown_room() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "PATCH",
            "/api/rooms/ghost/devices?token=",
            r#"{"add":["bulb1"]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn modify_devices_returns_401_for_wrong_token() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Room");
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "PATCH",
            &format!("/api/rooms/{room_id}/devices?token=wrong"),
            r#"{"add":["b1"]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // ── room_command ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn room_command_fans_out_to_devices() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(8);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);

        // Seed two devices on pi1
        seed_light(&state, "bulb1", "pi1");
        seed_light(&state, "bulb2", "pi1");

        let registry = make_registry();
        let room_id = make_room(&registry, "Study");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "bulb1");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "bulb2");

        let status = send(
            rooms_router(state, registry),
            "POST",
            &format!("/api/rooms/{room_id}/command?token="),
            r#"{"action":"on"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Both devices should have received a LightCommand
        let msg1 = rx.try_recv().unwrap();
        let msg2 = rx.try_recv().unwrap();
        for msg in [msg1, msg2] {
            match msg {
                MeshMessage::LightCommand(req) => {
                    assert!(matches!(req.command, LightAction::On));
                }
                other => panic!("unexpected message: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn room_command_returns_404_for_unknown_room() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "POST",
            "/api/rooms/ghost/command?token=",
            r#"{"action":"on"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn room_command_returns_400_for_unknown_action() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Hall");
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "POST",
            &format!("/api/rooms/{room_id}/command?token="),
            r#"{"action":"dance"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn room_command_returns_503_when_device_node_not_connected() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        // Add a device to the room but don't register it in DashboardState
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "ghost_bulb");
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "POST",
            &format!("/api/rooms/{room_id}/command?token="),
            r#"{"action":"off"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn room_command_returns_204_for_empty_room() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Empty Room");
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "POST",
            &format!("/api/rooms/{room_id}/command?token="),
            r#"{"action":"on"}"#,
        )
        .await;
        // No devices → nothing to fan out to → 204 (nothing failed)
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn room_command_returns_401_for_wrong_token() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Hall");
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "POST",
            &format!("/api/rooms/{room_id}/command?token=wrong"),
            r#"{"action":"on"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // ── reorder_rooms ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn reorder_rooms_returns_204() {
        let registry = make_registry();
        let a = make_room(&registry, "A");
        let b = make_room(&registry, "B");
        let status = send(
            rooms_router(make_state(vec![], empty_connections()), registry),
            "POST",
            "/api/rooms/reorder?token=",
            &format!(r#"{{"ids":["{b}","{a}"]}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn reorder_rooms_persists_order() {
        let registry = make_registry();
        let a = make_room(&registry, "A");
        let b = make_room(&registry, "B");
        let c = make_room(&registry, "C");
        send(
            rooms_router(
                make_state(vec![], empty_connections()),
                Arc::clone(&registry),
            ),
            "POST",
            "/api/rooms/reorder?token=",
            &format!(r#"{{"ids":["{c}","{a}","{b}"]}}"#),
        )
        .await;
        let rooms = registry.lock().unwrap().list_rooms();
        let pos = |id: &str| rooms.iter().find(|r| r.id == id).unwrap().position;
        assert_eq!(pos(&c), 0);
        assert_eq!(pos(&a), 1);
        assert_eq!(pos(&b), 2);
    }

    #[tokio::test]
    async fn reorder_rooms_returns_401_for_wrong_token() {
        let registry = make_registry();
        let a = make_room(&registry, "A");
        let status = send(
            rooms_router(
                make_state(vec!["secret".into()], empty_connections()),
                registry,
            ),
            "POST",
            "/api/rooms/reorder?token=wrong",
            &format!(r#"{{"ids":["{a}"]}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // ── scenes ────────────────────────────────────────────────────────────────

    fn scenes_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route("/api/scenes", post(save_scene))
            .route("/api/scenes/{id}/recall", post(recall_scene))
            .route("/api/scenes/{id}", axum::routing::delete(delete_scene))
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    #[tokio::test]
    async fn save_scene_returns_201_with_id() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Living Room");
        let state = make_state(vec![], empty_connections());
        let (status, body) = send_with_body(
            scenes_router(state, registry),
            "POST",
            "/api/scenes?token=",
            &format!(r#"{{"name":"Evening","room_id":"{room_id}"}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(
            body.contains("\"id\""),
            "response should contain id: {body}"
        );
    }

    #[tokio::test]
    async fn save_scene_returns_400_for_empty_name() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            scenes_router(state, registry),
            "POST",
            "/api/scenes?token=",
            r#"{"name":"  "}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_scene_returns_401_for_wrong_token() {
        let registry = make_registry();
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            scenes_router(state, registry),
            "POST",
            "/api/scenes?token=wrong",
            r#"{"name":"Test"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn save_scene_snapshots_room_devices() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "bulb1");
        let connections = empty_connections();
        let (tx, _rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "bulb1", "pi1");
        seed_light(&state, "bulb2", "pi1"); // not in room

        send(
            scenes_router(state.clone(), Arc::clone(&registry)),
            "POST",
            "/api/scenes?token=",
            &format!(r#"{{"name":"Night","room_id":"{room_id}"}}"#),
        )
        .await;

        let scenes = registry.lock().unwrap().list_scenes();
        assert_eq!(scenes.len(), 1);
        assert_eq!(scenes[0].states.len(), 1, "only bulb1 should be in scene");
        assert_eq!(scenes[0].states[0].device_id, "bulb1");
    }

    #[tokio::test]
    async fn save_scene_broadcasts_scenes_update() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let mut rx = state.tx.subscribe();
        send(
            scenes_router(state, registry),
            "POST",
            "/api/scenes?token=",
            r#"{"name":"Morning"}"#,
        )
        .await;
        use super::super::state::DashboardEvent;
        match rx.try_recv().unwrap() {
            DashboardEvent::ScenesUpdate { scenes } => {
                assert_eq!(scenes.len(), 1);
                assert_eq!(scenes[0].name, "Morning");
            }
            _ => panic!("expected ScenesUpdate"),
        }
    }

    #[tokio::test]
    async fn recall_scene_returns_404_for_unknown_id() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            scenes_router(state, registry),
            "POST",
            "/api/scenes/ghost-id/recall?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn recall_scene_returns_401_for_wrong_token() {
        let registry = make_registry();
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            scenes_router(state, registry),
            "POST",
            "/api/scenes/any/recall?token=wrong",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn recall_scene_fans_out_commands() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(16);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "bulb1", "pi1");

        let registry = make_registry();
        let scene_id = {
            let mut reg = registry.lock().unwrap();
            reg.save_scene(
                "Test",
                None,
                vec![crate::registry::DeviceSnapshot {
                    device_id: "bulb1".into(),
                    node_id: "node1".into(),
                    on: true,
                    brightness: Some(255),
                    color_xy: None,
                    color_temp: Some(370),
                }],
            )
            .id
        };

        let status = send(
            scenes_router(state, registry),
            "POST",
            &format!("/api/scenes/{scene_id}/recall?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Expect ColorTemp, Brightness, On — 3 commands
        let msgs: Vec<MeshMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(msgs.len(), 3, "should fan out 3 commands: {msgs:?}");
    }

    #[tokio::test]
    async fn recall_scene_returns_503_when_device_offline() {
        let registry = make_registry();
        let scene_id = registry
            .lock()
            .unwrap()
            .save_scene(
                "Night",
                None,
                vec![crate::registry::DeviceSnapshot {
                    device_id: "bulb1".into(),
                    node_id: "node1".into(),
                    on: true,
                    brightness: Some(255),
                    color_xy: None,
                    color_temp: Some(370),
                }],
            )
            .id;
        let state = make_state(vec![], empty_connections()); // device not in light_snapshot

        let status = send(
            scenes_router(state, registry),
            "POST",
            &format!("/api/scenes/{scene_id}/recall?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn delete_scene_returns_204() {
        let registry = make_registry();
        let scene_id = registry.lock().unwrap().save_scene("Temp", None, vec![]).id;
        let state = make_state(vec![], empty_connections());
        let status = send(
            scenes_router(state, registry),
            "DELETE",
            &format!("/api/scenes/{scene_id}?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_scene_returns_404_for_unknown_id() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            scenes_router(state, registry),
            "DELETE",
            "/api/scenes/ghost-id?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_scene_returns_401_for_wrong_token() {
        let registry = make_registry();
        let scene_id = registry.lock().unwrap().save_scene("Temp", None, vec![]).id;
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            scenes_router(state, registry),
            "DELETE",
            &format!("/api/scenes/{scene_id}?token=wrong"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delete_scene_broadcasts_scenes_update() {
        let registry = make_registry();
        let scene_id = registry.lock().unwrap().save_scene("Temp", None, vec![]).id;
        let state = make_state(vec![], empty_connections());
        let mut rx = state.tx.subscribe();
        send(
            scenes_router(state, registry),
            "DELETE",
            &format!("/api/scenes/{scene_id}?token="),
            "",
        )
        .await;
        use super::super::state::DashboardEvent;
        match rx.try_recv().unwrap() {
            DashboardEvent::ScenesUpdate { scenes } => {
                assert!(scenes.is_empty(), "scenes should be empty after delete");
            }
            _ => panic!("expected ScenesUpdate"),
        }
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

    // ── openings ──────────────────────────────────────────────────────────────

    fn openings_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route(
                "/api/rooms/{id}/openings",
                get(list_openings).post(create_opening),
            )
            .route(
                "/api/rooms/{id}/openings/{oid}",
                axum::routing::patch(update_opening).delete(delete_opening),
            )
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    #[tokio::test]
    async fn create_opening_returns_201_with_id() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let (status, body) = send_with_body(
            openings_router(make_state(vec![], empty_connections()), registry),
            "POST",
            &format!("/api/rooms/{room_id}/openings?token="),
            r#"{"opening_type":"window","wall_edge":"S","x_norm":0.5,"width_norm":0.3}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(
            body.contains("\"id\""),
            "response should contain id: {body}"
        );
    }

    #[tokio::test]
    async fn create_opening_rejects_invalid_type() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Hall");
        let status = send(
            openings_router(make_state(vec![], empty_connections()), registry),
            "POST",
            &format!("/api/rooms/{room_id}/openings?token="),
            r#"{"opening_type":"skylight","wall_edge":"N","x_norm":0.5,"width_norm":0.3}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_opening_rejects_invalid_wall_edge() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Hall");
        let status = send(
            openings_router(make_state(vec![], empty_connections()), registry),
            "POST",
            &format!("/api/rooms/{room_id}/openings?token="),
            r#"{"opening_type":"window","wall_edge":"X","x_norm":0.5,"width_norm":0.3}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_openings_returns_created_opening() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        registry
            .lock()
            .unwrap()
            .create_opening(&room_id, "door", "W", 0.5, 0.15, 0.1);
        let (_status, body) = send_with_body(
            openings_router(make_state(vec![], empty_connections()), registry),
            "GET",
            &format!("/api/rooms/{room_id}/openings?token="),
            "",
        )
        .await;
        assert!(
            body.contains("\"door\""),
            "listing should include door: {body}"
        );
        assert!(
            body.contains("\"W\""),
            "listing should include wall edge: {body}"
        );
    }

    #[tokio::test]
    async fn update_opening_can_move_wall_but_not_type() {
        // wall_edge is mutable (move feature); opening_type is immutable.
        let registry = make_registry();
        let room_id = make_room(&registry, "Study");
        let o = registry
            .lock()
            .unwrap()
            .create_opening(&room_id, "window", "S", 0.5, 0.3, 1.0);
        let status = send(
            openings_router(
                make_state(vec![], empty_connections()),
                Arc::clone(&registry),
            ),
            "PATCH",
            &format!("/api/rooms/{room_id}/openings/{}?token=", o.id),
            r#"{"opening_type":"door","wall_edge":"N","transmission":0.5}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let openings = registry.lock().unwrap().get_openings_for_room(&room_id);
        assert_eq!(openings[0].opening_type, "window"); // type unchanged — not in UpdateOpeningBody
        assert_eq!(openings[0].wall_edge, "N"); // wall moved
        assert!((openings[0].transmission - 0.5).abs() < 1e-4);
    }

    #[tokio::test]
    async fn delete_opening_returns_204() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let o = registry
            .lock()
            .unwrap()
            .create_opening(&room_id, "window", "E", 0.5, 0.3, 1.0);
        let status = send(
            openings_router(
                make_state(vec![], empty_connections()),
                Arc::clone(&registry),
            ),
            "DELETE",
            &format!("/api/rooms/{room_id}/openings/{}?token=", o.id),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(
            registry
                .lock()
                .unwrap()
                .get_openings_for_room(&room_id)
                .is_empty()
        );
    }

    #[tokio::test]
    async fn delete_opening_returns_404_for_unknown_id() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Office");
        let status = send(
            openings_router(make_state(vec![], empty_connections()), registry),
            "DELETE",
            &format!("/api/rooms/{room_id}/openings/ghost-id?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ── solar_config ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn solar_config_returns_lat_lon() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let router = Router::new()
            .route("/api/solar/config", axum::routing::get(solar_config))
            .layer(axum::Extension(registry))
            .with_state(state);
        let (status, body) = send_with_body(router, "GET", "/api/solar/config", "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"lat\""), "body: {body}");
        assert!(body.contains("\"lon\""), "body: {body}");
    }

    // ── set_room_orientation ──────────────────────────────────────────────────

    fn orientation_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route(
                "/api/rooms/{id}/orientation",
                axum::routing::patch(set_room_orientation),
            )
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    #[tokio::test]
    async fn patch_orientation_returns_401_for_wrong_token() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Office");
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            orientation_router(state, registry),
            "PATCH",
            &format!("/api/rooms/{room_id}/orientation?token=wrong"),
            r#"{"orientation_degrees":90.0}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn patch_orientation_returns_404_for_unknown_room() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            orientation_router(state, registry),
            "PATCH",
            "/api/rooms/ghost/orientation?token=",
            r#"{"orientation_degrees":90.0}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn patch_orientation_persists_and_clamps() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let state = make_state(vec![], empty_connections());
        let status = send(
            orientation_router(Arc::clone(&state), Arc::clone(&registry)),
            "PATCH",
            &format!("/api/rooms/{room_id}/orientation?token="),
            r#"{"orientation_degrees":400.0}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let rooms = registry.lock().unwrap().list_rooms();
        let deg = rooms
            .iter()
            .find(|r| r.id == room_id)
            .unwrap()
            .orientation_degrees;
        assert!((deg - 40.0).abs() < 0.01, "expected ~40°, got {deg}");
    }

    #[tokio::test]
    async fn patch_orientation_rejects_nan() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        let state = make_state(vec![], empty_connections());
        let status = send(
            orientation_router(state, registry),
            "PATCH",
            &format!("/api/rooms/{room_id}/orientation?token="),
            r#"{"orientation_degrees":null}"#,
        )
        .await;
        // null deserialises to a 422 Unprocessable from axum, or 400 from our guard
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "expected 400 or 422, got {status}"
        );
    }

    // ── effects (F-Effects-2.2) ─────────────────────────────────────────────

    fn effects_router(
        state: Arc<DashboardState>,
        registry: Arc<Mutex<Registry>>,
        effects: Arc<EffectRegistry>,
    ) -> Router {
        Router::new()
            .route("/api/effects", get(list_effects))
            .route(
                "/api/rooms/{id}/effect",
                post(set_room_effect).delete(clear_room_effect),
            )
            .layer(axum::Extension(registry))
            .layer(axum::Extension(effects))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_effects_returns_solar_metadata() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let effects = Arc::new(EffectRegistry::default());
        let (status, body) = send_with_body(
            effects_router(state, registry, effects),
            "GET",
            "/api/effects?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("\"id\":\"solar\""),
            "missing solar entry: {body}"
        );
        assert!(body.contains("TimeOfDay"), "missing category: {body}");
    }

    #[tokio::test]
    async fn list_effects_requires_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let registry = make_registry();
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(state, registry, effects),
            "GET",
            "/api/effects?token=nope",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn set_room_effect_unknown_effect_returns_400() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(state, registry, effects),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"does-not-exist"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn set_room_effect_missing_room_returns_404() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(state, registry, effects),
            "POST",
            "/api/rooms/no-such-room/effect?token=",
            r#"{"effect_id":"solar"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_room_effect_valid_returns_204_and_persists() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"solar"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let active = registry.lock().unwrap().get_active_effect(&room_id);
        assert_eq!(active.unwrap().effect_id, "solar");
    }

    #[tokio::test]
    async fn set_room_effect_omits_params_uses_default() {
        // Solar has no tunable params; body without `params` should still 204.
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"solar"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let active = registry
            .lock()
            .unwrap()
            .get_active_effect(&room_id)
            .unwrap();
        // Stored params should be the effect's default (an empty object for Solar).
        assert_eq!(active.params_json, "{}");
    }

    #[tokio::test]
    async fn set_room_effect_partial_params_merge_defaults() {
        // Sunset has three defaulted params. A body that supplies only one
        // should land in the DB with all three (the body's value plus the
        // effect's defaults for the rest).
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"sunset","params":{"duration_secs":600}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let stored: serde_json::Value = serde_json::from_str(
            &registry
                .lock()
                .unwrap()
                .get_active_effect(&room_id)
                .unwrap()
                .params_json,
        )
        .unwrap();
        assert_eq!(stored["duration_secs"], 600);
        assert_eq!(stored["peak_warmth"], 0.7); // default kept
        assert_eq!(stored["start_at"], "now"); // default kept
    }

    #[test]
    fn merge_with_defaults_overlays_partial_body_on_defaults() {
        let defaults = serde_json::json!({"a": 1, "b": 2, "c": 3});
        let body = Some(serde_json::json!({"b": 99}));
        let merged = merge_with_defaults(body, &defaults);
        assert_eq!(merged["a"], 1);
        assert_eq!(merged["b"], 99);
        assert_eq!(merged["c"], 3);
    }

    #[test]
    fn merge_with_defaults_none_body_returns_defaults() {
        let defaults = serde_json::json!({"a": 1});
        let merged = merge_with_defaults(None, &defaults);
        assert_eq!(merged, defaults);
    }

    #[tokio::test]
    async fn clear_room_effect_returns_204_and_disables() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        // Activate first.
        let _ = send(
            effects_router(
                Arc::clone(&state),
                Arc::clone(&registry),
                Arc::clone(&effects),
            ),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"solar"}"#,
        )
        .await;
        // Clear.
        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "DELETE",
            &format!("/api/rooms/{room_id}/effect?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(
            registry
                .lock()
                .unwrap()
                .get_active_effect(&room_id)
                .is_none()
        );
    }

    #[tokio::test]
    async fn set_room_effect_broadcasts_effect_update() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        let mut rx = state.tx.subscribe();
        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"solar"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        // The handler broadcasts RoomsUpdate (from the legacy mirror) and
        // EffectUpdate. We don't care about ordering — scan for the EffectUpdate.
        let mut saw_effect = false;
        while let Ok(evt) = rx.try_recv() {
            if let crate::http::state::DashboardEvent::EffectUpdate {
                room_id: rid,
                effect_id,
                ..
            } = evt
            {
                assert_eq!(rid, room_id);
                assert_eq!(effect_id, Some("solar".into()));
                saw_effect = true;
            }
        }
        assert!(saw_effect, "EffectUpdate was not broadcast");
    }
}
