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

use super::gen_request_id;
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
    /// When set, recall the scene to ONLY this device (used to resume a single
    /// light that was paused from an active scene). Absent ⇒ whole-scene recall.
    #[serde(default)]
    device_id: Option<String>,
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
    let transition_secs = body.transition_secs;
    let scene = registry.lock().unwrap().get_scene(&scene_id);
    let Some(scene) = scene else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut any_unavailable = false;
    for snap in &scene.states {
        // Single-device recall (resume one paused light) skips all other devices.
        if let Some(only) = &body.device_id
            && &snap.device_id != only
        {
            continue;
        }
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
            if !state.send_to_node(&node_id, MeshMessage::LightCommand(cmd)) {
                any_unavailable = true;
            }
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
            if !state.send_to_node(&node_id, MeshMessage::LightCommand(cmd)) {
                any_unavailable = true;
            }
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
            if !state.send_to_node(&node_id, MeshMessage::LightCommand(cmd)) {
                any_unavailable = true;
            }
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
            online: true,
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
            reg.save_scene("Test", None, vec![mk("bulb1"), mk("bulb2")])
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
        use crate::http::state::DashboardEvent;
        match rx.try_recv().unwrap() {
            DashboardEvent::ScenesUpdate { scenes } => {
                assert!(scenes.is_empty(), "scenes should be empty after delete");
            }
            _ => panic!("expected ScenesUpdate"),
        }
    }
}
