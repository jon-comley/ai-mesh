use crate::device_catalog;
use crate::http::state::{
    DashboardState, ModelEntry, NodeModelInfo, RoomInfo, SecurityEvent, SecurityEventKind,
};
use crate::registry::Registry;
use crate::scheduler::Scheduler;
use shared::frame::{
    FrameReadError, FrameVerifyError, SignedFrame, derive_hmac_key, read_bounded_frame,
};
use shared::{
    AdminMessage, HeartbeatPayload, InferenceRequest, MeshMessage, ModelLifecycleState,
    ModelLoadRequest, NodeRole,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, warn};

pub use crate::http::state::NodeConnections as Connections;
pub use crate::http::state::{PendingInferences, PendingIntents, PendingStreams};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

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
    pub pending_inferences: PendingInferences,
    pub pending_intents: PendingIntents,
    pub pending_streams: PendingStreams,
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
            pending_streams: Arc::new(Mutex::new(HashMap::new())),
            auth_tokens: Arc::new(vec![]),
            tls: None,
            dashboard: None,
        }
    }

    pub async fn run(&self) -> Result<(), ServerError> {
        let listener = TcpListener::bind(&self.addr).await?;

        loop {
            let (socket, peer_addr) = listener.accept().await?;
            let registry = self.registry.clone();
            let connections = self.connections.clone();
            let pending_inferences = self.pending_inferences.clone();
            let pending_intents = self.pending_intents.clone();
            let pending_streams = self.pending_streams.clone();
            let auth_tokens = self.auth_tokens.clone();
            let dashboard = self.dashboard.clone();

            if let Some(acceptor) = &self.tls {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    match acceptor.accept(socket).await {
                        Ok(tls_stream) => {
                            let _ = handle_connection(
                                tls_stream,
                                peer_addr,
                                registry,
                                connections,
                                pending_inferences,
                                pending_intents,
                                pending_streams,
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
                        peer_addr,
                        registry,
                        connections,
                        pending_inferences,
                        pending_intents,
                        pending_streams,
                        auth_tokens,
                        dashboard,
                    )
                    .await;
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_connection<S>(
    socket: S,
    peer_addr: SocketAddr,
    registry: Arc<Mutex<Registry>>,
    connections: Connections,
    pending_inferences: PendingInferences,
    pending_intents: PendingIntents,
    pending_streams: PendingStreams,
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
        let buf = match read_bounded_frame(&mut reader).await {
            Ok(buf) => buf,
            Err(FrameReadError::Closed) => return Ok(()),
            Err(FrameReadError::TooLarge(n)) => {
                warn!(
                    "rejected connection from {peer_addr}: auth frame length {n} exceeds MAX_FRAME_LEN"
                );
                return Ok(());
            }
        };
        match serde_json::from_slice::<MeshMessage>(&buf) {
            Ok(MeshMessage::AuthToken(token)) if auth_tokens.contains(&token) => {
                Some(derive_hmac_key(&token))
            }
            Ok(MeshMessage::AuthToken(_)) => {
                warn!("rejected connection: invalid auth token");
                if let Some(ref dash) = dashboard {
                    dash.push_security(SecurityEvent {
                        ts_ms: now_ms(),
                        kind: SecurityEventKind::NodeAuthFailed,
                        source: "unknown".into(),
                        detail: "invalid auth token on connect".into(),
                    });
                }
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
    let auth_tag: &'static str = if hmac_key.is_some() {
        "TLS+HMAC"
    } else {
        "no auth"
    };

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

    // Read timeout: a node that goes silent (e.g. a WSL2 suspend/resume that
    // leaves a half-open socket) must be detected, because nothing else will —
    // without this we block on read forever, never close the socket, and so the
    // agent's read half never sees EOF to drive its own reconnect. Closing here
    // sends a FIN that lets the agent notice and reconnect.
    //
    // The interval is configurable up to MAX_HEARTBEAT_SECS, so the timeout is
    // sized from each node's *observed* heartbeat cadence (3× the gap, floored at
    // 15s). It starts generous and only tightens once we've seen a real gap
    // between two heartbeats — the first heartbeat arrives immediately on connect,
    // so it isn't a representative interval and must not drive the timeout.
    const MAX_HEARTBEAT_SECS: u64 = 3600; // mirrors the bound in http::api::nodes::set_heartbeat_interval
    // Grace granted while a model is loading. Sized to the agent's own worst-case
    // load: `health_timeout_secs` caps at 900s for the largest models (180s for the
    // ≤7b models here), so the agent always reports Ready/Failed within this window.
    // Bounds a node that dies mid-load to ~15 min cleanup rather than the full cap.
    const MODEL_LOAD_GRACE_SECS: u64 = 900;
    let cap = Duration::from_secs(MAX_HEARTBEAT_SECS + 30);
    let load_grace = Duration::from_secs(MODEL_LOAD_GRACE_SECS);
    let mut read_timeout = cap;
    let mut last_gap: Option<Duration> = None;
    let mut last_heartbeat: Option<Instant> = None;
    // A model load goes silent for a long stretch — the node is CPU-pegged
    // downloading / launching llama-server and heartbeats stall — so while a model
    // is Loading we must grant a generous grace or we'd close the connection
    // mid-load. The agent sends ModelStatus{Loading} before that stretch and
    // Ready/Failed after, so this stays sticky across the silence.
    let mut loading = false;

    loop {
        let buf = match timeout(read_timeout, read_bounded_frame(&mut reader)).await {
            Ok(Ok(buf)) => buf,
            Ok(Err(FrameReadError::Closed)) => break,
            Ok(Err(FrameReadError::TooLarge(n))) => {
                warn!(
                    "dropping connection from {peer_addr}: frame length {n} exceeds MAX_FRAME_LEN"
                );
                break;
            }
            Err(_elapsed) => {
                warn!(
                    %peer_addr,
                    timeout_secs = read_timeout.as_secs(),
                    "no frame within read timeout — closing stale connection so the node can reconnect"
                );
                break;
            }
        };
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

        // Size the next read timeout. Normally it's 3× the heartbeat cadence,
        // measured between two real heartbeats (the first arrives immediately on
        // connect and is not a representative interval, so it must not drive the
        // timeout). While a model is Loading we use the generous cap instead.
        match &msg {
            MeshMessage::Heartbeat(_) => {
                let now = Instant::now();
                if let Some(prev) = last_heartbeat {
                    last_gap = Some(now.duration_since(prev));
                }
                last_heartbeat = Some(now);
            }
            MeshMessage::ModelStatus(report) => {
                loading = matches!(report.state, ModelLifecycleState::Loading);
            }
            _ => {}
        }
        read_timeout = next_read_timeout(loading, last_gap, cap, load_grace);

        let reply = process_message(
            msg,
            &registry,
            &connections,
            &pending_inferences,
            &pending_intents,
            &pending_streams,
            &tx,
            &mut node_id,
            &auth_tokens,
            dashboard.as_ref(),
            auth_tag,
        )
        .await;

        if let Some(reply) = reply
            && tx.send(reply).await.is_err()
        {
            break;
        }
    }

    // Security event: node disconnected.
    if let (Some(id), Some(dash)) = (&node_id, &dashboard) {
        dash.push_security(SecurityEvent {
            ts_ms: now_ms(),
            kind: SecurityEventKind::NodeLeave,
            source: id.clone(),
            detail: String::new(),
        });
    }

    // Remove this connection's routing channel when the connection closes.
    if let Some(id) = node_id {
        info!(node_id = %id, "connection closed, removing from connection map");
        connections.lock().unwrap().remove(&id);

        // Agent service stops kill llama-server, so clear stale model state now.
        // This runs on agent disconnect (TCP closes cleanly). On coordinator restart
        // Tokio cancels tasks before cleanup runs, so DB model state is preserved.
        {
            let mut reg = registry.lock().unwrap();
            // Was this the lighting node? Check before clearing anything.
            let was_lighting = reg.node_has_feature(&id, shared::Feature::Lighting);
            reg.clear_node_models(&id);
            if let Some(dash) = &dashboard {
                let snapshot = build_model_snapshot(&reg);
                dash.push_model_update(snapshot);
                // Losing the lighting node means we can no longer trust the last
                // bridge status it sent — reset to unknown so the dashboard stops
                // showing a stale "online" the node can't refute.
                if was_lighting {
                    info!(node_id = %id, "lighting node disconnected — bridge status → unknown");
                    dash.reset_zigbee_status();
                }
            }
        }

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
        drop(pending);

        // Same for in-flight streams: the SSE emitter turns this Error into
        // an error event + termination instead of holding the connection open.
        let mut streams = pending_streams.lock().unwrap();
        let to_fail: Vec<String> = streams
            .iter()
            .filter(|(_, (_, nid))| nid == &id)
            .map(|(k, _)| k.clone())
            .collect();
        for req_id in to_fail {
            if let Some((stx, _)) = streams.remove(&req_id) {
                warn!(node_id = %id, request_id = %req_id, "failing in-flight stream: agent disconnected");
                let _ = stx.try_send(MeshMessage::Error(format!(
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

/// Size the next read timeout from the gap between two heartbeats: 3× the gap
/// (generous tolerance for jitter), floored at 15s so a fast 5s cadence still
/// detects a dead peer promptly, and capped so a slow-heartbeat node configured
/// near the maximum interval is never falsely dropped.
fn read_timeout_from_gap(gap: Duration, cap: Duration) -> Duration {
    (gap * 3).clamp(Duration::from_secs(15), cap)
}

/// The next read timeout for a connection. While a model is loading the node goes
/// silent (no heartbeats during a long download / llama-server launch), so we use
/// `load_grace` and never trip the cadence timeout mid-load. Otherwise the timeout
/// is sized from the observed heartbeat cadence (`cap` until one is known).
fn next_read_timeout(
    loading: bool,
    last_gap: Option<Duration>,
    cap: Duration,
    load_grace: Duration,
) -> Duration {
    if loading {
        load_grace
    } else {
        last_gap.map_or(cap, |gap| read_timeout_from_gap(gap, cap))
    }
}

/// Heartbeat: validate the per-message auth token, refresh the registry and
/// connection map, and push topology/health to the dashboard.
#[allow(clippy::too_many_arguments)]
fn handle_heartbeat(
    payload: HeartbeatPayload,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    tx: &mpsc::Sender<MeshMessage>,
    node_id: &mut Option<String>,
    auth_tokens: &Arc<Vec<String>>,
    dashboard: Option<&DashboardState>,
    auth_tag: &'static str,
) -> Option<MeshMessage> {
    let HeartbeatPayload {
        identity,
        auth_token,
        cpu_usage_pct,
        ram_used_gb,
        ram_total_gb,
        gpu_usage_pct,
        gpu_vram_used_gb,
        gpu_vram_total_gb,
        disk_free_gb,
    } = payload;
    // When tokens are configured, require the heartbeat token to match exactly.
    if !auth_tokens.is_empty() && !auth_tokens.iter().any(|a| a == &auth_token) {
        warn!(node_id = %identity.id, "heartbeat rejected: missing or wrong auth token");
        if let Some(dash) = dashboard {
            dash.push_security(SecurityEvent {
                ts_ms: now_ms(),
                kind: SecurityEventKind::NodeAuthFailed,
                source: identity.id.clone(),
                detail: "heartbeat: wrong auth token".into(),
            });
        }
        return None;
    }
    info!(node_id = %identity.id, hostname = %identity.hostname, "heartbeat");
    let this_id = identity.id.clone();
    let this_hostname = identity.hostname.clone();
    let is_first_heartbeat = node_id.is_none();
    *node_id = Some(this_id.clone());
    let nodes = {
        let mut reg = registry.lock().unwrap();
        reg.update_heartbeat(identity.clone());
        if is_first_heartbeat {
            // Agent (re)connected — llama-server may have been killed since
            // the coordinator last saw it. Clear stale model state so the
            // scheduler doesn't route to a server that isn't running.
            reg.clear_node_models(&this_id);
        }
        reg.list_nodes()
    };
    connections.lock().unwrap().insert(identity.id, tx.clone());
    if let Some(dash) = dashboard {
        if is_first_heartbeat {
            dash.push_security(SecurityEvent {
                ts_ms: now_ms(),
                kind: SecurityEventKind::NodeJoin,
                source: this_id.clone(),
                detail: format!("{this_hostname} · {auth_tag}"),
            });
        }
        dash.push_topology(&nodes);
        dash.push_health(
            &this_id,
            cpu_usage_pct,
            ram_used_gb,
            ram_total_gb,
            gpu_usage_pct,
            gpu_vram_used_gb,
            gpu_vram_total_gb,
            disk_free_gb,
        );
    }
    Some(MeshMessage::Acknowledge)
}

/// CLI-peer inference (`mesh infer`): wait for the model to become Ready
/// (pull phase), forward to the serving agent, and await the result with a
/// separate generation timeout. The HTTP paths use `crate::inference` instead.
async fn handle_cli_inference(
    req: InferenceRequest,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_inferences: &PendingInferences,
) -> Option<MeshMessage> {
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

/// ModelLoad from a CLI peer: auto-place onto the best-fit node when no
/// target was given, then forward to that node's agent.
async fn handle_model_load(
    mut req: ModelLoadRequest,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
) -> Option<MeshMessage> {
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

/// LightState from a lighting node: persist, surface on the dashboard, and
/// auto-pause a room effect when one of its devices goes offline. The
/// authenticated connection id overrides the payload id (stale ids would
/// route commands to a dead connection).
fn handle_light_state(
    mut report: shared::LightStateReport,
    registry: &Arc<Mutex<Registry>>,
    node_id: Option<&str>,
    dashboard: Option<&DashboardState>,
) -> Option<MeshMessage> {
    // A Zigbee group publishes state on its base topic exactly like a
    // device. If one slipped past the capability's group filter (a group's
    // retained state can arrive before its group list on (re)connect),
    // don't persist or surface it — and scrub any row an earlier slip left.
    if dashboard.is_some_and(|d| d.is_known_group(&report.device_id)) {
        registry.lock().unwrap().delete_device(&report.device_id);
        return None;
    }
    // The authenticated connection IS the owning node — trust its id over
    // the report payload. A stale/'unknown' node_id (e.g. from a seeded DB)
    // would otherwise route device commands to a non-existent connection
    // (HTTP 503, lights dead until a state change re-reports them).
    if let Some(id) = node_id {
        report.node_id = id.to_string();
    }
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
                dash.push_effect_update(room_id.clone(), None, serde_json::json!({}), vec![]);
                dash.solar_sweep_notify.notify_one();
            }
        }
    }
    None
}

/// If this exact (device_id, action) pair has a binding (see
/// `registry::SwitchBindingRecord`), resolve its target room/group's *live*
/// membership and dispatch the bound command through the same
/// `dispatch_light_command` fan-out room/group commands already use — a
/// bound button press or dial rotation is the same "send this to a set of
/// devices" operation, just triggered by a Zigbee event instead of an HTTP
/// request. `brightness_step` reads each target device's current
/// brightness from the live snapshot and nudges it by the binding's signed
/// `step_delta`, clamped to 1..=254 — computed per-device (not once for the
/// whole target) since a group's members can be at different levels.
fn dispatch_switch_binding(
    device_id: &str,
    action: &str,
    registry: &Arc<Mutex<Registry>>,
    dash: &DashboardState,
) {
    let binding = registry
        .lock()
        .unwrap()
        .find_switch_binding(device_id, action);
    let Some(binding) = binding else { return };
    let targets = registry
        .lock()
        .unwrap()
        .resolve_switch_binding_targets(&binding);
    if targets.is_empty() {
        warn!(binding_id = %binding.id, "switch binding target has no devices, nothing to do");
        return;
    }
    match binding.command.as_str() {
        "on" => {
            crate::http::api::rooms::dispatch_light_command(
                dash,
                &targets,
                &shared::LightAction::On,
            );
        }
        "off" => {
            crate::http::api::rooms::dispatch_light_command(
                dash,
                &targets,
                &shared::LightAction::Off,
            );
        }
        "toggle" => {
            crate::http::api::rooms::dispatch_light_command(
                dash,
                &targets,
                &shared::LightAction::Toggle,
            );
        }
        "brightness_step" => {
            const DEFAULT_BRIGHTNESS: u8 = 128;
            let delta = binding.step_delta.unwrap_or(0);
            let snapshot = dash.get_light_snapshot();
            let current: HashMap<&str, u8> = snapshot
                .iter()
                .map(|l| {
                    (
                        l.device_id.as_str(),
                        l.brightness.unwrap_or(DEFAULT_BRIGHTNESS),
                    )
                })
                .collect();
            for target in &targets {
                let base = i32::from(*current.get(target.as_str()).unwrap_or(&DEFAULT_BRIGHTNESS));
                let new_value = (base + delta).clamp(1, 254) as u8;
                crate::http::api::rooms::dispatch_light_command(
                    dash,
                    std::slice::from_ref(target),
                    &shared::LightAction::Brightness(new_value),
                );
            }
        }
        other => {
            warn!(command = %other, binding_id = %binding.id, "unknown switch binding command")
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_message(
    msg: MeshMessage,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_inferences: &PendingInferences,
    pending_intents: &PendingIntents,
    pending_streams: &PendingStreams,
    tx: &mpsc::Sender<MeshMessage>,
    node_id: &mut Option<String>,
    auth_tokens: &Arc<Vec<String>>,
    dashboard: Option<&Arc<DashboardState>>,
    auth_tag: &'static str,
) -> Option<MeshMessage> {
    match msg {
        MeshMessage::Heartbeat(payload) => handle_heartbeat(
            payload,
            registry,
            connections,
            tx,
            node_id,
            auth_tokens,
            dashboard.map(|d| d.as_ref()),
            auth_tag,
        ),
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
            Some(match full {
                Some(info) => MeshMessage::NodeInfo(info),
                // A clear not-found beats a fabricated placeholder record —
                // "Hostname: unknown, 0ms heartbeat" reads like a live node.
                None => MeshMessage::Error(format!("no node with id '{id}'")),
            })
        }
        MeshMessage::RequestModelInference(req) => {
            handle_cli_inference(req, registry, connections, pending_inferences).await
        }
        MeshMessage::ModelInferenceResult(res) => {
            info!(
                request_id = %res.request_id,
                node_id    = %res.node_id,
                "model inference result received from agent"
            );
            // Streamed request: the result is the stream terminator. A Full
            // error is tolerable here — the emitter treats channel-closed-
            // without-terminal as an error and ends the SSE stream cleanly.
            let stream_entry = pending_streams.lock().unwrap().remove(&res.request_id);
            if let Some((stx, _)) = stream_entry {
                let _ = stx.try_send(MeshMessage::ModelInferenceResult(res));
                return None;
            }
            let entry = pending_inferences.lock().unwrap().remove(&res.request_id);
            if let Some((otx, _)) = entry {
                let _ = otx.send(MeshMessage::ModelInferenceResult(res));
            }
            None
        }
        MeshMessage::ModelInferenceChunk(chunk) => {
            // Clone the sender out of the lock — never send while holding it.
            let entry = pending_streams
                .lock()
                .unwrap()
                .get(&chunk.request_id)
                .map(|(stx, _)| stx.clone());
            let request_id = chunk.request_id.clone();
            let mut consumer_gone = false;
            if let Some(stx) = entry {
                use tokio::sync::mpsc::error::TrySendError;
                match stx.try_send(MeshMessage::ModelInferenceChunk(chunk)) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        // SSE client can't keep up — kill the stream rather
                        // than buffer unboundedly. The emitter sees the
                        // channel close without a terminal and errors out.
                        warn!(request_id = %request_id,
                              "stream buffer full — dropping slow stream");
                        pending_streams.lock().unwrap().remove(&request_id);
                        consumer_gone = true;
                    }
                    Err(TrySendError::Closed(_)) => {
                        pending_streams.lock().unwrap().remove(&request_id);
                        consumer_gone = true;
                    }
                }
            } else {
                // No entry: the client hung up (adapter removed it) or it was
                // already killed. Either way nobody is listening.
                consumer_gone = true;
            }
            // Tell the node to stop generating for a stream nobody consumes;
            // replying on this same connection reaches the serving agent.
            // Idempotent — repeated cancels for the same id are no-ops.
            consumer_gone.then_some(MeshMessage::CancelInference { request_id })
        }
        MeshMessage::ModelLoad(req) => handle_model_load(req, registry, connections).await,
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
            let device_states = dashboard
                .map(|d| d.get_light_snapshot())
                .unwrap_or_default();
            let sensor_states = dashboard
                .map(|d| d.get_sensor_snapshot())
                .unwrap_or_default();
            let reaper_online = dashboard
                .and_then(|d| d.get_reaper_snapshot())
                .is_some_and(|s| s.reaper_online);
            // Spawned, NOT awaited inline: handle_intent dispatches inference
            // to a node and waits for its ModelInferenceResult — which, when
            // the intent came from that same node (an agent's voice
            // capability asking for inference that lands back on itself),
            // arrives on the very connection this read loop serves. Awaiting
            // here deadlocks: the result sits unread in the socket while
            // handle_intent waits for it, until the inference timeout fires.
            // Caught live 2026-07-08 on the first voice→intent test — the
            // LLM finished in 42s but the response never came back. (Also
            // the source of the "frame timestamp is stale" warnings: the
            // node's heartbeats queued behind the blocked reader for 40s+
            // and were skew-rejected once finally read.)
            let registry = registry.clone();
            let connections = connections.clone();
            let pending_inferences = pending_inferences.clone();
            let pending_intents = pending_intents.clone();
            let reply_tx = tx.clone();
            let dashboard = dashboard.cloned();
            let transcript = req.text.clone();
            let source = req.source;
            tokio::spawn(async move {
                // Honor the Online AI tab for mesh-originated intents too
                // (voice, CLI) — same construction as the dashboard chat
                // path in http/api/chat.rs, same local fallback on cloud
                // failure inside handle_intent.
                let gateway = dashboard.as_ref().and_then(|state| {
                    let cfg = crate::cloud::GatewayConfig::load(&registry.lock().unwrap());
                    match cfg.provider() {
                        Some(provider) if cfg.enabled => Some(crate::cloud::GatewayInvocation {
                            provider,
                            engine: cfg.engine,
                            compress: cfg.compress,
                            state: state.clone(),
                        }),
                        _ => {
                            if cfg.enabled {
                                warn!(
                                    "online AI is enabled but not fully configured (missing API key or model); using local inference"
                                );
                            }
                            None
                        }
                    }
                });
                let used_gateway = gateway.is_some();
                let response = crate::intent::handle_intent(
                    req,
                    registry.clone(),
                    connections,
                    pending_inferences,
                    pending_intents,
                    device_states,
                    sensor_states,
                    reaper_online,
                    gateway,
                )
                .await;
                // Reflect updated cumulative stats / last-error on the
                // Gateway tab, exactly as the dashboard chat path does.
                if used_gateway && let Some(state) = &dashboard {
                    let snap = crate::http::api::gateway::gateway_snapshot(
                        &registry.lock().unwrap(),
                        state,
                    );
                    state.push_gateway_update(snap);
                }
                // Surface spoken exchanges to dashboard consumers — the chat
                // window today; this broadcast (and this exact spot) is the
                // seam where a future TTS/speaker output router taps the
                // response for audible replies.
                if source == shared::IntentSource::Voice
                    && let Some(state) = &dashboard
                {
                    state.push_voice_exchange(transcript, &response);
                }
                if reply_tx
                    .send(MeshMessage::IntentResponse(response))
                    .await
                    .is_err()
                {
                    warn!("intent requester disconnected before the response was ready");
                }
            });
            None
        }
        MeshMessage::SceneLoaded(report) => {
            let entry = pending_intents.lock().unwrap().remove(&report.request_id);
            if let Some(otx) = entry {
                let _ = otx.send(MeshMessage::SceneLoaded(report));
            }
            None
        }
        MeshMessage::TtsResponse(resp) => {
            let entry = pending_intents.lock().unwrap().remove(&resp.request_id);
            if let Some(otx) = entry {
                let _ = otx.send(MeshMessage::TtsResponse(resp));
            }
            None
        }
        MeshMessage::AudioAnnounce(req) => {
            let registry = registry.clone();
            let connections = connections.clone();
            let request_id = req.request_id.clone();
            let reply_tx = tx.clone();
            // Spawned like the IntentRequest arm above: fanning a broadcast
            // out to every audio node (or waiting on one room's connection)
            // must not block this connection's read loop. Reports delivery
            // back to the requester (AudioAnnounceResult) so the voice
            // pipeline's room routing can fall back to the puck instead of
            // the reply silently going nowhere when the sink is unreachable.
            tokio::spawn(async move {
                let delivered =
                    crate::audio::handle_audio_announce(req, &registry, &connections).await;
                let _ = reply_tx
                    .send(MeshMessage::AudioAnnounceResult(
                        shared::AudioAnnounceResult {
                            request_id,
                            delivered,
                        },
                    ))
                    .await;
            });
            None
        }
        MeshMessage::LightState(report) => handle_light_state(
            report,
            registry,
            node_id.as_deref(),
            dashboard.map(|d| d.as_ref()),
        ),
        MeshMessage::SensorState(mut report) => {
            // Trust the authenticated connection's id (see LightState above).
            if let Some(id) = node_id.as_deref() {
                report.node_id = id.to_string();
            }
            info!(
                node_id = %report.node_id,
                device_id = %report.device_id,
                online = %report.online,
                "sensor state report received"
            );
            // The dashboard snapshot owns the field-wise merge; persist the
            // merged record so partial publishes never wipe stored readings.
            let merged = match dashboard {
                Some(dash) => dash.push_sensor_update(report),
                None => report,
            };
            registry.lock().unwrap().save_sensor_state(&merged);
            None
        }
        MeshMessage::DeviceList(mut report) => {
            // Trust the authenticated connection's id (see LightState above) so a
            // stale payload id can't create a phantom device row.
            if let Some(id) = node_id.as_deref() {
                report.node_id = id.to_string();
            }
            info!(
                node_id = %report.node_id,
                devices = report.devices.len(),
                groups = ?report.groups,
                "device list received"
            );
            if let Some(dash) = dashboard {
                // The sender owns a Zigbee bridge — bridge-wide admin commands
                // (permit-join, device removal) route to it.
                dash.set_zigbee_node(&report.node_id);
                dash.push_group_update(&report.node_id, report.groups.clone());
                // Placeholder cards are a lighting-UI concept — seed them for
                // lights only; other device classes get their own snapshots.
                let light_names: Vec<String> = report
                    .devices
                    .iter()
                    .filter(|d| d.device_type == shared::DeviceType::Light)
                    .map(|d| d.id.clone())
                    .collect();
                dash.push_device_discovery(&report.node_id, light_names, true);
                dash.push_other_devices(&report.node_id, &report.devices);
            }
            {
                let mut reg = registry.lock().unwrap();
                // Scrub any persisted device rows that are actually groups — a
                // group's retained state can be saved as a device before its group
                // list arrives, and that row would otherwise reload every restart.
                for g in &report.groups {
                    reg.delete_device(g);
                }
                reg.update_devices(&report.node_id, report.devices, report.groups);
            }
            None
        }
        MeshMessage::ZigbeeStatus { online } => {
            if online {
                info!("zigbee: bridge online");
            } else {
                warn!("zigbee: bridge offline");
            }
            if let Some(dash) = dashboard {
                if let Some(id) = node_id.as_deref() {
                    dash.set_zigbee_node(id);
                }
                dash.push_zigbee_status(online);
            }
            None
        }
        MeshMessage::ZigbeeJoin(report) => {
            info!(
                event = %report.event,
                device_id = %report.device_id,
                "zigbee: pairing event"
            );
            if let Some(dash) = dashboard {
                // A device_leave is a much stronger, faster signal than
                // waiting on z2m's own availability timeout (which can be
                // ~25h for a battery/passive sensor) — act on it
                // immediately rather than leaving stale "online" readings
                // showing for that whole window.
                if report.event == "device_leave" {
                    dash.mark_device_offline(&report.device_id);
                }
                // The model is only known once, right here, on interview
                // success — auto-assign a default name from the catalog so
                // the device doesn't sit showing its raw hex id forever.
                // Never overwrites an existing name (auto-assigned earlier
                // or set by hand) — see plans/device-auto-naming.md.
                if report.event == "device_interview_successful"
                    && let Some(line) = report
                        .model
                        .as_deref()
                        .and_then(device_catalog::product_line_name)
                {
                    let mut reg = registry.lock().unwrap();
                    let mut names = reg.get_all_device_names();
                    if !names.contains_key(&report.device_id) {
                        let name = device_catalog::next_name_in_line(&names, line);
                        reg.set_device_name(&report.device_id, &name);
                        names.insert(report.device_id.clone(), name);
                        let rooms: Vec<RoomInfo> =
                            reg.list_rooms().into_iter().map(RoomInfo::from).collect();
                        drop(reg);
                        dash.push_rooms_update_with_names(rooms, names);
                    }
                }
                dash.push_join_event(report.event, report.device_id, report.model);
            }
            None
        }
        MeshMessage::SwitchAction(report) => {
            // debug, not info: unlike a pairing event (rare), a dial rotation
            // can fire many times per interaction — info! here would make
            // fiddling with one Tap Dial dominate the log.
            debug!(
                device_id = %report.device_id,
                action = %report.action,
                "zigbee: switch action"
            );
            if let Some(dash) = dashboard {
                dispatch_switch_binding(&report.device_id, &report.action, registry, dash);
                dash.push_switch_action(report.device_id, report.action);
            }
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
        MeshMessage::ReaperStatus(mut report) => {
            if let Some(id) = node_id.as_deref() {
                report.node_id = id.to_string();
            }
            if let Some(dash) = dashboard {
                dash.push_reaper_status(report);
            }
            None
        }
        MeshMessage::ReaperCommandResult(result) => {
            let entry = pending_intents.lock().unwrap().remove(&result.request_id);
            if let Some(otx) = entry {
                let _ = otx.send(MeshMessage::ReaperCommandResult(result));
            }
            None
        }
        MeshMessage::ReaperScriptResult(result) => {
            let entry = pending_intents.lock().unwrap().remove(&result.request_id);
            if let Some(otx) = entry {
                let _ = otx.send(MeshMessage::ReaperScriptResult(result));
            }
            None
        }
        MeshMessage::ArtStatus(mut report) => {
            if let Some(id) = node_id.as_deref() {
                report.node_id = id.to_string();
            }
            if let Some(dash) = dashboard {
                dash.push_art_status(report);
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::state::DashboardState;
    use shared::{HeartbeatPayload, LightStateReport, NodeIdentity, NodeRole};
    use tokio::net::TcpStream;

    #[test]
    fn read_timeout_sizes_to_heartbeat_cadence() {
        let cap = Duration::from_secs(3630);
        // Fast 5s cadence → 3× = 15s, which is also the floor.
        assert_eq!(
            read_timeout_from_gap(Duration::from_secs(5), cap),
            Duration::from_secs(15)
        );
        // A sub-second gap (e.g. a burst of frames) is floored, never tighter than 15s.
        assert_eq!(
            read_timeout_from_gap(Duration::from_millis(200), cap),
            Duration::from_secs(15)
        );
        // A 60s cadence scales linearly: 3× = 180s.
        assert_eq!(
            read_timeout_from_gap(Duration::from_secs(60), cap),
            Duration::from_secs(180)
        );
        // A near-max cadence is capped so the node is never falsely dropped.
        assert_eq!(read_timeout_from_gap(Duration::from_secs(3600), cap), cap);
    }

    #[test]
    fn read_timeout_grants_grace_while_loading() {
        let cap = Duration::from_secs(3630);
        let grace = Duration::from_secs(900);
        // While a model is loading, use the (bounded) load grace regardless of
        // cadence — a load goes silent for minutes and must not be timed out, but
        // the grace is far shorter than the cap so a dead-mid-load node is cleaned up.
        assert_eq!(
            next_read_timeout(true, Some(Duration::from_secs(5)), cap, grace),
            grace
        );
        assert_eq!(next_read_timeout(true, None, cap, grace), grace);
        // Not loading: fall back to the cadence-sized timeout (cap until known).
        assert_eq!(next_read_timeout(false, None, cap, grace), cap);
        assert_eq!(
            next_read_timeout(false, Some(Duration::from_secs(5)), cap, grace),
            Duration::from_secs(15)
        );
    }

    // Build the process_message arg set with an empty registry + dashboard, no TCP.
    #[allow(clippy::type_complexity)]
    fn test_deps() -> (
        Arc<Mutex<Registry>>,
        Connections,
        PendingInferences,
        PendingIntents,
        PendingStreams,
        mpsc::Sender<MeshMessage>,
        Arc<Vec<String>>,
        Arc<DashboardState>,
    ) {
        let registry = Arc::new(Mutex::new(Registry::new()));
        let connections: Connections = Arc::new(Mutex::new(HashMap::new()));
        let pending_inferences: PendingInferences = Arc::new(Mutex::new(HashMap::new()));
        let pending_intents: PendingIntents = Arc::new(Mutex::new(HashMap::new()));
        let pending_streams: PendingStreams = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(8);
        let auth_tokens = Arc::new(vec![]);
        let dashboard = DashboardState::new(Arc::new(vec![]), Arc::new(Mutex::new(HashMap::new())));
        (
            registry,
            connections,
            pending_inferences,
            pending_intents,
            pending_streams,
            tx,
            auth_tokens,
            dashboard,
        )
    }

    // A chunk for a stream nobody consumes (client hung up → adapter removed
    // the entry) must be answered with CancelInference so the node stops
    // generating instead of holding its inference slot to completion.
    #[tokio::test]
    async fn orphan_chunk_replies_with_cancel() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        let mut node_id = Some("node-1".to_string());

        let reply = process_message(
            MeshMessage::ModelInferenceChunk(shared::InferenceChunk {
                request_id: "chatcmpl-gone".into(),
                node_id: "node-1".into(),
                delta: "tok".into(),
                wire_version: shared::WIRE_VERSION,
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        assert_eq!(
            reply,
            Some(MeshMessage::CancelInference {
                request_id: "chatcmpl-gone".into()
            })
        );
    }

    // A chunk for a live stream is forwarded and NOT answered with a cancel.
    #[tokio::test]
    async fn live_chunk_is_forwarded_without_cancel() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        let mut node_id = Some("node-1".to_string());

        let (stx, mut srx) = mpsc::channel(4);
        ps.lock()
            .unwrap()
            .insert("chatcmpl-live".into(), (stx, "node-1".into()));

        let reply = process_message(
            MeshMessage::ModelInferenceChunk(shared::InferenceChunk {
                request_id: "chatcmpl-live".into(),
                node_id: "node-1".into(),
                delta: "tok".into(),
                wire_version: shared::WIRE_VERSION,
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        assert_eq!(reply, None);
        match srx.try_recv().unwrap() {
            MeshMessage::ModelInferenceChunk(c) => assert_eq!(c.delta, "tok"),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    // ── ZigbeeJoin device_leave → mark_device_offline ───────────────────────

    #[tokio::test]
    async fn device_leave_marks_a_known_sensor_offline() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        dashboard.push_sensor_update(shared::SensorReport {
            node_id: "node-1".into(),
            device_id: "sensor1".into(),
            temperature: Some(21.4),
            humidity: None,
            battery: None,
            occupancy: None,
            contact: None,
            illuminance: None,
            online: true,
        });
        let mut node_id = Some("zigbee-node".to_string());

        process_message(
            MeshMessage::ZigbeeJoin(shared::ZigbeeJoinEvent {
                node_id: "zigbee-node".into(),
                event: "device_leave".into(),
                device_id: "sensor1".into(),
                model: None,
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        let snap = dashboard.get_sensor_snapshot();
        let sensor = snap.iter().find(|s| s.device_id == "sensor1").unwrap();
        assert!(
            !sensor.online,
            "device_leave should immediately mark it offline"
        );
        assert_eq!(sensor.temperature, Some(21.4), "last reading must survive");
    }

    #[tokio::test]
    async fn other_join_events_do_not_mark_devices_offline() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        dashboard.push_sensor_update(shared::SensorReport {
            node_id: "node-1".into(),
            device_id: "sensor1".into(),
            temperature: Some(21.4),
            humidity: None,
            battery: None,
            occupancy: None,
            contact: None,
            illuminance: None,
            online: true,
        });
        let mut node_id = Some("zigbee-node".to_string());

        process_message(
            MeshMessage::ZigbeeJoin(shared::ZigbeeJoinEvent {
                node_id: "zigbee-node".into(),
                event: "device_announce".into(),
                device_id: "sensor1".into(),
                model: None,
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        let snap = dashboard.get_sensor_snapshot();
        let sensor = snap.iter().find(|s| s.device_id == "sensor1").unwrap();
        assert!(sensor.online, "only device_leave should flip offline");
    }

    // ── device_interview_successful → auto-naming ───────────────────────────

    #[tokio::test]
    async fn interview_success_auto_names_an_unnamed_device_from_its_model() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        let mut node_id = Some("zigbee-node".to_string());

        process_message(
            MeshMessage::ZigbeeJoin(shared::ZigbeeJoinEvent {
                node_id: "zigbee-node".into(),
                event: "device_interview_successful".into(),
                device_id: "0x001788010fa6772b".into(),
                model: Some("929003666501".into()),
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        let names = registry.lock().unwrap().get_all_device_names();
        assert_eq!(
            names.get("0x001788010fa6772b").map(String::as_str),
            Some("Hue GU10 Spot CCT/COL 1")
        );
    }

    #[tokio::test]
    async fn interview_success_numbers_devices_in_the_same_product_line() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        registry
            .lock()
            .unwrap()
            .set_device_name("0xalready-named", "Hue GU10 Spot CCT/COL 1");
        let mut node_id = Some("zigbee-node".to_string());

        process_message(
            MeshMessage::ZigbeeJoin(shared::ZigbeeJoinEvent {
                node_id: "zigbee-node".into(),
                event: "device_interview_successful".into(),
                device_id: "0xnew-spot".into(),
                model: Some("929003666501".into()),
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        let names = registry.lock().unwrap().get_all_device_names();
        assert_eq!(
            names.get("0xnew-spot").map(String::as_str),
            Some("Hue GU10 Spot CCT/COL 2")
        );
    }

    #[tokio::test]
    async fn interview_success_never_overwrites_an_existing_name() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        registry
            .lock()
            .unwrap()
            .set_device_name("0x001788010fa6772b", "Kitchen Spot");
        let mut node_id = Some("zigbee-node".to_string());

        process_message(
            MeshMessage::ZigbeeJoin(shared::ZigbeeJoinEvent {
                node_id: "zigbee-node".into(),
                event: "device_interview_successful".into(),
                device_id: "0x001788010fa6772b".into(),
                model: Some("929003666501".into()),
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        let names = registry.lock().unwrap().get_all_device_names();
        assert_eq!(
            names.get("0x001788010fa6772b").map(String::as_str),
            Some("Kitchen Spot"),
            "a device with an existing name (custom or previously auto-assigned) must not be renamed"
        );
    }

    #[tokio::test]
    async fn interview_success_with_unknown_model_does_not_assign_a_name() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        let mut node_id = Some("zigbee-node".to_string());

        process_message(
            MeshMessage::ZigbeeJoin(shared::ZigbeeJoinEvent {
                node_id: "zigbee-node".into(),
                event: "device_interview_successful".into(),
                device_id: "0xunknown-model".into(),
                model: Some("TOTALLY-UNKNOWN-MODEL".into()),
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        let names = registry.lock().unwrap().get_all_device_names();
        assert!(!names.contains_key("0xunknown-model"));
    }

    // ── SwitchAction → switch-binding dispatch ──────────────────────────────

    #[tokio::test]
    async fn switch_action_without_binding_sends_no_light_command() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        let (node_tx, mut node_rx) = mpsc::channel(4);
        dashboard
            .connections
            .lock()
            .unwrap()
            .insert("node-1".into(), node_tx);
        let mut node_id = Some("switch-node".to_string());

        let reply = process_message(
            MeshMessage::SwitchAction(shared::SwitchActionReport {
                node_id: "switch-node".into(),
                device_id: "dial1".into(),
                action: "button_1_press".into(),
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        assert_eq!(reply, None);
        assert!(
            node_rx.try_recv().is_err(),
            "no binding exists — nothing should be dispatched"
        );
    }

    #[tokio::test]
    async fn switch_action_with_room_binding_dispatches_toggle() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        let room_id = {
            let mut reg = registry.lock().unwrap();
            let room = reg.create_room("Larder");
            reg.add_device_to_room(&room.id, "bulb1");
            reg.create_switch_binding("dial1", "button_1_press", "room", &room.id, "toggle", None)
                .unwrap();
            room.id
        };
        dashboard.push_lighting_update(LightStateReport {
            node_id: "node-1".into(),
            device_id: "bulb1".into(),
            on: false,
            brightness: Some(100),
            color_xy: None,
            color_temp: None,
            online: true,
        });
        let (node_tx, mut node_rx) = mpsc::channel(4);
        dashboard
            .connections
            .lock()
            .unwrap()
            .insert("node-1".into(), node_tx);
        let mut node_id = Some("switch-node".to_string());

        let reply = process_message(
            MeshMessage::SwitchAction(shared::SwitchActionReport {
                node_id: "switch-node".into(),
                device_id: "dial1".into(),
                action: "button_1_press".into(),
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        assert_eq!(reply, None);
        let _ = room_id;
        match node_rx.try_recv().unwrap() {
            MeshMessage::LightCommand(cmd) => {
                assert_eq!(cmd.command, shared::LightAction::Toggle);
                assert_eq!(cmd.target, shared::LightTarget::Device("bulb1".into()));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn switch_action_brightness_step_uses_live_brightness_and_clamps() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        {
            let mut reg = registry.lock().unwrap();
            let room = reg.create_room("Larder");
            reg.add_device_to_room(&room.id, "bulb1");
            reg.create_switch_binding(
                "dial1",
                "brightness_step_up",
                "room",
                &room.id,
                "brightness_step",
                Some(50),
            )
            .unwrap();
        }
        // 220 + 50 = 270, clamped to 254 — verifies both the live-brightness
        // read and the upper clamp in the same case.
        dashboard.push_lighting_update(LightStateReport {
            node_id: "node-1".into(),
            device_id: "bulb1".into(),
            on: true,
            brightness: Some(220),
            color_xy: None,
            color_temp: None,
            online: true,
        });
        let (node_tx, mut node_rx) = mpsc::channel(4);
        dashboard
            .connections
            .lock()
            .unwrap()
            .insert("node-1".into(), node_tx);
        let mut node_id = Some("switch-node".to_string());

        process_message(
            MeshMessage::SwitchAction(shared::SwitchActionReport {
                node_id: "switch-node".into(),
                device_id: "dial1".into(),
                action: "brightness_step_up".into(),
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        match node_rx.try_recv().unwrap() {
            MeshMessage::LightCommand(cmd) => {
                assert_eq!(cmd.command, shared::LightAction::Brightness(254));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn switch_action_with_group_binding_targets_only_group_members() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        {
            let mut reg = registry.lock().unwrap();
            let room = reg.create_room("Kitchen");
            reg.add_device_to_room(&room.id, "counter1");
            reg.add_device_to_room(&room.id, "ceiling1");
            let group = reg.create_room_group(&room.id, "Counter");
            reg.set_device_group("counter1", Some(&group.id));
            reg.create_switch_binding("dial1", "button_1_press", "group", &group.id, "on", None)
                .unwrap();
        }
        for (device_id, node_id_val) in [("counter1", "node-1"), ("ceiling1", "node-1")] {
            dashboard.push_lighting_update(LightStateReport {
                node_id: node_id_val.into(),
                device_id: device_id.into(),
                on: false,
                brightness: Some(100),
                color_xy: None,
                color_temp: None,
                online: true,
            });
        }
        let (node_tx, mut node_rx) = mpsc::channel(4);
        dashboard
            .connections
            .lock()
            .unwrap()
            .insert("node-1".into(), node_tx);
        let mut node_id = Some("switch-node".to_string());

        process_message(
            MeshMessage::SwitchAction(shared::SwitchActionReport {
                node_id: "switch-node".into(),
                device_id: "dial1".into(),
                action: "button_1_press".into(),
            }),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        match node_rx.try_recv().unwrap() {
            MeshMessage::LightCommand(cmd) => {
                assert_eq!(cmd.target, shared::LightTarget::Device("counter1".into()));
            }
            other => panic!("unexpected message: {other:?}"),
        }
        // Only the group member should get a command — nothing else queued.
        assert!(node_rx.try_recv().is_err());
    }

    // A light report carrying a stale/'unknown' node_id must be routed by the
    // authenticated connection's id, not the payload — otherwise commands 503.
    #[tokio::test]
    async fn light_state_routes_on_connection_id_not_payload() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        let mut node_id = Some("pi1-real".to_string());

        let report = LightStateReport {
            node_id: "unknown".into(), // stale payload — must be ignored
            device_id: "bulb1".into(),
            on: true,
            brightness: Some(200),
            color_xy: None,
            color_temp: Some(370),
            online: true,
        };

        let reply = process_message(
            MeshMessage::LightState(report),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        assert!(reply.is_none(), "light state report produces no reply");
        assert_eq!(
            dashboard.get_node_for_device("bulb1"),
            Some("pi1-real".to_string()),
            "command routing must use the connection node_id, not the report payload",
        );
    }

    // A sensor report flows into both the dashboard snapshot and the registry,
    // keyed by the authenticated connection's id (same invariant as LightState).
    #[tokio::test]
    async fn sensor_state_persists_and_surfaces_on_connection_id() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        let mut node_id = Some("pi1-real".to_string());

        let report = shared::SensorReport {
            node_id: "unknown".into(), // stale payload — must be ignored
            device_id: "office_climate".into(),
            temperature: Some(21.4),
            humidity: Some(47.0),
            battery: Some(98),
            occupancy: None,
            contact: None,
            illuminance: None,
            online: true,
        };

        let reply = process_message(
            MeshMessage::SensorState(report),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        assert!(reply.is_none(), "sensor state report produces no reply");
        let snap = dashboard.get_sensor_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].node_id, "pi1-real", "connection id must win");
        assert_eq!(snap[0].temperature, Some(21.4));
        let persisted = registry.lock().unwrap().load_sensor_states();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].node_id, "pi1-real");
    }

    // Same invariant for the device-list report: a stale node_id must not create
    // a phantom light_devices row / mis-key group routing.
    #[tokio::test]
    async fn light_device_list_keys_on_connection_id_not_payload() {
        let (registry, connections, pi, pin, ps, tx, tokens, dashboard) = test_deps();
        let mut node_id = Some("pi1-real".to_string());

        let report = shared::DeviceListReport {
            node_id: "unknown".into(), // stale payload — must be ignored
            devices: vec![shared::DeviceEntry {
                id: "bulb1".into(),
                device_type: shared::DeviceType::Light,
            }],
            groups: vec!["all".into()],
        };

        let reply = process_message(
            MeshMessage::DeviceList(report),
            &registry,
            &connections,
            &pi,
            &pin,
            &ps,
            &tx,
            &mut node_id,
            &tokens,
            Some(&dashboard),
            "no auth",
        )
        .await;

        assert!(reply.is_none());
        assert_eq!(
            dashboard.get_node_for_group("all"),
            Some("pi1-real".to_string()),
            "group routing must use the connection node_id, not the report payload",
        );
    }

    async fn send_message(addr: &str, msg: &MeshMessage) -> Option<MeshMessage> {
        let mut stream = TcpStream::connect(addr).await.unwrap();

        let data = serde_json::to_vec(msg).unwrap();
        let len = (data.len() as u32).to_le_bytes();

        stream.write_all(&len).await.unwrap();
        stream.write_all(&data).await.unwrap();

        // Some messages generate no reply — use a short timeout instead of blocking forever.
        let read_reply = async {
            let buf = read_bounded_frame(&mut stream).await.ok()?;
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
                disk_free_gb: None,
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
                disk_free_gb: None,
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
                disk_free_gb: None,
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
                disk_free_gb: None,
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
            let buf = read_bounded_frame(&mut stream).await.ok()?;
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
                disk_free_gb: None,
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
                disk_free_gb: None,
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
                disk_free_gb: None,
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
                disk_free_gb: None,
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
                disk_free_gb: None,
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
                disk_free_gb: None,
            }),
        )
        .await;
        assert!(ack2.is_none(), "heartbeat should produce no reply");
        assert!(registry.lock().unwrap().get("new-token-node").is_some());
    }
}
