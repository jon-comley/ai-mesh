use agent::agent::Agent;
use agent::config::AgentConfig;
use agent::ollama;
use shared::hardware::NodeRole;
use shared::{InferenceResult, MeshMessage, ModelLifecycleState, ModelStatusReport, WIRE_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let role = read_role_from_env();
    let addr = read_coordinator_addr();

    info!("Agent starting");

    loop {
        info!("Connecting to coordinator at {}", addr);

        let stream = loop {
            match TcpStream::connect(&addr).await {
                Ok(s) => break s,
                Err(e) => {
                    warn!("Failed to connect to {}: {}. Retrying in 5s...", addr, e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        };

        info!("Connected to coordinator");

        let (mut reader, mut writer) = stream.into_split();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(32);

        let config = AgentConfig {
            role: role.clone(),
            heartbeat_interval_secs: 5,
        };
        let agent = Agent::new_with_config(config, tx.clone());
        let node_id = agent.node_id().to_string();
        tokio::spawn(async move {
            if let Err(e) = agent.run().await {
                warn!("agent run loop exited: {}", e);
            }
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

                match msg {
                    MeshMessage::ModelLoad(req) => {
                        info!(
                            "Received command to load model {} ({} MB)",
                            req.model_name, req.model_size_mb
                        );

                        let _ = tx_in
                            .send(MeshMessage::ModelStatus(ModelStatusReport {
                                node_id: node_id.clone(),
                                model_name: req.model_name.clone(),
                                size_mb: req.model_size_mb,
                                state: ModelLifecycleState::Loading,
                                wire_version: WIRE_VERSION,
                            }))
                            .await;

                        let tx2 = tx_in.clone();
                        let nid = node_id.clone();
                        let mname = req.model_name.clone();
                        let size = req.model_size_mb;
                        tokio::spawn(async move {
                            let state = match ollama::pull_model(&mname).await {
                                Ok(()) => {
                                    info!(model = %mname, "ollama pull complete");
                                    ModelLifecycleState::Ready
                                }
                                Err(e) => {
                                    warn!(model = %mname, error = %e, "ollama pull failed");
                                    ModelLifecycleState::Failed { reason: e }
                                }
                            };
                            let _ = tx2
                                .send(MeshMessage::ModelStatus(ModelStatusReport {
                                    node_id: nid,
                                    model_name: mname,
                                    size_mb: size,
                                    state,
                                    wire_version: WIRE_VERSION,
                                }))
                                .await;
                        });
                    }
                    MeshMessage::RequestModelInference(req) => {
                        info!(
                            request_id = %req.request_id,
                            model = %req.model_name,
                            "received inference request"
                        );
                        // skip inference if the connection already dropped
                        if tx_in.is_closed() {
                            warn!(request_id = %req.request_id, "inference aborted: channel closed");
                            continue;
                        }
                        let result = match ollama::generate(&req.model_name, &req.prompt).await {
                            Ok((output, tokens, duration_ms)) => InferenceResult {
                                request_id: req.request_id,
                                node_id: node_id.clone(),
                                model_name: req.model_name,
                                output,
                                tokens_generated: tokens,
                                duration_ms,
                                error: None,
                                wire_version: WIRE_VERSION,
                            },
                            Err(e) => {
                                warn!(error = %e, "ollama generate failed");
                                InferenceResult {
                                    request_id: req.request_id,
                                    node_id: node_id.clone(),
                                    model_name: req.model_name,
                                    output: String::new(),
                                    tokens_generated: 0,
                                    duration_ms: 0,
                                    error: Some(e),
                                    wire_version: WIRE_VERSION,
                                }
                            }
                        };
                        let _ = tx_in.send(MeshMessage::ModelInferenceResult(result)).await;
                    }
                    MeshMessage::ModelUnload(req) => {
                        info!("Received command to unload model {}", req.model_name);
                        let _ = tx_in
                            .send(MeshMessage::ModelStatus(ModelStatusReport {
                                node_id: node_id.clone(),
                                model_name: req.model_name,
                                size_mb: 0,
                                state: ModelLifecycleState::Unloaded,
                                wire_version: WIRE_VERSION,
                            }))
                            .await;
                    }
                    _ => {}
                }
            }
        });

        // Writer loop — drains the outbound mpsc channel onto the TCP stream.
        loop {
            if let Some(msg) = rx.recv().await {
                let data = serde_json::to_vec(&msg).unwrap();
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

fn read_coordinator_addr() -> String {
    let ip = std::env::var("COORDINATOR_IP").unwrap_or_else(|_| "127.0.0.1".into());
    let port = std::env::var("COORDINATOR_PORT").unwrap_or_else(|_| "9000".into());
    format!("{}:{}", ip.trim(), port.trim())
}
