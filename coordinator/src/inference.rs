//! Local inference dispatch shared by the intent pipeline and the
//! OpenAI-compatible HTTP API: pick a connected node serving the model,
//! send `RequestModelInference`, and await the result via `pending_inferences`.

use crate::http::state::PendingInferences;
use crate::registry::Registry;
use crate::scheduler::Scheduler;
use crate::server::Connections;
use shared::{ChatTurn, InferenceRequest, MeshMessage, WIRE_VERSION};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio::time::{Duration, timeout};

// Must exceed LLAMA_GENERATE_TIMEOUT_SECS on the agent (default 120 s) so the
// agent's own HTTP timeout fires first and sends back an error rather than us
// dropping the result mid-flight.
pub const INFERENCE_TIMEOUT_SECS: u64 = 150;

/// Dispatch an inference to a connected local node and await the result.
/// `request_id` is used verbatim on the wire, so callers namespace it
/// themselves (`intent-…`, `chatcmpl-…`). Returns the node's
/// `InferenceResult`, or an error message on any failure (no connected node,
/// channel closed, timeout).
#[allow(clippy::too_many_arguments)]
pub async fn dispatch_local_inference(
    request_id: &str,
    model_name: &str,
    messages: Vec<ChatTurn>,
    max_tokens: u32,
    temperature: Option<f32>,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_inferences: &PendingInferences,
) -> Result<shared::InferenceResult, String> {
    // Find a connected LLM node — skip any whose TCP channel has gone away.
    let connected: std::collections::HashSet<String> =
        connections.lock().unwrap().keys().cloned().collect();
    let llm_node_id = {
        let reg = registry.lock().unwrap();
        Scheduler::new(&reg)
            .select_node_for_inference(model_name)
            .filter(|n| connected.contains(&n.id))
            .map(|n| n.id)
    };
    let llm_node_id = llm_node_id
        .ok_or_else(|| format!("no connected node has model '{model_name}' in Ready state"))?;

    let agent_tx = connections.lock().unwrap().get(&llm_node_id).cloned();
    let agent_tx = agent_tx.ok_or_else(|| format!("LLM node '{llm_node_id}' is not connected"))?;

    let infer_req = InferenceRequest {
        request_id: request_id.to_string(),
        node_id: None,
        model_name: model_name.to_string(),
        messages,
        max_tokens,
        temperature,
        wire_version: WIRE_VERSION,
    };

    let (otx, orx) = oneshot::channel();
    pending_inferences
        .lock()
        .unwrap()
        .insert(request_id.to_string(), (otx, llm_node_id.clone()));

    if agent_tx
        .send(MeshMessage::RequestModelInference(infer_req))
        .await
        .is_err()
    {
        pending_inferences.lock().unwrap().remove(request_id);
        return Err("LLM node channel closed before inference could be sent".to_string());
    }

    match timeout(Duration::from_secs(INFERENCE_TIMEOUT_SECS), orx).await {
        Ok(Ok(MeshMessage::ModelInferenceResult(res))) => Ok(res),
        Ok(Ok(MeshMessage::Error(e))) => Err(format!("LLM error: {e}")),
        Ok(Ok(_)) => Err("unexpected message from LLM node".to_string()),
        Err(_) | Ok(Err(_)) => {
            pending_inferences.lock().unwrap().remove(request_id);
            Err(format!(
                "LLM inference timed out after {INFERENCE_TIMEOUT_SECS}s"
            ))
        }
    }
}
