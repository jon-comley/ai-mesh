use crate::registry::Registry;
use crate::scheduler::Scheduler;
use shared::{AdminMessage, MeshMessage, NodeRecordFull, NodeRole};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

type Connections = Arc<Mutex<HashMap<String, mpsc::Sender<MeshMessage>>>>;
pub type PendingInferences = Arc<Mutex<HashMap<String, oneshot::Sender<MeshMessage>>>>;

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
    connections: Connections,
    pending_inferences: PendingInferences,
}

impl Server {
    pub fn new(addr: impl Into<String>, registry: Arc<Mutex<Registry>>) -> Self {
        Self {
            addr: addr.into(),
            registry,
            connections: Arc::new(Mutex::new(HashMap::new())),
            pending_inferences: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run(&self) -> Result<(), ServerError> {
        let listener = TcpListener::bind(&self.addr).await?;

        loop {
            let (socket, _) = listener.accept().await?;
            let registry = self.registry.clone();
            let connections = self.connections.clone();
            let pending_inferences = self.pending_inferences.clone();

            tokio::spawn(async move {
                let _ = handle_connection(socket, registry, connections, pending_inferences).await;
            });
        }
    }
}

pub async fn handle_connection(
    socket: TcpStream,
    registry: Arc<Mutex<Registry>>,
    connections: Connections,
    pending_inferences: PendingInferences,
) -> Result<(), ServerError> {
    let (mut reader, mut writer) = socket.into_split();
    let (tx, mut rx) = mpsc::channel::<MeshMessage>(32);

    // Tracks the node ID once a Heartbeat has been received, for cleanup on disconnect.
    let mut node_id: Option<String> = None;

    // Writer task: drain the outbound channel onto the TCP write half.
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let data = match serde_json::to_vec(&msg) {
                Ok(d) => d,
                Err(_) => break,
            };
            let len = (data.len() as u32).to_le_bytes();
            if writer.write_all(&len).await.is_err() {
                break;
            }
            if writer.write_all(&data).await.is_err() {
                break;
            }
        }
    });

    loop {
        let mut len_buf = [0u8; 4];
        if reader.read_exact(&mut len_buf).await.is_err() {
            break;
        }
        let msg_len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; msg_len];
        if reader.read_exact(&mut buf).await.is_err() {
            break;
        }
        let msg: MeshMessage = match serde_json::from_slice(&buf) {
            Ok(m) => m,
            Err(e) => return Err(ServerError::Json(e)),
        };

        let reply = process_message(
            msg,
            &registry,
            &connections,
            &pending_inferences,
            &tx,
            &mut node_id,
        )
        .await;

        if tx.send(reply).await.is_err() {
            break;
        }
    }

    // Remove this connection's routing channel when the connection closes.
    if let Some(id) = node_id {
        info!(node_id = %id, "connection closed, removing from connection map");
        connections.lock().unwrap().remove(&id);
    }

    Ok(())
}

