//! Scenes: saved multi-device snapshots, recall fan-out, ordering.

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use shared::{LightAction, LightCommandRequest, LightStateReport, LightTarget, MeshMessage};
use std::sync::{Arc, Mutex};

use crate::http::state::{DashboardState, SceneInfo};
use crate::registry::{DeviceSnapshot, Registry};
use crate::server::Connections;

use super::gen_request_id;
use super::lights::{brightness_action, color_temp_action, color_xy_action};
use crate::http::auth::Authed;

// ── Scenes ────────────────────────────────────────────────────────────────────

fn scenes_from_registry(registry: &Arc<Mutex<Registry>>) -> Vec<SceneInfo> {
    registry
        .lock()
        .unwrap()
        .list_scenes()
        .into_iter()
        .map(|s| {
            let preview_color = s.preview_color();
            let effect_params = s
                .effect_params_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok());
            SceneInfo {
                id: s.id,
                name: s.name,
                room_id: s.room_id,
                created_at: s.created_at,
                position: s.position,
                preview_color,
                states: s.states,
                effect_id: s.effect_id,
                effect_params,
                group_id: s.group_id,
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
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<ReorderScenesBody>,
) -> impl IntoResponse {
    registry.lock().unwrap().reorder_scenes(&body.ids);
    state.push_scenes_update(scenes_from_registry(&registry));
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct SaveSceneBody {
    name: String,
    #[serde(default)]
    room_id: Option<String>,
    /// Scopes the scene to one of `room_id`'s groups instead of the whole
    /// room. Always a flat per-device snapshot when set — no effect
    /// capture, since effects stay room-wide only (see below).
    #[serde(default)]
    group_id: Option<String>,
}

pub async fn save_scene(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SaveSceneBody>,
) -> impl IntoResponse {
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name must not be empty").into_response();
    }
    let all_states = state.get_light_snapshot();
    let to_snapshot = |s: LightStateReport| DeviceSnapshot {
        device_id: s.device_id.clone(),
        node_id: s.node_id.clone(),
        on: s.on,
        brightness: s.brightness,
        color_xy: s.color_xy,
        color_temp: s.color_temp,
    };

    // A group-scoped save: resolve the group's own member devices and skip
    // effect capture entirely — a group scene is always a flat per-device
    // snapshot of just its members, regardless of any room-wide effect.
    if let Some(ref gid) = body.group_id {
        let group = registry.lock().unwrap().get_room_group(gid);
        let Some(group) = group else {
            return (StatusCode::BAD_REQUEST, "unknown group").into_response();
        };
        if let Some(ref rid) = body.room_id
            && &group.room_id != rid
        {
            return (
                StatusCode::BAD_REQUEST,
                "group does not belong to this room",
            )
                .into_response();
        }
        let device_states: Vec<DeviceSnapshot> = all_states
            .into_iter()
            .filter(|s| group.device_ids.contains(&s.device_id))
            .map(to_snapshot)
            .collect();
        let scene_id = {
            let mut reg = registry.lock().unwrap();
            reg.save_scene(
                &name,
                Some(&group.room_id),
                device_states,
                None,
                Some(gid.as_str()),
            )
            .id
        };
        state.push_scenes_update(scenes_from_registry(&registry));
        return (
            StatusCode::CREATED,
            Json(serde_json::json!({ "id": scene_id })),
        )
            .into_response();
    }

    // A room with an active effect gets a different kind of scene: the effect
    // (id + params) is what's saved, and `states` narrows to just the devices
    // manually overridden out of it — every other member is driven by the
    // effect on recall, not a frozen snapshot of whatever it happened to be
    // outputting at save time (which would look static and wrong the moment
    // the effect moves on).
    let active_effect = body
        .room_id
        .as_deref()
        .and_then(|rid| registry.lock().unwrap().get_active_effect(rid));

    let device_states: Vec<DeviceSnapshot> = if let Some(ref rid) = body.room_id {
        let room_device_ids: Vec<String> = registry
            .lock()
            .unwrap()
            .list_rooms()
            .into_iter()
            .find(|r| &r.id == rid)
            .map(|r| r.device_ids)
            .unwrap_or_default();
        let overridden: Option<Vec<String>> = active_effect
            .as_ref()
            .map(|eff| serde_json::from_str(&eff.overrides_json).unwrap_or_default());
        all_states
            .into_iter()
            .filter(|s| room_device_ids.contains(&s.device_id))
            .filter(|s| {
                overridden
                    .as_ref()
                    .is_none_or(|ov| ov.contains(&s.device_id))
            })
            .map(to_snapshot)
            .collect()
    } else {
        all_states.into_iter().map(to_snapshot).collect()
    };

    let effect_arg = active_effect
        .as_ref()
        .map(|eff| (eff.effect_id.as_str(), eff.params_json.as_str()));

    let scene_id = {
        let mut reg = registry.lock().unwrap();
        reg.save_scene(
            &name,
            body.room_id.as_deref(),
            device_states,
            effect_arg,
            None,
        )
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
    /// When set, recall the scene to ONLY this device (used to resume a single
    /// light that was paused from an active scene). Absent ⇒ whole-scene recall.
    #[serde(default)]
    device_id: Option<String>,
}

/// Cancel or reactivate the room's active effect to match `scene`, purely in
/// the registry — the part that must always happen regardless of whether a
/// `DashboardState` is attached to broadcast it. A scene with no captured
/// effect just cancels whatever's currently running; one saved while an
/// effect was active reactivates that effect (the scene's own `states` then
/// only re-applies the handful of devices that were manually overridden out
/// of it at save time — see `save_scene`).
fn apply_scene_effect_state(
    registry: &Arc<Mutex<Registry>>,
    room_id: &str,
    scene: &crate::registry::SceneRecord,
) {
    let mut reg = registry.lock().unwrap();
    match (&scene.effect_id, &scene.effect_params_json) {
        (Some(effect_id), Some(params_json)) => {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if reg
                .set_active_effect(room_id, effect_id, params_json, None, now_ms)
                .is_ok()
            {
                for snap in &scene.states {
                    let _ = reg.set_effect_override(room_id, &snap.device_id, true);
                }
            }
        }
        _ => {
            let _ = reg.disable_active_effect(room_id);
        }
    }
}

/// Broadcast the room's current effect state (as just left by
/// `apply_scene_effect_state`) to the dashboard.
fn broadcast_scene_effect_state(
    registry: &Arc<Mutex<Registry>>,
    dashboard: &Arc<DashboardState>,
    room_id: &str,
) {
    match registry.lock().unwrap().get_active_effect(room_id) {
        Some(a) => {
            let overrides: Vec<String> =
                serde_json::from_str(&a.overrides_json).unwrap_or_default();
            let params: serde_json::Value =
                serde_json::from_str(&a.params_json).unwrap_or(serde_json::json!({}));
            dashboard.push_effect_update(room_id.to_string(), Some(a.effect_id), params, overrides);
        }
        None => {
            dashboard.push_effect_update(room_id.to_string(), None, serde_json::json!({}), vec![])
        }
    }
    dashboard.solar_sweep_notify.notify_one();
}

/// Shared scene-recall logic — HTTP's `recall_scene` and the `scene_load`
/// intent tool (`intent.rs`) both call this, so a voice/chat-triggered
/// recall gets the same effect-cancel/reactivate handling the dashboard's
/// click path already had client-side (`scenes.js`'s `recallScene`)
/// instead of that safety being a JS-only convention every future caller
/// has to reimplement correctly. `device_states` supplies the current
/// device→node routing (the same source `DashboardState::get_node_for_device`
/// itself reads, just passed in — the `scene_load` intent tool already has
/// this snapshot without needing a live `DashboardState` reference for it).
/// Returns whether any device was unreachable.
pub(crate) fn recall_scene_core(
    scene: &crate::registry::SceneRecord,
    device_id_filter: Option<&str>,
    transition_secs: Option<f32>,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    device_states: &[LightStateReport],
    dashboard: Option<&Arc<DashboardState>>,
) -> bool {
    // A single-device resume (pausing/resuming one paused light out of an
    // active scene) is a narrower operation than a full scene switch and
    // leaves the room's effect alone — matches the existing dashboard
    // behavior, where only whole-room recall cancels/reactivates effects.
    if device_id_filter.is_none()
        && let Some(room_id) = &scene.room_id
    {
        apply_scene_effect_state(registry, room_id, scene);
        if let Some(dash) = dashboard {
            broadcast_scene_effect_state(registry, dash, room_id);
        }
    }

    let node_for_device: std::collections::HashMap<&str, &str> = device_states
        .iter()
        .map(|r| (r.device_id.as_str(), r.node_id.as_str()))
        .collect();

    let mut any_unavailable = false;
    for snap in &scene.states {
        // Single-device recall (resume one paused light) skips all other devices.
        if let Some(only) = device_id_filter
            && snap.device_id != only
        {
            continue;
        }
        let Some(&node_id) = node_for_device.get(snap.device_id.as_str()) else {
            any_unavailable = true;
            continue;
        };
        // Actions come from the lights-domain constructors so the transition
        // dispatch is shared with the HTTP command path, not re-derived here.
        {
            let mut send = |command: LightAction| {
                let cmd = LightCommandRequest {
                    request_id: gen_request_id(),
                    target: LightTarget::Device(snap.device_id.clone()),
                    command,
                };
                let sent = connections
                    .lock()
                    .unwrap()
                    .get(node_id)
                    .cloned()
                    .is_some_and(|tx| tx.try_send(MeshMessage::LightCommand(cmd)).is_ok());
                if !sent {
                    any_unavailable = true;
                }
            };
            if let Some((x, y)) = snap.color_xy {
                send(color_xy_action(x, y, transition_secs));
            } else if let Some(ct) = snap.color_temp {
                send(color_temp_action(ct, transition_secs));
            }
            if let Some(brightness) = snap.brightness {
                send(brightness_action(brightness, transition_secs));
            }
            send(if snap.on {
                LightAction::On
            } else {
                LightAction::Off
            });
        }

        // Update the dashboard snapshot so the UI sees the state from the scene.
        if let Some(dash) = dashboard {
            dash.push_lighting_update(LightStateReport {
                node_id: node_id.to_string(),
                device_id: snap.device_id.clone(),
                on: snap.on,
                brightness: snap.brightness,
                color_xy: snap.color_xy,
                color_temp: snap.color_temp,
                online: true,
            });
        }
    }
    any_unavailable
}

pub async fn recall_scene(
    Path(scene_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let body: RecallBody = {
        let bytes = axum::body::to_bytes(req.into_body(), 4096)
            .await
            .unwrap_or_default();
        if bytes.is_empty() {
            RecallBody::default()
        } else {
            serde_json::from_slice::<RecallBody>(&bytes).unwrap_or_default()
        }
    };
    let scene = registry.lock().unwrap().get_scene(&scene_id);
    let Some(scene) = scene else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let device_states = state.get_light_snapshot();
    let any_unavailable = recall_scene_core(
        &scene,
        body.device_id.as_deref(),
        body.transition_secs,
        &registry,
        &state.connections,
        &device_states,
        Some(&state),
    );
    if any_unavailable {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    } else {
        StatusCode::NO_CONTENT.into_response()
    }
}

pub async fn delete_scene(
    Path(scene_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::post;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

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
    async fn save_scene_with_active_effect_captures_effect_and_only_overrides() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        {
            let mut reg = registry.lock().unwrap();
            reg.add_device_to_room(&room_id, "bulb1");
            reg.add_device_to_room(&room_id, "bulb2");
            reg.set_active_effect(&room_id, "aurora", r#"{"speed":1}"#, None, 0)
                .unwrap();
            // Only bulb1 is manually overridden out of the effect; bulb2 stays
            // effect-driven and must NOT get a frozen snapshot in the scene.
            reg.set_effect_override(&room_id, "bulb1", true).unwrap();
        }
        let state = make_state(vec![], empty_connections());
        seed_light(&state, "bulb1", "pi1");
        seed_light(&state, "bulb2", "pi1");

        send(
            scenes_router(state, Arc::clone(&registry)),
            "POST",
            "/api/scenes?token=",
            &format!(r#"{{"name":"Aurora night","room_id":"{room_id}"}}"#),
        )
        .await;

        let scenes = registry.lock().unwrap().list_scenes();
        assert_eq!(scenes.len(), 1);
        assert_eq!(scenes[0].effect_id.as_deref(), Some("aurora"));
        assert_eq!(
            scenes[0].effect_params_json.as_deref(),
            Some(r#"{"speed":1}"#)
        );
        assert_eq!(
            scenes[0].states.len(),
            1,
            "only the overridden device should be snapshotted"
        );
        assert_eq!(scenes[0].states[0].device_id, "bulb1");
    }

    #[tokio::test]
    async fn save_scene_without_active_effect_has_no_effect_id() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "bulb1");
        let state = make_state(vec![], empty_connections());
        seed_light(&state, "bulb1", "pi1");

        send(
            scenes_router(state, Arc::clone(&registry)),
            "POST",
            "/api/scenes?token=",
            &format!(r#"{{"name":"Plain","room_id":"{room_id}"}}"#),
        )
        .await;

        let scenes = registry.lock().unwrap().list_scenes();
        assert!(scenes[0].effect_id.is_none());
        assert_eq!(scenes[0].states.len(), 1);
    }

    // ── group-scoped scenes ───────────────────────────────────────────────────

    #[tokio::test]
    async fn save_scene_with_group_id_snapshots_only_group_members() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let group_id = {
            let mut reg = registry.lock().unwrap();
            reg.add_device_to_room(&room_id, "spot1");
            reg.add_device_to_room(&room_id, "pendant1");
            let g = reg.create_room_group(&room_id, "Counter").id;
            reg.set_device_group("pendant1", Some(&g));
            g
        };
        let state = make_state(vec![], empty_connections());
        seed_light(&state, "spot1", "pi1");
        seed_light(&state, "pendant1", "pi1");

        let (status, body) = send_with_body(
            scenes_router(state, Arc::clone(&registry)),
            "POST",
            "/api/scenes?token=",
            &format!(r#"{{"name":"Bright","room_id":"{room_id}","group_id":"{group_id}"}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(body.contains("\"id\""));

        let scenes = registry.lock().unwrap().list_scenes();
        assert_eq!(scenes.len(), 1);
        assert_eq!(scenes[0].group_id.as_deref(), Some(group_id.as_str()));
        assert_eq!(
            scenes[0].states.len(),
            1,
            "only the group's own member should be snapshotted"
        );
        assert_eq!(scenes[0].states[0].device_id, "pendant1");
    }

    #[tokio::test]
    async fn save_scene_with_group_id_ignores_active_room_effect() {
        // Group scenes are always a flat snapshot — never carry an effect,
        // even if the room happens to have one active at save time.
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let group_id = {
            let mut reg = registry.lock().unwrap();
            reg.add_device_to_room(&room_id, "pendant1");
            let g = reg.create_room_group(&room_id, "Counter").id;
            reg.set_device_group("pendant1", Some(&g));
            reg.set_active_effect(&room_id, "aurora", r#"{"speed":1}"#, None, 0)
                .unwrap();
            g
        };
        let state = make_state(vec![], empty_connections());
        seed_light(&state, "pendant1", "pi1");

        send(
            scenes_router(state, Arc::clone(&registry)),
            "POST",
            "/api/scenes?token=",
            &format!(r#"{{"name":"Bright","room_id":"{room_id}","group_id":"{group_id}"}}"#),
        )
        .await;

        let scenes = registry.lock().unwrap().list_scenes();
        assert!(scenes[0].effect_id.is_none());
        assert_eq!(scenes[0].states.len(), 1);
    }

    #[tokio::test]
    async fn save_scene_returns_400_for_unknown_group() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let state = make_state(vec![], empty_connections());
        let status = send(
            scenes_router(state, registry),
            "POST",
            "/api/scenes?token=",
            &format!(r#"{{"name":"Bright","room_id":"{room_id}","group_id":"ghost"}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_scene_returns_400_for_group_in_different_room() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Kitchen");
        let other_room_id = make_room(&registry, "Lounge");
        let group_id = registry
            .lock()
            .unwrap()
            .create_room_group(&other_room_id, "Sofa")
            .id;
        let state = make_state(vec![], empty_connections());
        let status = send(
            scenes_router(state, registry),
            "POST",
            "/api/scenes?token=",
            &format!(r#"{{"name":"Bright","room_id":"{room_id}","group_id":"{group_id}"}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_scene_with_active_effect_and_no_overrides_has_empty_states() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        {
            let mut reg = registry.lock().unwrap();
            reg.add_device_to_room(&room_id, "bulb1");
            reg.add_device_to_room(&room_id, "bulb2");
            reg.set_active_effect(&room_id, "aurora", r#"{"speed":1}"#, None, 0)
                .unwrap();
            // No overrides at all — every member is effect-driven.
        }
        let state = make_state(vec![], empty_connections());
        seed_light(&state, "bulb1", "pi1");
        seed_light(&state, "bulb2", "pi1");

        send(
            scenes_router(state, Arc::clone(&registry)),
            "POST",
            "/api/scenes?token=",
            &format!(r#"{{"name":"Aurora night","room_id":"{room_id}"}}"#),
        )
        .await;

        let scenes = registry.lock().unwrap().list_scenes();
        assert_eq!(scenes[0].effect_id.as_deref(), Some("aurora"));
        assert!(
            scenes[0].states.is_empty(),
            "no device should be snapshotted when nothing is overridden"
        );
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
        use crate::http::state::DashboardEvent;
        match rx.try_recv().unwrap() {
            DashboardEvent::ScenesUpdate { scenes } => {
                assert_eq!(scenes.len(), 1);
                assert_eq!(scenes[0].name, "Morning");
            }
            _ => panic!("expected ScenesUpdate"),
        }
    }

    // The client detects a scene-vs-live-state divergence (chat command,
    // physical switch, ...) by comparing against this — it must actually
    // carry the saved per-device values, not just an empty placeholder.
    #[tokio::test]
    async fn scenes_update_carries_saved_device_states() {
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
        let mut rx = state.tx.subscribe();

        send(
            scenes_router(state, Arc::clone(&registry)),
            "POST",
            "/api/scenes?token=",
            &format!(r#"{{"name":"Night","room_id":"{room_id}"}}"#),
        )
        .await;

        use crate::http::state::DashboardEvent;
        match rx.try_recv().unwrap() {
            DashboardEvent::ScenesUpdate { scenes } => {
                assert_eq!(scenes[0].states.len(), 1);
                assert_eq!(scenes[0].states[0].device_id, "bulb1");
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
                None,
                None,
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
    async fn recall_scene_cancels_a_running_effect_with_no_captured_effect() {
        // A room-scoped scene with no effect_id recalled while an effect is
        // running must stop that effect server-side — previously this was
        // only ever done by scenes.js's client-side recallScene(), so a
        // caller that skips the JS (voice/chat's scene_load tool, the CLI)
        // would leave the effect running to fight the recalled state on its
        // next tick.
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(16);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "bulb1", "pi1");

        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        registry
            .lock()
            .unwrap()
            .set_active_effect(&room_id, "aurora", r#"{"speed":1}"#, None, 0)
            .unwrap();
        let scene_id = registry
            .lock()
            .unwrap()
            .save_scene(
                "Night",
                Some(&room_id),
                vec![crate::registry::DeviceSnapshot {
                    device_id: "bulb1".into(),
                    node_id: "pi1".into(),
                    on: false,
                    brightness: None,
                    color_xy: None,
                    color_temp: None,
                }],
                None,
                None,
            )
            .id;
        // The scene was saved with no active effect, so it carries none —
        // recalling it must cancel the effect still running in the room.
        assert!(
            registry
                .lock()
                .unwrap()
                .get_active_effect(&room_id)
                .is_some()
        );

        let mut evt_rx = state.tx.subscribe();
        let status = send(
            scenes_router(state, Arc::clone(&registry)),
            "POST",
            &format!("/api/scenes/{scene_id}/recall?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert!(
            registry
                .lock()
                .unwrap()
                .get_active_effect(&room_id)
                .is_none(),
            "effect should be disabled by recall"
        );
        let events: Vec<_> = std::iter::from_fn(|| evt_rx.try_recv().ok()).collect();
        assert!(
            events.iter().any(|e| matches!(
                e,
                crate::http::state::DashboardEvent::EffectUpdate {
                    effect_id: None,
                    ..
                }
            )),
            "expected an EffectUpdate clearing the effect: {events:?}"
        );
        let _ = rx.try_recv(); // drain the light command, not under test here
    }

    #[tokio::test]
    async fn recall_scene_reactivates_its_captured_effect() {
        // A scene saved while an effect was running carries that effect —
        // recalling it must reactivate the effect (not just replay the
        // handful of manually-overridden bulbs it also stored).
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(16);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "bulb1", "pi1");

        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        let scene_id = registry
            .lock()
            .unwrap()
            .save_scene(
                "Aurora night",
                Some(&room_id),
                vec![crate::registry::DeviceSnapshot {
                    device_id: "bulb1".into(),
                    node_id: "pi1".into(),
                    on: true,
                    brightness: Some(120),
                    color_xy: None,
                    color_temp: None,
                }],
                Some(("aurora", r#"{"speed":2}"#)),
                None,
            )
            .id;

        let mut evt_rx = state.tx.subscribe();
        let status = send(
            scenes_router(state, Arc::clone(&registry)),
            "POST",
            &format!("/api/scenes/{scene_id}/recall?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let active = registry.lock().unwrap().get_active_effect(&room_id);
        assert_eq!(
            active.as_ref().map(|a| a.effect_id.as_str()),
            Some("aurora")
        );
        assert_eq!(
            active.as_ref().map(|a| a.params_json.as_str()),
            Some(r#"{"speed":2}"#)
        );
        let overrides: Vec<String> =
            serde_json::from_str(&active.unwrap().overrides_json).unwrap_or_default();
        assert_eq!(
            overrides,
            vec!["bulb1".to_string()],
            "the scene's own snapshot devices must be excluded from the reactivated effect"
        );
        let events: Vec<_> = std::iter::from_fn(|| evt_rx.try_recv().ok()).collect();
        assert!(
            events.iter().any(|e| matches!(
                e,
                crate::http::state::DashboardEvent::EffectUpdate { effect_id: Some(id), .. } if id == "aurora"
            )),
            "expected an EffectUpdate reactivating aurora: {events:?}"
        );
        let _ = rx.try_recv(); // drain the light command, not under test here
    }

    #[tokio::test]
    async fn recall_scene_with_device_id_targets_only_that_device() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(16);
        connections.lock().unwrap().insert("pi1".into(), tx);
        let state = make_state(vec![], connections);
        seed_light(&state, "bulb1", "pi1");
        seed_light(&state, "bulb2", "pi1");

        let registry = make_registry();
        let mk = |id: &str| crate::registry::DeviceSnapshot {
            device_id: id.into(),
            node_id: "pi1".into(),
            on: true,
            brightness: Some(255),
            color_xy: None,
            color_temp: Some(370),
        };
        let scene_id = {
            let mut reg = registry.lock().unwrap();
            reg.save_scene("Test", None, vec![mk("bulb1"), mk("bulb2")], None, None)
                .id
        };

        // Recall to only bulb1.
        let status = send(
            scenes_router(state, registry),
            "POST",
            &format!("/api/scenes/{scene_id}/recall?token="),
            r#"{"device_id":"bulb1"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let msgs: Vec<MeshMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(
            msgs.len(),
            3,
            "only bulb1's 3 commands should fire: {msgs:?}"
        );
        for m in &msgs {
            if let MeshMessage::LightCommand(req) = m {
                assert!(
                    matches!(&req.target, LightTarget::Device(d) if d == "bulb1"),
                    "every command must target bulb1, got {:?}",
                    req.target
                );
            }
        }
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
                None,
                None,
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
        let scene_id = registry
            .lock()
            .unwrap()
            .save_scene("Temp", None, vec![], None, None)
            .id;
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
        let scene_id = registry
            .lock()
            .unwrap()
            .save_scene("Temp", None, vec![], None, None)
            .id;
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
        let scene_id = registry
            .lock()
            .unwrap()
            .save_scene("Temp", None, vec![], None, None)
            .id;
        let state = make_state(vec![], empty_connections());
        let mut rx = state.tx.subscribe();
        send(
            scenes_router(state, registry),
            "DELETE",
            &format!("/api/scenes/{scene_id}?token="),
            "",
        )
        .await;
        use crate::http::state::DashboardEvent;
        match rx.try_recv().unwrap() {
            DashboardEvent::ScenesUpdate { scenes } => {
                assert!(scenes.is_empty(), "scenes should be empty after delete");
            }
            _ => panic!("expected ScenesUpdate"),
        }
    }
}
