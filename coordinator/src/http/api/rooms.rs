//! Rooms — the *domain-agnostic spatial container*: CRUD, device
//! membership, geometry (orientation/origin/dimensions/positions),
//! openings, room-wide command fan-out, and the public solar config.
//! Device-domain logic stays in the domain modules (see `lights`).

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use shared::{LightCommandRequest, LightTarget, MeshMessage};
use std::sync::{Arc, Mutex};

use crate::http::state::{DashboardState, RoomInfo};
use crate::registry::{Opening, Registry};

use super::gen_request_id;
use super::lights::{LightCommandBody, build_light_action};
use crate::http::auth::Authed;

// ── Rooms ─────────────────────────────────────────────────────────────────────

pub(crate) fn rooms_from_registry(registry: &Arc<Mutex<Registry>>) -> Vec<RoomInfo> {
    registry
        .lock()
        .unwrap()
        .list_rooms()
        .into_iter()
        .map(RoomInfo::from)
        .collect()
}

/// Push a rooms update carrying the registry's full current device-name
/// map. Every room-list change must go through this, not the raw
/// `push_rooms_update_with_names` with a partial map — see that method's
/// doc comment for why a partial map corrupts every other connected
/// client's names.
fn push_rooms_and_names(state: &Arc<DashboardState>, registry: &Arc<Mutex<Registry>>) {
    let rooms = rooms_from_registry(registry);
    let names = registry.lock().unwrap().get_all_device_names();
    state.push_rooms_update_with_names(rooms, names);
}

#[derive(Deserialize)]
pub struct ReorderRoomsBody {
    ids: Vec<String>,
}

pub async fn reorder_rooms(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<ReorderRoomsBody>,
) -> impl IntoResponse {
    {
        let mut reg = registry.lock().unwrap();
        let refs: Vec<&str> = body.ids.iter().map(|s| s.as_str()).collect();
        reg.set_room_positions(&refs);
    }
    push_rooms_and_names(&state, &registry);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct CreateRoomBody {
    name: String,
}

pub async fn create_room(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<CreateRoomBody>,
) -> impl IntoResponse {
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name must not be empty").into_response();
    }
    let room_id = registry.lock().unwrap().create_room(&name).id;
    push_rooms_and_names(&state, &registry);
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": room_id })),
    )
        .into_response()
}

pub async fn delete_room(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        reg.delete_room(&room_id);
    }
    push_rooms_and_names(&state, &registry);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct RenameRoomBody {
    name: String,
}

pub async fn rename_room(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<RenameRoomBody>,
) -> impl IntoResponse {
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
    push_rooms_and_names(&state, &registry);
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
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<ModifyRoomDevicesBody>,
) -> impl IntoResponse {
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
    push_rooms_and_names(&state, &registry);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct ReorderRoomDevicesBody {
    ids: Vec<String>,
}

pub async fn reorder_room_devices(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<ReorderRoomDevicesBody>,
) -> impl IntoResponse {
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        reg.reorder_room_devices(&room_id, &body.ids);
    }
    push_rooms_and_names(&state, &registry);
    StatusCode::NO_CONTENT.into_response()
}

/// Fan a light action out to a set of device ids: resolve each device's
/// node, apply the optimistic snapshot update, send. Returns false if any
/// device was unreachable (unknown node or send failure) - shared by
/// `room_command` and `group_command` so the fan-out logic lives once.
/// `pub(crate)` so `server.rs`'s switch-binding dispatch can reuse it too —
/// a bound button press/dial rotation is the same "fan a command out to a
/// room/group's devices" operation these already do.
pub(crate) fn dispatch_light_command(
    state: &DashboardState,
    device_ids: &[String],
    command: &shared::LightAction,
) -> bool {
    let mut all_ok = true;
    for device_id in device_ids {
        let Some(node_id) = state.get_node_for_device(device_id) else {
            tracing::warn!(
                device_id,
                "dispatch_light_command: no node known for device"
            );
            all_ok = false;
            continue;
        };
        state.apply_command_to_snapshot(device_id, command);
        let cmd = LightCommandRequest {
            request_id: gen_request_id(),
            target: LightTarget::Device(device_id.clone()),
            command: command.clone(),
        };
        if !state.send_to_node(&node_id, MeshMessage::LightCommand(cmd)) {
            tracing::warn!(
                device_id,
                node_id,
                "dispatch_light_command: send_to_node failed"
            );
            all_ok = false;
        }
    }
    all_ok
}

pub async fn room_command(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<LightCommandBody>,
) -> impl IntoResponse {
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
    if dispatch_light_command(&state, &device_ids, &command) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}