async fn process_message(
    msg: MeshMessage,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_inferences: &PendingInferences,
    tx: &mpsc::Sender<MeshMessage>,
    node_id: &mut Option<String>,
) -> MeshMessage {
    match msg {
        MeshMessage::Heartbeat(identity) => {
            info!(node_id = %identity.id, hostname = %identity.hostname, "heartbeat");
            *node_id = Some(identity.id.clone());
            registry.lock().unwrap().update_heartbeat(identity.clone());
            connections.lock().unwrap().insert(identity.id, tx.clone());
            MeshMessage::Acknowledge
        }
        MeshMessage::HardwareReport(hw) => {
            if let Some(id) = node_id.as_deref() {
                registry.lock().unwrap().update_hardware(id, hw);
            }
            MeshMessage::Acknowledge
        }
        MeshMessage::Capabilities(caps) => {
            if let Some(id) = node_id.as_deref() {
                registry.lock().unwrap().update_capabilities(id, caps);
            }
            MeshMessage::Acknowledge
        }
        MeshMessage::RequestNodes => {
            let nodes = registry.lock().unwrap().list_nodes();
            MeshMessage::NodeList(nodes)
        }
        MeshMessage::RequestNodeInfo(id) => {
            let full = registry.lock().unwrap().get_node_full(&id);
            MeshMessage::NodeInfo(full.unwrap_or_else(|| NodeRecordFull {
                id,
                hostname: "unknown".into(),
                ip: "unknown".into(),
                role: NodeRole::Compute,
                last_heartbeat_ms: 0,
                hardware: None,
                capabilities: None,
                models: vec![],
            }))
        }
        MeshMessage::RequestModelInference(req) => {
            let selected = {
                let reg = registry.lock().unwrap();
                Scheduler::new(&reg).select_node_for_inference(&req.model_name)
            };
            match selected {
                Some(node) => {
                    let agent_tx = connections.lock().unwrap().get(&node.id).cloned();
                    match agent_tx {
                        Some(agent_tx) => {
                            let (otx, orx) = oneshot::channel();
                            let request_id = req.request_id.clone();
                            pending_inferences
                                .lock()
                                .unwrap()
                                .insert(request_id.clone(), otx);
                            info!(
                                node_id    = %node.id,
                                model_name = %req.model_name,
                                request_id = %req.request_id,
                                "forwarding inference request to agent"
                            );
                            let _ = agent_tx.send(MeshMessage::RequestModelInference(req)).await;
                            // Block until the agent returns a result or the timeout fires.
                            match timeout(Duration::from_secs(30), orx).await {
                                Ok(Ok(result)) => result,
                                Ok(Err(_)) => {
                                    pending_inferences.lock().unwrap().remove(&request_id);
                                    MeshMessage::Error(
                                        "inference channel closed unexpectedly".into(),
                                    )
                                }
                                Err(_) => {
                                    pending_inferences.lock().unwrap().remove(&request_id);
                                    MeshMessage::Error("inference request timed out".into())
                                }
                            }
                        }
                        None => {
                            warn!(
                                node_id    = %node.id,
                                model_name = %req.model_name,
                                request_id = %req.request_id,
                                "scheduler selected node but agent is not connected"
                            );
                            MeshMessage::Error(format!(
                                "compute node '{}' dropped from connections map",
                                node.id
                            ))
                        }
                    }
                }
                None => {
                    warn!(
                        model_name = %req.model_name,
                        request_id = %req.request_id,
                        "no node ready to serve inference request"
                    );
                    MeshMessage::Error(format!(
                        "no node has model '{}' in Ready state",
                        req.model_name
                    ))
                }
            }
        }
        MeshMessage::ModelInferenceResult(res) => {
            info!(
                request_id = %res.request_id,
                node_id    = %res.node_id,
                "model inference result received from agent"
            );
            let otx = pending_inferences.lock().unwrap().remove(&res.request_id);
            if let Some(otx) = otx {
                let _ = otx.send(MeshMessage::ModelInferenceResult(res));
            }
            MeshMessage::Acknowledge
        }
        MeshMessage::ModelLoad(req) => {
            let agent_tx = connections.lock().unwrap().get(&req.node_id).cloned();
            match agent_tx {
                Some(agent_tx) => {
                    info!(
                        node_id    = %req.node_id,
                        model_name = %req.model_name,
                        "forwarding ModelLoad to agent"
                    );
                    let _ = agent_tx.send(MeshMessage::ModelLoad(req)).await;
                }
                None => {
                    warn!(
                        node_id = %req.node_id,
                        "ModelLoad target node not connected"
                    );
                }
            }
            MeshMessage::Acknowledge
        }
        MeshMessage::ModelUnload(req) => {
            let agent_tx = connections.lock().unwrap().get(&req.node_id).cloned();
            match agent_tx {
                Some(agent_tx) => {
                    info!(
                        node_id    = %req.node_id,
                        model_name = %req.model_name,
                        "forwarding ModelUnload to agent"
                    );
                    let _ = agent_tx.send(MeshMessage::ModelUnload(req)).await;
                }
                None => {
                    warn!(
                        node_id = %req.node_id,
                        "ModelUnload target node not connected"
                    );
                }
            }
            MeshMessage::Acknowledge
        }
        MeshMessage::ModelStatus(report) => {
            info!(
                node_id    = %report.node_id,
                model_name = %report.model_name,
                state      = ?report.state,
                "model status update received"
            );
            registry.lock().unwrap().update_model_status(
                &report.node_id,
                &report.model_name,
                report.size_mb,
                report.state,
            );
            MeshMessage::Acknowledge
        }
        MeshMessage::Admin(admin) => match admin {
            AdminMessage::ResetRegistry => {
                registry.lock().unwrap().clear_all();
                tracing::warn!("Registry cleared via AdminMessage::ResetRegistry");
                MeshMessage::Acknowledge
            }
        },
        _ => MeshMessage::Acknowledge,
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
