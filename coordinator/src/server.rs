use crate::http::state::{DashboardState, ModelEntry, NodeModelInfo};
use crate::intent::PendingIntents;
use crate::registry::Registry;
use crate::scheduler::Scheduler;
use shared::frame::{FrameVerifyError, SignedFrame, derive_hmac_key};
use shared::{
    AdminMessage, HeartbeatPayload, MeshMessage, ModelLifecycleState, NodeRecordFull, NodeRole,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};

pub use crate::http::state::NodeConnections as Connections;
/// Maps request_id → (reply_channel, node_id_that_was_selected)
pub type PendingInferences = Arc<Mutex<HashMap<String, (oneshot::Sender<MeshMessage>, String)>>>;

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
    pub connections: Connections,
    pending_inferences: PendingInferences,
    pending_intents: PendingIntents,
    /// Valid auth tokens. Empty = no authentication (dev/test mode).
    pub auth_tokens: Arc<Vec<String>>,
    /// TLS acceptor. None = plain TCP (tests and MESH_INSECURE=1 mode).
    pub tls: Option<TlsAcceptor>,
    /// Dashboard broadcast state. None = no dashboard (tests).
    pub dashboard: Option<Arc<DashboardState>>,
}

impl Server {
    pub fn new(addr: impl Into<String>, registry: Arc<Mutex<Registry>>) -> Self {
        Self {
            addr: addr.into(),
            registry,
            connections: Arc::new(Mutex::new(HashMap::new())),
            pending_inferences: Arc::new(Mutex::new(HashMap::new())),
            pending_intents: Arc::new(Mutex::new(HashMap::new())),
            auth_tokens: Arc::new(vec![]),
            tls: None,
            dashboard: None,
        }
    }

    pub async fn run(&self) -> Result<(), ServerError> {
        let listener = TcpListener::bind(&self.addr).await?;

        loop {
            let (socket, _) = listener.accept().await?;
            let registry = self.registry.clone();
            let connections = self.connections.clone();
            let pending_inferences = self.pending_inferences.clone();
            let pending_intents = self.pending_intents.clone();
            let auth_tokens = self.auth_tokens.clone();
            let dashboard = self.dashboard.clone();

            if let Some(acceptor) = &self.tls {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    match acceptor.accept(socket).await {
                        Ok(tls_stream) => {
                            let _ = handle_connection(
                                tls_stream,
                                registry,
                                connections,
                                pending_inferences,
                                pending_intents,
                                auth_tokens,
                                dashboard,
                            )
                            .await;
                        }
                        Err(e) => warn!("TLS handshake failed: {}", e),
                    }
                });
            } else {
                tokio::spawn(async move {
                    let _ = handle_connection(
                        socket,
                        registry,
                        connections,
                        pending_inferences,
                        pending_intents,
                        auth_tokens,
                        dashboard,
                    )
                    .await;
                });
            }
        }
    }
}

