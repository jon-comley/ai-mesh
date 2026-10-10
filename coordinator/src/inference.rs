//! Local inference dispatch shared by the intent pipeline and the
//! OpenAI-compatible HTTP API: pick a connected node serving the model,
//! send `RequestModelInference`, and await the result via `pending_inferences`.

use crate::http::state::{PendingInferences, PendingStreams, STREAM_CHANNEL_CAP};
use crate::registry::Registry;
use crate::server::Connections;
use crate::work_router::{self, Class, InFlightGuard};
use shared::{
    ChatTurn, InferenceRequest, MeshMessage, WIRE_VERSION, WorkInferenceDone, WorkInferenceRequest,
    WorkOutcome, WorkerInfo, WorkerSnapshot,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

// Must exceed LLAMA_GENERATE_TIMEOUT_SECS on the agent (default 120 s) so the
// agent's own HTTP timeout fires first and sends back an error rather than us
// dropping the result mid-flight.
pub const INFERENCE_TIMEOUT_SECS: u64 = 150;

/// Pass as `model_name` to let the router choose: any control model, on the
/// least busy machine (see `work_router`). Callers that used to default to
/// "the largest Ready model" pass this instead, so a home command can go to
/// beelink1 while mac1 is busy with a review rather than wait behind it.
pub const AUTO_CONTROL_MODEL: &str = "";

fn connected_ids(connections: &Connections) -> HashSet<String> {
    connections.lock().unwrap().keys().cloned().collect()
}

/// Pick the machine (and, for [`AUTO_CONTROL_MODEL`], the model) for a
/// control request, pausing any review work that is in its way. Returns the
/// node id, the model name and the node's sender.
fn select_control_target(
    model_name: &str,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
) -> Result<(String, String, mpsc::Sender<MeshMessage>), String> {
    let connected = connected_ids(connections);
    let cands = work_router::candidates(&registry.lock().unwrap(), &connected);
    let requested = (model_name != AUTO_CONTROL_MODEL).then_some(model_name);
    let (pick, paused) = {
        let mut st = work_router::state().lock().unwrap();
        let pick = st
            .pick_control(&cands, requested)
            .ok_or_else(|| match requested {
                Some(m) => format!("no connected node has model '{m}' in Ready state"),
                None => "no LLM model is ready on any node".to_string(),
            })?;
        let paused = st.preempt(&pick.preempt, std::time::Instant::now());
        (pick, paused)
    };
    let conns = connections.lock().unwrap();
    for (request_id, node_id) in paused {
        info!(%request_id, %node_id, "pausing review work for a home command");
        // The work task also sends this when it sees the pause; sending it here
        // as well puts it on the wire *before* the control request below.
        if let Some(tx) = conns.get(&node_id) {
            let _ = tx.try_send(MeshMessage::CancelInference { request_id });
        }
    }
    let node_id = pick.target.node_id;
    let agent_tx = conns
        .get(&node_id)
        .cloned()
        .ok_or_else(|| format!("LLM node '{node_id}' is not connected"))?;
    Ok((node_id, pick.target.model_name, agent_tx))
}

fn build_infer_request(
    request_id: &str,
    model_name: &str,
    messages: Vec<ChatTurn>,
    stream: bool,
    max_tokens: u32,
    temperature: Option<f32>,
) -> InferenceRequest {
    InferenceRequest {
        request_id: request_id.to_string(),
        node_id: None,
        model_name: model_name.to_string(),
        messages,
        stream,
        max_tokens,
        temperature,
        tools: None,
        wire_version: WIRE_VERSION,
    }
}

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
    dispatch_local_inference_with_tools(
        request_id,
        model_name,
        messages,
        max_tokens,
        temperature,
        None,
        registry,
        connections,
        pending_inferences,
    )
    .await
}

