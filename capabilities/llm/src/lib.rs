mod llama;

use async_trait::async_trait;
use capability_core::Capability;
use shared::{
    InferenceChunk, InferenceRequest, InferenceResult, MeshMessage, ModelLifecycleState,
    ModelStatusReport, WIRE_VERSION,
};
use std::sync::OnceLock;
use tokio::sync::{Mutex, Semaphore, mpsc, mpsc::Sender};
use tracing::{info, warn};

/// Run one inference (streamed or not) and build the terminal result.
/// In streaming mode the deltas are forwarded to the coordinator as
/// `ModelInferenceChunk` messages via `tx`; the forwarder is drained before
/// returning so no chunk can trail the terminal `ModelInferenceResult` on the
/// connection's FIFO writer channel.
async fn run_inference(
    req: InferenceRequest,
    node_id: String,
    tx: Sender<MeshMessage>,
) -> InferenceResult {
    let temperature = req.temperature.unwrap_or(0.8);
    let res = if req.stream {
        let (dtx, mut drx) = mpsc::channel::<String>(32);
        let fwd_tx = tx;
        let fwd_nid = node_id.clone();
        let fwd_rid = req.request_id.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(delta) = drx.recv().await {
                let chunk = InferenceChunk {
                    request_id: fwd_rid.clone(),
                    node_id: fwd_nid.clone(),
                    delta,
                    wire_version: WIRE_VERSION,
                };
                if fwd_tx
                    .send(MeshMessage::ModelInferenceChunk(chunk))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let res = llama::generate_stream(
            &req.model_name,
            &req.messages,
            req.max_tokens,
            temperature,
            dtx,
        )
        .await;
        let _ = forwarder.await;
        res
    } else {
        llama::generate(&req.model_name, &req.messages, req.max_tokens, temperature).await
    };
    match res {
        Ok(outcome) => InferenceResult {
            request_id: req.request_id,
            node_id,
            model_name: req.model_name,
            output: outcome.output,
            tokens_generated: outcome.tokens_generated,
            prompt_tokens: outcome.prompt_tokens,
            duration_ms: outcome.duration_ms,
            prompt_eval_ms: outcome.prompt_eval_ms,
            error: None,
            wire_version: WIRE_VERSION,
        },
        Err(e) => {
            warn!(error = %e, "llama generate failed");
            InferenceResult {
                request_id: req.request_id,
                node_id,
                model_name: req.model_name,
                output: String::new(),
                tokens_generated: 0,
                prompt_tokens: 0,
                duration_ms: 0,
                prompt_eval_ms: 0,
                error: Some(e),
                wire_version: WIRE_VERSION,
            }
        }
    }
}

// Process-wide: only one llama-server inference at a time.
// Prevents concurrent requests (e.g. after reconnect) from doubling GPU memory usage.
static INFER_SEM: OnceLock<Semaphore> = OnceLock::new();
fn infer_sem() -> &'static Semaphore {
    INFER_SEM.get_or_init(|| Semaphore::new(1))
}

// In-flight inference tasks by request_id, so a `CancelInference` from the
// coordinator can abort one (dropping the task releases the semaphore permit
// and drops the llama-server connection, which cancels generation).
static INFLIGHT: OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, tokio::task::AbortHandle>>,
> = OnceLock::new();
fn inflight()
-> &'static std::sync::Mutex<std::collections::HashMap<String, tokio::task::AbortHandle>> {
    INFLIGHT.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

// Single-flight model loads. Serializes every ModelLoad so only one pull_model
// runs at a time, and holds the (name, size_mb) of the model currently loaded so a
// duplicate ModelLoad (double-click, auto-placement retry, reconnect re-send)
// is a no-op instead of racing a second download into the same temp file
// (the "rename failed … No such file or directory" error) or stomping the
// running llama-server. The inner Option is None while nothing is loaded or a
// load is in progress; it is set to Some((model, size)) only once a load reports
// Ready. The size is kept so `start()` can re-report the loaded model on reconnect.
static LOAD_GUARD: OnceLock<Mutex<Option<(String, u64)>>> = OnceLock::new();
fn load_guard() -> &'static Mutex<Option<(String, u64)>> {
    LOAD_GUARD.get_or_init(|| Mutex::new(None))
}