// ── Room groups ─────────────────────────────────────────────────────────────
// Named, exclusive-membership subsets of a room's devices (e.g. Kitchen's
// Counter pendants vs. its ceiling spots). Groups live here, not a new
// device-domain module: a group is a partition of room membership, same
// reasoning that already keeps `room_command` (an actual device-domain
// action) in this domain-agnostic file. Never confuse with `light_groups` -
// real Zigbee/z2m groups dispatched as one MQTT command; a room group fans
// a command out to each member device individually via the same
// `dispatch_light_command` helper `room_command` uses.

#[derive(Deserialize)]
pub struct CreateRoomGroupBody {
    name: String,
}

pub async fn create_room_group(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<CreateRoomGroupBody>,
) -> impl IntoResponse {
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name must not be empty").into_response();
    }
    let group_id = {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        reg.create_room_group(&room_id, &name).id
    };
    push_rooms_and_names(&state, &registry);
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": group_id })),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct RenameRoomGroupBody {
    name: String,
}

pub async fn rename_room_group(
    Path((room_id, group_id)): Path<(String, String)>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<RenameRoomGroupBody>,
) -> impl IntoResponse {
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name must not be empty").into_response();
    }
    {
        let mut reg = registry.lock().unwrap();
        match reg.get_room_group(&group_id) {
            Some(g) if g.room_id == room_id => {}
            _ => return StatusCode::NOT_FOUND.into_response(),
        }
        reg.rename_room_group(&group_id, &name);
    }
    push_rooms_and_names(&state, &registry);
    StatusCode::NO_CONTENT.into_response()
}

pub async fn delete_room_group(
    Path((room_id, group_id)): Path<(String, String)>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    {
        let mut reg = registry.lock().unwrap();
        match reg.get_room_group(&group_id) {
            Some(g) if g.room_id == room_id => {}
            _ => return StatusCode::NOT_FOUND.into_response(),
        }
        reg.delete_room_group(&group_id);
    }
    push_rooms_and_names(&state, &registry);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct SetDeviceGroupBody {
    group_id: Option<String>,
}

pub async fn set_device_group(
    Path((room_id, device_id)): Path<(String, String)>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetDeviceGroupBody>,
) -> impl IntoResponse {
    {
        let reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        if reg.get_room_for_device(&device_id).as_deref() != Some(room_id.as_str()) {
            return (
                StatusCode::BAD_REQUEST,
                "device is not a member of this room",
            )
                .into_response();
        }
        if let Some(gid) = &body.group_id {
            match reg.get_room_group(gid) {
                Some(g) if g.room_id == room_id => {}
                _ => {
                    return (
                        StatusCode::BAD_REQUEST,
                        "group does not belong to this room",
                    )
                        .into_response();
                }
            }
        }
    }
    registry
        .lock()
        .unwrap()
        .set_device_group(&device_id, body.group_id.as_deref());
    push_rooms_and_names(&state, &registry);
    StatusCode::NO_CONTENT.into_response()
}

pub async fn group_command(
    Path((room_id, group_id)): Path<(String, String)>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<LightCommandBody>,
) -> impl IntoResponse {
    let device_ids: Vec<String> = {
        let reg = registry.lock().unwrap();
        match reg.get_room_group(&group_id) {
            Some(g) if g.room_id == room_id => g.device_ids,
            _ => return StatusCode::NOT_FOUND.into_response(),
        }
    };
    let Some(command) = build_light_action(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            "unknown action or missing required fields",
        )
            .into_response();
    };
    if dispatch_light_command(&state, &device_ids, &command) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
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
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetOrientationBody>,
) -> impl IntoResponse {
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
    push_rooms_and_names(&state, &registry);
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
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetOriginBody>,
) -> impl IntoResponse {
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        reg.set_room_origin(&room_id, body.origin_x, body.origin_y);
    }
    push_rooms_and_names(&state, &registry);
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
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetDimensionsBody>,
) -> impl IntoResponse {
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
    push_rooms_and_names(&state, &registry);
    StatusCode::NO_CONTENT.into_response()
}