/// [`dispatch_local_inference`] with native tool definitions for llama-server's
/// `tools` field. An agent that predates wire v12 drops them silently and its
/// result comes back with `native_tools: false`, which the caller must check.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch_local_inference_with_tools(
    request_id: &str,
    model_name: &str,
    messages: Vec<ChatTurn>,
    max_tokens: u32,
    temperature: Option<f32>,
    tools: Option<Vec<serde_json::Value>>,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_inferences: &PendingInferences,
) -> Result<shared::InferenceResult, String> {
    let (llm_node_id, model_name, agent_tx) =
        select_control_target(model_name, registry, connections)?;
    let mut infer_req = build_infer_request(
        request_id,
        &model_name,
        messages,
        false,
        max_tokens,
        temperature,
    );
    infer_req.node_id = Some(llm_node_id.clone());
    infer_req.tools = tools;
    work_router::state()
        .lock()
        .unwrap()
        .begin(request_id, &llm_node_id, Class::Control, None);
    let _in_flight = InFlightGuard(request_id.to_string());

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
        Ok(Err(_)) => {
            pending_inferences.lock().unwrap().remove(request_id);
            Err("LLM inference channel closed".to_string())
        }
        Err(_) => {
            pending_inferences.lock().unwrap().remove(request_id);
            // Free the node's inference slot too: without this the node kept
            // generating for nobody, and the next request queued behind it.
            let _ = agent_tx.try_send(MeshMessage::CancelInference {
                request_id: request_id.to_string(),
            });
            Err(format!(
                "LLM inference timed out after {INFERENCE_TIMEOUT_SECS}s"
            ))
        }
    }
}

/// Register a streaming inference and return the channel the TCP demux feeds:
/// N `ModelInferenceChunk` messages, then one terminal `ModelInferenceResult`
/// (or a `MeshMessage::Error` if the node dies mid-stream). No timeout here —
/// the SSE emitter owns the first-chunk and inter-chunk deadlines. The caller
/// must remove the `pending_streams` entry when it stops consuming.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch_local_inference_stream(
    request_id: &str,
    model_name: &str,
    messages: Vec<ChatTurn>,
    max_tokens: u32,
    temperature: Option<f32>,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_streams: &PendingStreams,
) -> Result<mpsc::Receiver<MeshMessage>, String> {
    let (llm_node_id, model_name, agent_tx) =
        select_control_target(model_name, registry, connections)?;
    let mut infer_req = build_infer_request(
        request_id,
        &model_name,
        messages,
        true,
        max_tokens,
        temperature,
    );
    infer_req.node_id = Some(llm_node_id.clone());

    let (stx, srx) = mpsc::channel(STREAM_CHANNEL_CAP);
    pending_streams
        .lock()
        .unwrap()
        .insert(request_id.to_string(), (stx, llm_node_id.clone()));

    if agent_tx
        .send(MeshMessage::RequestModelInference(infer_req))
        .await
        .is_err()
    {
        pending_streams.lock().unwrap().remove(request_id);
        return Err("LLM node channel closed before inference could be sent".to_string());
    }

    Ok(srx)
}

/// How long a work request may wait for its first token. Reading a 100k-token
/// prompt on mac1 takes a minute or two; on beelink1 a 30k prompt can take
/// several, so this is generous.
const WORK_FIRST_TOKEN_SECS: u64 = 1200;
/// Longest gap between tokens once a work request is generating.
const WORK_TOKEN_GAP_SECS: u64 = 180;

/// The machines that could take review work, for mac1's planner.
pub fn worker_snapshot(
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
) -> WorkerSnapshot {
    let connected = connected_ids(connections);
    let cands = work_router::candidates(&registry.lock().unwrap(), &connected);
    let st = work_router::state().lock().unwrap();
    let now = std::time::Instant::now();
    WorkerSnapshot {
        workers: cands
            .into_iter()
            .map(|c| WorkerInfo {
                busy: st.busy(&c.node_id),
                resting: st.resting(&c.node_id, now),
                control: st.roles.is_control(&c.model_name),
                work: st.roles.is_work(&c.model_name),
                node_id: c.node_id,
                hostname: c.hostname,
                model_name: c.model_name,
                ctx_size: c.ctx_size,
            })
            .collect(),
    }
}

