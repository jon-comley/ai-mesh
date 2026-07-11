use capability_core::Capability;
use shared::MeshMessage;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;
use tracing::{Instrument, warn};

/// Build the capability list for this node from compile-time feature flags.
/// Called once before the reconnect loop; capabilities survive reconnects via Arc.
#[allow(unused_variables)]
pub fn build_capabilities(node_id: &str) -> Vec<Arc<dyn Capability + Send + Sync>> {
    vec![
        #[cfg(feature = "llm")]
        Arc::new(capability_llm::LlmCapability::new(node_id)),
        #[cfg(feature = "lighting")]
        Arc::new(capability_lighting::LightingCapability::new(node_id)),
        #[cfg(feature = "reaper")]
        Arc::new(capability_reaper::ReaperCapability::new(node_id)),
        #[cfg(feature = "sensors")]
        Arc::new(capability_sensors::SensorsCapability::new(node_id)),
        #[cfg(feature = "art")]
        Arc::new(capability_art::ArtCapability::new(node_id)),
        #[cfg(feature = "voice")]
        Arc::new(capability_voice::VoiceCapability::new(node_id)),
        #[cfg(feature = "audio")]
        Arc::new(capability_audio::AudioCapability::new(node_id)),
        #[cfg(feature = "music")]
        Arc::new(capability_music::MusicCapability::new(node_id)),
    ]
}

/// Each capability's dispatch slot, paired with a lock that serializes calls
/// to *that* capability's `handle()` — never blocks a different capability,
/// never blocks the reader loop itself (see `dispatch`'s doc comment).
struct Slot {
    cap: Arc<dyn Capability + Send + Sync>,
    lock: Mutex<()>,
}

/// Built once from `build_capabilities()` and shared (via `Arc`) across
/// every reconnect — a capability's in-flight lock reflects a real ongoing
/// operation (e.g. a `bluetoothctl` command still running) that doesn't
/// reset just because the mesh connection dropped and reconnected.
pub struct DispatchTable {
    slots: Vec<Slot>,
}

impl DispatchTable {
    pub fn new(caps: Vec<Arc<dyn Capability + Send + Sync>>) -> Arc<Self> {
        Arc::new(Self {
            slots: caps
                .into_iter()
                .map(|cap| Slot {
                    cap,
                    lock: Mutex::new(()),
                })
                .collect(),
        })
    }
}

