//! Coordinator-side audio routing (Phase 2/3/6 of
//! `plans/audio-output-integration.md`): resolving which node should play
//! a clip (a specific room's configured sink, or every audio-capable node
//! at once for a broadcast), and getting a coordinator-initiated
//! announcement synthesized at all — an announcement that didn't start as
//! a spoken request has no transcript-side node to ask, so this reaches
//! out to whichever node is running Piper the same way `intent.rs`'s
//! `connected_feature_node` reaches a lighting node.
//!
//! Deliberately not a new agent capability: unlike audio *playback*
//! (Phase 2/3, genuinely tied to a specific physical Pi near a speaker),
//! nothing here needs to run on a mesh node at all — it's the coordinator
//! resolving registry state and sending mesh messages, the same shape as
//! `intent.rs`'s dispatch functions.

use std::sync::{Arc, Mutex};

use shared::{AudioPlayRequest, MeshMessage, TtsRequest, TtsResponse};
use tokio::sync::oneshot;
use tracing::warn;

use crate::http::api::prefs::PREF_USER_ID;
use crate::http::state::PendingIntents;
use crate::registry::Registry;
use crate::server::Connections;

/// How long a coordinator-initiated synthesis is allowed to take. Shorter
/// than `capability-voice`'s own `VOICE_INTENT_TIMEOUT_SECS` (60s) —
/// there's no LLM call in this path, just Piper, which is sub-second once
/// warm (see `plans/audio-output-integration.md`'s Phase 1 record).
const TTS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

fn gen_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// First connected node advertising `feature`, with its sender — same
/// shape as `intent.rs`'s private `connected_feature_node`, duplicated
/// rather than made `pub(crate)` there to keep that module's surface
/// intent-specific; if a third call site appears, promote it instead.
fn connected_feature_node(
    feature: shared::Feature,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
) -> Option<(String, tokio::sync::mpsc::Sender<MeshMessage>)> {
    let candidates: Vec<String> = registry
        .lock()
        .unwrap()
        .nodes_with_feature(feature)
        .into_iter()
        .map(|n| n.id)
        .collect();
    let conns = connections.lock().unwrap();
    candidates
        .into_iter()
        .find_map(|id| conns.get(&id).cloned().map(|tx| (id, tx)))
}

/// Ask whichever connected node is running Piper to synthesize `text`,
/// and return the fetchable clip URL it hands back. Mirrors the
/// IntentRequest/IntentResponse request/reply shape via the same
/// `pending_intents` map `server.rs` already uses for SceneLoaded and
/// IntentResponse — `TtsResponse` is just another reply type through the
/// same pending-request-id mechanism.
pub async fn request_tts(
    text: &str,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_intents: &PendingIntents,
) -> Result<String, String> {
    let Some((_, tx)) = connected_feature_node(shared::Feature::Voice, registry, connections)
    else {
        return Err("no voice-capable node connected".into());
    };

    let request_id = gen_request_id();
    let (otx, orx) = oneshot::channel();
    pending_intents
        .lock()
        .unwrap()
        .insert(request_id.clone(), otx);

    if tx
        .send(MeshMessage::TtsRequest(TtsRequest {
            request_id: request_id.clone(),
            text: text.to_string(),
        }))
        .await
        .is_err()
    {
        pending_intents.lock().unwrap().remove(&request_id);
        return Err("voice node disconnected before request could be sent".into());
    }

    match tokio::time::timeout(TTS_TIMEOUT, orx).await {
        Ok(Ok(MeshMessage::TtsResponse(TtsResponse { url: Some(url), .. }))) => Ok(url),
        Ok(Ok(MeshMessage::TtsResponse(TtsResponse {
            error: Some(err), ..
        }))) => Err(err),
        Ok(Ok(_)) => Err("unexpected reply type for TtsRequest".into()),
        Ok(Err(_)) => Err("voice node dropped the request".into()),
        Err(_) => {
            pending_intents.lock().unwrap().remove(&request_id);
            Err("TTS request timed out".into())
        }
    }
}

/// The registry-stored preferred audio sink for a room, if one's been
/// configured (`room-audio-sink:<room>` in the same K/V preferences store
/// `tts-voice`/`voice-in-chat` already use — no new schema). `None` means
/// "no dedicated speaker for this room," the caller's cue to fall back to
/// whatever room-independent default it already has (the puck, for the
/// voice pipeline).
///
/// The preference value is `<node_id>` or `<node_id>:<sink>` — the sink
/// suffix picks which of that node's `AUDIO_BACKENDS` to use (a node can
/// run more than one backend at once, e.g. HDMI to a TV *and* Bluetooth to
/// a room speaker on the same Pi — see `capabilities/audio`). No suffix
/// means "that node's default backend."
pub fn resolve_room_sink(
    room: &str,
    registry: &Arc<Mutex<Registry>>,
) -> Option<(String, Option<String>)> {
    let value = registry
        .lock()
        .unwrap()
        .get_preference(PREF_USER_ID, &format!("room-audio-sink:{room}"))?;
    Some(match value.split_once(':') {
        Some((node_id, sink)) => (node_id.to_string(), Some(sink.to_string())),
        None => (value, None),
    })
}