/// Run one review-work request from an agent (mac1) and report how it ended
/// on `origin`. Streams from the worker so the agent's non-streaming 90 s cap
/// does not apply and a pause stops it within a token.
pub async fn run_work_request(
    req: WorkInferenceRequest,
    origin: mpsc::Sender<MeshMessage>,
    registry: Arc<Mutex<Registry>>,
    connections: Connections,
    pending_streams: PendingStreams,
) {
    let started = std::time::Instant::now();
    let reply =
        |outcome: WorkOutcome, node: &str, model: &str, res: Option<&shared::InferenceResult>| {
            MeshMessage::WorkInferenceDone(WorkInferenceDone {
                request_id: req.request_id.clone(),
                node_id: node.to_string(),
                model_name: model.to_string(),
                outcome,
                output: res.map(|r| r.output.clone()).unwrap_or_default(),
                prompt_tokens: res.map(|r| r.prompt_tokens).unwrap_or(0),
                tokens_generated: res.map(|r| r.tokens_generated).unwrap_or(0),
                duration_ms: started.elapsed().as_millis() as u64,
            })
        };
    // Namespaced so it cannot collide with a control request id on the worker.
    let wire_id = format!("work-{}", req.request_id);

    let connected = connected_ids(&connections);
    let cands = work_router::candidates(&registry.lock().unwrap(), &connected);
    let (pause_tx, mut pause_rx) = oneshot::channel::<()>();
    let picked = {
        let mut st = work_router::state().lock().unwrap();
        let picked = st.pick_work(
            &cands,
            req.node_id.as_deref(),
            req.model_name.as_deref(),
            std::time::Instant::now(),
        );
        if let Ok(c) = &picked {
            // Claimed in the same lock as the pick, so two requests cannot
            // both see the machine as idle.
            st.begin(&wire_id, &c.node_id, Class::Work, Some(pause_tx));
        }
        picked
    };
    let target = match picked {
        Ok(t) => t,
        Err(reason) => {
            let _ = origin
                .send(reply(WorkOutcome::NoWorker { reason }, "", "", None))
                .await;
            return;
        }
    };
    let _in_flight = InFlightGuard(wire_id.clone());
    let (node, model) = (target.node_id.clone(), target.model_name.clone());
    let Some(agent_tx) = connections.lock().unwrap().get(&node).cloned() else {
        let reason = format!("'{node}' is not connected");
        let _ = origin
            .send(reply(WorkOutcome::NoWorker { reason }, &node, &model, None))
            .await;
        return;
    };

    let mut infer_req = build_infer_request(
        &wire_id,
        &model,
        req.messages.clone(),
        true,
        req.max_tokens,
        req.temperature,
    );
    infer_req.node_id = Some(node.clone());
    let (stx, mut srx) = mpsc::channel(STREAM_CHANNEL_CAP);
    pending_streams
        .lock()
        .unwrap()
        .insert(wire_id.clone(), (stx, node.clone()));
    if agent_tx
        .send(MeshMessage::RequestModelInference(infer_req))
        .await
        .is_err()
    {
        pending_streams.lock().unwrap().remove(&wire_id);
        let reason = format!("'{node}' disconnected before the work was sent");
        let _ = origin
            .send(reply(WorkOutcome::Failed { reason }, &node, &model, None))
            .await;
        return;
    }
    info!(request_id = %req.request_id, %node, %model, "review work dispatched");

    let mut first = true;
    let mut pause_open = true;
    let outcome_msg = loop {
        let wait = Duration::from_secs(if first {
            WORK_FIRST_TOKEN_SECS
        } else {
            WORK_TOKEN_GAP_SECS
        });
        tokio::select! {
            paused = &mut pause_rx, if pause_open => {
                if paused.is_err() {
                    // Sender dropped without a pause: nothing to do but keep going.
                    pause_open = false;
                    continue;
                }
                pending_streams.lock().unwrap().remove(&wire_id);
                let _ = agent_tx.try_send(MeshMessage::CancelInference { request_id: wire_id.clone() });
                break reply(WorkOutcome::Preempted, &node, &model, None);
            }
            msg = timeout(wait, srx.recv()) => match msg {
                Ok(Some(MeshMessage::ModelInferenceChunk(_))) => first = false,
                Ok(Some(MeshMessage::ModelInferenceResult(res))) => {
                    break match &res.error {
                        Some(e) => reply(WorkOutcome::Failed { reason: e.clone() }, &node, &model, Some(&res)),
                        None => reply(WorkOutcome::Finished, &node, &model, Some(&res)),
                    };
                }
                Ok(Some(MeshMessage::Error(e))) => {
                    break reply(WorkOutcome::Failed { reason: e }, &node, &model, None);
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    break reply(
                        WorkOutcome::Failed { reason: "the worker's stream closed".into() },
                        &node, &model, None,
                    );
                }
                Err(_) => {
                    pending_streams.lock().unwrap().remove(&wire_id);
                    let _ = agent_tx.try_send(MeshMessage::CancelInference { request_id: wire_id.clone() });
                    warn!(request_id = %req.request_id, %node, "review work timed out");
                    break reply(
                        WorkOutcome::Failed { reason: format!("no output for {}s", wait.as_secs()) },
                        &node, &model, None,
                    );
                }
            }
        }
    };
    if origin.send(outcome_msg).await.is_err() {
        warn!(request_id = %req.request_id, "review node went away before its result arrived");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{
        InferenceChunk, InferenceResult, ModelLifecycleState, NodeCapabilities, NodeIdentity,
        NodeRole,
    };
    use std::collections::HashMap;

    /// A registry with one LLM node per `(node_id, model)`, and a connection
    /// for each whose inbound side the test reads.
    fn mesh(
        nodes: &[(&str, &str)],
    ) -> (
        Arc<Mutex<Registry>>,
        Connections,
        HashMap<String, mpsc::Receiver<MeshMessage>>,
    ) {
        let mut reg = Registry::new();
        let conns: Connections = Arc::new(Mutex::new(HashMap::new()));
        let mut rxs = HashMap::new();
        for (node, model) in nodes {
            reg.update_heartbeat(NodeIdentity {
                id: (*node).into(),
                hostname: (*node).into(),
                ip: "10.0.0.1".into(),
                role: NodeRole::Compute,
            });
            reg.update_capabilities(
                node,
                NodeCapabilities {
                    features: vec![shared::Feature::Llm],
                    llm_ctx_size: Some(32768),
                    ..NodeCapabilities::default()
                },
            );
            reg.update_model_status(node, model, 4096, ModelLifecycleState::Ready);
            let (tx, rx) = mpsc::channel(16);
            conns.lock().unwrap().insert((*node).into(), tx);
            rxs.insert((*node).to_string(), rx);
        }
        (Arc::new(Mutex::new(reg)), conns, rxs)
    }

    fn work_req(id: &str, node: &str) -> WorkInferenceRequest {
        WorkInferenceRequest {
            request_id: id.into(),
            node_id: Some(node.into()),
            model_name: None,
            messages: vec![ChatTurn::user("review this")],
            max_tokens: 512,
            temperature: Some(0.1),
        }
    }

    async fn recv(rx: &mut mpsc::Receiver<MeshMessage>) -> MeshMessage {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for a message")
            .expect("channel closed")
    }

    #[tokio::test]
    async fn work_streams_from_the_chosen_node_and_reports_the_output() {
        let (registry, conns, mut rxs) = mesh(&[("wt1-mac", "wt1-coder")]);
        let streams: PendingStreams = Arc::new(Mutex::new(HashMap::new()));
        let (otx, mut orx) = mpsc::channel(4);
        tokio::spawn(run_work_request(
            work_req("r1", "wt1-mac"),
            otx,
            registry,
            conns,
            streams.clone(),
        ));
        let node_rx = rxs.get_mut("wt1-mac").unwrap();
        let MeshMessage::RequestModelInference(req) = recv(node_rx).await else {
            panic!("expected an inference request");
        };
        assert!(req.stream);
        assert_eq!(req.request_id, "work-r1");
        assert_eq!(req.node_id.as_deref(), Some("wt1-mac"));

        let stx = streams.lock().unwrap().get("work-r1").unwrap().0.clone();
        stx.send(MeshMessage::ModelInferenceChunk(InferenceChunk {
            request_id: "work-r1".into(),
            node_id: "wt1-mac".into(),
            delta: "[]".into(),
            wire_version: WIRE_VERSION,
        }))
        .await
        .unwrap();
        stx.send(MeshMessage::ModelInferenceResult(InferenceResult {
            request_id: "work-r1".into(),
            node_id: "wt1-mac".into(),
            model_name: "wt1-coder".into(),
            output: "[]".into(),
            tokens_generated: 1,
            prompt_tokens: 900,
            duration_ms: 5,
            prompt_eval_ms: 0,
            error: None,
            tool_calls: vec![],
            native_tools: false,
            wire_version: WIRE_VERSION,
        }))
        .await
        .unwrap();

        let MeshMessage::WorkInferenceDone(done) = recv(&mut orx).await else {
            panic!("expected WorkInferenceDone");
        };
        assert_eq!(done.request_id, "r1");
        assert_eq!(done.outcome, WorkOutcome::Finished);
        assert_eq!(done.output, "[]");
        assert_eq!(done.prompt_tokens, 900);
        assert!(!work_router::state().lock().unwrap().busy("wt1-mac"));
    }

    #[tokio::test]
    async fn a_home_command_pauses_work_on_the_only_machine_and_runs_first() {
        let (registry, conns, mut rxs) = mesh(&[("wt2-mac", "wt2-coder")]);
        let streams: PendingStreams = Arc::new(Mutex::new(HashMap::new()));
        let pending: PendingInferences = Arc::new(Mutex::new(HashMap::new()));
        let (otx, mut orx) = mpsc::channel(4);
        tokio::spawn(run_work_request(
            work_req("r2", "wt2-mac"),
            otx,
            registry.clone(),
            conns.clone(),
            streams.clone(),
        ));
        let node_rx = rxs.get_mut("wt2-mac").unwrap();
        assert!(matches!(
            recv(node_rx).await,
            MeshMessage::RequestModelInference(_)
        ));

        let control = {
            let (registry, conns, pending) = (registry.clone(), conns.clone(), pending.clone());
            tokio::spawn(async move {
                dispatch_local_inference(
                    "intent-wt2",
                    "wt2-coder",
                    vec![ChatTurn::user("lights off")],
                    64,
                    None,
                    &registry,
                    &conns,
                    &pending,
                )
                .await
            })
        };
        // The cancel reaches the node before the home command does.
        match recv(node_rx).await {
            MeshMessage::CancelInference { request_id } => assert_eq!(request_id, "work-r2"),
            other => panic!("expected CancelInference first, got {other:?}"),
        }
        let mut saw_intent = false;
        for _ in 0..2 {
            match recv(node_rx).await {
                MeshMessage::RequestModelInference(r) => {
                    assert_eq!(r.request_id, "intent-wt2");
                    saw_intent = true;
                    break;
                }
                MeshMessage::CancelInference { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(saw_intent);
        let (otx_res, _) = pending.lock().unwrap().remove("intent-wt2").unwrap();
        otx_res
            .send(MeshMessage::ModelInferenceResult(InferenceResult {
                request_id: "intent-wt2".into(),
                node_id: "wt2-mac".into(),
                model_name: "wt2-coder".into(),
                output: "ok".into(),
                tokens_generated: 1,
                prompt_tokens: 1,
                duration_ms: 1,
                prompt_eval_ms: 0,
                error: None,
                tool_calls: vec![],
                native_tools: false,
                wire_version: WIRE_VERSION,
            }))
            .unwrap();
        assert_eq!(control.await.unwrap().unwrap().output, "ok");

        let MeshMessage::WorkInferenceDone(done) = recv(&mut orx).await else {
            panic!("expected WorkInferenceDone");
        };
        assert_eq!(done.outcome, WorkOutcome::Preempted);
        assert!(!streams.lock().unwrap().contains_key("work-r2"));

        // The machine now rests: new work is refused, and the snapshot says why.
        let snap = worker_snapshot(&registry, &conns);
        let w = snap
            .workers
            .iter()
            .find(|w| w.node_id == "wt2-mac")
            .unwrap();
        assert!(w.resting && !w.busy);
        assert_eq!(w.ctx_size, Some(32768));
        let (otx2, mut orx2) = mpsc::channel(4);
        run_work_request(work_req("r3", "wt2-mac"), otx2, registry, conns, streams).await;
        let MeshMessage::WorkInferenceDone(refused) = recv(&mut orx2).await else {
            panic!("expected WorkInferenceDone");
        };
        assert!(matches!(refused.outcome, WorkOutcome::NoWorker { .. }));
    }

    #[tokio::test]
    async fn a_home_command_goes_to_an_idle_machine_instead_of_pausing_work() {
        let (registry, conns, mut rxs) =
            mesh(&[("wt3-mac", "wt3-coder"), ("wt3-beelink", "wt3-small")]);
        let streams: PendingStreams = Arc::new(Mutex::new(HashMap::new()));
        let (otx, _orx) = mpsc::channel(4);
        tokio::spawn(run_work_request(
            work_req("r4", "wt3-mac"),
            otx,
            registry.clone(),
            conns.clone(),
            streams,
        ));
        assert!(matches!(
            recv(rxs.get_mut("wt3-mac").unwrap()).await,
            MeshMessage::RequestModelInference(_)
        ));
        let (node, model, _tx) =
            select_control_target(AUTO_CONTROL_MODEL, &registry, &conns).unwrap();
        assert_eq!(
            (node.as_str(), model.as_str()),
            ("wt3-beelink", "wt3-small")
        );
        assert!(work_router::state().lock().unwrap().busy("wt3-mac"));
    }

    #[tokio::test]
    async fn work_for_an_unknown_machine_reports_no_worker() {
        let (registry, conns, _rxs) = mesh(&[("wt4-mac", "wt4-coder")]);
        let streams: PendingStreams = Arc::new(Mutex::new(HashMap::new()));
        let (otx, mut orx) = mpsc::channel(4);
        run_work_request(work_req("r5", "nowhere"), otx, registry, conns, streams).await;
        let MeshMessage::WorkInferenceDone(done) = recv(&mut orx).await else {
            panic!("expected WorkInferenceDone");
        };
        assert!(matches!(done.outcome, WorkOutcome::NoWorker { .. }));
    }
}