pub async fn handle_connection<S>(
    socket: S,
    registry: Arc<Mutex<Registry>>,
    connections: Connections,
    pending_inferences: PendingInferences,
    pending_intents: PendingIntents,
    auth_tokens: Arc<Vec<String>>,
    dashboard: Option<Arc<DashboardState>>,
) -> Result<(), ServerError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(socket);
    let (tx, mut rx) = mpsc::channel::<MeshMessage>(32);

    // Auth check: if tokens are configured, the first message must be AuthToken.
    // On success, derive the per-connection HMAC key from the validated token.
    // All subsequent frames in both directions are HMAC-signed SignedFrames.
    let hmac_key: Option<[u8; 32]> = if !auth_tokens.is_empty() {
        let mut len_buf = [0u8; 4];
        if reader.read_exact(&mut len_buf).await.is_err() {
            return Ok(());
        }
        let msg_len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; msg_len];
        if reader.read_exact(&mut buf).await.is_err() {
            return Ok(());
        }
        match serde_json::from_slice::<MeshMessage>(&buf) {
            Ok(MeshMessage::AuthToken(token)) if auth_tokens.contains(&token) => {
                Some(derive_hmac_key(&token))
            }
            Ok(MeshMessage::AuthToken(_)) => {
                warn!("rejected connection: invalid auth token");
                return Ok(());
            }
            Ok(other) => {
                warn!("rejected connection: expected AuthToken, got {:?}", other);
                return Ok(());
            }
            Err(_) => {
                warn!("rejected connection: could not parse first message");
                return Ok(());
            }
        }
    } else {
        None
    };

    // Tracks the node ID once a Heartbeat has been received, for cleanup on disconnect.
    let mut node_id: Option<String> = None;

    // Writer task: drain the outbound channel onto the TCP write half.
    // When HMAC is active, every outgoing message is wrapped in a SignedFrame.
    let writer_key = hmac_key;
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let data = if let Some(key) = &writer_key {
                let payload = match serde_json::to_vec(&msg) {
                    Ok(d) => d,
                    Err(_) => break,
                };
                let frame = SignedFrame::sign(key, payload);
                match serde_json::to_vec(&frame) {
                    Ok(d) => d,
                    Err(_) => break,
                }
            } else {
                match serde_json::to_vec(&msg) {
                    Ok(d) => d,
                    Err(_) => break,
                }
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
        let msg: MeshMessage = if let Some(key) = &hmac_key {
            match serde_json::from_slice::<SignedFrame>(&buf) {
                Ok(frame) => match frame.verify(key) {
                    Ok(payload) => match serde_json::from_slice(payload) {
                        Ok(m) => m,
                        Err(e) => return Err(ServerError::Json(e)),
                    },
                    Err(e) => {
                        if matches!(e, FrameVerifyError::Stale { .. }) {
                            warn!(
                                "dropping frame: {} — check that node clock is NTP-synced",
                                e
                            );
                        } else {
                            warn!("dropping frame: {}", e);
                        }
                        return Ok(());
                    }
                },
                Err(e) => return Err(ServerError::Json(e)),
            }
        } else {
            match serde_json::from_slice(&buf) {
                Ok(m) => m,
                Err(e) => return Err(ServerError::Json(e)),
            }
        };

        let reply = process_message(
            msg,
            &registry,
            &connections,
            &pending_inferences,
            &pending_intents,
            &tx,
            &mut node_id,
            &auth_tokens,
            dashboard.as_deref(),
        )
        .await;

        if let Some(reply) = reply
            && tx.send(reply).await.is_err()
        {
            break;
        }
    }

    // Remove this connection's routing channel when the connection closes.
    if let Some(id) = node_id {
        info!(node_id = %id, "connection closed, removing from connection map");
        connections.lock().unwrap().remove(&id);

        // Immediately fail any pending inferences that were routed to this node,
        // so CLI clients get a fast error instead of waiting GENERATE_TIMEOUT_SECS.
        let mut pending = pending_inferences.lock().unwrap();
        let to_fail: Vec<String> = pending
            .iter()
            .filter(|(_, (_, nid))| nid == &id)
            .map(|(k, _)| k.clone())
            .collect();
        for req_id in to_fail {
            if let Some((otx, _)) = pending.remove(&req_id) {
                warn!(node_id = %id, request_id = %req_id, "failing pending inference: agent disconnected");
                let _ = otx.send(MeshMessage::Error(format!(
                    "compute node '{}' disconnected during inference",
                    id
                )));
            }
        }
    }

    Ok(())
}

fn lifecycle_to_str(state: &ModelLifecycleState) -> (&'static str, Option<String>) {
    match state {
        ModelLifecycleState::Loading => ("Loading", None),
        ModelLifecycleState::Ready => ("Ready", None),
        ModelLifecycleState::Failed { reason } => ("Failed", Some(reason.clone())),
        ModelLifecycleState::Unloaded => ("Unloaded", None),
    }
}