/// The `Unloaded` status report for whatever was previously loaded, if
/// anything — sent before starting a new model so the coordinator's registry
/// (and the scheduler's "is this model Ready on this node" checks) don't keep
/// believing the old one is still there after it's been implicitly killed by
/// a switch. `None` when nothing was loaded (first load on this node).
fn unload_status_for(node_id: &str, previous: &Option<(String, u64)>) -> Option<ModelStatusReport> {
    previous.as_ref().map(|(name, size)| ModelStatusReport {
        node_id: node_id.to_string(),
        model_name: name.clone(),
        size_mb: *size,
        state: ModelLifecycleState::Unloaded,
        wire_version: WIRE_VERSION,
    })
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
                | MeshMessage::CancelInference { .. }
        )
    }

    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String> {
        // Re-report a currently-loaded model on every (re)connect. The coordinator
        // clears a node's model state whenever its connection drops (e.g. a
        // read-timeout close while the node is briefly silent), so without this a
        // model still resident in llama-server would vanish from the dashboard
        // until the next explicit load. The guard is process-static: if it holds a
        // model, this process loaded it and llama-server is still serving it (an
        // agent restart resets the guard and kills llama-server together), so
        // re-reporting Ready is safe. On the first connect the guard is None.
        if let Some((model_name, size_mb)) = load_guard().lock().await.clone() {
            info!(model = %model_name, "re-reporting loaded model to coordinator on (re)connect");
            let _ = tx
                .send(MeshMessage::ModelStatus(ModelStatusReport {
                    node_id: self.node_id.clone(),
                    model_name,
                    size_mb,
                    state: ModelLifecycleState::Ready,
                    wire_version: WIRE_VERSION,
                }))
                .await;
        }
        Ok(())
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
                    if loaded.as_ref().map(|(n, _)| n.as_str()) == Some(mname.as_str()) {
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
                    let previous = loaded.clone();
                    *loaded = None;
                    // pull_model()'s kill_existing() is about to SIGKILL whatever
                    // was running, but that's silent to the coordinator — its
                    // registry only ever upserts the model named in a status
                    // report, so without this the OLD model's entry stays
                    // "Ready" forever after an implicit switch (only an explicit
                    // ModelUnload message previously sent one). Tell the
                    // coordinator the old one is gone before starting the new.
                    if let Some(report) = unload_status_for(&nid, &previous) {
                        let _ = tx2.send(MeshMessage::ModelStatus(report)).await;
                    }
                    let state = match llama::pull_model(&mname, size).await {
                        Ok(()) => {
                            info!(model = %mname, "llama pull complete");
                            *loaded = Some((mname.clone(), size));
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
                    stream = req.stream,
                    "received inference request"
                );
                let tx2 = tx.clone();
                let nid = self.node_id.clone();
                let rid = req.request_id.clone();
                let task = tokio::spawn(async move {
                    let _permit = infer_sem().acquire().await.unwrap();
                    let request_id = req.request_id.clone();
                    // If the connection drops while waiting for the GPU (or
                    // mid-stream), cancel the reqwest future so llama-server
                    // frees the slot immediately.
                    tokio::select! {
                        _ = tx2.closed() => {
                            warn!(request_id = %request_id,
                                  "inference cancelled: connection dropped");
                        }
                        result = run_inference(req, nid, tx2.clone()) => {
                            let _ = tx2.send(MeshMessage::ModelInferenceResult(result)).await;
                        }
                    }
                    inflight().lock().unwrap().remove(&request_id);
                });
                inflight().lock().unwrap().insert(rid, task.abort_handle());
            }

            MeshMessage::CancelInference { request_id } => {
                let handle = inflight().lock().unwrap().remove(&request_id);
                if let Some(handle) = handle {
                    // Aborting the task drops the llama-server connection
                    // (cancelling generation) and releases the infer permit.
                    handle.abort();
                    info!(request_id = %request_id, "inference cancelled by coordinator");
                }
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
    fn unload_status_for_reports_the_previous_model() {
        let previous = Some(("old_model".to_string(), 1234));
        let report = unload_status_for("node-1", &previous).unwrap();
        assert_eq!(report.node_id, "node-1");
        assert_eq!(report.model_name, "old_model");
        assert_eq!(report.size_mb, 1234);
        assert!(matches!(report.state, ModelLifecycleState::Unloaded));
    }

    #[test]
    fn unload_status_for_none_when_nothing_was_loaded() {
        assert!(unload_status_for("node-1", &None).is_none());
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
            messages: vec![shared::ChatTurn::user("hello")],
            stream: false,
            max_tokens: 64,
            temperature: None,
            wire_version: WIRE_VERSION,
        });
        assert!(make_cap().handles(&msg));
    }

    #[test]
    fn handles_cancel_inference() {
        let msg = MeshMessage::CancelInference {
            request_id: "r4".into(),
        };
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