/// Send `url` to a specific node's tracked connection, on the named
/// `sink` (or that node's default if `None`). `false` if the node isn't
/// currently connected — callers decide what that means for them (the
/// voice pipeline's caller falls back to the puck; a broadcast caller
/// just logs and continues with the other targets).
async fn send_audio_play(
    node_id: &str,
    url: &str,
    sink: Option<&str>,
    connections: &Connections,
) -> bool {
    let tx = connections.lock().unwrap().get(node_id).cloned();
    match tx {
        Some(tx) => tx
            .send(MeshMessage::AudioPlay(AudioPlayRequest {
                request_id: gen_request_id(),
                url: url.to_string(),
                sink: sink.map(str::to_string),
            }))
            .await
            .is_ok(),
        None => false,
    }
}

/// Handle an `AudioAnnounceRequest` arriving from any agent (currently:
/// the voice pipeline, wanting its reply routed to a room's speaker
/// instead of the puck). Resolves target(s) and fans the clip out —
/// independent parallel sends, no synchronization between them, matching
/// the "broadcast is just send-to-every-sink" design from
/// `plans/audio-output-integration.md`'s Phase 6.
///
/// Returns whether the clip actually reached a connected sink — `false`
/// covers both "no sink configured" and "sink configured but not
/// currently connected." The caller (`server.rs`) reports this back to
/// the requesting node via `AudioAnnounceResult` so a room-routed voice
/// reply can fall back to the puck instead of silently going nowhere;
/// see the doc comment on `capability-voice`'s `pending_announce`.
pub async fn handle_audio_announce(
    req: shared::AudioAnnounceRequest,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
) -> bool {
    if req.broadcast {
        let targets: Vec<String> = registry
            .lock()
            .unwrap()
            .nodes_with_feature(shared::Feature::Audio)
            .into_iter()
            .map(|n| n.id)
            .collect();
        if targets.is_empty() {
            warn!("audio broadcast requested but no Feature::Audio nodes are known");
        }
        let mut delivered = false;
        for node_id in targets {
            // Broadcast alerts use each node's own default backend — no
            // per-room sink selection makes sense for "reach the whole
            // house at once".
            if send_audio_play(&node_id, &req.url, None, connections).await {
                delivered = true;
            } else {
                warn!(node_id, "audio broadcast: node not connected, skipped");
            }
        }
        return delivered;
    }

    let Some(room) = req.room else {
        warn!("AudioAnnounceRequest with neither room nor broadcast set — nothing to do");
        return false;
    };
    let Some((node_id, sink)) = resolve_room_sink(&room, registry) else {
        // Not a warning: most rooms simply have no dedicated speaker yet
        // (only the kitchen does, once Phase 2 hardware exists) — the
        // voice pipeline's own puck fallback is the expected outcome.
        return false;
    };
    if send_audio_play(&node_id, &req.url, sink.as_deref(), connections).await {
        true
    } else {
        warn!(room, node_id, "room audio sink not connected");
        false
    }
}

