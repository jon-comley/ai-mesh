use agent::agent::Agent;
use agent::config::AgentConfig;
use agent::dispatch::{build_capabilities, dispatch};
use agent::identity::detect_identity;
use agent::tls::make_connector;
use rustls::crypto::ring;
use rustls::pki_types::ServerName;
use shared::MeshMessage;
use shared::frame::{FrameVerifyError, SignedFrame, derive_hmac_key};
use shared::hardware::NodeRole;
use socket2::{SockRef, TcpKeepalive};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[tokio::main]
async fn main() {
    ring::default_provider()
        .install_default()
        .expect("failed to install ring crypto provider");
    tracing_subscriber::fmt().init();

    let role = read_role_from_env();
    let addr = resolve_coordinator_addr().await;

    // Resolve node_id once — persisted to ~/.ai-mesh/node-id so it's stable
    // across reconnects. Capabilities are built once and survive reconnects via Arc.
    let node_id = detect_identity(role.clone())
        .map(|i| i.id)
        .unwrap_or_else(|_| {
            warn!("identity detection failed; using 'unknown' as node_id");
            "unknown".into()
        });

    let caps = build_capabilities(&node_id);
    if caps.is_empty() {
        warn!("no capabilities loaded — agent will not handle inference or lighting commands");
    } else {
        info!(
            "capabilities: {}",
            caps.iter().map(|c| c.name()).collect::<Vec<_>>().join(", ")
        );
    }

    info!("Agent starting, coordinator: {}", addr);

    loop {
        info!("Connecting to coordinator at {}", addr);

        let connector = make_connector();
        let server_name = ServerName::try_from("ai-mesh-coordinator")
            .expect("invalid server name")
            .to_owned();

        let stream = loop {
            match TcpStream::connect(&addr).await {
                Ok(s) => break s,
                Err(e) => {
                    warn!("Failed to connect to {}: {}. Retrying in 5s...", addr, e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        };

        // Enable TCP keepalive so NIC power-management or network idle timeouts
        // don't drop the connection during a long inference.
        {
            let sock = SockRef::from(&stream);
            let ka = TcpKeepalive::new()
                .with_time(std::time::Duration::from_secs(10))
                .with_interval(std::time::Duration::from_secs(5));
            if let Err(e) = sock.set_tcp_keepalive(&ka) {
                warn!("Failed to set TCP keepalive: {}", e);
            }
        }

        let tls_stream = match connector.connect(server_name.clone(), stream).await {
            Ok(s) => s,
            Err(e) => {
                warn!("TLS handshake failed: {}. Retrying in 5s...", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        info!("Connected to coordinator (TLS)");

        let (mut reader, mut writer) = tokio::io::split(tls_stream);

        // Send AuthToken (unsigned) as the first frame if configured.
        // Derive the per-connection HMAC key from the same token for all subsequent frames.
        let hmac_key: Option<[u8; 32]> = if let Ok(token) = std::env::var("MESH_AUTH_TOKEN") {
            let token = token.trim().to_string();
            if !token.is_empty() {
                let data = serde_json::to_vec(&MeshMessage::AuthToken(token.clone())).unwrap();
                let len = (data.len() as u32).to_le_bytes();
                if writer.write_all(&len).await.is_err() || writer.write_all(&data).await.is_err() {
                    warn!("Failed to send AuthToken. Retrying in 5s...");
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    continue;
                }
                Some(derive_hmac_key(&token))
            } else {
                None
            }
        } else {
            None
        };
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(32);

        // Heartbeat loop.
        let config = AgentConfig {
            role: role.clone(),
            heartbeat_interval_secs: 5,
        };
        let agent = Agent::new_with_config(config, tx.clone());
        let interval_handle = agent.interval_handle();
        tokio::spawn(async move {
            if let Err(e) = agent.run().await {
                warn!("agent run loop exited: {}", e);
            }
        });

        // Spawn start() for each capability. LLM's start is a no-op; lighting's
        // start will run the MQTT event loop. Both get the current connection's tx.
        for cap in &caps {
            let cap = Arc::clone(cap);
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Err(e) = cap.start(tx).await {
                    warn!("capability '{}' failed to start: {}", cap.name(), e);
                }
            });
        }

        // Reader task — routes inbound coordinator commands to capabilities.
        // SetHeartbeatInterval is handled here; everything else goes to dispatch().
        let tx_in = tx.clone();
        let caps_reader = caps.clone();
        let reader_key = hmac_key;
        let reader_interval = interval_handle.clone();
        tokio::spawn(async move {
            use std::sync::atomic::Ordering;
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
                let msg: MeshMessage = if let Some(key) = &reader_key {
                    match serde_json::from_slice::<SignedFrame>(&buf) {
                        Ok(frame) => match frame.verify(key) {
                            Ok(payload) => match serde_json::from_slice(payload) {
                                Ok(m) => m,
                                Err(_) => continue,
                            },
                            Err(e) => {
                                if matches!(e, FrameVerifyError::Stale { .. }) {
                                    warn!("dropping inbound frame: {} — check NTP sync", e);
                                } else {
                                    warn!("dropping inbound frame: {}", e);
                                }
                                break;
                            }
                        },
                        Err(_) => continue,
                    }
                } else {
                    match serde_json::from_slice(&buf) {
                        Ok(m) => m,
                        Err(_) => continue,
                    }
                };

                match msg {
                    MeshMessage::SetHeartbeatInterval { secs } => {
                        info!(secs, "heartbeat interval updated");
                        reader_interval.store(secs, Ordering::Relaxed);
                    }
                    other => dispatch(other, &caps_reader, tx_in.clone()).await,
                }
            }
        });

        // Writer loop — drains the outbound mpsc channel onto the TCP stream.
        // When HMAC is active, every outgoing message is wrapped in a SignedFrame.
        loop {
            if let Some(msg) = rx.recv().await {
                let data = if let Some(key) = &hmac_key {
                    let payload = serde_json::to_vec(&msg).unwrap();
                    let frame = SignedFrame::sign(key, payload);
                    serde_json::to_vec(&frame).unwrap()
                } else {
                    serde_json::to_vec(&msg).unwrap()
                };
                let len = (data.len() as u32).to_le_bytes();

                if let Err(e) = writer.write_all(&len).await {
                    warn!("Write error: {}", e);
                    break;
                }
                if let Err(e) = writer.write_all(&data).await {
                    warn!("Write error: {}", e);
                    break;
                }
            }
        }

        warn!("Disconnected from coordinator. Reconnecting in 5s...");
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}

fn read_role_from_env() -> NodeRole {
    match std::env::var("AGENT_ROLE").as_deref() {
        Ok("controller") => NodeRole::Controller,
        _ => NodeRole::Compute,
    }
}

async fn resolve_coordinator_addr() -> String {
    let port = std::env::var("COORDINATOR_PORT").unwrap_or_else(|_| "9000".into());
    let port = port.trim().to_string();

    if let Ok(ip) = std::env::var("COORDINATOR_IP") {
        return format!("{}:{}", ip.trim(), port);
    }

    if let Some(addr) =
        agent::discovery::discover_coordinator(std::time::Duration::from_secs(5)).await
    {
        return addr;
    }

    warn!(
        "mDNS: no coordinator found; falling back to 127.0.0.1:{}",
        port
    );
    format!("127.0.0.1:{}", port)
}
