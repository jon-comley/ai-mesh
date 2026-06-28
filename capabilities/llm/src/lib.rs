mod llama;

use async_trait::async_trait;
use capability_core::Capability;
use shared::{InferenceResult, MeshMessage, ModelLifecycleState, ModelStatusReport, WIRE_VERSION};
use std::sync::OnceLock;
use tokio::sync::{Mutex, Semaphore, mpsc::Sender};
use tracing::{info, warn};

// Process-wide: only one llama-server inference at a time.
// Prevents concurrent requests (e.g. after reconnect) from doubling GPU memory usage.
static INFER_SEM: OnceLock<Semaphore> = OnceLock::new();
fn infer_sem() -> &'static Semaphore {
    INFER_SEM.get_or_init(|| Semaphore::new(1))
}

// Single-flight model loads. Serializes every ModelLoad so only one pull_model
// runs at a time, and holds the name of the model currently loaded so a
// duplicate ModelLoad (double-click, auto-placement retry, reconnect re-send)
// is a no-op instead of racing a second download into the same temp file
// (the "rename failed … No such file or directory" error) or stomping the
// running llama-server. The inner Option is None while nothing is loaded or a
// load is in progress; it is set to Some(model) only once a load reports Ready.
static LOAD_GUARD: OnceLock<Mutex<Option<String>>> = OnceLock::new();
fn load_guard() -> &'static Mutex<Option<String>> {
    LOAD_GUARD.get_or_init(|| Mutex::new(None))
}

pub struct LlmCapability {
    node_id: String,
}

impl LlmCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
        }
    }
}

