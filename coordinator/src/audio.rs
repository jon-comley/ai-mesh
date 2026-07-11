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
/// `pending_intents` map `server.rs` already uses for IntentResponse —
/// `TtsResponse` is just another reply type through the same
/// pending-request-id mechanism.
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

/// What a room-assigned sink is *for*. A room can have more than one sink
/// at once (e.g. a Bluetooth speaker for quiet spoken replies and a
/// separate HDMI/TV chain for media) — role is how the resolver picks the
/// right one for a given purpose instead of the first thing it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkRole {
    /// Handles both purposes — the historical (and still-default) meaning
    /// of a bare assignment with no role specified.
    Any,
    /// Spoken voice-assistant replies.
    Reply,
    /// Music/media/announcements explicitly targeting this room.
    Media,
}

impl SinkRole {
    pub fn as_str(self) -> &'static str {
        match self {
            SinkRole::Any => "any",
            SinkRole::Reply => "reply",
            SinkRole::Media => "media",
        }
    }

    pub fn parse(s: &str) -> SinkRole {
        match s {
            "reply" => SinkRole::Reply,
            "media" => SinkRole::Media,
            _ => SinkRole::Any,
        }
    }

    /// Whether a sink tagged with this role should be used for a request
    /// that wants `wanted`. `Any` satisfies every purpose; otherwise the
    /// roles must match exactly.
    fn serves(self, wanted: SinkRole) -> bool {
        self == SinkRole::Any || self == wanted
    }
}

/// One sink assigned to a room, parsed from a `room-audio-sink:<room>`
/// preference entry.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomSink {
    pub node_id: String,
    /// Which of the node's `AUDIO_BACKENDS` to use — `None` means that
    /// node's default (first-configured) backend.
    pub sink: Option<String>,
    pub role: SinkRole,
}

impl RoomSink {
    pub fn parse(entry: &str) -> Option<RoomSink> {
        let entry = entry.trim();
        if entry.is_empty() {
            return None;
        }
        let mut parts = entry.splitn(3, ':');
        let node_id = parts.next()?.to_string();
        let sink = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
        let role = parts.next().map(SinkRole::parse).unwrap_or(SinkRole::Any);
        Some(RoomSink {
            node_id,
            sink,
            role,
        })
    }

    fn render(&self) -> String {
        format!(
            "{}:{}:{}",
            self.node_id,
            self.sink.as_deref().unwrap_or(""),
            self.role.as_str()
        )
    }

    /// The `<node_id>` or `<node_id>:<sink>` id this sink is addressed by
    /// elsewhere (AV device ids, `AudioPlayRequest.sink`) — role isn't
    /// part of a sink's identity, just how a room uses it.
    pub fn device_id(&self) -> String {
        match &self.sink {
            Some(sink) => format!("{}:{}", self.node_id, sink),
            None => self.node_id.clone(),
        }
    }
}

fn room_sink_pref_key(room: &str) -> String {
    format!("room-audio-sink:{room}")
}

/// Parse a raw `room-audio-sink:<room>` preference value (comma-separated
/// entries) into its sink list — the one place this format is parsed, so
/// the dashboard's listing endpoint (`http/api/av.rs`) and the resolver
/// that actually routes audio can never drift apart.
pub fn parse_room_sink_list(value: &str) -> Vec<RoomSink> {
    value.split(',').filter_map(RoomSink::parse).collect()
}

/// Every sink currently assigned to a room (`room-audio-sink:<room>`,
/// comma-separated entries, same K/V preferences store `tts-voice`/
/// `voice-in-chat` already use — no new schema). Empty means "no
/// dedicated speaker for this room," the caller's cue to fall back to
/// whatever room-independent default it already has (the puck, for the
/// voice pipeline).
pub fn resolve_room_sinks(room: &str, registry: &Arc<Mutex<Registry>>) -> Vec<RoomSink> {
    let Some(value) = registry
        .lock()
        .unwrap()
        .get_preference(PREF_USER_ID, &room_sink_pref_key(room))
    else {
        return vec![];
    };
    parse_room_sink_list(&value)
}

/// The first room sink that serves `role` — e.g. the sink a spoken reply
/// in this room should route to. `None` means no sink in this room
/// handles that purpose (an `Any`-tagged sink counts for every purpose).
pub fn resolve_room_sink_for(
    room: &str,
    role: SinkRole,
    registry: &Arc<Mutex<Registry>>,
) -> Option<RoomSink> {
    resolve_room_sinks(room, registry)
        .into_iter()
        .find(|s| s.role.serves(role))
}