/// Synthesize `text` and broadcast it to every audio-capable node —
/// the coordinator-initiated counterpart to a spoken reply, for alerts
/// ("someone's at the door") that didn't originate from a voice request.
/// Used by both the `play_announcement` intent tool and a future
/// dashboard "announce" button — one function, two callers, matching
/// how `handle_intent` itself is shared between the dashboard and CLI.
pub async fn broadcast_announcement(
    text: &str,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_intents: &PendingIntents,
) -> Result<(), String> {
    let url = request_tts(text, registry, connections, pending_intents).await?;
    handle_audio_announce(
        shared::AudioAnnounceRequest {
            request_id: gen_request_id(),
            url,
            room: None,
            broadcast: true,
        },
        registry,
        connections,
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use shared::hardware::{NodeCapabilities, NodeIdentity, NodeRole};
    use std::collections::HashMap;

    fn make_identity(id: &str) -> NodeIdentity {
        NodeIdentity {
            id: id.into(),
            hostname: id.into(),
            role: NodeRole::Compute,
            ip: "127.0.0.1".into(),
        }
    }

    #[test]
    fn resolve_room_sink_reads_the_preference() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        registry.lock().unwrap().set_preference(
            PREF_USER_ID,
            "room-audio-sink:Kitchen",
            "pi-zero-1",
        );
        assert_eq!(
            resolve_room_sink("Kitchen", &registry),
            Some(("pi-zero-1".into(), None))
        );
        assert_eq!(resolve_room_sink("Bedroom", &registry), None);
    }

    #[test]
    fn resolve_room_sink_parses_an_explicit_sink_suffix() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        registry.lock().unwrap().set_preference(
            PREF_USER_ID,
            "room-audio-sink:LivingRoom",
            "pi2:bluetooth",
        );
        assert_eq!(
            resolve_room_sink("LivingRoom", &registry),
            Some(("pi2".into(), Some("bluetooth".into())))
        );
    }

    #[tokio::test]
    async fn handle_audio_announce_room_with_no_sink_is_a_quiet_noop() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let connections: Connections = Arc::new(Mutex::new(HashMap::new()));
        // Must not panic and must not require any connection to exist.
        let delivered = handle_audio_announce(
            shared::AudioAnnounceRequest {
                request_id: "r1".into(),
                url: "http://example/clip.wav".into(),
                room: Some("Nowhere".into()),
                broadcast: false,
            },
            &registry,
            &connections,
        )
        .await;
        assert!(!delivered);
    }

    #[tokio::test]
    async fn handle_audio_announce_room_sink_configured_but_disconnected_reports_undelivered() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        registry.lock().unwrap().set_preference(
            PREF_USER_ID,
            "room-audio-sink:Kitchen",
            "pi-zero-1",
        );
        // No connection registered for "pi-zero-1" — configured but offline.
        let connections: Connections = Arc::new(Mutex::new(HashMap::new()));
        let delivered = handle_audio_announce(
            shared::AudioAnnounceRequest {
                request_id: "r1".into(),
                url: "http://example/clip.wav".into(),
                room: Some("Kitchen".into()),
                broadcast: false,
            },
            &registry,
            &connections,
        )
        .await;
        assert!(!delivered);
    }

    #[tokio::test]
    async fn handle_audio_announce_room_sink_connected_reports_delivered() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        registry.lock().unwrap().set_preference(
            PREF_USER_ID,
            "room-audio-sink:Kitchen",
            "pi-zero-1",
        );
        let connections: Connections = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        connections.lock().unwrap().insert("pi-zero-1".into(), tx);
        let delivered = handle_audio_announce(
            shared::AudioAnnounceRequest {
                request_id: "r1".into(),
                url: "http://example/clip.wav".into(),
                room: Some("Kitchen".into()),
                broadcast: false,
            },
            &registry,
            &connections,
        )
        .await;
        assert!(delivered);
        assert!(matches!(rx.try_recv(), Ok(MeshMessage::AudioPlay(_))));
    }

    #[tokio::test]
    async fn handle_audio_announce_room_sink_with_explicit_backend_is_forwarded() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        registry.lock().unwrap().set_preference(
            PREF_USER_ID,
            "room-audio-sink:Office",
            "pi2:bluetooth",
        );
        let connections: Connections = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        connections.lock().unwrap().insert("pi2".into(), tx);
        let delivered = handle_audio_announce(
            shared::AudioAnnounceRequest {
                request_id: "r1".into(),
                url: "http://example/clip.wav".into(),
                room: Some("Office".into()),
                broadcast: false,
            },
            &registry,
            &connections,
        )
        .await;
        assert!(delivered);
        match rx.try_recv() {
            Ok(MeshMessage::AudioPlay(req)) => assert_eq!(req.sink, Some("bluetooth".into())),
            other => panic!("expected AudioPlay, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_audio_announce_broadcast_sends_to_every_audio_node() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        registry
            .lock()
            .unwrap()
            .update_heartbeat(make_identity("a"));
        registry.lock().unwrap().update_capabilities(
            "a",
            NodeCapabilities {
                features: vec![shared::Feature::Audio],
                ..NodeCapabilities::default()
            },
        );
        registry
            .lock()
            .unwrap()
            .update_heartbeat(make_identity("b"));
        registry.lock().unwrap().update_capabilities(
            "b",
            NodeCapabilities {
                features: vec![shared::Feature::Audio],
                ..NodeCapabilities::default()
            },
        );
        let connections: Connections = Arc::new(Mutex::new(HashMap::new()));
        let (tx_a, mut rx_a) = tokio::sync::mpsc::channel(4);
        let (tx_b, mut rx_b) = tokio::sync::mpsc::channel(4);
        connections.lock().unwrap().insert("a".into(), tx_a);
        connections.lock().unwrap().insert("b".into(), tx_b);

        let delivered = handle_audio_announce(
            shared::AudioAnnounceRequest {
                request_id: "r1".into(),
                url: "http://example/clip.wav".into(),
                room: None,
                broadcast: true,
            },
            &registry,
            &connections,
        )
        .await;

        assert!(delivered);
        assert!(matches!(rx_a.try_recv(), Ok(MeshMessage::AudioPlay(_))));
        assert!(matches!(rx_b.try_recv(), Ok(MeshMessage::AudioPlay(_))));
    }

    #[tokio::test]
    async fn handle_audio_announce_broadcast_with_no_audio_nodes_reports_undelivered() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let connections: Connections = Arc::new(Mutex::new(HashMap::new()));
        let delivered = handle_audio_announce(
            shared::AudioAnnounceRequest {
                request_id: "r1".into(),
                url: "http://example/clip.wav".into(),
                room: None,
                broadcast: true,
            },
            &registry,
            &connections,
        )
        .await;
        assert!(!delivered);
    }
}