/// Route one inbound message to the first capability that claims it.
/// Unhandled messages are logged and dropped.
///
/// Callers should `tokio::spawn` this rather than awaiting it inline in a
/// message-reading loop: a slow `handle()` call (a Bluetooth scan/pair can
/// run tens of seconds — see `capabilities/audio/src/bluetooth.rs`) would
/// otherwise stall the reader loop and delay every other inbound mesh
/// message, for every capability, until it finished — confirmed as a real
/// risk 2026-07-11 once `PAIR_STEP_TIMEOUT` grew and `clear_cache` started
/// iterating potentially many cached devices. The per-capability lock in
/// `DispatchTable` keeps this safe to spawn freely: two messages for the
/// *same* capability still serialize (critical for Bluetooth — concurrent
/// `bluetoothctl` invocations for one adapter is exactly the kind of race
/// that caused hours of live debugging tonight), while messages for
/// *different* capabilities, or the reader loop reading the next frame,
/// never wait on each other.
pub async fn dispatch(msg: MeshMessage, table: Arc<DispatchTable>, tx: Sender<MeshMessage>) {
    for slot in &table.slots {
        if slot.cap.handles(&msg) {
            // Now that dispatch runs as its own spawned task rather than
            // inline in the reader loop, a plain `warn!`/`info!` inside a
            // capability's `handle()` no longer has any surrounding
            // context tying it to which inbound message triggered it —
            // this span puts the capability name on every log line
            // underneath it for free. `.instrument()`, not `.enter()`:
            // this block awaits (the lock, then `handle()` itself), and a
            // span guard held across an `.await` can bleed into whatever
            // else happens to run on the same executor thread meanwhile —
            // `.instrument()` is the sound way to attach a span to a
            // future that spans multiple await points.
            let span = tracing::info_span!("dispatch", cap = slot.cap.name());
            async {
                let _guard = slot.lock.lock().await;
                slot.cap.handle(msg, tx).await;
            }
            .instrument(span)
            .await;
            return;
        }
    }
    // Acknowledge is sent by the coordinator to confirm a heartbeat — nothing to do.
    if matches!(msg, MeshMessage::Acknowledge) {
        return;
    }
    warn!("no capability handles: {:?}", msg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use shared::{HeartbeatPayload, ModelLoadRequest, NodeIdentity, NodeRole, WIRE_VERSION};
    use tokio::sync::{Mutex, mpsc};

    // ── TestCapability ────────────────────────────────────────────────────────

    struct TestCapability {
        cap_name: &'static str,
        handled: Arc<Mutex<Vec<MeshMessage>>>,
        handles_fn: fn(&MeshMessage) -> bool,
    }

    impl TestCapability {
        fn new(cap_name: &'static str, handles_fn: fn(&MeshMessage) -> bool) -> Arc<Self> {
            Arc::new(Self {
                cap_name,
                handled: Arc::new(Mutex::new(vec![])),
                handles_fn,
            })
        }
    }

    #[async_trait]
    impl Capability for TestCapability {
        fn name(&self) -> &'static str {
            self.cap_name
        }
        fn handles(&self, msg: &MeshMessage) -> bool {
            (self.handles_fn)(msg)
        }
        async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> {
            Ok(())
        }
        async fn handle(&self, msg: MeshMessage, _tx: Sender<MeshMessage>) {
            self.handled.lock().await.push(msg);
        }
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    fn model_load() -> MeshMessage {
        MeshMessage::ModelLoad(ModelLoadRequest {
            request_id: "r1".into(),
            node_id: Some("n1".into()),
            model_name: "qwen2.5:7b".into(),
            model_size_mb: 4096,
            wire_version: WIRE_VERSION,
        })
    }

    fn heartbeat() -> MeshMessage {
        MeshMessage::Heartbeat(HeartbeatPayload {
            identity: NodeIdentity {
                id: "n1".into(),
                hostname: "host".into(),
                ip: "127.0.0.1".into(),
                role: NodeRole::Compute,
            },
            auth_token: String::new(),
            cpu_usage_pct: 0.0,
            ram_used_gb: 0.0,
            ram_total_gb: 0.0,
            gpu_usage_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
            disk_free_gb: None,
        })
    }

    fn make_caps(
        a: Arc<TestCapability>,
        b: Arc<TestCapability>,
    ) -> Vec<Arc<dyn Capability + Send + Sync>> {
        vec![
            a as Arc<dyn Capability + Send + Sync>,
            b as Arc<dyn Capability + Send + Sync>,
        ]
    }

    // ── dispatch tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn routes_to_matching_capability() {
        let cap_a = TestCapability::new("a", |m| matches!(m, MeshMessage::ModelLoad(_)));
        let cap_b = TestCapability::new("b", |m| matches!(m, MeshMessage::Heartbeat(_)));
        let table = DispatchTable::new(make_caps(Arc::clone(&cap_a), Arc::clone(&cap_b)));
        let (tx, _rx) = mpsc::channel(8);

        dispatch(model_load(), Arc::clone(&table), tx.clone()).await;
        dispatch(heartbeat(), Arc::clone(&table), tx.clone()).await;

        assert_eq!(cap_a.handled.lock().await.len(), 1);
        assert_eq!(cap_b.handled.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn does_not_cross_route() {
        let cap_a = TestCapability::new("a", |m| matches!(m, MeshMessage::ModelLoad(_)));
        let cap_b = TestCapability::new("b", |m| matches!(m, MeshMessage::Heartbeat(_)));
        let table = DispatchTable::new(make_caps(Arc::clone(&cap_a), Arc::clone(&cap_b)));
        let (tx, _rx) = mpsc::channel(8);

        // send a heartbeat — cap_a should not receive it
        dispatch(heartbeat(), Arc::clone(&table), tx.clone()).await;
        assert_eq!(cap_a.handled.lock().await.len(), 0);
        assert_eq!(cap_b.handled.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn stops_at_first_match() {
        // both caps claim to handle everything — only the first should fire
        let cap_a = TestCapability::new("a", |_| true);
        let cap_b = TestCapability::new("b", |_| true);
        let table = DispatchTable::new(make_caps(Arc::clone(&cap_a), Arc::clone(&cap_b)));
        let (tx, _rx) = mpsc::channel(8);

        dispatch(model_load(), Arc::clone(&table), tx.clone()).await;

        assert_eq!(cap_a.handled.lock().await.len(), 1);
        assert_eq!(cap_b.handled.lock().await.len(), 0);
    }

    /// Records "start"/"end" for each `handle()` call, with a delay between
    /// them — lets a test observe whether two concurrent calls interleave
    /// (a race) or strictly alternate (properly serialized).
    struct SlowCapability {
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    impl SlowCapability {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                log: Arc::new(Mutex::new(vec![])),
            })
        }
    }

    #[async_trait]
    impl Capability for SlowCapability {
        fn name(&self) -> &'static str {
            "slow"
        }
        fn handles(&self, _msg: &MeshMessage) -> bool {
            true
        }
        async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> {
            Ok(())
        }
        async fn handle(&self, _msg: MeshMessage, _tx: Sender<MeshMessage>) {
            self.log.lock().await.push("start");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.log.lock().await.push("end");
        }
    }

    #[tokio::test]
    async fn concurrent_dispatches_to_the_same_capability_are_serialized() {
        let cap = SlowCapability::new();
        let table = DispatchTable::new(vec![Arc::clone(&cap) as Arc<dyn Capability + Send + Sync>]);
        let (tx, _rx) = mpsc::channel(8);

        // Two dispatches racing for the same capability, exactly the
        // scenario the per-capability lock exists to protect (e.g. two
        // Bluetooth requests landing close together must not both run
        // `bluetoothctl` at once).
        tokio::join!(
            dispatch(model_load(), Arc::clone(&table), tx.clone()),
            dispatch(heartbeat(), Arc::clone(&table), tx.clone())
        );

        // Interleaved ("start", "start", "end", "end") would mean both
        // handle() calls ran concurrently — a real regression. Serialized
        // is the only correct outcome: ["start", "end", "start", "end"].
        assert_eq!(*cap.log.lock().await, vec!["start", "end", "start", "end"]);
    }

    #[tokio::test]
    async fn empty_caps_does_not_panic() {
        let table = DispatchTable::new(vec![]);
        let (tx, _rx) = mpsc::channel(8);
        dispatch(heartbeat(), table, tx).await; // should log + return cleanly
    }

    // ── build_capabilities tests ──────────────────────────────────────────────

    #[cfg(feature = "llm")]
    #[test]
    fn build_includes_llm() {
        let caps = build_capabilities("node-1");
        assert!(!caps.is_empty());
        assert!(caps.iter().any(|c| c.name() == "llm"));
    }

    #[cfg(feature = "art")]
    #[test]
    fn build_includes_art() {
        let caps = build_capabilities("node-1");
        assert!(!caps.is_empty());
        assert!(caps.iter().any(|c| c.name() == "art"));
    }

    #[cfg(not(feature = "llm"))]
    #[test]
    fn build_empty_without_features() {
        let caps = build_capabilities("node-1");
        assert!(caps.is_empty());
    }
}