/// Add (or update the role of) one sink in a room's assignment list,
/// without disturbing any other sink already assigned there — read the
/// current list, upsert by `(node_id, sink)`, write the whole value back.
/// Registry access is mutex-serialized so this is safe against concurrent
/// dashboard writes to the *same* room; it is not a general CRDT, just
/// enough for one user's dashboard.
pub fn add_room_sink(
    room: &str,
    node_id: &str,
    sink: Option<&str>,
    role: SinkRole,
    registry: &Arc<Mutex<Registry>>,
) {
    let reg = registry.lock().unwrap();
    let mut sinks: Vec<RoomSink> = reg
        .get_preference(PREF_USER_ID, &room_sink_pref_key(room))
        .map(|v| parse_room_sink_list(&v))
        .unwrap_or_default();
    let entry = RoomSink {
        node_id: node_id.to_string(),
        sink: sink.map(str::to_string),
        role,
    };
    match sinks
        .iter_mut()
        .find(|s| s.node_id == node_id && s.sink.as_deref() == sink)
    {
        Some(existing) => existing.role = role,
        None => sinks.push(entry),
    }
    let value = sinks
        .iter()
        .map(RoomSink::render)
        .collect::<Vec<_>>()
        .join(",");
    reg.set_preference(PREF_USER_ID, &room_sink_pref_key(room), &value);
}

/// Remove one sink from a room's assignment list, leaving any other sinks
/// assigned there untouched. Deletes the preference entirely once the
/// list would be empty, rather than leaving a dangling empty value.
pub fn remove_room_sink(
    room: &str,
    node_id: &str,
    sink: Option<&str>,
    registry: &Arc<Mutex<Registry>>,
) {
    let reg = registry.lock().unwrap();
    let key = room_sink_pref_key(room);
    let remaining: Vec<RoomSink> = reg
        .get_preference(PREF_USER_ID, &key)
        .map(|v| parse_room_sink_list(&v))
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !(s.node_id == node_id && s.sink.as_deref() == sink))
        .collect();
    if remaining.is_empty() {
        reg.delete_preference(PREF_USER_ID, &key);
    } else {
        let value = remaining
            .iter()
            .map(RoomSink::render)
            .collect::<Vec<_>>()
            .join(",");
        reg.set_preference(PREF_USER_ID, &key, &value);
    }
}

