//! AV endpoints: the unified "Speakers & displays" inventory behind the
//! dashboard's AV section (Phase A of `plans/av-devices-ui.md`), plus
//! room-assignment mutation for sinks (a room can hold more than one at
//! once — see `crate::audio::RoomSink`/`SinkRole`).
//!
//! Listing is read-only: one GET that flattens every audio-capable
//! endpoint the coordinator knows about into a uniform list, whatever its
//! transport — each backend of each `Feature::Audio` node (`pi2:hdmi`,
//! `pi2:bluetooth`, …), the configured LAN appliances (soundbar / TV,
//! present only when their `-ip` preference is set), and the voice puck
//! (inferred from a `Feature::Voice` node).
//!
//! Room assignment for sinks goes through `PUT`/`DELETE
//! /api/av-devices/{id}/rooms/{room}` (this module, backed by
//! `crate::audio::add_room_sink`/`remove_room_sink` so multiple devices
//! can share a room without clobbering each other). The puck's room and
//! device renames still ride the plain preferences API
//! (`av-room:puck`, `av-name:<id>`) — both are genuinely single-valued,
//! no room-list semantics needed.

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use crate::audio::{RoomSink, SinkRole, add_room_sink, remove_room_sink};
use crate::http::auth::Authed;
use crate::http::state::DashboardState;
use crate::registry::Registry;

use crate::http::api::prefs::PREF_USER_ID;

#[derive(Serialize)]
pub struct RoomAssignment {
    pub room: String,
    /// "any" | "reply" | "media" — see `crate::audio::SinkRole`.
    pub role: String,
}

#[derive(Serialize)]
pub struct AvDevice {
    /// Stable id: `<node_id>:<backend>` for node sinks, `appliance:<x>`
    /// for LAN appliances, `puck` for the voice puck. This exact string
    /// (split back into node_id/backend) is what the room-assignment
    /// endpoints below address.
    pub id: String,
    pub name: String,
    /// "sink" (playable via AudioPlay, room-assignable through this
    /// module's endpoints) or "appliance" (controlled, not a playback
    /// target — not room-assignable here).
    pub kind: &'static str,
    /// zigbee-equivalent of DeviceType for badging: "hdmi", "bluetooth",
    /// "wifi", …
    pub transport: String,
    /// The mesh node hosting this sink (None for appliances/puck).
    pub node_id: Option<String>,
    pub hostname: Option<String>,
    /// None = unknown (appliances aren't probed), Some = live mesh state.
    pub online: Option<bool>,
    /// Every room this sink is assigned to, with the role it serves
    /// there. Plural on both axes: one sink can serve several rooms, and
    /// (separately) one room can have several sinks — see
    /// `crate::audio::RoomSink`.
    pub rooms: Vec<RoomAssignment>,
}

/// `(room name, parsed sink)` for every `room-audio-sink:*` preference —
/// reuses `crate::audio`'s own entry format so the list view and the
/// resolver that actually routes audio never drift apart.
fn all_room_sinks(prefs: &[(String, String)]) -> Vec<(String, RoomSink)> {
    prefs
        .iter()
        .filter_map(|(k, v)| {
            let room = k.strip_prefix("room-audio-sink:")?;
            Some((room, v))
        })
        .flat_map(|(room, value)| {
            crate::audio::parse_room_sink_list(value)
                .into_iter()
                .map(move |sink| (room.to_string(), sink))
        })
        .collect()
}

fn custom_name(prefs: &[(String, String)], id: &str) -> Option<String> {
    let key = format!("av-name:{id}");
    prefs
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.clone())
}

/// Split an AV device id into `(node_id, backend)` — the inverse of
/// `RoomSink::device_id()`. `"pi2:bluetooth"` → `("pi2", Some("bluetooth"))`,
/// `"pi2"` → `("pi2", None)`.
fn split_device_id(id: &str) -> (String, Option<String>) {
    match id.split_once(':') {
        Some((node_id, sink)) => (node_id.to_string(), Some(sink.to_string())),
        None => (id.to_string(), None),
    }
}

