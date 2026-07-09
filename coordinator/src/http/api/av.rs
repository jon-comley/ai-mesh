//! AV endpoints: the unified "Speakers & displays" inventory behind the
//! dashboard's AV section (Phase A of `plans/av-devices-ui.md`).
//!
//! Read-only — one GET that flattens every audio-capable endpoint the
//! coordinator knows about into a uniform list, whatever its transport:
//! each backend of each `Feature::Audio` node (`pi2:hdmi`,
//! `pi2:bluetooth`, …), the configured LAN appliances (soundbar / TV,
//! present only when their `-ip` preference is set), and the voice puck
//! (inferred from a `Feature::Voice` node). Writes go through the
//! existing preferences API — room assignment is
//! `room-audio-sink:<room>` = `<node_id>:<backend>` and renames are
//! `av-name:<device_id>` — so no new mutation surface is added here.

use axum::{Extension, Json, extract::State, response::IntoResponse};
use serde::Serialize;
use std::sync::{Arc, Mutex};

use crate::http::auth::Authed;
use crate::http::state::DashboardState;
use crate::registry::Registry;

use crate::http::api::prefs::PREF_USER_ID;

#[derive(Serialize)]
pub struct AvDevice {
    /// Stable id: `<node_id>:<backend>` for node sinks, `appliance:<x>`
    /// for LAN appliances, `puck` for the voice puck. This exact string
    /// is what `room-audio-sink:<room>` preferences store.
    pub id: String,
    pub name: String,
    /// "sink" (playable via AudioPlay) or "appliance" (controlled, not a
    /// playback target the mesh can push clips to).
    pub kind: &'static str,
    /// zigbee-equivalent of DeviceType for badging: "hdmi", "bluetooth",
    /// "wifi", …
    pub transport: String,
    /// The mesh node hosting this sink (None for appliances/puck).
    pub node_id: Option<String>,
    pub hostname: Option<String>,
    /// None = unknown (appliances aren't probed), Some = live mesh state.
    pub online: Option<bool>,
    /// Room names whose `room-audio-sink` preference points at this
    /// device. Plural because two rooms may share one sink.
    pub rooms: Vec<String>,
}

/// Room-name → (node, sink) pairs parsed from `room-audio-sink:*`
/// preferences — mirrors `crate::audio::resolve_room_sink`'s parsing.
fn room_sink_prefs(prefs: &[(String, String)]) -> Vec<(String, String, Option<String>)> {
    prefs
        .iter()
        .filter_map(|(k, v)| {
            let room = k.strip_prefix("room-audio-sink:")?;
            let (node, sink) = match v.split_once(':') {
                Some((n, s)) => (n.to_string(), Some(s.to_string())),
                None => (v.clone(), None),
            };
            Some((room.to_string(), node, sink))
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
    let room_sinks = room_sink_prefs(&prefs);

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
                .filter(|(_, n, s)| {
                    n == &node.id
                        && match s {
                            Some(s) => s == backend,
                            // A bare node-id preference means "that
                            // node's default backend".
                            None => Some(backend) == default_backend.as_ref(),
                        }
                })
                .map(|(room, _, _)| room.clone())
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
    // `room_with_audio_sink`).
    if let Some(voice_node) = voice_nodes.first() {
        let rooms = prefs
            .iter()
            .find(|(k, _)| k == "av-room:puck")
            .map(|(_, v)| vec![v.clone()])
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::{empty_connections, make_registry, make_state};
    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;

    async fn get_devices(
        registry: Arc<Mutex<Registry>>,
        connections: crate::http::state::NodeConnections,
    ) -> serde_json::Value {
        let state = make_state(vec!["tok".into()], connections);
        let router = Router::new()
            .route("/api/av-devices", get(list_av_devices))
            .layer(Extension(registry))
            .with_state(state);
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
            reg.set_preference(PREF_USER_ID, "room-audio-sink:Kitchen", "pi2:bluetooth");
            // Bare node id → that node's default (first) backend.
            reg.set_preference(PREF_USER_ID, "room-audio-sink:Lounge", "pi2");
            reg.set_preference(PREF_USER_ID, "av-name:pi2:bluetooth", "Fishman amp");
        }
        let json = get_devices(registry, empty_connections()).await;
        let devices = json["devices"].as_array().unwrap();
        let hdmi = devices.iter().find(|d| d["id"] == "pi2:hdmi").unwrap();
        let bt = devices.iter().find(|d| d["id"] == "pi2:bluetooth").unwrap();
        assert_eq!(bt["rooms"], serde_json::json!(["Kitchen"]));
        assert_eq!(bt["name"], "Fishman amp");
        assert_eq!(hdmi["rooms"], serde_json::json!(["Lounge"]));
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
        assert_eq!(puck["rooms"], serde_json::json!(["Kitchen"]));
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
}
