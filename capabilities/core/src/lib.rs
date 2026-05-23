use async_trait::async_trait;
use shared::MeshMessage;
use tokio::sync::mpsc::Sender;

/// A pluggable unit of agent behaviour.
///
/// Each capability handles a subset of inbound `MeshMessage`s and may run a
/// persistent background task (e.g. an MQTT event loop). Capabilities are
/// selected at build time via Cargo feature flags and registered in the agent
/// shell at startup.
#[async_trait]
pub trait Capability: Send + Sync {
    /// Short identifier used in logs and the `NodeCapabilities.features` list.
    fn name(&self) -> &'static str;

    /// Returns true if this capability should handle `msg`.
    /// Must be cheap — called synchronously on every inbound message.
    fn handles(&self, msg: &MeshMessage) -> bool;

    /// Long-running background task spawned once at startup, outside the
    /// reconnect loop. Returns `Err` if the capability cannot initialise
    /// (e.g. MQTT broker unreachable); the agent logs the error and continues.
    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String>;

    /// Handle one inbound message routed by `handles()`.
    async fn handle(&self, msg: MeshMessage, tx: Sender<MeshMessage>);

    /// Tool schemas exposed to the intent router.
    /// Default implementation returns empty — override in capabilities that
    /// want to be callable from `mesh intent`.
    fn tools(&self) -> Vec<ToolSchema> {
        vec![]
    }
}

/// Describes one callable action for the intent router's LLM system prompt.
/// `parameters` is a JSON Schema object (use `serde_json::json!({...})`).
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{MeshMessage, NodeIdentity, NodeRole};
    use tokio::sync::mpsc;

    struct AlwaysHandles;

    #[async_trait]
    impl Capability for AlwaysHandles {
        fn name(&self) -> &'static str {
            "always"
        }
        fn handles(&self, _msg: &MeshMessage) -> bool {
            true
        }
        async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> {
            Ok(())
        }
        async fn handle(&self, _msg: MeshMessage, _tx: Sender<MeshMessage>) {}
    }

    struct NeverHandles;

    #[async_trait]
    impl Capability for NeverHandles {
        fn name(&self) -> &'static str {
            "never"
        }
        fn handles(&self, _msg: &MeshMessage) -> bool {
            false
        }
        async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> {
            Ok(())
        }
        async fn handle(&self, _msg: MeshMessage, _tx: Sender<MeshMessage>) {}
    }

    fn heartbeat() -> MeshMessage {
        use shared::HeartbeatPayload;
        MeshMessage::Heartbeat(HeartbeatPayload {
            identity: NodeIdentity {
                id: "n1".into(),
                hostname: "host".into(),
                ip: "127.0.0.1".into(),
                role: NodeRole::Compute,
            },
            auth_token: String::new(),
        })
    }

    #[test]
    fn always_handles_returns_true() {
        assert!(AlwaysHandles.handles(&heartbeat()));
    }

    #[test]
    fn never_handles_returns_false() {
        assert!(!NeverHandles.handles(&heartbeat()));
    }

    #[test]
    fn default_tools_is_empty() {
        assert!(AlwaysHandles.tools().is_empty());
    }

    #[tokio::test]
    async fn start_ok_returns_ok() {
        let (tx, _rx) = mpsc::channel(1);
        assert!(AlwaysHandles.start(tx).await.is_ok());
    }
}
