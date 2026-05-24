use capability_core::Capability;
use shared::MeshMessage;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tracing::warn;

/// Build the capability list for this node from compile-time feature flags.
/// Called once before the reconnect loop; capabilities survive reconnects via Arc.
#[allow(unused_variables)]
pub fn build_capabilities(node_id: &str) -> Vec<Arc<dyn Capability + Send + Sync>> {
    vec![
        #[cfg(feature = "llm")]
        Arc::new(capability_llm::LlmCapability::new(node_id)),
        #[cfg(feature = "lighting")]
        Arc::new(capability_lighting::LightingCapability::new(node_id)),
    ]
}

/// Route one inbound message to the first capability that claims it.
/// Unhandled messages are logged and dropped.
pub async fn dispatch(
    msg: MeshMessage,
    caps: &[Arc<dyn Capability + Send + Sync>],
    tx: Sender<MeshMessage>,
) {
    for cap in caps {
        if cap.handles(&msg) {
            cap.handle(msg, tx).await;
            return;
        }
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
        let caps = make_caps(Arc::clone(&cap_a), Arc::clone(&cap_b));
        let (tx, _rx) = mpsc::channel(8);

        dispatch(model_load(), &caps, tx.clone()).await;
        dispatch(heartbeat(), &caps, tx.clone()).await;

        assert_eq!(cap_a.handled.lock().await.len(), 1);
        assert_eq!(cap_b.handled.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn does_not_cross_route() {
        let cap_a = TestCapability::new("a", |m| matches!(m, MeshMessage::ModelLoad(_)));
        let cap_b = TestCapability::new("b", |m| matches!(m, MeshMessage::Heartbeat(_)));
        let caps = make_caps(Arc::clone(&cap_a), Arc::clone(&cap_b));
        let (tx, _rx) = mpsc::channel(8);

        // send a heartbeat — cap_a should not receive it
        dispatch(heartbeat(), &caps, tx.clone()).await;
        assert_eq!(cap_a.handled.lock().await.len(), 0);
        assert_eq!(cap_b.handled.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn stops_at_first_match() {
        // both caps claim to handle everything — only the first should fire
        let cap_a = TestCapability::new("a", |_| true);
        let cap_b = TestCapability::new("b", |_| true);
        let caps = make_caps(Arc::clone(&cap_a), Arc::clone(&cap_b));
        let (tx, _rx) = mpsc::channel(8);

        dispatch(model_load(), &caps, tx.clone()).await;

        assert_eq!(cap_a.handled.lock().await.len(), 1);
        assert_eq!(cap_b.handled.lock().await.len(), 0);
    }

    #[tokio::test]
    async fn empty_caps_does_not_panic() {
        let caps: Vec<Arc<dyn Capability + Send + Sync>> = vec![];
        let (tx, _rx) = mpsc::channel(8);
        dispatch(heartbeat(), &caps, tx).await; // should log + return cleanly
    }

    // ── build_capabilities tests ──────────────────────────────────────────────

    #[cfg(feature = "llm")]
    #[test]
    fn build_includes_llm() {
        let caps = build_capabilities("node-1");
        assert!(!caps.is_empty());
        assert!(caps.iter().any(|c| c.name() == "llm"));
    }

    #[cfg(not(feature = "llm"))]
    #[test]
    fn build_empty_without_features() {
        let caps = build_capabilities("node-1");
        assert!(caps.is_empty());
    }
}