pub async fn list_av_devices(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    let (audio_nodes, voice_nodes, prefs) = {
        let reg = registry.lock().unwrap();
        (
            reg.nodes_with_feature(shared::Feature::Audio),
            reg.nodes_with_feature(shared::Feature::Voice),
            reg.get_all_preferences(PREF_USER_ID),
        )
    };
    let connected: std::collections::HashSet<String> =
        state.connections.lock().unwrap().keys().cloned().collect();
    let room_sinks = all_room_sinks(&prefs);

    let mut devices: Vec<AvDevice> = Vec::new();

    for node in &audio_nodes {
        let backends = node
            .capabilities
            .as_ref()
            .map(|c| c.audio_backends.clone())
            .unwrap_or_default();
        // An older agent advertises Feature::Audio without the backend
        // list — show it as a single opaque sink addressed by bare node
        // id so it's still visible and assignable.
        let backends = if backends.is_empty() {
            vec![String::new()]
        } else {
            backends
        };
        let default_backend = backends.first().cloned();
        for backend in &backends {
            let id = if backend.is_empty() {
                node.id.clone()
            } else {
                format!("{}:{}", node.id, backend)
            };
            let rooms = room_sinks
                .iter()
                .filter(|(_, s)| {
                    s.node_id == node.id
                        && match &s.sink {
                            Some(sink) => sink == backend,
                            // A bare node-id entry means "that node's
                            // default backend".
                            None => Some(backend) == default_backend.as_ref(),
                        }
                })
                .map(|(room, s)| RoomAssignment {
                    room: room.clone(),
                    role: s.role.as_str().to_string(),
                })
                .collect();
            let default_name = if backend.is_empty() {
                format!("{} audio", node.hostname)
            } else {
                format!("{} · {}", node.hostname, backend)
            };
            devices.push(AvDevice {
                name: custom_name(&prefs, &id).unwrap_or(default_name),
                id,
                kind: "sink",
                transport: if backend.is_empty() {
                    "audio".into()
                } else {
                    backend.clone()
                },
                node_id: Some(node.id.clone()),
                hostname: Some(node.hostname.clone()),
                online: Some(connected.contains(&node.id)),
                rooms,
            });
        }
    }

    for (pref_key, id, default_name) in [
        ("soundbar-ip", "appliance:soundbar", "Soundbar"),
        ("tv-ip", "appliance:tv", "TV"),
    ] {
        if prefs.iter().any(|(k, _)| k == pref_key) {
            devices.push(AvDevice {
                id: id.into(),
                name: custom_name(&prefs, id).unwrap_or_else(|| default_name.into()),
                kind: "appliance",
                transport: "wifi".into(),
                node_id: None,
                hostname: None,
                online: None,
                rooms: vec![],
            });
        }
    }

    // The puck (inferred from whichever node runs capability-voice) is
    // room-assignable like the sinks, but through its own preference:
    // `av-room:puck` names the room the puck physically sits in — the
    // voice pipeline reads the same key to decide whether a spoken reply
    // should divert to that room's speaker (see capability-voice's
    // `room_with_audio_sink`). Genuinely single-valued (a physical puck
    // sits in one room), unlike the multi-room-sink model above.
    if let Some(voice_node) = voice_nodes.first() {
        let rooms = prefs
            .iter()
            .find(|(k, _)| k == "av-room:puck")
            .map(|(_, v)| {
                vec![RoomAssignment {
                    room: v.clone(),
                    role: SinkRole::Any.as_str().to_string(),
                }]
            })
            .unwrap_or_default();
        devices.push(AvDevice {
            id: "puck".into(),
            name: custom_name(&prefs, "puck").unwrap_or_else(|| "Voice puck".into()),
            kind: "appliance",
            transport: "wifi".into(),
            node_id: Some(voice_node.id.clone()),
            hostname: Some(voice_node.hostname.clone()),
            online: Some(connected.contains(&voice_node.id)),
            rooms,
        });
    }

    Json(serde_json::json!({ "devices": devices })).into_response()
}

#[derive(Deserialize, Default)]
pub struct AssignRoomBody {
    /// "any" | "reply" | "media" — defaults to "any" (serves every
    /// purpose) when omitted, matching a plain drag-onto-a-room gesture
    /// with no purpose specified.
    #[serde(default)]
    role: Option<String>,
}

/// Add (or retag the role of) this sink in `room`, without disturbing any
/// other sink already assigned there.
pub async fn assign_room(
    Path((id, room)): Path<(String, String)>,
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Json(body): Json<AssignRoomBody>,
) -> impl IntoResponse {
    let (node_id, sink) = split_device_id(&id);
    let role = body
        .role
        .as_deref()
        .map(SinkRole::parse)
        .unwrap_or(SinkRole::Any);
    add_room_sink(&room, &node_id, sink.as_deref(), role, &registry);
    StatusCode::OK
}