pub async fn get_room_positions(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
) -> impl IntoResponse {
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
/// Sentinel `wall_edge` for a ceiling-mounted opening (skylight / glass
/// ceiling) — it has no compass-facing wall, so it's excluded from
/// `VALID_WALL_EDGES` and the directional facing math in `effects/solar.rs`
/// (`wall_edge_to_degrees`/facing-diff), which only apply to wall openings.
const CEILING_WALL_EDGE: &str = "C";

pub async fn list_openings(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
) -> impl IntoResponse {
    let openings: Vec<Opening> = registry.lock().unwrap().get_openings_for_room(&room_id);
    Json(openings).into_response()
}

pub async fn create_opening(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    Json(body): Json<CreateOpeningBody>,
) -> impl IntoResponse {
    let opening_type = body.opening_type.as_str();
    if opening_type != "window" && opening_type != "door" && opening_type != "skylight" {
        return (
            StatusCode::BAD_REQUEST,
            "opening_type must be 'window', 'door', or 'skylight'",
        )
            .into_response();
    }
    // A skylight has no compass-facing wall — it must use the ceiling
    // sentinel, never a real wall edge; conversely a wall opening can't use
    // the ceiling sentinel.
    let wall_edge_ok = if opening_type == "skylight" {
        body.wall_edge == CEILING_WALL_EDGE
    } else {
        VALID_WALL_EDGES.contains(&body.wall_edge.as_str())
    };
    if !wall_edge_ok {
        let msg = if opening_type == "skylight" {
            "wall_edge must be 'C' (ceiling) for a skylight"
        } else {
            "wall_edge must be N, S, E, or W"
        };
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }
    {
        let reg = registry.lock().unwrap();
        if !reg.room_exists(&room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
    let default_transmission = if opening_type == "door" { 0.1 } else { 1.0 };
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
    _: Authed,
    Json(body): Json<UpdateOpeningBody>,
) -> impl IntoResponse {
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
    _: Authed,
) -> impl IntoResponse {
    let _ = room_id;
    let mut reg = registry.lock().unwrap();
    if !reg.opening_exists(&opening_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    reg.delete_opening(&opening_id);
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::{get, post};
    use shared::LightAction;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    // ── rooms ─────────────────────────────────────────────────────────────────

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
            .route("/api/rooms/{id}/groups", post(create_room_group))
            .route(
                "/api/rooms/{id}/groups/{gid}/name",
                axum::routing::patch(rename_room_group),
            )
            .route(
                "/api/rooms/{id}/groups/{gid}",
                axum::routing::delete(delete_room_group),
            )
            .route("/api/rooms/{id}/groups/{gid}/command", post(group_command))
            .route(
                "/api/rooms/{id}/devices/{did}/group",
                axum::routing::patch(set_device_group),
            )
            .layer(axum::Extension(registry))
            .with_state(state)
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
        use crate::http::state::DashboardEvent;
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

    // ── room groups ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_room_group_returns_201_with_id() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let state = make_state(vec![], empty_connections());
        let (status, body) = send_with_body(
            rooms_router(state, registry),
            "POST",
            &format!("/api/rooms/{room_id}/groups?token="),
            r#"{"name":"Counter"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(
            body.contains("\"id\""),
            "response should contain id: {body}"
        );
    }

    #[tokio::test]
    async fn create_room_group_returns_400_for_empty_name() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "POST",
            &format!("/api/rooms/{room_id}/groups?token="),
            r#"{"name":"  "}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_room_group_returns_404_for_unknown_room() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "POST",
            "/api/rooms/ghost/groups?token=",
            r#"{"name":"Counter"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rename_room_group_returns_204() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let group_id = registry
            .lock()
            .unwrap()
            .create_room_group(&room_id, "Counter")
            .id;
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry.clone()),
            "PATCH",
            &format!("/api/rooms/{room_id}/groups/{group_id}/name?token="),
            r#"{"name":"Island"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            registry
                .lock()
                .unwrap()
                .get_room_group(&group_id)
                .unwrap()
                .name,
            "Island"
        );
    }

    #[tokio::test]
    async fn rename_room_group_returns_404_for_group_in_wrong_room() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let other_room_id = make_room(&registry, "Lounge");
        let group_id = registry
            .lock()
            .unwrap()
            .create_room_group(&room_id, "Counter")
            .id;
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "PATCH",
            &format!("/api/rooms/{other_room_id}/groups/{group_id}/name?token="),
            r#"{"name":"Island"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_room_group_returns_204_and_ungroups_members() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "pendant1");
        let group_id = {
            let mut reg = registry.lock().unwrap();
            let g = reg.create_room_group(&room_id, "Counter").id;
            reg.set_device_group("pendant1", Some(&g));
            g
        };
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry.clone()),
            "DELETE",
            &format!("/api/rooms/{room_id}/groups/{group_id}?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let rooms = registry.lock().unwrap().list_rooms();
        let room = rooms.iter().find(|r| r.id == room_id).unwrap();
        assert!(room.groups.is_empty());
        assert!(room.device_ids.contains(&"pendant1".to_string()));
    }

    #[tokio::test]
    async fn set_device_group_returns_204_and_updates_membership() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "pendant1");
        let group_id = registry
            .lock()
            .unwrap()
            .create_room_group(&room_id, "Counter")
            .id;
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry.clone()),
            "PATCH",
            &format!("/api/rooms/{room_id}/devices/pendant1/group?token="),
            &format!(r#"{{"group_id":"{group_id}"}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let group = registry.lock().unwrap().get_room_group(&group_id).unwrap();
        assert_eq!(group.device_ids, vec!["pendant1".to_string()]);
    }

    #[tokio::test]
    async fn set_device_group_returns_400_for_device_in_other_room() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let other_room_id = make_room(&registry, "Lounge");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&other_room_id, "lamp1");
        let group_id = registry
            .lock()
            .unwrap()
            .create_room_group(&room_id, "Counter")
            .id;
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "PATCH",
            &format!("/api/rooms/{room_id}/devices/lamp1/group?token="),
            &format!(r#"{{"group_id":"{group_id}"}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn set_device_group_returns_400_for_group_in_different_room() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let other_room_id = make_room(&registry, "Lounge");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "pendant1");
        let group_id = registry
            .lock()
            .unwrap()
            .create_room_group(&other_room_id, "Sofa")
            .id;
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "PATCH",
            &format!("/api/rooms/{room_id}/devices/pendant1/group?token="),
            &format!(r#"{{"group_id":"{group_id}"}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn set_device_group_null_clears_membership() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "pendant1");
        let group_id = {
            let mut reg = registry.lock().unwrap();
            let g = reg.create_room_group(&room_id, "Counter").id;
            reg.set_device_group("pendant1", Some(&g));
            g
        };
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry.clone()),
            "PATCH",
            &format!("/api/rooms/{room_id}/devices/pendant1/group?token="),
            r#"{"group_id":null}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let group = registry.lock().unwrap().get_room_group(&group_id).unwrap();
        assert!(group.device_ids.is_empty());
    }

    #[tokio::test]
    async fn group_command_fans_out_to_group_members_only() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(8);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);

        seed_light(&state, "spot1", "pi1");
        seed_light(&state, "pendant1", "pi1");

        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "spot1");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "pendant1");
        let group_id = {
            let mut reg = registry.lock().unwrap();
            let g = reg.create_room_group(&room_id, "Counter").id;
            reg.set_device_group("pendant1", Some(&g));
            g
        };

        let status = send(
            rooms_router(state, registry),
            "POST",
            &format!("/api/rooms/{room_id}/groups/{group_id}/command?token="),
            r#"{"action":"on"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Only pendant1 (the group member) should have received a command.
        let msg = rx.try_recv().unwrap();
        match msg {
            MeshMessage::LightCommand(req) => {
                assert_eq!(req.target, LightTarget::Device("pendant1".into()));
            }
            other => panic!("unexpected message: {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "spot1 (not in the group) should not have received a command"
        );
    }

    #[tokio::test]
    async fn group_command_returns_404_for_group_in_wrong_room() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let other_room_id = make_room(&registry, "Lounge");
        let group_id = registry
            .lock()
            .unwrap()
            .create_room_group(&room_id, "Counter")
            .id;
        let state = make_state(vec![], empty_connections());
        let status = send(
            rooms_router(state, registry),
            "POST",
            &format!("/api/rooms/{other_room_id}/groups/{group_id}/command?token="),
            r#"{"action":"on"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
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
            r#"{"opening_type":"toaster","wall_edge":"N","x_norm":0.5,"width_norm":0.3}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_opening_accepts_skylight_with_ceiling_wall_edge() {
        // A glass/partial-glass ceiling: skylight paired with the ceiling
        // sentinel "C" — no compass-facing wall to assign.
        let registry = make_registry();
        let room_id = make_room(&registry, "Conservatory");
        let (status, body) = send_with_body(
            openings_router(make_state(vec![], empty_connections()), registry),
            "POST",
            &format!("/api/rooms/{room_id}/openings?token="),
            r#"{"opening_type":"skylight","wall_edge":"C","x_norm":0.5,"width_norm":1.0}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(body.contains("\"id\""));
    }

    #[tokio::test]
    async fn create_opening_rejects_skylight_with_a_wall_edge() {
        // A skylight has no compass-facing wall — "N" is a real wall opening's
        // edge, not a valid placement for a ceiling opening.
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
    async fn create_opening_rejects_window_with_ceiling_wall_edge() {
        // The ceiling sentinel is skylight-only — a window/door must use a
        // real compass wall edge.
        let registry = make_registry();
        let room_id = make_room(&registry, "Hall");
        let status = send(
            openings_router(make_state(vec![], empty_connections()), registry),
            "POST",
            &format!("/api/rooms/{room_id}/openings?token="),
            r#"{"opening_type":"window","wall_edge":"C","x_norm":0.5,"width_norm":0.3}"#,
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
}
