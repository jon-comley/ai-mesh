use agent::agent::Agent;
use agent::config::AgentConfig;
use shared::hardware::NodeRole;
use shared::{MeshMessage, ModelLifecycleState, ModelStatusReport, WIRE_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let role = read_role_from_env();
    let addr = read_coordinator_addr();

    info!("Agent starting");
    info!("Connecting to coordinator at {}", addr);

    let stream = match TcpStream::connect(&addr).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to connect to coordinator at {}: {}", addr, e);
            return;
        }
    };

    let (mut reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<MeshMessage>(32);

    let config = AgentConfig {
        role,
        heartbeat_interval_secs: 5,
    };
    let agent = Agent::new_with_config(config, tx.clone());
    // Reuse the node_id the agent stamped onto its Heartbeat so ModelStatus messages
    // match the registry entry.
    let node_id = agent.node_id().to_string();
    tokio::spawn(async move {
        let _ = agent.run().await;
    });

    // Reader task — handles coordinator-initiated commands.
    let tx_in = tx.clone();
    tokio::spawn(async move {
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
                Err(_) => continue,
            };

            if let MeshMessage::ModelLoad(req) = msg {
                info!(
                    "Received command to load model {} ({} MB)",
                    req.model_name, req.model_size_mb
                );

                // Immediately report Loading state.
                let _ = tx_in
                    .send(MeshMessage::ModelStatus(ModelStatusReport {
                        node_id: node_id.clone(),
                        model_name: req.model_name.clone(),
                        size_mb: req.model_size_mb,
                        state: ModelLifecycleState::Loading,
                        wire_version: WIRE_VERSION,
                    }))
                    .await;

                // Background task: simulate load delay then report Ready.
                let tx2 = tx_in.clone();
                let nid = node_id.clone();
                let mname = req.model_name.clone();
                let size = req.model_size_mb;
                tokio::spawn(async move {
                    sleep(Duration::from_secs(2)).await;
                    let _ = tx2
                        .send(MeshMessage::ModelStatus(ModelStatusReport {
                            node_id: nid,
                            model_name: mname,
                            size_mb: size,
                            state: ModelLifecycleState::Ready,
                            wire_version: WIRE_VERSION,
                        }))
                        .await;
                });
            }
        }
    });

    // Writer loop — drains the outbound mpsc channel onto the TCP stream.
    loop {
        if let Some(msg) = rx.recv().await {
            let data = serde_json::to_vec(&msg).unwrap();
            let len = (data.len() as u32).to_le_bytes();

            if let Err(e) = writer.write_all(&len).await {
                eprintln!("Write error: {}", e);
                break;
            }
            if let Err(e) = writer.write_all(&data).await {
                eprintln!("Write error: {}", e);
                break;
            }
        }
    }
}

fn read_role_from_env() -> NodeRole {
    match std::env::var("AGENT_ROLE").as_deref() {
        Ok("controller") => NodeRole::Controller,
        _ => NodeRole::Compute,
    }
}

fn read_coordinator_addr() -> String {
    let ip = std::env::var("COORDINATOR_IP").unwrap_or_else(|_| "127.0.0.1".into());
    let port = std::env::var("COORDINATOR_PORT").unwrap_or_else(|_| "9000".into());
    format!("{}:{}", ip, port)
}
