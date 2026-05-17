use crate::registry::Registry;
use shared::{AdminMessage, MeshMessage, NodeRecordFull, NodeRole};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, info_span};

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct Server {
    pub addr: String,
    pub registry: Arc<Mutex<Registry>>,
}

impl Server {
    pub fn new(addr: impl Into<String>, registry: Arc<Mutex<Registry>>) -> Self {
        Self {
            addr: addr.into(),
            registry,
        }
    }

    pub async fn run(&self) -> Result<(), ServerError> {
        let listener = TcpListener::bind(&self.addr).await?;

        loop {
            let (socket, _) = listener.accept().await?;
            let registry = self.registry.clone();

            tokio::spawn(async move {
                let _ = handle_connection(socket, registry).await;
            });
        }
    }
}

pub async fn handle_connection(
    mut socket: TcpStream,
    registry: Arc<Mutex<Registry>>,
) -> Result<(), ServerError> {
    loop {
        // Read message length (u32)
        let mut len_buf = [0u8; 4];
        if socket.read_exact(&mut len_buf).await.is_err() {
            return Ok(()); // connection closed
        }

        let msg_len = u32::from_le_bytes(len_buf) as usize;

        // Read message body
        let mut buf = vec![0u8; msg_len];
        socket.read_exact(&mut buf).await?;

        let msg: MeshMessage = serde_json::from_slice(&buf)?;

        // Process message (no awaits inside)
        let reply = {
            let mut reg = registry.lock().unwrap();

            match msg {
                MeshMessage::Heartbeat(identity) => {
                    reg.update_heartbeat(identity);
                    MeshMessage::Acknowledge
                }
                MeshMessage::HardwareReport(hw) => {
                    if let Some(id) = reg.first_node_id() {
                        reg.update_hardware(&id, hw);
                    }
                    MeshMessage::Acknowledge
                }
                MeshMessage::Capabilities(caps) => {
                    if let Some(id) = reg.first_node_id() {
                        reg.update_capabilities(&id, caps);
                    }
                    MeshMessage::Acknowledge
                }
                MeshMessage::RequestNodes => {
                    let nodes = reg.list_nodes();
                    MeshMessage::NodeList(nodes)
                }
                MeshMessage::RequestNodeInfo(id) => {
                    let full = reg.get_node_full(&id);
                    MeshMessage::NodeInfo(full.unwrap_or_else(|| NodeRecordFull {
                        id,
                        hostname: "unknown".into(),
                        ip: "unknown".into(),
                        role: NodeRole::Compute,
                        last_heartbeat_ms: 0,
                        hardware: None,
                        capabilities: None,
                    }))
                }
                // ── Phase 6 model scheduling ──────────────────────────────────────
                //
                // These arms are intentionally synchronous stubs.
                // Migration path for async dispatch (Phase 6):
                //   1. Extract each arm into `async fn handle_*(req) -> MeshMessage`.
                //   2. Release the registry lock before calling the handler.
                //   3. Call `handle_*(req).instrument(span).await` at the call site.
                MeshMessage::RequestModelInference(req) => {
                    let span = info_span!(
                        "model_inference",
                        node_id    = %req.node_id,
                        model_name = %req.model_name,
                        request_id = %req.request_id,
                    );
                    let _enter = span.enter();
                    info!("inference request received; dispatch pending Phase 6");
                    MeshMessage::Acknowledge
                }
                MeshMessage::ModelLoad(req) => {
                    info!(
                        node_id    = %req.node_id,
                        model_name = %req.model_name,
                        size_mb    = req.model_size_mb,
                        "model load requested"
                    );
                    MeshMessage::Acknowledge
                }
                MeshMessage::ModelUnload(req) => {
                    info!(
                        node_id    = %req.node_id,
                        model_name = %req.model_name,
                        "model unload requested"
                    );
                    MeshMessage::Acknowledge
                }
                MeshMessage::ModelStatus(rep) => {
                    info!(
                        node_id    = %rep.node_id,
                        model_name = %rep.model_name,
                        state      = ?rep.state,
                        "model status update received"
                    );
                    MeshMessage::Acknowledge
                }
                MeshMessage::Admin(admin) => match admin {
                    AdminMessage::ResetRegistry => {
                        reg.clear_all();
                        tracing::warn!("Registry cleared via AdminMessage::ResetRegistry");
                        MeshMessage::Acknowledge
                    }
                },
                _ => MeshMessage::Acknowledge,
            }
        };

        // Now send reply (after lock is dropped)
        let data = serde_json::to_vec(&reply)?;
        let len = (data.len() as u32).to_le_bytes();
        socket.write_all(&len).await?;
        socket.write_all(&data).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{NodeIdentity, NodeRole};
    use tokio::net::TcpStream;

    async fn send_message(addr: &str, msg: &MeshMessage) -> MeshMessage {
        let mut stream = TcpStream::connect(addr).await.unwrap();

        let data = serde_json::to_vec(msg).unwrap();
        let len = (data.len() as u32).to_le_bytes();

        stream.write_all(&len).await.unwrap();
        stream.write_all(&data).await.unwrap();

        // Read ack
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.unwrap();
        let msg_len = u32::from_le_bytes(len_buf) as usize;

        let mut buf = vec![0u8; msg_len];
        stream.read_exact(&mut buf).await.unwrap();

        serde_json::from_slice(&buf).unwrap()
    }

    #[tokio::test]
    async fn test_server_receives_heartbeat() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let server = Server::new("127.0.0.1:9001", registry.clone());

        tokio::spawn(async move {
            let _ = server.run().await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let ident = NodeIdentity {
            id: "node1".into(),
            hostname: "test".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        };

        let ack = send_message("127.0.0.1:9001", &MeshMessage::Heartbeat(ident.clone())).await;

        match ack {
            MeshMessage::Acknowledge => {}
            _ => panic!("Expected Acknowledge"),
        }

        let reg = registry.lock().unwrap();
        assert!(reg.get("node1").is_some());
    }

    #[tokio::test]
    async fn test_server_request_node_info() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let server = Server::new("127.0.0.1:9003", registry.clone());

        tokio::spawn(async move {
            let _ = server.run().await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let ident = NodeIdentity {
            id: "nodeA".into(),
            hostname: "host-a".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        };

        // Register the node via heartbeat
        send_message("127.0.0.1:9003", &MeshMessage::Heartbeat(ident.clone())).await;

        // Request full node info
        let reply = send_message(
            "127.0.0.1:9003",
            &MeshMessage::RequestNodeInfo("nodeA".into()),
        )
        .await;

        match reply {
            MeshMessage::NodeInfo(info) => {
                assert_eq!(info.id, "nodeA");
                assert_eq!(info.hostname, "host-a");
            }
            _ => panic!("Expected NodeInfo"),
        }
    }
}