fn build_model_snapshot(registry: &Registry) -> Vec<NodeModelInfo> {
    registry
        .list_nodes()
        .into_iter()
        .filter(|n| matches!(n.role, NodeRole::Compute))
        .filter_map(|lite| registry.get_node_full(&lite.id))
        .map(|full| {
            let ram_gb = full.hardware.as_ref().map(|h| h.ram_gb).unwrap_or(0.0);
            let models = full
                .models
                .into_iter()
                .filter(|m| !matches!(m.state, ModelLifecycleState::Unloaded))
                .map(|m| {
                    let (state, reason) = lifecycle_to_str(&m.state);
                    ModelEntry {
                        name: m.model_name,
                        size_mb: m.size_mb,
                        state: state.into(),
                        reason,
                    }
                })
                .collect();
            NodeModelInfo {
                node_id: full.id,
                hostname: full.hostname,
                role: "Compute".into(),
                ram_gb,
                models,
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn process_message(
    msg: MeshMessage,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_inferences: &PendingInferences,
    pending_intents: &PendingIntents,
    tx: &mpsc::Sender<MeshMessage>,
    node_id: &mut Option<String>,
    auth_tokens: &Arc<Vec<String>>,
    dashboard: Option<&DashboardState>,
) -> Option<MeshMessage> {
    match msg {
        MeshMessage::Heartbeat(HeartbeatPayload {
            identity,
            auth_token,
            cpu_usage_pct,
            ram_used_gb,
            ram_total_gb,
            gpu_usage_pct,
            gpu_vram_used_gb,
            gpu_vram_total_gb,
        }) => {
            // When tokens are configured, require the heartbeat token to match exactly.
            if !auth_tokens.is_empty() && !auth_tokens.iter().any(|a| a == &auth_token) {
                warn!(node_id = %identity.id, "heartbeat rejected: missing or wrong auth token");
                return None;
            }
            info!(node_id = %identity.id, hostname = %identity.hostname, "heartbeat");
            let this_id = identity.id.clone();
            *node_id = Some(this_id.clone());
            let nodes = {
                let mut reg = registry.lock().unwrap();
                reg.update_heartbeat(identity.clone());
                reg.list_nodes()
            };
            connections.lock().unwrap().insert(identity.id, tx.clone());
            if let Some(dash) = dashboard {
                dash.push_topology(&nodes);
                dash.push_health(
                    &this_id,
                    cpu_usage_pct,
                    ram_used_gb,
                    ram_total_gb,
                    gpu_usage_pct,
                    gpu_vram_used_gb,
                    gpu_vram_total_gb,
                );
            }
            Some(MeshMessage::Acknowledge)
        }
        MeshMessage::HardwareReport(hw) => {
            if let Some(id) = node_id.as_deref() {
                let mut reg = registry.lock().unwrap();
                reg.update_hardware(id, hw);
                if let Some(dash) = dashboard {
                    dash.push_model_update(build_model_snapshot(&reg));
                }
            }
            None
        }
        MeshMessage::Capabilities(caps) => {
            if let Some(id) = node_id.as_deref() {
                registry.lock().unwrap().update_capabilities(id, caps);
            }
            None
        }
        MeshMessage::RequestNodes => {
            let nodes = registry.lock().unwrap().list_nodes();
            Some(MeshMessage::NodeList(nodes))
        }
        MeshMessage::RequestNodeInfo(id) => {
            let full = registry.lock().unwrap().get_node_full(&id);
            Some(MeshMessage::NodeInfo(full.unwrap_or_else(|| {
                NodeRecordFull {
                    id,
                    hostname: "unknown".into(),
                    ip: "unknown".into(),
                    role: NodeRole::Compute,
                    last_heartbeat_ms: 0,
                    hardware: None,
                    capabilities: None,
                    models: vec![],
                }
            })))
        }
        MeshMessage::RequestModelInference(req) => {
            // Phase 1 — pull wait: if the model is still being pulled on a Compute node,
            // poll the registry until it becomes Ready or the pull deadline expires.
            // This decouples the pull duration from the generation timeout.
            const PULL_TIMEOUT_SECS: u64 = 300;
            const GENERATE_TIMEOUT_SECS: u64 = 300;

            let deadline = tokio::time::Instant::now() + Duration::from_secs(PULL_TIMEOUT_SECS);
            let mut pull_timed_out = false;

            let selected = loop {
                let (ready_node, is_loading) = {
                    let reg = registry.lock().unwrap();
                    let ready = Scheduler::new(&reg).select_node_for_inference(&req.model_name);
                    let loading = ready.is_none() && reg.model_is_loading(&req.model_name);
                    (ready, loading)
                };

                if let Some(node) = ready_node {
                    break Some(node);
                }
                if !is_loading {
                    // Model not present or in Failed state — no point waiting.
                    break None;
                }
                if tokio::time::Instant::now() >= deadline {
                    pull_timed_out = true;
                    break None;
                }
                info!(
                    model_name = %req.model_name,
                    request_id = %req.request_id,
                    "model still loading, waiting for Ready state"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
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
                                .insert(request_id.clone(), (otx, node.id.clone()));
                            info!(
                                node_id    = %node.id,
                                model_name = %req.model_name,
                                request_id = %req.request_id,
                                "forwarding inference request to agent"
                            );
                            if agent_tx
                                .send(MeshMessage::RequestModelInference(req))
                                .await
                                .is_err()
                            {
                                // Writer task exited — clean up immediately rather than
                                // waiting the full generation timeout for a response that
                                // will never arrive.
                                pending_inferences.lock().unwrap().remove(&request_id);
                                connections.lock().unwrap().remove(&node.id);
                                warn!(
                                    node_id    = %node.id,
                                    request_id = %request_id,
                                    "inference send failed: agent channel closed"
                                );
                                return Some(MeshMessage::Error(format!(
                                    "compute node '{}' dropped from connections map",
                                    node.id
                                )));
                            }
                            // Phase 2 — generation timeout: separate, shorter window.
                            // The oneshot is also resolved early if the agent disconnects.
                            match timeout(Duration::from_secs(GENERATE_TIMEOUT_SECS), orx).await {
                                Ok(Ok(result)) => Some(result),
                                Ok(Err(_)) => {
                                    pending_inferences.lock().unwrap().remove(&request_id);
                                    Some(MeshMessage::Error(
                                        "inference channel closed unexpectedly".into(),
                                    ))
                                }
                                Err(_) => {
                                    pending_inferences.lock().unwrap().remove(&request_id);
                                    Some(MeshMessage::Error(format!(
                                        "inference generation timed out after {}s",
                                        GENERATE_TIMEOUT_SECS
                                    )))
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
                            Some(MeshMessage::Error(format!(
                                "compute node '{}' dropped from connections map",
                                node.id
                            )))
                        }
                    }
                }
                None => {
                    if pull_timed_out {
                        warn!(
                            model_name = %req.model_name,
                            request_id = %req.request_id,
                            "model pull did not complete within {}s",
                            PULL_TIMEOUT_SECS
                        );
                        Some(MeshMessage::Error(format!(
                            "model '{}' pull did not complete within {}s",
                            req.model_name, PULL_TIMEOUT_SECS
                        )))
                    } else {
                        warn!(
                            model_name = %req.model_name,
                            request_id = %req.request_id,
                            "no node ready to serve inference request"
                        );
                        Some(MeshMessage::Error(format!(
                            "no node has model '{}' in Ready state",
                            req.model_name
                        )))
                    }
                }
            }
        }
        MeshMessage::ModelInferenceResult(res) => {
            info!(
                request_id = %res.request_id,
                node_id    = %res.node_id,
                "model inference result received from agent"
            );
            let entry = pending_inferences.lock().unwrap().remove(&res.request_id);
            if let Some((otx, _)) = entry {
                let _ = otx.send(MeshMessage::ModelInferenceResult(res));
            }
            None
        }
        MeshMessage::ModelLoad(mut req) => {
            // Auto-place: if no node_id supplied, pick the best-fit node by headroom.
            if req.node_id.is_none() {
                let selected = {
                    let reg = registry.lock().unwrap();
                    Scheduler::new(&reg)
                        .select_node_for_model(req.model_size_mb)
                        .map(|n| n.id)
                };
                match selected {
                    Some(id) => {
                        info!(node_id = %id, model_name = %req.model_name, "auto-placed ModelLoad");
                        req.node_id = Some(id);
                    }
                    None => {
                        warn!(model_name = %req.model_name, "no node has capacity for auto-placed ModelLoad");
                        return Some(MeshMessage::Acknowledge);
                    }
                }
            }
            let target_id = req.node_id.as_deref().unwrap_or("");
            let agent_tx = connections.lock().unwrap().get(target_id).cloned();
            match agent_tx {
                Some(agent_tx) => {
                    info!(
                        node_id    = %target_id,
                        model_name = %req.model_name,
                        "forwarding ModelLoad to agent"
                    );
                    let _ = agent_tx.send(MeshMessage::ModelLoad(req)).await;
                }
                None => {
                    warn!(node_id = %target_id, "ModelLoad target node not connected");
                }
            }
            Some(MeshMessage::Acknowledge)
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
            Some(MeshMessage::Acknowledge)
        }
        MeshMessage::ModelStatus(report) => {
            info!(
                node_id    = %report.node_id,
                model_name = %report.model_name,
                state      = ?report.state,
                "model status update received"
            );
            let snapshot = {
                let mut reg = registry.lock().unwrap();
                reg.update_model_status(
                    &report.node_id,
                    &report.model_name,
                    report.size_mb,
                    report.state,
                );
                dashboard.map(|_| build_model_snapshot(&reg))
            };
            if let (Some(dash), Some(snap)) = (dashboard, snapshot) {
                dash.push_model_update(snap);
            }
            None
        }
        MeshMessage::IntentRequest(req) => {
            info!(request_id = %req.request_id, "intent request received");
            let response = crate::intent::handle_intent(
                req,
                registry.clone(),
                connections.clone(),
                pending_inferences.clone(),
                pending_intents.clone(),
            )
            .await;
            Some(MeshMessage::IntentResponse(response))
        }
        MeshMessage::SceneLoaded(report) => {
            let entry = pending_intents.lock().unwrap().remove(&report.request_id);
            if let Some(otx) = entry {
                let _ = otx.send(MeshMessage::SceneLoaded(report));
            }
            None
        }
        MeshMessage::LightState(report) => {
            info!(
                node_id = %report.node_id,
                device_id = %report.device_id,
                on = %report.on,
                "light state report received"
            );
            let device_id = report.device_id.clone();
            let went_offline = !report.online;
            registry.lock().unwrap().save_light_state(&report);
            if let Some(dash) = dashboard {
                dash.push_lighting_update(report.clone());

                if went_offline {
                    // Get room + check for active effect in a single lock acquisition.
                    let (maybe_room, has_effect) = {
                        let reg = registry.lock().unwrap();
                        let room_id = reg.get_room_for_device(&device_id);
                        let has = room_id
                            .as_deref()
                            .and_then(|rid| reg.get_active_effect(rid))
                            .is_some();
                        (room_id, has)
                    };
                    if has_effect && let Some(room_id) = maybe_room {
                        info!(
                            room_id = %room_id,
                            device_id = %device_id,
                            "device offline — auto-pausing effect"
                        );
                        let _ = registry.lock().unwrap().disable_active_effect(&room_id);
                        dash.push_effect_update(
                            room_id.clone(),
                            None,
                            serde_json::json!({}),
                            vec![],
                        );
                        dash.solar_sweep_notify.notify_one();
                    }
                }
            }
            None
        }
        MeshMessage::LightDeviceList(report) => {
            info!(
                node_id = %report.node_id,
                devices = ?report.devices,
                groups = ?report.groups,
                "light device list received"
            );
            if let Some(dash) = dashboard {
                dash.push_group_update(&report.node_id, report.groups.clone());
                dash.push_device_discovery(&report.node_id, report.devices.clone(), true);
            }
            registry.lock().unwrap().update_light_devices(
                &report.node_id,
                report.devices,
                report.groups,
            );
            None
        }
        MeshMessage::Admin(admin) => match admin {
            AdminMessage::ResetRegistry => {
                registry.lock().unwrap().clear_all();
                tracing::warn!("Registry cleared via AdminMessage::ResetRegistry");
                Some(MeshMessage::Acknowledge)
            }
        },
        MeshMessage::Ping => Some(MeshMessage::Acknowledge),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{HeartbeatPayload, NodeIdentity, NodeRole};
    use tokio::net::TcpStream;

    async fn send_message(addr: &str, msg: &MeshMessage) -> Option<MeshMessage> {
        let mut stream = TcpStream::connect(addr).await.unwrap();

        let data = serde_json::to_vec(msg).unwrap();
        let len = (data.len() as u32).to_le_bytes();

        stream.write_all(&len).await.unwrap();
        stream.write_all(&data).await.unwrap();

        // Some messages generate no reply — use a short timeout instead of blocking forever.
        let read_reply = async {
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.ok()?;
            let msg_len = u32::from_le_bytes(len_buf) as usize;
            let mut buf = vec![0u8; msg_len];
            stream.read_exact(&mut buf).await.ok()?;
            serde_json::from_slice(&buf).ok()
        };

        tokio::time::timeout(std::time::Duration::from_millis(200), read_reply)
            .await
            .ok()
            .flatten()
    }

    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
    async fn test_server_receives_heartbeat() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let mut server = Server::new("127.0.0.1:9020", registry.clone());
        server.auth_tokens = Arc::new(vec![]);

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

        let ack = send_message(
            "127.0.0.1:9020",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: ident.clone(),
                auth_token: String::new(),
                cpu_usage_pct: 0.0,
                ram_used_gb: 0.0,
                ram_total_gb: 0.0,
                gpu_usage_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
            }),
        )
        .await;

        assert!(ack.is_none(), "heartbeat should produce no reply");

        let reg = registry.lock().unwrap();
        assert!(reg.get("node1").is_some());
    }

    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
    async fn test_server_accepts_heartbeat_with_health_values() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let mut server = Server::new("127.0.0.1:9021", registry.clone());
        server.auth_tokens = Arc::new(vec![]);

        tokio::spawn(async move {
            let _ = server.run().await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let ack = send_message(
            "127.0.0.1:9021",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: NodeIdentity {
                    id: "health-node".into(),
                    hostname: "beelink1".into(),
                    ip: "192.168.1.14".into(),
                    role: NodeRole::Compute,
                },
                auth_token: String::new(),
                cpu_usage_pct: 42.5,
                ram_used_gb: 6.1,
                ram_total_gb: 15.9,
                gpu_usage_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
            }),
        )
        .await;

        assert!(
            ack.is_none(),
            "expected no reply for fire-and-forget message"
        );
        assert!(registry.lock().unwrap().get("health-node").is_some());
    }

    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
    async fn test_heartbeat_pushes_health_update_to_dashboard() {
        use crate::http::state::{DashboardEvent, DashboardState};

        let registry = Arc::new(Mutex::new(Registry::new()));
        let dashboard = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = dashboard.tx.subscribe();

        let mut server = Server::new("127.0.0.1:9025", registry.clone());
        server.auth_tokens = Arc::new(vec![]);
        server.dashboard = Some(dashboard);

        tokio::spawn(async move {
            let _ = server.run().await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        send_message(
            "127.0.0.1:9025",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: NodeIdentity {
                    id: "dash-node".into(),
                    hostname: "beelink1".into(),
                    ip: "192.168.1.14".into(),
                    role: NodeRole::Compute,
                },
                auth_token: String::new(),
                cpu_usage_pct: 55.0,
                ram_used_gb: 7.5,
                ram_total_gb: 15.9,
                gpu_usage_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
            }),
        )
        .await;

        // The broadcast channel may carry a TopologyUpdate first, then a HealthUpdate.
        let mut health_evt = None;
        for _ in 0..5 {
            match rx.try_recv() {
                Ok(DashboardEvent::HealthUpdate { node_id, samples }) => {
                    health_evt = Some((node_id, samples));
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        let (node_id, samples) = health_evt.expect("expected HealthUpdate");
        assert_eq!(node_id, "dash-node");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].cpu_pct, 55.0);
        assert_eq!(samples[0].ram_used_gb, 7.5);
        assert_eq!(samples[0].ram_total_gb, 15.9);
    }

    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
    async fn test_rejected_heartbeat_does_not_emit_health_update() {
        use crate::http::state::{DashboardEvent, DashboardState};

        let registry = Arc::new(Mutex::new(Registry::new()));
        let dashboard = DashboardState::new(
            Arc::new(vec!["correct-token".into()]),
            Arc::new(Mutex::new(HashMap::new())),
        );
        let mut rx = dashboard.tx.subscribe();

        let mut server = Server::new("127.0.0.1:9026", registry.clone());
        server.auth_tokens = Arc::new(vec!["correct-token".into()]);
        server.dashboard = Some(dashboard);

        tokio::spawn(async move {
            let _ = server.run().await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connection-level auth is correct ("correct-token"), but the Heartbeat
        // payload carries a wrong per-message auth_token — the per-message check
        // must reject it and must not call push_health.
        authenticated_send(
            "127.0.0.1:9026",
            "correct-token",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: NodeIdentity {
                    id: "bad-node".into(),
                    hostname: "rogue".into(),
                    ip: "10.0.0.18".into(),
                    role: NodeRole::Compute,
                },
                auth_token: "wrong-token".into(),
                cpu_usage_pct: 99.0,
                ram_used_gb: 15.0,
                ram_total_gb: 16.0,
                gpu_usage_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
            }),
        )
        .await;

        // No HealthUpdate should have been broadcast.
        let mut got_health = false;
        while let Ok(evt) = rx.try_recv() {
            if matches!(evt, DashboardEvent::HealthUpdate { .. }) {
                got_health = true;
            }
        }
        assert!(
            !got_health,
            "HealthUpdate must not be broadcast for rejected heartbeat"
        );
        assert!(registry.lock().unwrap().get("bad-node").is_none());
    }

    /// Open a raw TCP connection, send `AuthToken` then `msg`, read one signed reply.
    /// Used to test scenarios that require the connection-level AuthToken first frame.
    async fn authenticated_send(
        addr: &str,
        auth_token: &str,
        msg: &MeshMessage,
    ) -> Option<MeshMessage> {
        use shared::frame::{SignedFrame, derive_hmac_key};
        use tokio::io::AsyncWriteExt;

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let key = derive_hmac_key(auth_token);

        // Send AuthToken (unsigned) as the first frame.
        let raw_frame = |m: &MeshMessage| -> Vec<u8> {
            let data = serde_json::to_vec(m).unwrap();
            let mut out = (data.len() as u32).to_le_bytes().to_vec();
            out.extend_from_slice(&data);
            out
        };
        stream
            .write_all(&raw_frame(&MeshMessage::AuthToken(auth_token.into())))
            .await
            .unwrap();

        // Send the actual message as a signed frame.
        let payload = serde_json::to_vec(msg).unwrap();
        let signed = SignedFrame::sign(&key, payload);
        let signed_bytes = serde_json::to_vec(&signed).unwrap();
        let len = (signed_bytes.len() as u32).to_le_bytes();
        stream.write_all(&len).await.unwrap();
        stream.write_all(&signed_bytes).await.unwrap();

        // Some messages generate no reply — use a short timeout instead of blocking forever.
        let read_reply = async {
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.ok()?;
            let msg_len = u32::from_le_bytes(len_buf) as usize;
            let mut buf = vec![0u8; msg_len];
            stream.read_exact(&mut buf).await.ok()?;
            let frame: SignedFrame = serde_json::from_slice(&buf).ok()?;
            let payload = frame.verify(&key).ok()?;
            serde_json::from_slice(payload).ok()
        };

        tokio::time::timeout(std::time::Duration::from_millis(200), read_reply)
            .await
            .ok()
            .flatten()
    }

    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
    async fn test_heartbeat_wrong_token_not_registered() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let mut server = Server::new("127.0.0.1:9010", registry.clone());
        server.auth_tokens = Arc::new(vec!["correct-token".into()]);

        tokio::spawn(async move {
            let _ = server.run().await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connection-level auth passes, but heartbeat carries a wrong token.
        let ack = authenticated_send(
            "127.0.0.1:9010",
            "correct-token",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: NodeIdentity {
                    id: "rogue-node".into(),
                    hostname: "evil".into(),
                    ip: "127.0.0.1".into(),
                    role: NodeRole::Compute,
                },
                auth_token: "wrong-token".into(),
                cpu_usage_pct: 0.0,
                ram_used_gb: 0.0,
                ram_total_gb: 0.0,
                gpu_usage_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
            }),
        )
        .await;
        assert!(
            ack.is_none(),
            "expected no reply for fire-and-forget message"
        );
        assert!(registry.lock().unwrap().get("rogue-node").is_none());
    }

    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
    async fn test_heartbeat_empty_token_not_registered() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let mut server = Server::new("127.0.0.1:9012", registry.clone());
        server.auth_tokens = Arc::new(vec!["required-token".into()]);

        tokio::spawn(async move {
            let _ = server.run().await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Heartbeat carries an empty auth_token — must be rejected when tokens are configured.
        let ack = authenticated_send(
            "127.0.0.1:9012",
            "required-token",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: NodeIdentity {
                    id: "no-token-node".into(),
                    hostname: "old-agent".into(),
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
            }),
        )
        .await;
        assert!(
            ack.is_none(),
            "expected no reply for fire-and-forget message"
        );
        assert!(registry.lock().unwrap().get("no-token-node").is_none());
    }

    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
    async fn test_heartbeat_correct_token_registered() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let mut server = Server::new("127.0.0.1:9011", registry.clone());
        server.auth_tokens = Arc::new(vec!["good-token".into()]);

        tokio::spawn(async move {
            let _ = server.run().await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let ack = authenticated_send(
            "127.0.0.1:9011",
            "good-token",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: NodeIdentity {
                    id: "legit-node".into(),
                    hostname: "trusted".into(),
                    ip: "127.0.0.1".into(),
                    role: NodeRole::Compute,
                },
                auth_token: "good-token".into(),
                cpu_usage_pct: 0.0,
                ram_used_gb: 0.0,
                ram_total_gb: 0.0,
                gpu_usage_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
            }),
        )
        .await;
        assert!(
            ack.is_none(),
            "expected no reply for fire-and-forget message"
        );
        assert!(registry.lock().unwrap().get("legit-node").is_some());
    }

    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
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
        send_message(
            "127.0.0.1:9003",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: ident.clone(),
                auth_token: String::new(),
                cpu_usage_pct: 0.0,
                ram_used_gb: 0.0,
                ram_total_gb: 0.0,
                gpu_usage_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
            }),
        )
        .await;

        // Request full node info
        let reply = send_message(
            "127.0.0.1:9003",
            &MeshMessage::RequestNodeInfo("nodeA".into()),
        )
        .await;

        match reply.expect("RequestNodeInfo should produce a reply") {
            MeshMessage::NodeInfo(info) => {
                assert_eq!(info.id, "nodeA");
                assert_eq!(info.hostname, "host-a");
            }
            _ => panic!("Expected NodeInfo"),
        }
    }

    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
    async fn model_load_without_node_id_is_acknowledged() {
        // Auto-placement: coordinator receives ModelLoad with node_id=None.
        // No compute node is registered so auto-placement finds nothing and
        // returns Acknowledge (fail-silent, so the CLI doesn't hang). The important thing is it doesn't panic.
        let registry = Arc::new(Mutex::new(Registry::new()));
        let server = Server::new("127.0.0.1:9005", registry.clone());
        tokio::spawn(async move {
            let _ = server.run().await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let ack = send_message(
            "127.0.0.1:9005",
            &MeshMessage::ModelLoad(shared::ModelLoadRequest {
                request_id: "auto-1".into(),
                node_id: None,
                model_name: "qwen2.5:1.5b".into(),
                model_size_mb: 1024,
                wire_version: shared::WIRE_VERSION,
            }),
        )
        .await;

        assert!(
            matches!(ack, Some(MeshMessage::Acknowledge)),
            "auto-placement failure should still acknowledge so the CLI doesn't hang"
        );
    }

    /// During token rotation the coordinator accepts both the old and new token.
    /// A node carrying the old token and a node carrying the new token should
    /// both be registered successfully in the same rotation window.
    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
    async fn test_both_rotation_tokens_accepted() {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let mut server = Server::new("127.0.0.1:9013", registry.clone());
        server.auth_tokens = Arc::new(vec!["old-token".into(), "new-token".into()]);

        tokio::spawn(async move {
            let _ = server.run().await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Node carrying the old token — should be accepted.
        let ack = authenticated_send(
            "127.0.0.1:9013",
            "old-token",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: NodeIdentity {
                    id: "old-token-node".into(),
                    hostname: "pi1".into(),
                    ip: "127.0.0.1".into(),
                    role: NodeRole::Compute,
                },
                auth_token: "old-token".into(),
                cpu_usage_pct: 0.0,
                ram_used_gb: 0.0,
                ram_total_gb: 0.0,
                gpu_usage_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
            }),
        )
        .await;
        assert!(
            ack.is_none(),
            "expected no reply for fire-and-forget message"
        );
        assert!(registry.lock().unwrap().get("old-token-node").is_some());

        // Node already updated to the new token — should also be accepted.
        let ack2 = authenticated_send(
            "127.0.0.1:9013",
            "new-token",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: NodeIdentity {
                    id: "new-token-node".into(),
                    hostname: "beelink1".into(),
                    ip: "127.0.0.1".into(),
                    role: NodeRole::Compute,
                },
                auth_token: "new-token".into(),
                cpu_usage_pct: 0.0,
                ram_used_gb: 0.0,
                ram_total_gb: 0.0,
                gpu_usage_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
            }),
        )
        .await;
        assert!(ack2.is_none(), "heartbeat should produce no reply");
        assert!(registry.lock().unwrap().get("new-token-node").is_some());
    }
}