#[async_trait]
impl Capability for LlmCapability {
    fn name(&self) -> &'static str {
        "llm"
    }

    fn handles(&self, msg: &MeshMessage) -> bool {
        matches!(
            msg,
            MeshMessage::ModelLoad(_)
                | MeshMessage::ModelUnload(_)
                | MeshMessage::RequestModelInference(_)
        )
    }

    async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> {
        Ok(()) // llama-server is launched on ModelLoad; no background loop needed
    }

    async fn handle(&self, msg: MeshMessage, tx: Sender<MeshMessage>) {
        match msg {
            MeshMessage::ModelLoad(req) => {
                info!(
                    "Received command to load model {} ({} MB)",
                    req.model_name, req.model_size_mb
                );

                let _ = tx
                    .send(MeshMessage::ModelStatus(ModelStatusReport {
                        node_id: self.node_id.clone(),
                        model_name: req.model_name.clone(),
                        size_mb: req.model_size_mb,
                        state: ModelLifecycleState::Loading,
                        wire_version: WIRE_VERSION,
                    }))
                    .await;

                let tx2 = tx.clone();
                let nid = self.node_id.clone();
                let mname = req.model_name.clone();
                let size = req.model_size_mb;
                tokio::spawn(async move {
                    // Hold the load guard for the whole pull so concurrent
                    // ModelLoad messages run one at a time instead of racing the
                    // same download temp file or stomping each other's server.
                    let mut loaded = match load_guard().try_lock() {
                        Ok(g) => g,
                        Err(_) => {
                            info!(model = %mname, "another model load in progress — queuing behind it");
                            load_guard().lock().await
                        }
                    };
                    if loaded.as_deref() == Some(mname.as_str()) {
                        info!(model = %mname, "model already loaded — skipping duplicate load");
                        let _ = tx2
                            .send(MeshMessage::ModelStatus(ModelStatusReport {
                                node_id: nid,
                                model_name: mname,
                                size_mb: size,
                                state: ModelLifecycleState::Ready,
                                wire_version: WIRE_VERSION,
                            }))
                            .await;
                        return;
                    }
                    // A switch to a different model invalidates the current one;
                    // clear it up front so a mid-load crash doesn't leave a stale
                    // "loaded" marker that would suppress a later reload.
                    *loaded = None;
                    let state = match llama::pull_model(&mname, size).await {
                        Ok(()) => {
                            info!(model = %mname, "llama pull complete");
                            *loaded = Some(mname.clone());
                            Some(ModelLifecycleState::Ready)
                        }
                        Err(e) if e == "unloaded" => {
                            info!(model = %mname, "load cancelled by unload — suppressing Failed status");
                            None
                        }
                        Err(e) => {
                            warn!(model = %mname, error = %e, "llama pull failed");
                            Some(ModelLifecycleState::Failed { reason: e })
                        }
                    };
                    if let Some(state) = state {
                        let _ = tx2
                            .send(MeshMessage::ModelStatus(ModelStatusReport {
                                node_id: nid,
                                model_name: mname,
                                size_mb: size,
                                state,
                                wire_version: WIRE_VERSION,
                            }))
                            .await;
                    }
                });
            }

            MeshMessage::RequestModelInference(req) => {
                info!(
                    request_id = %req.request_id,
                    model = %req.model_name,
                    "received inference request"
                );
                let tx2 = tx.clone();
                let nid = self.node_id.clone();
                tokio::spawn(async move {
                    let _permit = infer_sem().acquire().await.unwrap();
                    // If the connection drops while waiting for the GPU, cancel
                    // the reqwest future so llama-server frees memory immediately.
                    tokio::select! {
                        _ = tx2.closed() => {
                            warn!(request_id = %req.request_id,
                                  "inference cancelled: connection dropped");
                        }
                        res = llama::generate(&req.model_name, req.system_prompt.as_deref(), &req.prompt, req.max_tokens, req.temperature.unwrap_or(0.8)) => {
                            let result = match res {
                                Ok((output, tokens, duration_ms, prompt_eval_ms)) => InferenceResult {
                                    request_id: req.request_id,
                                    node_id: nid,
                                    model_name: req.model_name,
                                    output,
                                    tokens_generated: tokens,
                                    duration_ms,
                                    prompt_eval_ms,
                                    error: None,
                                    wire_version: WIRE_VERSION,
                                },
                                Err(e) => {
                                    warn!(error = %e, "llama generate failed");
                                    InferenceResult {
                                        request_id: req.request_id,
                                        node_id: nid,
                                        model_name: req.model_name,
                                        output: String::new(),
                                        tokens_generated: 0,
                                        duration_ms: 0,
                                        prompt_eval_ms: 0,
                                        error: Some(e),
                                        wire_version: WIRE_VERSION,
                                    }
                                }
                            };
                            let _ = tx2.send(MeshMessage::ModelInferenceResult(result)).await;
                        }
                    }
                });
            }

            MeshMessage::ModelUnload(req) => {
                info!("Received command to unload model {}", req.model_name);
                // Drop the single-flight marker so a later load of the same
                // model re-pulls instead of being deduped away.
                *load_guard().lock().await = None;
                match llama::unload_model().await {
                    Ok(()) => info!(model = %req.model_name, "model unloaded"),
                    Err(e) => {
                        warn!(model = %req.model_name, error = %e, "failed to cleanly unload model")
                    }
                }
                let _ = tx
                    .send(MeshMessage::ModelStatus(ModelStatusReport {
                        node_id: self.node_id.clone(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{InferenceRequest, ModelLoadRequest, ModelUnloadRequest, NodeIdentity, NodeRole};

    fn make_cap() -> LlmCapability {
        LlmCapability::new("node-1")
    }

    #[test]
    fn handles_model_load() {
        let msg = MeshMessage::ModelLoad(ModelLoadRequest {
            request_id: "r1".into(),
            node_id: Some("node-1".into()),
            model_name: "qwen2.5:7b".into(),
            model_size_mb: 4096,
            wire_version: WIRE_VERSION,
        });
        assert!(make_cap().handles(&msg));
    }

    #[test]
    fn handles_model_unload() {
        let msg = MeshMessage::ModelUnload(ModelUnloadRequest {
            request_id: "r2".into(),
            node_id: "node-1".into(),
            model_name: "qwen2.5:7b".into(),
            wire_version: WIRE_VERSION,
        });
        assert!(make_cap().handles(&msg));
    }

    #[test]
    fn handles_inference_request() {
        let msg = MeshMessage::RequestModelInference(InferenceRequest {
            request_id: "r3".into(),
            node_id: Some("node-1".into()),
            model_name: "qwen2.5:7b".into(),
            system_prompt: None,
            prompt: "hello".into(),
            max_tokens: 64,
            temperature: None,
            wire_version: WIRE_VERSION,
        });
        assert!(make_cap().handles(&msg));
    }

    #[test]
    fn does_not_handle_heartbeat() {
        use shared::HeartbeatPayload;
        let msg = MeshMessage::Heartbeat(HeartbeatPayload {
            identity: NodeIdentity {
                id: "node-1".into(),
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
        });
        assert!(!make_cap().handles(&msg));
    }

    #[test]
    fn does_not_handle_light_command() {
        let msg = MeshMessage::LightCommand(shared::LightCommandRequest {
            request_id: "lc-1".into(),
            target: shared::LightTarget::Group("all".into()),
            command: shared::LightAction::On,
        });
        assert!(!make_cap().handles(&msg));
    }

    #[test]
    fn tools_returns_empty() {
        assert!(make_cap().tools().is_empty());
    }

    #[tokio::test]
    async fn start_returns_ok() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        assert!(make_cap().start(tx).await.is_ok());
    }
}