/// Remove this sink from `room`, leaving any other sink assigned there
/// untouched.
pub async fn unassign_room(
    Path((id, room)): Path<(String, String)>,
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    let (node_id, sink) = split_device_id(&id);
    remove_room_sink(&room, &node_id, sink.as_deref(), &registry);
    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::{empty_connections, make_registry, make_state};
    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::routing::{get, put};
    use tower::ServiceExt;

    fn av_router(
        registry: Arc<Mutex<Registry>>,
        connections: crate::http::state::NodeConnections,
    ) -> Router {
        let state = make_state(vec!["tok".into()], connections);
        // Same path shape registered for real in http/mod.rs.
        Router::new()
            .route("/api/av-devices", get(list_av_devices))
            .route(
                "/api/av-devices/{id}/rooms/{room}",
                put(assign_room).delete(unassign_room),
            )
            .layer(Extension(registry))
            .with_state(state)
    }

    async fn get_devices(
        registry: Arc<Mutex<Registry>>,
        connections: crate::http::state::NodeConnections,
    ) -> serde_json::Value {
        let router = av_router(registry, connections);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/av-devices?token=tok")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn seed_audio_node(registry: &Arc<Mutex<Registry>>, id: &str, backends: &[&str]) {
        let mut reg = registry.lock().unwrap();
        reg.update_heartbeat(shared::NodeIdentity {
            id: id.into(),
            hostname: id.into(),
            ip: "10.0.0.16".into(),
            role: shared::NodeRole::Compute,
        });
        reg.update_capabilities(
            id,
            shared::NodeCapabilities {
                features: vec![shared::Feature::Audio],
                audio_backends: backends.iter().map(|s| s.to_string()).collect(),
                ..shared::NodeCapabilities::default()
            },
        );
    }

    #[tokio::test]
    async fn empty_registry_lists_nothing() {
        let json = get_devices(make_registry(), empty_connections()).await;
        assert_eq!(json["devices"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn each_backend_is_its_own_device() {
        let registry = make_registry();
        seed_audio_node(&registry, "pi2", &["hdmi", "bluetooth"]);
        let json = get_devices(registry, empty_connections()).await;
        let devices = json["devices"].as_array().unwrap();
        let ids: Vec<&str> = devices.iter().map(|d| d["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["pi2:hdmi", "pi2:bluetooth"]);
        assert_eq!(devices[0]["transport"], "hdmi");
        assert_eq!(devices[0]["online"], false); // no live connection
        assert_eq!(devices[0]["kind"], "sink");
    }

    #[tokio::test]
    async fn room_assignments_and_custom_names_come_from_prefs() {
        let registry = make_registry();
        seed_audio_node(&registry, "pi2", &["hdmi", "bluetooth"]);
        {
            let reg = registry.lock().unwrap();
            reg.set_preference(
                PREF_USER_ID,
                "room-audio-sink:Kitchen",
                "pi2:bluetooth:reply",
            );
            // Bare node id → that node's default (first) backend.
            reg.set_preference(PREF_USER_ID, "room-audio-sink:Lounge", "pi2");
            reg.set_preference(PREF_USER_ID, "av-name:pi2:bluetooth", "Fishman amp");
        }
        let json = get_devices(registry, empty_connections()).await;
        let devices = json["devices"].as_array().unwrap();
        let hdmi = devices.iter().find(|d| d["id"] == "pi2:hdmi").unwrap();
        let bt = devices.iter().find(|d| d["id"] == "pi2:bluetooth").unwrap();
        assert_eq!(
            bt["rooms"],
            serde_json::json!([{"room": "Kitchen", "role": "reply"}])
        );
        assert_eq!(bt["name"], "Fishman amp");
        assert_eq!(
            hdmi["rooms"],
            serde_json::json!([{"room": "Lounge", "role": "any"}])
        );
    }

    #[tokio::test]
    async fn a_room_can_hold_more_than_one_sink() {
        let registry = make_registry();
        seed_audio_node(&registry, "pi2", &["hdmi", "bluetooth"]);
        add_room_sink(
            "Kitchen",
            "pi2",
            Some("bluetooth"),
            SinkRole::Reply,
            &registry,
        );
        add_room_sink("Kitchen", "pi2", Some("hdmi"), SinkRole::Media, &registry);
        let json = get_devices(registry, empty_connections()).await;
        let devices = json["devices"].as_array().unwrap();
        let hdmi = devices.iter().find(|d| d["id"] == "pi2:hdmi").unwrap();
        let bt = devices.iter().find(|d| d["id"] == "pi2:bluetooth").unwrap();
        assert_eq!(
            bt["rooms"],
            serde_json::json!([{"room": "Kitchen", "role": "reply"}])
        );
        assert_eq!(
            hdmi["rooms"],
            serde_json::json!([{"room": "Kitchen", "role": "media"}])
        );
    }

    #[tokio::test]
    async fn appliances_appear_once_their_ip_is_configured() {
        let registry = make_registry();
        registry
            .lock()
            .unwrap()
            .set_preference(PREF_USER_ID, "soundbar-ip", "10.0.0.20");
        let json = get_devices(registry, empty_connections()).await;
        let devices = json["devices"].as_array().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["id"], "appliance:soundbar");
        assert_eq!(devices[0]["kind"], "appliance");
        assert_eq!(devices[0]["online"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn connected_node_reports_online() {
        let registry = make_registry();
        seed_audio_node(&registry, "pi2", &["hdmi"]);
        let connections = empty_connections();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        connections.lock().unwrap().insert("pi2".into(), tx);
        let json = get_devices(registry, connections).await;
        assert_eq!(json["devices"][0]["online"], true);
    }

    #[tokio::test]
    async fn puck_room_comes_from_the_av_room_preference() {
        let registry = make_registry();
        {
            let mut reg = registry.lock().unwrap();
            reg.update_heartbeat(shared::NodeIdentity {
                id: "pi1".into(),
                hostname: "pi1".into(),
                ip: "10.0.0.10".into(),
                role: shared::NodeRole::Compute,
            });
            reg.update_capabilities(
                "pi1",
                shared::NodeCapabilities {
                    features: vec![shared::Feature::Voice],
                    ..shared::NodeCapabilities::default()
                },
            );
            reg.set_preference(PREF_USER_ID, "av-room:puck", "Kitchen");
        }
        let json = get_devices(registry, empty_connections()).await;
        let devices = json["devices"].as_array().unwrap();
        let puck = devices.iter().find(|d| d["id"] == "puck").unwrap();
        assert_eq!(
            puck["rooms"],
            serde_json::json!([{"room": "Kitchen", "role": "any"}])
        );
    }

    #[tokio::test]
    async fn legacy_audio_node_without_backend_list_is_one_bare_sink() {
        let registry = make_registry();
        seed_audio_node(&registry, "old-pi", &[]);
        let json = get_devices(registry, empty_connections()).await;
        let devices = json["devices"].as_array().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["id"], "old-pi");
        assert_eq!(devices[0]["transport"], "audio");
    }

    #[tokio::test]
    async fn assign_room_endpoint_adds_without_clobbering_a_sibling_backend() {
        let registry = make_registry();
        seed_audio_node(&registry, "pi2", &["hdmi", "bluetooth"]);
        let router = av_router(registry.clone(), empty_connections());
        for (path, role) in [
            (
                "/api/av-devices/pi2:bluetooth/rooms/Kitchen?token=tok",
                "reply",
            ),
            ("/api/av-devices/pi2:hdmi/rooms/Kitchen?token=tok", "media"),
        ] {
            let resp = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(format!(r#"{{"role":"{role}"}}"#)))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }
        let sinks = crate::audio::resolve_room_sinks("Kitchen", &registry);
        assert_eq!(
            sinks.len(),
            2,
            "both backends should be assigned: {sinks:?}"
        );
    }

    #[tokio::test]
    async fn unassign_room_endpoint_removes_only_that_sink() {
        let registry = make_registry();
        seed_audio_node(&registry, "pi2", &["hdmi", "bluetooth"]);
        add_room_sink(
            "Kitchen",
            "pi2",
            Some("bluetooth"),
            SinkRole::Reply,
            &registry,
        );
        add_room_sink("Kitchen", "pi2", Some("hdmi"), SinkRole::Media, &registry);
        let router = av_router(registry.clone(), empty_connections());
        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/av-devices/pi2:bluetooth/rooms/Kitchen?token=tok")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let sinks = crate::audio::resolve_room_sinks("Kitchen", &registry);
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].sink.as_deref(), Some("hdmi"));
    }
}