/// Node reports back within this long, or it counts as a failed delivery.
///
/// This has to cover a cold Bluetooth reconnect AND the clip's actual
/// playback duration — `AudioPlayResult` is only sent after `play_url()`'s
/// `paplay`/`aplay` process exits, i.e. after the whole reply has finished
/// playing, not after playback merely started. A longer spoken reply can
/// easily run 10-20+ seconds; the previous 10s value here (and the
/// matching `ANNOUNCE_RESULT_TIMEOUT` in capability-voice, which wraps
/// this call) was sized as if this were a quick dispatch ack, so it fired
/// before a perfectly successful delivery could ever report back
/// (confirmed live 2026-07-12).
const AUDIO_PLAY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Send `url` to a specific node's tracked connection, on the named
/// `sink` (or that node's default if `None`), and wait for the node's own
/// `AudioPlayResult` ack. `false` covers "node not connected" *and*
/// "connected but `aplay`/`paplay` actually failed" (unpaired Bluetooth
/// sink, wrong ALSA device, etc.) — without waiting for the real result,
/// this would report success as soon as the message left the coordinator,
/// which is exactly how a broken sink can silently swallow a reply instead
/// of triggering the caller's fallback (the voice pipeline falls back to
/// the puck; a broadcast caller just logs and continues with the other
/// targets).
async fn send_audio_play(
    node_id: &str,
    url: &str,
    sink: Option<&str>,
    connections: &Connections,
    pending_intents: &PendingIntents,
) -> bool {
    let Some(tx) = connections.lock().unwrap().get(node_id).cloned() else {
        return false;
    };

    let request_id = gen_request_id();
    let (otx, orx) = oneshot::channel();
    pending_intents
        .lock()
        .unwrap()
        .insert(request_id.clone(), otx);

    if tx
        .send(MeshMessage::AudioPlay(AudioPlayRequest {
            request_id: request_id.clone(),
            url: url.to_string(),
            sink: sink.map(str::to_string),
        }))
        .await
        .is_err()
    {
        pending_intents.lock().unwrap().remove(&request_id);
        return false;
    }

    match tokio::time::timeout(AUDIO_PLAY_TIMEOUT, orx).await {
        Ok(Ok(MeshMessage::AudioPlayResult(shared::AudioPlayResult {
            success, error, ..
        }))) => {
            if let Some(err) = error.filter(|_| !success) {
                warn!(node_id, error = %err, "audio: node reported playback failure");
            }
            success
        }
        Ok(Ok(_)) => false,
        Ok(Err(_)) => false,
        Err(_) => {
            pending_intents.lock().unwrap().remove(&request_id);
            warn!(node_id, "audio: playback ack timed out");
            false
        }
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
    pending_intents: &PendingIntents,
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
            if send_audio_play(&node_id, &req.url, None, connections, pending_intents).await {
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
    // Non-broadcast AudioAnnounce only ever originates from the voice
    // pipeline routing a spoken reply — always the Reply-role sink, never
    // whatever the room's Media sink happens to be.
    let Some(room_sink) = resolve_room_sink_for(&room, SinkRole::Reply, registry) else {
        // Not a warning: most rooms simply have no dedicated speaker yet
        // (only the kitchen does, once Phase 2 hardware exists) — the
        // voice pipeline's own puck fallback is the expected outcome.
        return false;
    };
    if send_audio_play(
        &room_sink.node_id,
        &req.url,
        room_sink.sink.as_deref(),
        connections,
        pending_intents,
    )
    .await
    {
        true
    } else {
        warn!(
            room,
            node_id = %room_sink.node_id,
            sink = %room_sink.sink.as_deref().unwrap_or("default"),
            "room audio sink not connected"
        );
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
        pending_intents,
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
    fn resolve_room_sinks_reads_the_preference() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        registry.lock().unwrap().set_preference(
            PREF_USER_ID,
            "room-audio-sink:Kitchen",
            "pi-zero-1",
        );
        assert_eq!(
            resolve_room_sinks("Kitchen", &registry),
            vec![RoomSink {
                node_id: "pi-zero-1".into(),
                sink: None,
                role: SinkRole::Any,
            }]
        );
        assert!(resolve_room_sinks("Bedroom", &registry).is_empty());
    }

    #[test]
    fn resolve_room_sinks_parses_an_explicit_sink_suffix() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        registry.lock().unwrap().set_preference(
            PREF_USER_ID,
            "room-audio-sink:LivingRoom",
            "pi2:bluetooth",
        );
        assert_eq!(
            resolve_room_sinks("LivingRoom", &registry),
            vec![RoomSink {
                node_id: "pi2".into(),
                sink: Some("bluetooth".into()),
                role: SinkRole::Any,
            }]
        );
    }

    #[test]
    fn add_room_sink_appends_without_disturbing_existing_entries() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        add_room_sink(
            "Kitchen",
            "pi2",
            Some("bluetooth"),
            SinkRole::Reply,
            &registry,
        );
        add_room_sink("Kitchen", "pi2", Some("hdmi"), SinkRole::Media, &registry);
        let sinks = resolve_room_sinks("Kitchen", &registry);
        assert_eq!(sinks.len(), 2, "both sinks should coexist: {sinks:?}");
        assert!(
            sinks
                .iter()
                .any(|s| s.sink.as_deref() == Some("bluetooth") && s.role == SinkRole::Reply)
        );
        assert!(
            sinks
                .iter()
                .any(|s| s.sink.as_deref() == Some("hdmi") && s.role == SinkRole::Media)
        );
    }

    #[test]
    fn add_room_sink_updates_role_in_place_for_the_same_sink() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        add_room_sink(
            "Kitchen",
            "pi2",
            Some("bluetooth"),
            SinkRole::Any,
            &registry,
        );
        add_room_sink(
            "Kitchen",
            "pi2",
            Some("bluetooth"),
            SinkRole::Reply,
            &registry,
        );
        let sinks = resolve_room_sinks("Kitchen", &registry);
        assert_eq!(
            sinks.len(),
            1,
            "re-adding the same sink should update, not duplicate"
        );
        assert_eq!(sinks[0].role, SinkRole::Reply);
    }

    #[test]
    fn remove_room_sink_leaves_other_sinks_in_the_room_intact() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        add_room_sink(
            "Kitchen",
            "pi2",
            Some("bluetooth"),
            SinkRole::Reply,
            &registry,
        );
        add_room_sink("Kitchen", "pi2", Some("hdmi"), SinkRole::Media, &registry);
        remove_room_sink("Kitchen", "pi2", Some("bluetooth"), &registry);
        let sinks = resolve_room_sinks("Kitchen", &registry);
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].sink.as_deref(), Some("hdmi"));
    }

    #[test]
    fn remove_room_sink_deletes_the_preference_once_empty() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        add_room_sink(
            "Kitchen",
            "pi2",
            Some("bluetooth"),
            SinkRole::Any,
            &registry,
        );
        remove_room_sink("Kitchen", "pi2", Some("bluetooth"), &registry);
        assert!(
            registry
                .lock()
                .unwrap()
                .get_preference(PREF_USER_ID, "room-audio-sink:Kitchen")
                .is_none()
        );
    }

    #[test]
    fn resolve_room_sink_for_role_is_exclusive_when_no_any_sink_exists() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        add_room_sink(
            "Kitchen",
            "pi2",
            Some("bluetooth"),
            SinkRole::Reply,
            &registry,
        );
        // A Reply-only sink must not answer a Media request — that's the
        // whole point of the split (a quiet reply speaker shouldn't also
        // be where "play music" goes unless it's actually tagged for it).
        assert!(resolve_room_sink_for("Kitchen", SinkRole::Reply, &registry).is_some());
        assert!(resolve_room_sink_for("Kitchen", SinkRole::Media, &registry).is_none());
    }

    #[test]
    fn resolve_room_sink_for_role_finds_the_matching_sink_among_several() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        add_room_sink(
            "Kitchen",
            "pi2",
            Some("bluetooth"),
            SinkRole::Reply,
            &registry,
        );
        add_room_sink("Kitchen", "pi2", Some("hdmi"), SinkRole::Media, &registry);
        let reply = resolve_room_sink_for("Kitchen", SinkRole::Reply, &registry).unwrap();
        let media = resolve_room_sink_for("Kitchen", SinkRole::Media, &registry).unwrap();
        assert_eq!(reply.sink.as_deref(), Some("bluetooth"));
        assert_eq!(media.sink.as_deref(), Some("hdmi"));
    }

    #[test]
    fn resolve_room_sink_for_any_role_serves_every_purpose() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        add_room_sink("Office", "node-x", None, SinkRole::Any, &registry);
        assert!(resolve_room_sink_for("Office", SinkRole::Reply, &registry).is_some());
        assert!(resolve_room_sink_for("Office", SinkRole::Media, &registry).is_some());
    }

    fn empty_pending_intents() -> PendingIntents {
        Arc::new(Mutex::new(HashMap::new()))
    }

    /// Drains one `AudioPlay` from `rx` and resolves its pending intent
    /// with a successful `AudioPlayResult`, simulating the node's ack so
    /// tests don't have to wait out `AUDIO_PLAY_TIMEOUT`.
    async fn ack_next_audio_play(
        rx: &mut tokio::sync::mpsc::Receiver<MeshMessage>,
        pending_intents: &PendingIntents,
    ) {
        if let Some(MeshMessage::AudioPlay(req)) = rx.recv().await
            && let Some(otx) = pending_intents.lock().unwrap().remove(&req.request_id)
        {
            let _ = otx.send(MeshMessage::AudioPlayResult(shared::AudioPlayResult {
                request_id: req.request_id,
                success: true,
                error: None,
            }));
        }
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
            &empty_pending_intents(),
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
            &empty_pending_intents(),
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
        let pending_intents = empty_pending_intents();
        let (delivered, _) = tokio::join!(
            handle_audio_announce(
                shared::AudioAnnounceRequest {
                    request_id: "r1".into(),
                    url: "http://example/clip.wav".into(),
                    room: Some("Kitchen".into()),
                    broadcast: false,
                },
                &registry,
                &connections,
                &pending_intents,
            ),
            ack_next_audio_play(&mut rx, &pending_intents)
        );
        assert!(delivered);
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
        let pending_intents = empty_pending_intents();
        let (delivered, sink_seen) = tokio::join!(
            handle_audio_announce(
                shared::AudioAnnounceRequest {
                    request_id: "r1".into(),
                    url: "http://example/clip.wav".into(),
                    room: Some("Office".into()),
                    broadcast: false,
                },
                &registry,
                &connections,
                &pending_intents,
            ),
            async {
                let msg = rx.recv().await;
                let sink = match &msg {
                    Some(MeshMessage::AudioPlay(req)) => req.sink.clone(),
                    other => panic!("expected AudioPlay, got {other:?}"),
                };
                if let Some(MeshMessage::AudioPlay(req)) = msg
                    && let Some(otx) = pending_intents.lock().unwrap().remove(&req.request_id)
                {
                    let _ = otx.send(MeshMessage::AudioPlayResult(shared::AudioPlayResult {
                        request_id: req.request_id,
                        success: true,
                        error: None,
                    }));
                }
                sink
            }
        );
        assert!(delivered);
        assert_eq!(sink_seen, Some("bluetooth".into()));
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
        let pending_intents = empty_pending_intents();

        let (delivered, ..) = tokio::join!(
            handle_audio_announce(
                shared::AudioAnnounceRequest {
                    request_id: "r1".into(),
                    url: "http://example/clip.wav".into(),
                    room: None,
                    broadcast: true,
                },
                &registry,
                &connections,
                &pending_intents,
            ),
            ack_next_audio_play(&mut rx_a, &pending_intents),
            ack_next_audio_play(&mut rx_b, &pending_intents)
        );

        assert!(delivered);
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
            &empty_pending_intents(),
        )
        .await;
        assert!(!delivered);
    }
}
