use crate::http::state::{PendingInferences, PendingIntents};
use crate::registry::Registry;
use crate::scheduler::Scheduler;
use crate::server::Connections;
use shared::{
    InferenceRequest, IntentRequest, IntentResponse, LightAction, LightCommandRequest,
    LightStateReport, LightTarget, MeshMessage, ReaperCommandRequest, ReaperScriptRequest,
    SceneLoadRequest, ToolCallRecord, WIRE_VERSION,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

// Must exceed LLAMA_GENERATE_TIMEOUT_SECS on the agent (default 120 s) so the
// agent's own HTTP timeout fires first and sends back an error rather than us
// dropping the result mid-flight.
const INTENT_INFERENCE_TIMEOUT_SECS: u64 = 150;
const TOOL_RESPONSE_TIMEOUT_SECS: u64 = 10;

#[allow(clippy::too_many_arguments)]
pub async fn handle_intent(
    request: IntentRequest,
    registry: Arc<Mutex<Registry>>,
    connections: Connections,
    pending_inferences: PendingInferences,
    pending_intents: PendingIntents,
    device_states: Vec<LightStateReport>,
    reaper_online: bool,
    gateway: Option<crate::cloud::GatewayInvocation>,
) -> IntentResponse {
    let started = std::time::Instant::now();
    let fail = |msg: String| IntentResponse {
        request_id: request.request_id.clone(),
        node_id: String::new(),
        model_name: String::new(),
        text: None,
        tool_calls: vec![],
        error: Some(msg),
        duration_ms: 0,
        tokens_generated: 0,
        prompt_eval_ms: 0,
        total_ms: started.elapsed().as_millis() as u64,
        compression_applied: false,
        prompt_tokens_before: 0,
        prompt_tokens_after: 0,
    };

    // 1. Collect tool schemas from nodes that advertise a given feature
    let schemas = {
        let reg = registry.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut schemas = Vec::new();
        for feature in ["lighting", "reaper"] {
            if !reg.nodes_with_feature(feature).is_empty() {
                for schema in tool_schemas_for_feature(feature) {
                    let name = schema["name"].as_str().unwrap_or("").to_string();
                    if seen.insert(name) {
                        schemas.push(schema);
                    }
                }
            }
        }
        schemas
    };

    // 2. Choose model. In cloud mode the label is the cloud model; a local model
    //    is still resolved when available so we can fall back if the cloud call
    //    fails. Only hard-fail on "no model" when we have neither.
    let local_model: Option<String> = request
        .model_name
        .clone()
        .or_else(|| registry.lock().unwrap().any_ready_llm_model());
    let model_name = match (&gateway, &local_model) {
        (Some(gw), _) => gw.provider.model().to_string(),
        (None, Some(m)) => m.clone(),
        (None, None) => return fail("no LLM model is ready on any node".into()),
    };

    // 3. Build system prompt + conversation
    let (known_devices, known_groups) = registry.lock().unwrap().all_light_device_names();
    let device_room_map = registry.lock().unwrap().device_room_name_map();
    let scene_names: Vec<String> = registry
        .lock()
        .unwrap()
        .list_scenes()
        .into_iter()
        .map(|s| s.name)
        .collect();
    let system_prompt = build_system_prompt(&schemas);
    let device_ctx = build_device_context(
        &known_devices,
        &known_groups,
        &device_states,
        &device_room_map,
        &scene_names,
    );
    // Conversation history is the bulky, compressible part. The device context
    // and the user's current question are kept verbatim so tool-calling fidelity
    // (exact device names, the actual request) is never degraded.
    const MAX_CONTEXT_TURNS: usize = 20;
    let context = if request.context.len() > MAX_CONTEXT_TURNS {
        &request.context[request.context.len() - MAX_CONTEXT_TURNS..]
    } else {
        &request.context[..]
    };
    let mut history_blob = String::new();
    for turn in context {
        match turn.role {
            shared::IntentRole::User => {
                history_blob.push_str(&format!("User: {}\n", turn.content));
            }
            shared::IntentRole::Assistant => {
                history_blob.push_str(&format!("Assistant: {}\n", turn.content));
            }
        }
    }

    // Compress the history only when forwarding to the cloud — local inference is
    // not token-billed, so it keeps full fidelity.
    let (history_for_prompt, compression_applied, prompt_tokens_before, prompt_tokens_after) =
        if let Some(gw) = gateway.as_ref().filter(|gw| gw.compress) {
            let outcome = crate::compress::compress(&history_blob, gw.engine);
            if outcome.compressed {
                info!(
                    request_id = %request.request_id,
                    before = outcome.orig_tokens,
                    after = outcome.new_tokens,
                    ratio = outcome.ratio,
                    "compressed history for cloud forward"
                );
            }
            (
                outcome.text,
                outcome.compressed,
                outcome.orig_tokens as u32,
                outcome.new_tokens as u32,
            )
        } else {
            (history_blob, false, 0, 0)
        };

    let user_prompt = format!("{device_ctx}{history_for_prompt}{}", request.text);

    // 4/5. Run inference. Cloud mode calls the online provider and, on any
    //       failure, records the error and falls back to local. Both paths share
    //       the same system + user prompt and the same tool-parsing below.
    let llm_result = if let Some(gw) = &gateway {
        match gw
            .provider
            .complete(Some(&system_prompt), &user_prompt)
            .await
        {
            Ok(reply) => {
                gw.state
                    .record_gateway_call(prompt_tokens_before as u64, prompt_tokens_after as u64);
                shared::InferenceResult {
                    request_id: request.request_id.clone(),
                    node_id: gw.provider.provider_name().to_string(),
                    model_name: model_name.clone(),
                    output: reply.text,
                    tokens_generated: reply.completion_tokens,
                    duration_ms: 0,
                    prompt_eval_ms: 0,
                    error: None,
                    wire_version: WIRE_VERSION,
                }
            }
            Err(e) => {
                warn!(
                    request_id = %request.request_id,
                    "cloud provider failed: {e}; falling back to local"
                );
                gw.state.record_gateway_error(e.to_string());
                match run_local_inference(
                    &request.request_id,
                    local_model.as_deref(),
                    &system_prompt,
                    &user_prompt,
                    &registry,
                    &connections,
                    &pending_inferences,
                )
                .await
                {
                    Ok(r) => r,
                    Err(msg) => {
                        return fail(format!("cloud failed ({e}); local fallback failed: {msg}"));
                    }
                }
            }
        }
    } else {
        match run_local_inference(
            &request.request_id,
            local_model.as_deref(),
            &system_prompt,
            &user_prompt,
            &registry,
            &connections,
            &pending_inferences,
        )
        .await
        {
            Ok(r) => r,
            Err(msg) => return fail(msg),
        }
    };

    let node_id = llm_result.node_id.clone();
    let model_name = llm_result.model_name.clone();
    let duration_ms = llm_result.duration_ms;
    let tokens_generated = llm_result.tokens_generated;
    let prompt_eval_ms = llm_result.prompt_eval_ms;
    if let Some(e) = llm_result.error {
        let hostname = registry
            .lock()
            .unwrap()
            .get_node_hostname(&node_id)
            .unwrap_or_else(|| node_id.clone());
        return fail(format!("inference error from {hostname}: {e}"));
    }
    let raw = strip_think_blocks(llm_result.output.trim());
    info!(
        request_id = %request.request_id,
        node_id = %node_id,
        "intent LLM output: {:?}",
        raw
    );

    // 6. Parse as tool call(s) or return as free text
    if let Some(calls) = try_parse_tool_calls(&raw) {
        let mut records: Vec<ToolCallRecord> = Vec::new();

        for call in &calls {
            let tool_name = call["tool"].as_str().unwrap_or("").to_string();
            let mut args = call["args"].clone();

            // If the model emitted action=color but forgot color_name/cx/cy,
            // extract the colour from the user's original text as a fallback.
            if args.get("action").and_then(|v| v.as_str()) == Some("color")
                && args.get("cx").is_none()
                && args.get("color_name").is_none()
                && let Some(color) = extract_color_from_text(&request.text)
            {
                args["color_name"] = serde_json::Value::String(color.to_string());
            }

            // A REAPER tool was requested but REAPER isn't running: launch it and tell the
            // user to retry, rather than dispatching into a dead daemon and timing out.
            let tool_result = if tool_name.starts_with("reaper_") && !reaper_online {
                launch_reaper_and_advise(
                    &request.request_id,
                    &registry,
                    &connections,
                    &pending_intents,
                )
                .await
            } else {
                dispatch_tool(
                    &request.request_id,
                    &tool_name,
                    args.clone(),
                    &registry,
                    &connections,
                    &pending_intents,
                    &device_states,
                )
                .await
            };

            records.push(ToolCallRecord {
                tool: tool_name,
                args,
                result: Some(tool_result),
            });
        }

        let text = offline_skip_summary(&records, &device_room_map);

        return IntentResponse {
            request_id: request.request_id,
            node_id,
            model_name,
            text,
            tool_calls: records,
            error: None,
            duration_ms,
            tokens_generated,
            prompt_eval_ms,
            total_ms: started.elapsed().as_millis() as u64,
            compression_applied,
            prompt_tokens_before,
            prompt_tokens_after,
        };
    }

    IntentResponse {
        request_id: request.request_id,
        node_id,
        model_name,
        text: Some(raw),
        tool_calls: vec![],
        error: None,
        duration_ms,
        tokens_generated,
        prompt_eval_ms,
        total_ms: started.elapsed().as_millis() as u64,
        compression_applied,
        prompt_tokens_before,
        prompt_tokens_after,
    }
}

/// Dispatch an inference to a connected local node and await the result.
/// Returns the node's `InferenceResult`, or an error message on any failure
/// (no model, no connected node, channel closed, timeout).
#[allow(clippy::too_many_arguments)]
async fn run_local_inference(
    request_id: &str,
    local_model: Option<&str>,
    system_prompt: &str,
    user_prompt: &str,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_inferences: &PendingInferences,
) -> Result<shared::InferenceResult, String> {
    let model_name = local_model.ok_or_else(|| "no LLM model is ready on any node".to_string())?;

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

    let infer_req_id = format!("intent-{request_id}");
    let infer_req = InferenceRequest {
        request_id: infer_req_id.clone(),
        node_id: None,
        model_name: model_name.to_string(),
        system_prompt: Some(system_prompt.to_string()),
        prompt: user_prompt.to_string(),
        max_tokens: 2048,
        temperature: Some(0.4),
        wire_version: WIRE_VERSION,
    };

    let (otx, orx) = oneshot::channel();
    pending_inferences
        .lock()
        .unwrap()
        .insert(infer_req_id.clone(), (otx, llm_node_id.clone()));

    if agent_tx
        .send(MeshMessage::RequestModelInference(infer_req))
        .await
        .is_err()
    {
        pending_inferences.lock().unwrap().remove(&infer_req_id);
        return Err("LLM node channel closed before inference could be sent".to_string());
    }

    match timeout(Duration::from_secs(INTENT_INFERENCE_TIMEOUT_SECS), orx).await {
        Ok(Ok(MeshMessage::ModelInferenceResult(res))) => Ok(res),
        Ok(Ok(MeshMessage::Error(e))) => Err(format!("LLM error: {e}")),
        Ok(Ok(_)) => Err("unexpected message from LLM node".to_string()),
        Err(_) | Ok(Err(_)) => {
            pending_inferences.lock().unwrap().remove(&infer_req_id);
            Err(format!(
                "LLM inference timed out after {INTENT_INFERENCE_TIMEOUT_SECS}s"
            ))
        }
    }
}

fn device_is_offline(target: &str, states: &[LightStateReport]) -> bool {
    states
        .iter()
        .find(|s| s.device_id == target)
        .is_some_and(|s| !s.online)
}

/// The tool result `dispatch_tool` returns when it refuses a light command because the
/// target is offline. Kept as one function so the producer (dispatch) and the parser
/// ([`parse_offline_skip`], used to build the proactive summary) can never drift apart.
fn offline_skip_result(target: &str) -> String {
    format!("device '{target}' is currently offline")
}

/// Inverse of [`offline_skip_result`]: recover the device target from such a result,
/// or `None` if the result is anything else.
fn parse_offline_skip(result: &str) -> Option<&str> {
    result
        .strip_prefix("device '")
        .and_then(|s| s.strip_suffix("' is currently offline"))
}

/// Build a short, friendly note about light targets that were skipped because they're
/// offline (powered down / unreachable), grouped by room, so the chat reply proactively
/// tells the user instead of relying on the model to honour the `[OFFLINE]` markers.
/// Returns `None` when nothing was skipped.
fn offline_skip_summary(
    records: &[ToolCallRecord],
    device_room_map: &HashMap<String, String>,
) -> Option<String> {
    let offline: Vec<&str> = records
        .iter()
        .filter(|r| r.tool == "light_command")
        .filter_map(|r| r.result.as_deref().and_then(parse_offline_skip))
        .collect();
    if offline.is_empty() {
        return None;
    }
    // Count per room, preserving first-seen order; targets with no known room are
    // grouped under an empty key and labelled generically.
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for t in &offline {
        let room = device_room_map.get(*t).cloned().unwrap_or_default();
        if !counts.contains_key(&room) {
            order.push(room.clone());
        }
        *counts.entry(room).or_insert(0) += 1;
    }
    let total = offline.len();
    let plural = if total == 1 { "light is" } else { "lights are" };
    let them = if total == 1 { "it" } else { "them" };
    // Single known room → the natural "the kitchen lights are powered off" phrasing.
    if order.len() == 1 && !order[0].is_empty() {
        let room = &order[0];
        return Some(format!(
            "Heads up: {total} {room} {plural} powered off (or unreachable), so I skipped {them}."
        ));
    }
    let breakdown = order
        .iter()
        .map(|r| {
            let c = counts[r];
            if r.is_empty() {
                format!("{c} with no room")
            } else {
                format!("{c} in {r}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "Heads up: {total} {plural} powered off (or unreachable), so I skipped {them} ({breakdown})."
    ))
}

async fn dispatch_tool(
    request_id: &str,
    tool_name: &str,
    mut args: serde_json::Value,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_intents: &PendingIntents,
    device_states: &[LightStateReport],
) -> String {
    // Find a lighting node that is currently connected
    let lighting_node_id = {
        let reg = registry.lock().unwrap();
        let lighting: Vec<String> = reg
            .nodes_with_feature("lighting")
            .into_iter()
            .map(|n| n.id)
            .collect();
        drop(reg);
        let conns = connections.lock().unwrap();
        lighting.into_iter().find(|id| conns.contains_key(id))
    };

    let Some(lighting_node_id) = lighting_node_id else {
        return "no lighting node connected".into();
    };

    let lighting_tx = connections.lock().unwrap().get(&lighting_node_id).cloned();
    let Some(lighting_tx) = lighting_tx else {
        return "lighting node channel not found".into();
    };

    match tool_name {
        "light_command" => {
            // Validate target; if it names a room instead of a device, resolve to
            // the first device in that room so the command still goes through.
            if let Some(target) = args
                .get("target")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let (devices, groups) = registry.lock().unwrap().all_light_device_names();
                let known: Vec<&str> = devices
                    .iter()
                    .chain(groups.iter())
                    .map(String::as_str)
                    .collect();
                if !known.is_empty() && !known.contains(&target) {
                    // Not a device/group — try matching against a room name.
                    let resolved = registry
                        .lock()
                        .unwrap()
                        .list_rooms()
                        .into_iter()
                        .find(|r| r.name.eq_ignore_ascii_case(target))
                        .and_then(|r| r.device_ids.into_iter().next());
                    match resolved {
                        Some(device_id) => {
                            args["target"] = serde_json::Value::String(device_id);
                        }
                        None => {
                            return format!(
                                "unknown target '{}' — known targets: {}",
                                target,
                                known.join(", ")
                            );
                        }
                    }
                }
            }
            // Reject commands to individual devices that are known to be offline.
            // Groups are not checked here — the lighting node handles partial groups.
            let final_target = args
                .get("target")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if device_is_offline(&final_target, device_states) {
                return offline_skip_result(&final_target);
            }
            let Some(cmds) = build_light_command(request_id, &args) else {
                return "unrecognised colour — no command sent".into();
            };
            for cmd in cmds {
                if lighting_tx
                    .send(MeshMessage::LightCommand(cmd))
                    .await
                    .is_err()
                {
                    warn!(request_id, "failed to send LightCommand to lighting node");
                    return "failed to send LightCommand to lighting node".into();
                }
            }
            "ok".into()
        }
        "scene_load" => {
            let scene_name = args
                .get("scene")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let transition_ms = args
                .get("transition_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(1000) as u32;

            let (otx, orx) = oneshot::channel();
            pending_intents
                .lock()
                .unwrap()
                .insert(request_id.to_string(), otx);

            let req = SceneLoadRequest {
                request_id: request_id.to_string(),
                scene_name,
                transition_ms,
            };
            if lighting_tx.send(MeshMessage::SceneLoad(req)).await.is_err() {
                pending_intents.lock().unwrap().remove(request_id);
                return "failed to send SceneLoad to lighting node".into();
            }

            match timeout(Duration::from_secs(TOOL_RESPONSE_TIMEOUT_SECS), orx).await {
                Ok(Ok(MeshMessage::SceneLoaded(report))) => {
                    if report.success {
                        format!("scene '{}' loaded", report.scene_name)
                    } else {
                        report.error.unwrap_or_else(|| "scene load failed".into())
                    }
                }
                _ => {
                    pending_intents.lock().unwrap().remove(request_id);
                    format!("scene load timed out after {TOOL_RESPONSE_TIMEOUT_SECS}s")
                }
            }
        }
        "reaper_transport" | "reaper_action" => {
            // Node selection: first connected reaper node.
            // If multiple REAPER nodes exist in future, extend this to a policy.
            let reaper_node_id = {
                let reg = registry.lock().unwrap();
                let nodes: Vec<String> = reg
                    .nodes_with_feature("reaper")
                    .into_iter()
                    .map(|n| n.id)
                    .collect();
                drop(reg);
                let conns = connections.lock().unwrap();
                nodes.into_iter().find(|id| conns.contains_key(id))
            };
            let Some(reaper_node_id) = reaper_node_id else {
                return "no REAPER node connected".into();
            };

            let action = if tool_name == "reaper_transport" {
                args["action"].as_str().unwrap_or("").to_string()
            } else {
                args["action_id"].as_str().unwrap_or("").to_string()
            };

            let cmd = ReaperCommandRequest {
                request_id: request_id.to_string(),
                action,
                params: args.clone(),
            };

            let (otx, orx) = oneshot::channel();
            pending_intents
                .lock()
                .unwrap()
                .insert(request_id.to_string(), otx);

            let sent = connections
                .lock()
                .unwrap()
                .get(&reaper_node_id)
                .map(|tx| tx.try_send(MeshMessage::ReaperCommand(cmd)).is_ok())
                .unwrap_or(false);

            if !sent {
                pending_intents.lock().unwrap().remove(request_id);
                return "failed to send ReaperCommand to node".into();
            }

            // 5 s timeout — prevents the LLM hanging if REAPER is unresponsive.
            match timeout(Duration::from_secs(5), orx).await {
                Ok(Ok(MeshMessage::ReaperCommandResult(r))) => {
                    if r.ok {
                        "ok".into()
                    } else {
                        r.message
                    }
                }
                _ => {
                    pending_intents.lock().unwrap().remove(request_id);
                    "REAPER command timed out".into()
                }
            }
        }
        "reaper_script" => {
            // `code` is required by the schema; empty -> daemon runs an empty file
            // (harmless no-op), matching the unwrap_or("") convention used by
            // reaper_transport/reaper_action above.
            let code = args["code"].as_str().unwrap_or("").to_string();
            run_reaper_lua(request_id, code, registry, connections, pending_intents).await
        }
        "reaper_add_track" => {
            let name = args["name"].as_str().unwrap_or("").trim().to_string();
            if name.is_empty() {
                return "reaper_add_track requires a non-empty track name".into();
            }
            // rec_input may arrive as a number or a numeric string from the model.
            let rec_input = args["rec_input"].as_i64().or_else(|| {
                args["rec_input"]
                    .as_str()
                    .and_then(|s| s.trim().parse().ok())
            });
            // New tracks default to armed (and exclusively so) — adding a track is
            // almost always a prelude to recording into it.
            let arm = args["arm"].as_bool().unwrap_or(true);
            let code = build_add_track_lua(&name, rec_input, arm);
            run_reaper_lua(request_id, code, registry, connections, pending_intents).await
        }
        "reaper_remove_track" => {
            let name = args["name"].as_str().unwrap_or("").trim().to_string();
            if name.is_empty() {
                return "reaper_remove_track requires a track name".into();
            }
            let code = build_remove_track_lua(&name);
            run_reaper_lua(request_id, code, registry, connections, pending_intents).await
        }
        "reaper_set_tempo" => {
            // tempo/ts_* may arrive as numbers or numeric strings from the model.
            let tempo = args["tempo"]
                .as_f64()
                .or_else(|| args["tempo"].as_str().and_then(|s| s.trim().parse().ok()));
            let ts_num = args["ts_num"]
                .as_u64()
                .or_else(|| args["ts_num"].as_str().and_then(|s| s.trim().parse().ok()))
                .map(|n| n as u32);
            let ts_denom = args["ts_denom"]
                .as_u64()
                .or_else(|| {
                    args["ts_denom"]
                        .as_str()
                        .and_then(|s| s.trim().parse().ok())
                })
                .map(|n| n as u32);
            if tempo.is_none() && ts_num.is_none() && ts_denom.is_none() {
                return "reaper_set_tempo requires a tempo and/or a time signature \
                        (ts_num/ts_denom)"
                    .into();
            }
            let code = build_set_tempo_lua(tempo, ts_num, ts_denom);
            run_reaper_lua(request_id, code, registry, connections, pending_intents).await
        }
        "reaper_get_project" => {
            run_reaper_lua(
                request_id,
                build_project_info_lua(),
                registry,
                connections,
                pending_intents,
            )
            .await
        }
        "reaper_add_fx" => {
            let track = args["track"].as_str().unwrap_or("").trim().to_string();
            let fx = args["fx"].as_str().unwrap_or("").trim().to_string();
            if track.is_empty() || fx.is_empty() {
                return "reaper_add_fx requires a track name and an fx (plugin) name".into();
            }
            let code = build_add_fx_lua(&track, &fx);
            run_reaper_lua(request_id, code, registry, connections, pending_intents).await
        }
        "reaper_list_fx" => {
            let track = args["track"].as_str().unwrap_or("").trim().to_string();
            if track.is_empty() {
                return "reaper_list_fx requires a track name".into();
            }
            let code = build_list_fx_lua(&track);
            run_reaper_lua(request_id, code, registry, connections, pending_intents).await
        }
        "reaper_list_fx_params" => {
            let track = args["track"].as_str().unwrap_or("").trim().to_string();
            let fx = args["fx"].as_str().unwrap_or("").trim().to_string();
            if track.is_empty() || fx.is_empty() {
                return "reaper_list_fx_params requires a track name and an fx (plugin) name"
                    .into();
            }
            let code = build_list_fx_params_lua(&track, &fx);
            run_reaper_lua(request_id, code, registry, connections, pending_intents).await
        }
        other => format!("unknown tool: {other}"),
    }
}

/// REAPER is offline but a REAPER tool was asked for: ask the node to spawn REAPER and
/// return a message telling the user to retry once it's loaded. We don't wait for REAPER
/// to become ready here (cold start + plugin scan far exceeds the intent timeout) — that
/// auto-retry is deferred (see roadmap). The launch reuses the `ReaperCommand` path with
/// action "launch"; its result is awaited here (via the pending-intent oneshot) to report
/// whether the spawn succeeded.
async fn launch_reaper_and_advise(
    request_id: &str,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_intents: &PendingIntents,
) -> String {
    let reaper_node_id = {
        let reg = registry.lock().unwrap();
        let nodes: Vec<String> = reg
            .nodes_with_feature("reaper")
            .into_iter()
            .map(|n| n.id)
            .collect();
        drop(reg);
        let conns = connections.lock().unwrap();
        nodes.into_iter().find(|id| conns.contains_key(id))
    };
    let Some(reaper_node_id) = reaper_node_id else {
        return "REAPER isn't running and no REAPER node is connected to start it.".into();
    };

    let cmd = ReaperCommandRequest {
        request_id: request_id.to_string(),
        action: "launch".to_string(),
        params: serde_json::Value::Null,
    };

    let (otx, orx) = oneshot::channel();
    pending_intents
        .lock()
        .unwrap()
        .insert(request_id.to_string(), otx);

    let sent = connections
        .lock()
        .unwrap()
        .get(&reaper_node_id)
        .map(|tx| tx.try_send(MeshMessage::ReaperCommand(cmd)).is_ok())
        .unwrap_or(false);
    if !sent {
        pending_intents.lock().unwrap().remove(request_id);
        return "REAPER isn't running and I couldn't reach the node to start it.".into();
    }

    match timeout(Duration::from_secs(8), orx).await {
        Ok(Ok(MeshMessage::ReaperCommandResult(r))) if r.ok => {
            if r.message == "REAPER is already starting" {
                "REAPER is still starting up — give it a few more seconds, then ask again.".into()
            } else {
                "REAPER wasn't running, so I've started it. Give it ~15 seconds to finish \
                 loading, then ask again."
                    .into()
            }
        }
        Ok(Ok(MeshMessage::ReaperCommandResult(r))) => {
            format!(
                "REAPER isn't running and I couldn't start it: {}",
                r.message
            )
        }
        _ => {
            pending_intents.lock().unwrap().remove(request_id);
            "REAPER isn't running; I tried to start it but got no confirmation. Check the \
             REAPER box, then try again."
                .into()
        }
    }
}

/// Send a Lua snippet to the connected REAPER node and await its result.
/// Shared by `reaper_script` (model-authored code) and the structured REAPER
/// tools that compile their own Lua, so the dispatch/timeout path lives once.
async fn run_reaper_lua(
    request_id: &str,
    code: String,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_intents: &PendingIntents,
) -> String {
    let reaper_node_id = {
        let reg = registry.lock().unwrap();
        let nodes: Vec<String> = reg
            .nodes_with_feature("reaper")
            .into_iter()
            .map(|n| n.id)
            .collect();
        drop(reg);
        let conns = connections.lock().unwrap();
        nodes.into_iter().find(|id| conns.contains_key(id))
    };
    let Some(reaper_node_id) = reaper_node_id else {
        return "no REAPER node connected".into();
    };

    let req = ReaperScriptRequest {
        request_id: request_id.to_string(),
        code,
    };

    let (otx, orx) = oneshot::channel();
    pending_intents
        .lock()
        .unwrap()
        .insert(request_id.to_string(), otx);

    let sent = connections
        .lock()
        .unwrap()
        .get(&reaper_node_id)
        .map(|tx| tx.try_send(MeshMessage::ReaperScript(req)).is_ok())
        .unwrap_or(false);

    if !sent {
        pending_intents.lock().unwrap().remove(request_id);
        return "failed to send ReaperScript to node".into();
    }

    match timeout(Duration::from_secs(10), orx).await {
        Ok(Ok(MeshMessage::ReaperScriptResult(r))) => {
            // Structured tools `return` a human-readable summary the daemon relays
            // back in `message`; surface it. Bare scripts that return nothing fall
            // back to a plain "ok".
            if r.ok && r.message.trim().is_empty() {
                "ok".into()
            } else {
                r.message
            }
        }
        _ => {
            pending_intents.lock().unwrap().remove(request_id);
            "REAPER script timed out".into()
        }
    }
}

/// Escape a string for a single-quoted Lua literal so quotes/backslashes/newlines
/// can't break out of it.
fn lua_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace(['\n', '\r'], " ")
}

/// Title-case a track name: each whitespace-separated word gets a capital first
/// letter and lowercase rest ("vocal track" → "Vocal Track", "DRUM bus" → "Drum Bus").
fn title_case(name: &str) -> String {
    name.split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build correct Lua to add a named track to the open project. Centralising this
/// is the whole point of `reaper_add_track`: small models routinely mishandle the
/// `InsertTrackAtIndex` (returns nothing) → `GetTrack` → `GetSetMediaTrackInfo_String`
/// sequence and produce a blank-named track.
///
/// Names are title-cased (`vocal` → `Vocal`). The Lua resolves a unique name
/// (appending ` 2`, ` 3`, … if the requested name is already taken), arms exclusively
/// when asked, and `return`s a human-readable summary the daemon relays back so the
/// chat reports the *actual* name/position.
fn build_add_track_lua(name: &str, rec_input: Option<i64>, arm: bool) -> String {
    let base = lua_escape(&title_case(name));
    let mut lua = String::new();
    // Resolve a unique name against existing tracks before inserting.
    lua.push_str("local function name_taken(n)\n");
    lua.push_str("  for i = 0, reaper.CountTracks(0) - 1 do\n");
    lua.push_str(
        "    local _, nm = reaper.GetSetMediaTrackInfo_String(reaper.GetTrack(0, i), 'P_NAME', '', false)\n",
    );
    lua.push_str("    if nm == n then return true end\n");
    lua.push_str("  end\n");
    lua.push_str("  return false\n");
    lua.push_str("end\n");
    lua.push_str(&format!("local base = '{base}'\n"));
    lua.push_str("local name = base\n");
    lua.push_str("local suffix = 2\n");
    lua.push_str(
        "while name_taken(name) do name = base .. ' ' .. suffix; suffix = suffix + 1 end\n",
    );
    lua.push_str("local idx = reaper.CountTracks(0)\n");
    lua.push_str("reaper.InsertTrackAtIndex(idx, true)\n");
    lua.push_str("local t = reaper.GetTrack(0, idx)\n");
    lua.push_str("reaper.GetSetMediaTrackInfo_String(t, 'P_NAME', name, true)\n");
    if let Some(input) = rec_input {
        lua.push_str(&format!(
            "reaper.SetMediaTrackInfo_Value(t, 'I_RECINPUT', {input})\n"
        ));
    }
    if arm {
        // Exclusive record-arm: disarm every track first, then arm + monitor the
        // new one, so only the freshly-added track is record-ready.
        lua.push_str("for i = 0, reaper.CountTracks(0) - 1 do\n");
        lua.push_str("  reaper.SetMediaTrackInfo_Value(reaper.GetTrack(0, i), 'I_RECARM', 0)\n");
        lua.push_str("end\n");
        lua.push_str("reaper.SetMediaTrackInfo_Value(t, 'I_RECARM', 1)\n");
        lua.push_str("reaper.SetMediaTrackInfo_Value(t, 'I_RECMON', 1)\n");
    }
    lua.push_str("reaper.UpdateArrange()\n");
    let armed = if arm { " (armed)" } else { "" };
    lua.push_str(&format!(
        "return \"Added '\" .. name .. \"' as track \" .. (idx + 1) .. \"{armed}\"\n"
    ));
    lua
}

/// Build Lua to delete the first track whose name matches `name`. Returns a summary
/// the daemon relays back (or a "not found" note if no track matched).
fn build_remove_track_lua(name: &str) -> String {
    // Match case-insensitively: tracks are stored Title Case but the user (or model)
    // usually types the name lowercase. `display` keeps their spelling for messages.
    let target = lua_escape(&name.to_lowercase());
    let display = lua_escape(name);
    let mut lua = String::new();
    lua.push_str(&format!("local target = '{target}'\n"));
    lua.push_str(&format!("local display = '{display}'\n"));
    lua.push_str("local removed = nil\n");
    lua.push_str("local rname = nil\n");
    // Collect every track name so a miss can report what DOES exist — lets the model
    // (which often guesses a default like 'Track 1') self-correct on the next turn
    // instead of looping on the same wrong name, and makes "delete all tracks" work.
    lua.push_str("local names = {}\n");
    lua.push_str("for i = 0, reaper.CountTracks(0) - 1 do\n");
    lua.push_str("  local tr = reaper.GetTrack(0, i)\n");
    lua.push_str("  local _, nm = reaper.GetSetMediaTrackInfo_String(tr, 'P_NAME', '', false)\n");
    lua.push_str("  if nm:lower() == target then reaper.DeleteTrack(tr); removed = i + 1; rname = nm; break end\n");
    lua.push_str("  local label = nm\n");
    lua.push_str("  if label == '' then label = 'Track ' .. (i + 1) end\n");
    lua.push_str("  names[#names + 1] = label\n");
    lua.push_str("end\n");
    lua.push_str("reaper.UpdateArrange()\n");
    lua.push_str("if removed then return \"Removed track '\" .. rname .. \"' (was track \" .. removed .. \")\"\n");
    lua.push_str("elseif #names == 0 then return \"No track named '\" .. display .. \"' found (project has no tracks)\"\n");
    lua.push_str("else return \"No track named '\" .. display .. \"' found. Tracks: \" .. table.concat(names, \", \") end\n");
    lua
}

/// Build Lua to add an FX (plugin) by name to the first track whose name matches
/// `track` (case-insensitively, like `build_remove_track_lua`). The plugin is matched
/// by the bare product name REAPER knows it as — NO `VST:`/`VST3:` prefix is forced,
/// because the available format varies per machine (the first plugin we tested, Valhalla
/// Supermassive, surfaced only as VST2 on OmniLink1). `TrackFX_AddByName` returns -1 when
/// REAPER can't resolve the name, which we relay as a clear "not installed/scanned" note
/// rather than a silent no-op. Returns a summary the daemon relays back.
fn build_add_fx_lua(track: &str, fx: &str) -> String {
    let target = lua_escape(&track.to_lowercase());
    let display = lua_escape(track);
    let fx_name = lua_escape(fx);
    let mut lua = String::new();
    lua.push_str(&format!("local target = '{target}'\n"));
    lua.push_str(&format!("local display = '{display}'\n"));
    lua.push_str(&format!("local fx = '{fx_name}'\n"));
    lua.push_str("local tr = nil\n");
    lua.push_str("for i = 0, reaper.CountTracks(0) - 1 do\n");
    lua.push_str("  local t = reaper.GetTrack(0, i)\n");
    lua.push_str("  local _, nm = reaper.GetSetMediaTrackInfo_String(t, 'P_NAME', '', false)\n");
    lua.push_str("  if nm:lower() == target then tr = t; break end\n");
    lua.push_str("end\n");
    lua.push_str("if not tr then return \"No track named '\" .. display .. \"' found\" end\n");
    // instantiate = -1 → always add a new instance; returns the new FX slot index, or -1
    // if REAPER can't find/scan a plugin matching the name.
    lua.push_str("local idx = reaper.TrackFX_AddByName(tr, fx, false, -1)\n");
    lua.push_str("if idx < 0 then return \"FX '\" .. fx .. \"' not found — is it installed and scanned in REAPER?\" end\n");
    // Read back the name REAPER actually assigned (carries the format prefix, e.g. 'VST: ...').
    lua.push_str("local _, real = reaper.TrackFX_GetFXName(tr, idx, '')\n");
    lua.push_str("local _, tn = reaper.GetSetMediaTrackInfo_String(tr, 'P_NAME', '', false)\n");
    lua.push_str("reaper.UpdateArrange()\n");
    lua.push_str(
        "return \"Added '\" .. real .. \"' to track '\" .. tn .. \"' (FX slot \" .. (idx + 1) .. \")\"\n",
    );
    lua
}

/// Build Lua that lists every FX in a named track's chain as `name + 1-based slot`,
/// flagging bypassed slots. The discovery primitive that lets the coordinator (never
/// the LLM) resolve a plugin name → chain index, guarding against FX index drift
/// (quirk #1): callers see what's actually loaded before touching params or presets.
fn build_list_fx_lua(track: &str) -> String {
    let target = lua_escape(&track.to_lowercase());
    let display = lua_escape(track);
    let mut lua = String::new();
    lua.push_str(&format!("local target = '{target}'\n"));
    lua.push_str(&format!("local display = '{display}'\n"));
    lua.push_str("local tr = nil\n");
    lua.push_str("for i = 0, reaper.CountTracks(0) - 1 do\n");
    lua.push_str("  local t = reaper.GetTrack(0, i)\n");
    lua.push_str("  local _, nm = reaper.GetSetMediaTrackInfo_String(t, 'P_NAME', '', false)\n");
    lua.push_str("  if nm:lower() == target then tr = t; break end\n");
    lua.push_str("end\n");
    lua.push_str("if not tr then return \"No track named '\" .. display .. \"' found\" end\n");
    lua.push_str("local _, tn = reaper.GetSetMediaTrackInfo_String(tr, 'P_NAME', '', false)\n");
    lua.push_str("local n = reaper.TrackFX_GetCount(tr)\n");
    lua.push_str("if n == 0 then return \"Track '\" .. tn .. \"' has no FX\" end\n");
    lua.push_str("local lines = { \"FX on track '\" .. tn .. \"':\" }\n");
    lua.push_str("for i = 0, n - 1 do\n");
    lua.push_str("  local _, fxname = reaper.TrackFX_GetFXName(tr, i, '')\n");
    lua.push_str("  local on = reaper.TrackFX_GetEnabled(tr, i)\n");
    lua.push_str(
        "  table.insert(lines, '  ' .. (i + 1) .. '. ' .. fxname .. (on and '' or ' [bypassed]'))\n",
    );
    lua.push_str("end\n");
    lua.push_str("return table.concat(lines, '\\n')\n");
    lua
}

/// Build Lua that lists every parameter of a named FX on a named track: index, name,
/// formatted value (e.g. '12.0 dB') and the raw 0–1 normalised value. The FX is
/// resolved by **name match** against the live chain (quirk #1 — never trust a raw
/// index from the model); the bare product name is matched as a substring so a
/// format-prefixed real name ('VST: ValhallaSupermassive …') still resolves (quirk #2).
/// A 0-param result is reported rather than shown empty, since heavy plugins build
/// their param map lazily on UI init (quirk #3). This is the discovery step that maps
/// param names↔indices, and reveals whether a plugin's "modes" are params or presets.
fn build_list_fx_params_lua(track: &str, fx: &str) -> String {
    let target = lua_escape(&track.to_lowercase());
    let display = lua_escape(track);
    let fx_lc = lua_escape(&fx.to_lowercase());
    let fx_disp = lua_escape(fx);
    let mut lua = String::new();
    lua.push_str(&format!("local target = '{target}'\n"));
    lua.push_str(&format!("local display = '{display}'\n"));
    lua.push_str(&format!("local fx = '{fx_lc}'\n"));
    lua.push_str(&format!("local fxdisplay = '{fx_disp}'\n"));
    lua.push_str("local tr = nil\n");
    lua.push_str("for i = 0, reaper.CountTracks(0) - 1 do\n");
    lua.push_str("  local t = reaper.GetTrack(0, i)\n");
    lua.push_str("  local _, nm = reaper.GetSetMediaTrackInfo_String(t, 'P_NAME', '', false)\n");
    lua.push_str("  if nm:lower() == target then tr = t; break end\n");
    lua.push_str("end\n");
    lua.push_str("if not tr then return \"No track named '\" .. display .. \"' found\" end\n");
    // Resolve the FX by matching the bare name as a plain substring of the real name.
    lua.push_str("local fxidx = -1\n");
    lua.push_str("local realname = ''\n");
    lua.push_str("for i = 0, reaper.TrackFX_GetCount(tr) - 1 do\n");
    lua.push_str("  local _, nm = reaper.TrackFX_GetFXName(tr, i, '')\n");
    lua.push_str("  if nm:lower():find(fx, 1, true) then fxidx = i; realname = nm; break end\n");
    lua.push_str("end\n");
    lua.push_str(
        "if fxidx < 0 then return \"No FX matching '\" .. fxdisplay .. \"' on track '\" .. display .. \"'\" end\n",
    );
    lua.push_str("local np = reaper.TrackFX_GetNumParams(tr, fxidx)\n");
    lua.push_str(
        "if np == 0 then return \"FX '\" .. realname .. \"' reports 0 parameters (it may still be initialising)\" end\n",
    );
    lua.push_str("local lines = { realname .. ' parameters:' }\n");
    lua.push_str("for i = 0, np - 1 do\n");
    lua.push_str("  local _, pname = reaper.TrackFX_GetParamName(tr, fxidx, i, '')\n");
    lua.push_str("  local val = reaper.TrackFX_GetParam(tr, fxidx, i)\n");
    lua.push_str("  local _, fmt = reaper.TrackFX_GetFormattedParamValue(tr, fxidx, i, '')\n");
    lua.push_str("  if fmt == '' then fmt = string.format('%.3f', val) end\n");
    lua.push_str(
        "  table.insert(lines, '  ' .. i .. '. ' .. pname .. ' = ' .. fmt .. ' (' .. string.format('%.3f', val) .. ')')\n",
    );
    lua.push_str("end\n");
    lua.push_str("return table.concat(lines, '\\n')\n");
    lua
}

/// Build Lua to set the project tempo and/or time signature, returning a summary.
/// A tempo-only change uses `SetCurrentBPM` (no marker injected). A time-signature
/// change needs a tempo/time-sig marker at the project start; unspecified fields are
/// filled from the project's current values so "set the time signature to 3/4" keeps
/// the existing tempo (and vice versa).
fn build_set_tempo_lua(tempo: Option<f64>, ts_num: Option<u32>, ts_denom: Option<u32>) -> String {
    let mut lua = String::new();
    if ts_num.is_some() || ts_denom.is_some() {
        lua.push_str("local cur_num, cur_denom, cur_bpm = reaper.TimeMap_GetTimeSigAtTime(0, 0)\n");
        match tempo {
            Some(t) => lua.push_str(&format!("local bpm = {t}\n")),
            None => lua.push_str("local bpm = cur_bpm\n"),
        }
        match ts_num {
            Some(n) => lua.push_str(&format!("local num = {n}\n")),
            None => lua.push_str("local num = cur_num\n"),
        }
        match ts_denom {
            Some(d) => lua.push_str(&format!("local denom = {d}\n")),
            None => lua.push_str("local denom = cur_denom\n"),
        }
        // Reuse the marker already at position 0 if present, else insert a new one.
        lua.push_str("local idx = -1\n");
        lua.push_str("for i = 0, reaper.CountTempoTimeSigMarkers(0) - 1 do\n");
        lua.push_str("  local ok, timepos = reaper.GetTempoTimeSigMarker(0, i)\n");
        lua.push_str("  if ok and timepos == 0 then idx = i; break end\n");
        lua.push_str("end\n");
        lua.push_str("reaper.SetTempoTimeSigMarker(0, idx, 0, -1, -1, bpm, num, denom, false)\n");
        lua.push_str("reaper.UpdateTimeline()\n");
        lua.push_str(
            "return 'Set tempo to ' .. string.format('%.4g', bpm) .. ' BPM, ' .. math.floor(num) .. '/' .. math.floor(denom)\n",
        );
    } else {
        let t = tempo.unwrap_or(120.0);
        lua.push_str(&format!("reaper.SetCurrentBPM(0, {t}, true)\n"));
        lua.push_str("reaper.UpdateTimeline()\n");
        lua.push_str(&format!("return 'Set tempo to {t} BPM'\n"));
    }
    lua
}

/// Build Lua that returns a multi-line snapshot of the open project: name, tempo,
/// time signature, transport state, and the track list (number, name, armed). The
/// daemon relays the returned string verbatim; the agent reads the whole result file.
fn build_project_info_lua() -> String {
    r#"local lines = {}
local name = reaper.GetProjectName(0, '')
if name == '' then name = '(unsaved)' end
table.insert(lines, 'Project: ' .. name)
local num, denom, bpm = reaper.TimeMap_GetTimeSigAtTime(0, 0)
table.insert(lines, 'Tempo: ' .. string.format('%.4g', bpm) .. ' BPM, ' .. math.floor(num) .. '/' .. math.floor(denom))
local p = reaper.GetPlayState()
local ps = 'stopped'
if p & 4 ~= 0 then ps = 'recording' elseif p & 1 ~= 0 then ps = 'playing' elseif p & 2 ~= 0 then ps = 'paused' end
table.insert(lines, 'Transport: ' .. ps)
local n = reaper.CountTracks(0)
table.insert(lines, 'Tracks: ' .. n)
for i = 0, n - 1 do
  local tr = reaper.GetTrack(0, i)
  local _, nm = reaper.GetSetMediaTrackInfo_String(tr, 'P_NAME', '', false)
  if nm == '' then nm = '(unnamed)' end
  local armed = reaper.GetMediaTrackInfo_Value(tr, 'I_RECARM') == 1
  table.insert(lines, '  ' .. (i + 1) .. '. ' .. nm .. (armed and ' [armed]' or ''))
end
return table.concat(lines, '\n')
"#
    .to_string()
}

fn extract_color_from_text(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    // Multi-word colours checked before single words to avoid false matches.
    let colors: &[&str] = &[
        "sky blue",
        "light blue",
        "light green",
        "red",
        "orange",
        "yellow",
        "green",
        "cyan",
        "teal",
        "blue",
        "violet",
        "purple",
        "indigo",
        "pink",
        "magenta",
        "white",
        "lime",
    ];
    colors.iter().copied().find(|c| lower.contains(c))
}

fn named_color_to_xy(name: &str) -> Option<(f32, f32)> {
    match name.to_lowercase().trim() {
        "red" => Some((0.675, 0.322)),
        "orange" => Some((0.600, 0.380)),
        "yellow" => Some((0.500, 0.470)),
        "green" => Some((0.409, 0.518)),
        "cyan" | "teal" => Some((0.225, 0.330)),
        "blue" => Some((0.167, 0.040)),
        "violet" | "purple" | "indigo" => Some((0.200, 0.100)),
        "pink" | "magenta" => Some((0.430, 0.280)),
        "white" => Some((0.313, 0.329)),
        "sky blue" | "sky_blue" => Some((0.240, 0.240)),
        "light blue" | "light_blue" => Some((0.220, 0.180)),
        "light green" | "light_green" | "lime" => Some((0.380, 0.500)),
        _ => None,
    }
}

fn build_light_command(
    request_id: &str,
    args: &serde_json::Value,
) -> Option<Vec<LightCommandRequest>> {
    let action_str = args.get("action").and_then(|v| v.as_str()).unwrap_or("on");
    let value = args.get("value").and_then(|v| v.as_f64());

    let command = match action_str {
        "off" => LightAction::Off,
        "toggle" => LightAction::Toggle,
        // Zigbee brightness range is 0–254; 255 is reserved by the spec.
        "brightness" => LightAction::Brightness(value.unwrap_or(128.0).clamp(0.0, 254.0) as u8),
        "color_temp" => {
            // Convert Kelvin → mireds (1_000_000 / K). Guard the denominator
            // against zero/negative input (which would yield inf/garbage on the
            // u16 cast) and clamp to the Zigbee mired range (153≈6500K … 500≈2000K).
            let kelvin = value.unwrap_or(4000.0).max(1.0);
            let mireds = (1_000_000.0 / kelvin).clamp(153.0, 500.0) as u16;
            LightAction::ColorTemp(mireds)
        }
        "color" => {
            let x = args.get("cx").and_then(|v| v.as_f64());
            let y = args.get("cy").and_then(|v| v.as_f64());
            match (x, y) {
                (Some(x), Some(y)) => LightAction::ColorXY {
                    x: x.clamp(0.0, 1.0) as f32,
                    y: y.clamp(0.0, 1.0) as f32,
                },
                _ => {
                    let color_name = args.get("color_name").and_then(|v| v.as_str());
                    match color_name.and_then(named_color_to_xy) {
                        Some((x, y)) => LightAction::ColorXY { x, y },
                        None => {
                            tracing::warn!(
                                "color action missing cx/cy and unrecognised color_name — command dropped"
                            );
                            return None;
                        }
                    }
                }
            }
        }
        _ => LightAction::On,
    };

    let target = match args
        .get("target")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        Some(name) => LightTarget::Device(name.to_string()),
        None => LightTarget::Group("all".into()),
    };

    Some(vec![LightCommandRequest {
        request_id: request_id.to_string(),
        target,
        command,
    }])
}

/// Remove DeepSeek R1-style <think>…</think> chain-of-thought blocks from output.
fn strip_think_blocks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find("<think>") {
            None => {
                out.push_str(rest);
                break;
            }
            Some(start) => {
                out.push_str(&rest[..start]);
                rest = &rest[start + "<think>".len()..];
                if let Some(end) = rest.find("</think>") {
                    rest = &rest[end + "</think>".len()..];
                } else {
                    break; // unclosed tag — drop the rest
                }
            }
        }
    }
    out.trim().to_string()
}

fn is_tool_call(v: &serde_json::Value) -> bool {
    v.get("tool").is_some() && v["args"].is_object()
}

/// Lift arguments a small model nested under a stray `properties` key. Some models
/// (e.g. gemma3) mirror the JSON-Schema and emit `args: {properties: {name: …}}`
/// instead of `args: {name: …}`; without this the real args are a level too deep
/// and every field reads as missing. Existing top-level keys win over nested ones.
fn normalize_tool_args(mut call: serde_json::Value) -> serde_json::Value {
    let nested = call
        .get("args")
        .and_then(|a| a.get("properties"))
        .and_then(|p| p.as_object())
        .cloned();
    if let Some(nested) = nested
        && let Some(args) = call.get_mut("args").and_then(|a| a.as_object_mut())
    {
        for (k, v) in nested {
            args.entry(k).or_insert(v);
        }
    }
    call
}

/// Collect tool calls a value yields: a bare object is one call; an array is
/// flattened. Non-tool-call values contribute nothing.
fn collect_tool_calls(v: serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match v {
        serde_json::Value::Array(arr) => {
            for el in arr {
                if is_tool_call(&el) {
                    out.push(normalize_tool_args(el));
                }
            }
        }
        other if is_tool_call(&other) => out.push(normalize_tool_args(other)),
        _ => {}
    }
}

pub fn try_parse_tool_calls(output: &str) -> Option<Vec<serde_json::Value>> {
    // Small models often wrap each tool call in its own ```json … ``` block and
    // may emit several per reply (or several bare objects back-to-back). Strip all
    // fence markers, then read consecutive JSON values rather than a single one.
    let cleaned = output.replace("```json", " ").replace("```", " ");
    let cleaned = cleaned.trim();

    let mut calls: Vec<serde_json::Value> = Vec::new();
    let mut saw_value = false;
    for v in serde_json::Deserializer::from_str(cleaned).into_iter::<serde_json::Value>() {
        let Ok(v) = v else { break }; // stop at first non-JSON / trailing junk
        saw_value = true;
        collect_tool_calls(v, &mut calls);
    }

    // No complete value parsed → try repairing a single object missing its closing
    // brace (a common small-model truncation).
    if !saw_value && let Ok(v) = serde_json::from_str::<serde_json::Value>(&format!("{cleaned}}}"))
    {
        collect_tool_calls(v, &mut calls);
    }

    if calls.is_empty() { None } else { Some(calls) }
}

fn tool_schemas_for_feature(feature: &str) -> Vec<serde_json::Value> {
    match feature {
        "lighting" => vec![
            serde_json::json!({
                "name": "light_command",
                "description": "Turn lights on/off, set brightness, colour temperature, or colour",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target": {
                            "type": "string",
                            "description": "Room or device name, e.g. 'living_room'"
                        },
                        "action": {
                            "type": "string",
                            "enum": ["on", "off", "toggle", "brightness", "color_temp", "color"],
                            "description": "Use 'color' for any named colour — set color_name (e.g. \"blue\") OR cx+cy. Use 'color_temp' only for white-light warmth (warm, cool, daylight). 'light blue', 'pale green' etc. are colours — use 'color'."
                        },
                        "value": {
                            "type": "number",
                            "description": "Brightness 0–255 or colour temp in Kelvin (for brightness/color_temp actions)"
                        },
                        "color_name": {
                            "type": "string",
                            "description": "Named colour for action=color. Supported: red, orange, yellow, green, cyan, teal, blue, violet, purple, indigo, pink, magenta, white, lime, sky blue, light blue, light green. Prefer this over cx+cy."
                        },
                        "cx": {
                            "type": "number",
                            "description": "CIE 1931 x chromaticity (0.0–1.0). Only needed when color_name is not sufficient."
                        },
                        "cy": {
                            "type": "number",
                            "description": "CIE 1931 y chromaticity (0.0–1.0). Only needed when color_name is not sufficient."
                        }
                    },
                    "required": ["target", "action"]
                }
            }),
            serde_json::json!({
                "name": "scene_load",
                "description": "Load a named lighting scene (e.g. 'cozy', 'bright', 'movie')",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "scene": { "type": "string" },
                        "room": {
                            "type": "string",
                            "description": "Optional — omit to apply everywhere"
                        },
                        "transition_ms": { "type": "integer" }
                    },
                    "required": ["scene"]
                }
            }),
        ],
        "reaper" => vec![
            serde_json::json!({
                "name": "reaper_transport",
                "description": "Control REAPER DAW transport or project. Use to start/stop playback, recording, or manage projects.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["play", "stop", "pause", "record", "rewind", "new_project", "save"],
                            "description": "Transport or project action: play/stop/pause/record/rewind control playback; new_project creates a new empty project; save saves the current project"
                        }
                    },
                    "required": ["action"]
                }
            }),
            serde_json::json!({
                "name": "reaper_action",
                "description": "Run a REAPER action by numeric command ID or named string ID. Examples: '40075' = toggle repeat, '1007' = stop, '_SWS_ABOUT' for SWS extension actions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action_id": {
                            "type": "string",
                            "description": "REAPER action ID — numeric string ('40075') or named action string ('_SWS_ABOUT')"
                        }
                    },
                    "required": ["action_id"]
                }
            }),
            serde_json::json!({
                "name": "reaper_script",
                "description": "Execute Lua/ReaScript inside REAPER for automation not covered by reaper_transport/reaper_action: creating tracks, setting recording inputs, arming, naming, saving. CRITICAL RULES: (1) reaper.InsertTrackAtIndex(idx, true) returns NOTHING — never assign it; get the new track with reaper.GetTrack(0, idx) afterwards. (2) To name a track use reaper.GetSetMediaTrackInfo_String (there is NO SetMediaTrackInfo_String). (3) I_RECINPUT 0=input1, 1=input2 (mono). To add one armed track at index N, follow this exact template:\nreaper.InsertTrackAtIndex(N, true)\nlocal t = reaper.GetTrack(0, N)\nreaper.GetSetMediaTrackInfo_String(t, 'P_NAME', 'NAME', true)\nreaper.SetMediaTrackInfo_Value(t, 'I_RECINPUT', INPUT)\nreaper.SetMediaTrackInfo_Value(t, 'I_RECARM', 1)\nreaper.SetMediaTrackInfo_Value(t, 'I_RECMON', 1)\nRepeat with incrementing N for more tracks. Save with reaper.Main_SaveProjectEx(0, 'C:\\\\path\\\\name.rpp', 0). Always finish with reaper.UpdateArrange().",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "code": {
                            "type": "string",
                            "description": "Lua code to execute in REAPER's scripting environment"
                        }
                    },
                    "required": ["code"]
                }
            }),
            serde_json::json!({
                "name": "reaper_add_track",
                "description": "Add a new named track to the open REAPER project. ALWAYS use this for any 'add/create a track called X' request instead of reaper_script — it cannot produce a blank-named track. The 'name' must be the label only, WITHOUT the word 'track' (for 'add another vocal track' use name 'vocal', not 'vocal track'). The name is auto-formatted to Title Case and auto-numbered if it already exists ('vocal' → 'Vocal', then 'Vocal 2'). The new track is armed for recording by default and all other tracks are disarmed, so only the new one is record-ready. Optionally set a mono record input.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Track name, e.g. 'guitar', 'lead vocal'"
                        },
                        "rec_input": {
                            "type": "integer",
                            "description": "Optional mono record input index (0 = input 1, 1 = input 2). Omit if not recording."
                        },
                        "arm": {
                            "type": "boolean",
                            "description": "Optional — defaults to true: arms the new track (with input monitoring) and disarms all other tracks. Set false only to leave existing record-arm states untouched and add the track unarmed."
                        }
                    },
                    "required": ["name"]
                }
            }),
            serde_json::json!({
                "name": "reaper_remove_track",
                "description": "Delete a track from the open REAPER project by its name (the first track with that exact name). Use for 'remove/delete the track called X'.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Exact name of the track to delete, e.g. 'Guitar'"
                        }
                    },
                    "required": ["name"]
                }
            }),
            serde_json::json!({
                "name": "reaper_set_tempo",
                "description": "Set the REAPER project tempo (BPM) and/or time signature. Use for 'set tempo to 120', 'change the BPM to 90', 'set the time signature to 3/4', 'make it 6/8 at 100 bpm'. Provide tempo, or both ts_num and ts_denom, or all three.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "tempo": {
                            "type": "number",
                            "description": "Tempo in beats per minute, e.g. 120. Omit to keep the current tempo when changing only the time signature."
                        },
                        "ts_num": {
                            "type": "integer",
                            "description": "Time signature numerator (top number), e.g. 3 for 3/4. Provide together with ts_denom."
                        },
                        "ts_denom": {
                            "type": "integer",
                            "description": "Time signature denominator (bottom number), e.g. 4 for 3/4. Provide together with ts_num."
                        }
                    }
                }
            }),
            serde_json::json!({
                "name": "reaper_get_project",
                "description": "Read the current REAPER project state: project name, tempo, time signature, transport (playing/stopped/recording), and the track list (number, name, armed status). Use to answer questions like 'what tracks are in the project?', \"what's the tempo?\", 'how many tracks do I have?'.",
                "parameters": {
                    "type": "object",
                    "properties": {}
                }
            }),
            serde_json::json!({
                "name": "reaper_add_fx",
                "description": "Add an audio effect / plugin (FX) to a named track in REAPER. Use for 'add reverb to the vocal track', 'put ValhallaSupermassive on the guitar', 'add ReaEQ to drums'. 'track' is the track's name; 'fx' is the plugin name as REAPER lists it in the FX browser — pass the bare product name (e.g. 'ValhallaSupermassive', 'ReaComp', 'ReaEQ') WITHOUT a 'VST:'/'VST3:' prefix, since the available format differs per machine.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "track": {
                            "type": "string",
                            "description": "Name of the track to add the FX to, e.g. 'vocal', 'guitar'"
                        },
                        "fx": {
                            "type": "string",
                            "description": "Plugin name as shown in REAPER's FX browser, e.g. 'ValhallaSupermassive', 'ReaComp', 'ReaEQ'. No 'VST:'/'VST3:' prefix."
                        }
                    },
                    "required": ["track", "fx"]
                }
            }),
            serde_json::json!({
                "name": "reaper_list_fx",
                "description": "List the audio effects / plugins (FX) currently loaded on a named track in REAPER, in chain order, flagging any that are bypassed. Use for 'what FX are on the vocal track?', 'list the plugins on the guitar', 'what reverb is on drums?'. 'track' is the track's name.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "track": {
                            "type": "string",
                            "description": "Name of the track whose FX chain to list, e.g. 'vocal', 'guitar'"
                        }
                    },
                    "required": ["track"]
                }
            }),
            serde_json::json!({
                "name": "reaper_list_fx_params",
                "description": "List the parameters (controllable knobs) of one plugin on a named track, with each parameter's name, current value, and index. Use to discover what can be tweaked before changing it, e.g. 'what can I adjust on ValhallaSupermassive?', 'show the parameters of the reverb on the vocal'. 'track' is the track's name; 'fx' is the plugin name as REAPER lists it (bare product name, no 'VST:'/'VST3:' prefix).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "track": {
                            "type": "string",
                            "description": "Name of the track the plugin is on, e.g. 'vocal', 'guitar'"
                        },
                        "fx": {
                            "type": "string",
                            "description": "Plugin name as shown in REAPER's FX browser, e.g. 'ValhallaSupermassive', 'ReaEQ'. No 'VST:'/'VST3:' prefix."
                        }
                    },
                    "required": ["track", "fx"]
                }
            }),
        ],
        _ => vec![],
    }
}

pub fn build_system_prompt(schemas: &[serde_json::Value]) -> String {
    if schemas.is_empty() {
        return "You are a helpful assistant. Answer the user's question directly.".into();
    }

    let schema_json = serde_json::to_string(schemas).unwrap_or_default();

    format!(
        r#"You are a helpful smart home assistant embedded in ai-mesh. You have direct control of and live state for all listed devices.

To control one device, reply with ONLY this JSON (no extra text):
{{"tool": "<name>", "args": {{ ... }}}}

To control multiple devices in one request, reply with ONLY a JSON array (no extra text):
[{{"tool": "<name>", "args": {{ ... }}}}, {{"tool": "<name>", "args": {{ ... }}}}]

Available tools:
{schema_json}

Rules:
- The "target" field must be an exact device or group name from the known list. Never invent a target name.
- When the user names a room (e.g. "kitchen lights", "the bedroom"), find devices tagged [RoomName]. If no group for that room exists, pick the first online device in that room.
- If the user says "one of", "just one", or "a single", always pick the FIRST online device listed for that room.
- If the user says "all" or "everything", use a group if one exists; otherwise emit one array element per online device in that room.
- For compound requests (e.g. "dim warm light"), emit one array element per command — brightness and colour/temperature are separate tool calls.
- Never issue a command to a device shown as [OFFLINE — not responding].
- For ANY question about state, count, names, or available scenes — answer directly in plain text from the device and scene lists. Count devices sharing a [RoomName] tag to answer "how many". do NOT output JSON for these questions.
- Only output JSON when the user is explicitly asking you to CHANGE or CONTROL something."#
    )
}

pub fn build_device_context(
    known_devices: &[String],
    known_groups: &[String],
    device_states: &[LightStateReport],
    device_room_map: &HashMap<String, String>,
    scene_names: &[String],
) -> String {
    let device_section = if known_devices.is_empty() && known_groups.is_empty() {
        String::new()
    } else {
        let mut lines = Vec::new();
        if !known_devices.is_empty() {
            lines.push("Known devices:".to_string());
            for name in known_devices {
                let room_tag = device_room_map
                    .get(name)
                    .map(|r| format!(" [{}]", r))
                    .unwrap_or_default();
                let state = device_states.iter().find(|s| &s.device_id == name);
                let status = match state {
                    None => "  (no state yet)".to_string(),
                    Some(s) if !s.online => "  [OFFLINE — not responding]".to_string(),
                    Some(s) => {
                        let mut parts = Vec::new();
                        parts.push(if s.on { "on" } else { "off" }.to_string());
                        if let Some(b) = s.brightness {
                            parts.push(format!("{}% brightness", (b as u32 * 100 / 254).min(100)));
                        }
                        if let Some(ct) = s.color_temp
                            && ct > 0
                        {
                            parts.push(format!("{} K", 1_000_000u32 / ct as u32));
                        }
                        format!("  (online, {})", parts.join(", "))
                    }
                };
                lines.push(format!("  - {name}{room_tag}{status}"));
            }
        }
        if !known_groups.is_empty() {
            lines.push(format!(
                "Known groups (control all members at once): {}",
                known_groups.join(", ")
            ));
        }
        lines.join("\n")
    };

    let scene_section = if scene_names.is_empty() {
        String::new()
    } else {
        format!("Available scenes: {}", scene_names.join(", "))
    };

    match (device_section.is_empty(), scene_section.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("{device_section}\n\n"),
        (true, false) => format!("{scene_section}\n\n"),
        (false, false) => format!("{device_section}\n\n{scene_section}\n\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_system_prompt_no_schemas_returns_plain() {
        let p = build_system_prompt(&[]);
        assert!(!p.contains("tool"));
        assert!(p.contains("helpful assistant"));
    }

    #[test]
    fn build_system_prompt_with_schemas_includes_tool_section() {
        let schemas = tool_schemas_for_feature("lighting");
        let p = build_system_prompt(&schemas);
        assert!(p.contains("light_command"));
        assert!(p.contains("scene_load"));
        assert!(p.contains(r#"{"tool":"#));
    }

    #[test]
    fn build_system_prompt_no_devices_omits_device_section() {
        let schemas = tool_schemas_for_feature("lighting");
        let p = build_system_prompt(&schemas);
        assert!(!p.contains("Known devices"));
        assert!(!p.contains("Known groups"));
    }

    #[test]
    fn build_system_prompt_does_not_contain_no_think() {
        // /no_think is applied per model family in llama.rs, not baked into the
        // static system prompt. If it appears here the KV-cache benefit is lost
        // for non-Qwen models and the token is sent incorrectly to phi4/gemma/etc.
        let schemas = tool_schemas_for_feature("lighting");
        let p = build_system_prompt(&schemas);
        assert!(
            !p.contains("/no_think"),
            "system prompt must not contain /no_think"
        );
        assert!(
            !p.contains("/think"),
            "system prompt must not contain /think"
        );
    }

    #[test]
    fn build_device_context_injects_known_devices_and_groups() {
        let devices = vec!["test_bulb".to_string(), "desk_lamp".to_string()];
        let groups = vec!["all".to_string()];
        let ctx = build_device_context(&devices, &groups, &[], &HashMap::new(), &[]);
        assert!(ctx.contains("test_bulb"));
        assert!(ctx.contains("desk_lamp"));
        assert!(ctx.contains("all"));
        assert!(ctx.contains("Known devices"));
        assert!(ctx.contains("Known groups"));
    }

    #[test]
    fn build_device_context_empty_returns_empty() {
        let ctx = build_device_context(&[], &[], &[], &HashMap::new(), &[]);
        assert!(ctx.is_empty());
    }

    #[test]
    fn try_parse_tool_calls_valid_json() {
        let raw = r#"{"tool":"light_command","args":{"target":"living_room","action":"on"}}"#;
        let result = try_parse_tool_calls(raw).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["tool"], "light_command");
        assert_eq!(result[0]["args"]["action"], "on");
    }

    #[test]
    fn try_parse_tool_calls_free_text_returns_none() {
        let raw = "The capital of France is Paris.";
        assert!(try_parse_tool_calls(raw).is_none());
    }

    #[test]
    fn try_parse_tool_calls_strips_code_fence() {
        let raw = "```json\n{\"tool\":\"scene_load\",\"args\":{\"scene\":\"cozy\"}}\n```";
        let result = try_parse_tool_calls(raw).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["tool"], "scene_load");
    }

    #[test]
    fn try_parse_tool_calls_multiple_fenced_blocks() {
        // Small models sometimes emit one ```json block per call instead of a
        // single array — both must execute (the Guitar/guitar2 regression).
        let raw = "```json\n{\"tool\":\"reaper_add_track\",\"args\":{\"name\":\"Guitar\"}}\n```\n\n```json\n{\"tool\":\"reaper_add_track\",\"args\":{\"name\":\"guitar2\"}}\n```";
        let result = try_parse_tool_calls(raw).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["args"]["name"], "Guitar");
        assert_eq!(result[1]["args"]["name"], "guitar2");
    }

    #[test]
    fn try_parse_tool_calls_consecutive_bare_objects() {
        let raw = r#"{"tool":"reaper_add_track","args":{"name":"a"}} {"tool":"reaper_add_track","args":{"name":"b"}}"#;
        let result = try_parse_tool_calls(raw).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn try_parse_tool_calls_unwraps_schema_properties_wrapper() {
        // gemma3 mirrors the schema and nests args under "properties"; the real
        // fields must be lifted so name/arm aren't read as missing.
        let raw = "```json\n[{\"tool\": \"reaper_add_track\", \"args\": {\"properties\": {\"name\": \"vocal\", \"arm\": true}}}]\n```";
        let result = try_parse_tool_calls(raw).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["args"]["name"], "vocal");
        assert_eq!(result[0]["args"]["arm"], true);
    }

    #[test]
    fn try_parse_tool_calls_missing_args_returns_none() {
        let raw = r#"{"tool":"light_command"}"#;
        assert!(try_parse_tool_calls(raw).is_none());
    }

    #[test]
    fn try_parse_tool_calls_truncated_json_repaired() {
        let raw = r#"{"tool":"light_command","args":{"target":"test_bulb","action":"off"}"#;
        let result = try_parse_tool_calls(raw).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["tool"], "light_command");
        assert_eq!(result[0]["args"]["action"], "off");
    }

    #[test]
    fn try_parse_tool_calls_json_without_tool_field_returns_none() {
        let raw = r#"{"action":"on","target":"living_room"}"#;
        assert!(try_parse_tool_calls(raw).is_none());
    }

    #[test]
    fn try_parse_tool_calls_array_form() {
        let raw = r#"[{"tool":"light_command","args":{"target":"kitchen","action":"brightness","value":30}},{"tool":"light_command","args":{"target":"kitchen","action":"color","color_name":"warm"}}]"#;
        let result = try_parse_tool_calls(raw).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["args"]["action"], "brightness");
        assert_eq!(result[1]["args"]["action"], "color");
    }

    #[test]
    fn try_parse_tool_calls_array_with_invalid_element_skipped() {
        let raw = r#"[{"tool":"light_command","args":{"target":"kitchen","action":"on"}},{"not_a_tool":"x"}]"#;
        let result = try_parse_tool_calls(raw).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["tool"], "light_command");
    }

    #[test]
    fn tool_schemas_for_lighting_returns_two() {
        let schemas = tool_schemas_for_feature("lighting");
        assert_eq!(schemas.len(), 2);
        assert_eq!(schemas[0]["name"], "light_command");
        assert_eq!(schemas[1]["name"], "scene_load");
    }

    #[test]
    fn tool_schemas_for_unknown_feature_returns_empty() {
        assert!(tool_schemas_for_feature("nonexistent").is_empty());
    }

    #[test]
    fn build_light_command_on() {
        let args = serde_json::json!({"target": "kitchen", "action": "on"});
        let cmd = build_light_command("r1", &args).unwrap().remove(0);
        assert!(matches!(cmd.command, LightAction::On));
        assert_eq!(cmd.request_id, "r1");
        assert!(matches!(cmd.target, LightTarget::Device(ref n) if n == "kitchen"));
    }

    #[test]
    fn build_light_command_no_target_falls_back_to_group() {
        let args = serde_json::json!({"action": "on"});
        let cmd = build_light_command("r0", &args).unwrap().remove(0);
        assert!(matches!(cmd.target, LightTarget::Group(ref g) if g == "all"));
    }

    #[test]
    fn build_light_command_off() {
        let args = serde_json::json!({"target": "bedroom", "action": "off"});
        let cmd = build_light_command("r2", &args).unwrap().remove(0);
        assert!(matches!(cmd.command, LightAction::Off));
    }

    #[test]
    fn build_light_command_brightness() {
        let args = serde_json::json!({"target": "lounge", "action": "brightness", "value": 200});
        let cmd = build_light_command("r3", &args).unwrap().remove(0);
        assert!(matches!(cmd.command, LightAction::Brightness(200)));
    }

    #[test]
    fn build_light_command_brightness_clamped_to_254() {
        let args = serde_json::json!({"target": "lounge", "action": "brightness", "value": 255});
        let cmd = build_light_command("r3b", &args).unwrap().remove(0);
        assert!(matches!(cmd.command, LightAction::Brightness(254)));
    }

    #[test]
    fn build_light_command_color_temp_kelvin_to_mireds() {
        // 4000K → 250 mireds
        let args = serde_json::json!({"target": "office", "action": "color_temp", "value": 4000});
        let cmd = build_light_command("r4", &args).unwrap().remove(0);
        assert!(matches!(cmd.command, LightAction::ColorTemp(250)));
    }

    #[test]
    fn build_light_command_unknown_action_defaults_to_on() {
        let args = serde_json::json!({"target": "hall", "action": "sparkle"});
        let cmd = build_light_command("r5", &args).unwrap().remove(0);
        assert!(matches!(cmd.command, LightAction::On));
    }

    #[test]
    fn build_light_command_toggle() {
        let args = serde_json::json!({"target": "lounge", "action": "toggle"});
        let cmd = build_light_command("r6", &args).unwrap().remove(0);
        assert!(matches!(cmd.command, LightAction::Toggle));
    }

    #[test]
    fn build_light_command_empty_target_falls_back_to_group() {
        let args = serde_json::json!({"target": "", "action": "on"});
        let cmd = build_light_command("r7", &args).unwrap().remove(0);
        assert!(matches!(cmd.target, LightTarget::Group(ref g) if g == "all"));
    }

    #[test]
    fn build_light_command_color_with_xy() {
        let args =
            serde_json::json!({"target": "test_bulb", "action": "color", "cx": 0.675, "cy": 0.322});
        let cmd = build_light_command("r8", &args).unwrap().remove(0);
        assert!(
            matches!(cmd.command, LightAction::ColorXY { x, y } if (x - 0.675).abs() < 1e-4 && (y - 0.322).abs() < 1e-4)
        );
    }

    #[test]
    fn build_light_command_color_clamps_out_of_range_xy() {
        let args =
            serde_json::json!({"target": "test_bulb", "action": "color", "cx": 1.5, "cy": -0.1});
        let cmd = build_light_command("r9", &args).unwrap().remove(0);
        assert!(matches!(cmd.command, LightAction::ColorXY { x, y } if x == 1.0 && y == 0.0));
    }

    #[test]
    fn build_light_command_color_brightness_field_ignored() {
        // brightness alongside color is no longer honoured — one command only
        let args = serde_json::json!({"target": "test_bulb", "action": "color", "cx": 0.167, "cy": 0.04, "brightness": 60});
        let cmds = build_light_command("r12", &args).unwrap();
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0].command, LightAction::ColorXY { .. }));
    }

    #[test]
    fn build_light_command_color_missing_xy_returns_none() {
        let args = serde_json::json!({"target": "test_bulb", "action": "color"});
        assert!(build_light_command("r10", &args).is_none());
    }

    #[test]
    fn build_light_command_color_missing_cy_returns_none() {
        let args = serde_json::json!({"target": "test_bulb", "action": "color", "cx": 0.5});
        assert!(build_light_command("r11", &args).is_none());
    }

    #[test]
    fn build_device_context_devices_only_no_groups() {
        let devices = vec!["test_bulb".to_string()];
        let ctx = build_device_context(&devices, &[], &[], &HashMap::new(), &[]);
        assert!(ctx.contains("Known devices"));
        assert!(!ctx.contains("Known groups"));
    }

    #[test]
    fn build_device_context_groups_only_no_devices() {
        let groups = vec!["all".to_string()];
        let ctx = build_device_context(&[], &groups, &[], &HashMap::new(), &[]);
        assert!(!ctx.contains("Known devices"));
        assert!(ctx.contains("Known groups"));
    }

    #[test]
    fn build_device_context_injects_device_list_into_target_description() {
        let devices = vec!["test_bulb".to_string()];
        let groups = vec!["all".to_string()];
        let ctx = build_device_context(&devices, &groups, &[], &HashMap::new(), &[]);
        assert!(ctx.contains("test_bulb"));
        assert!(ctx.contains("all"));
    }

    #[test]
    fn build_system_prompt_forbids_special_tags() {
        let schemas = tool_schemas_for_feature("lighting");
        let p = build_system_prompt(&schemas);
        assert!(p.contains("Only output JSON"));
        assert!(p.contains("do NOT output JSON"));
    }

    #[test]
    fn build_system_prompt_schema_is_compact_json() {
        let schemas = tool_schemas_for_feature("lighting");
        let p = build_system_prompt(&schemas);
        for schema in schemas {
            let s = serde_json::to_string(&schema).unwrap();
            assert!(!s.contains('\n'), "schema JSON must be compact");
            assert!(
                p.contains(&s),
                "system prompt must embed compact schema JSON"
            );
        }
    }

    #[test]
    fn lighting_schema_has_cx_cy_not_color_string() {
        let schemas = tool_schemas_for_feature("lighting");
        let light_cmd = schemas
            .iter()
            .find(|s| s["name"] == "light_command")
            .expect("light_command schema missing");
        let props = &light_cmd["parameters"]["properties"];
        assert!(props.get("cx").is_some(), "cx field missing from schema");
        assert!(props.get("cy").is_some(), "cy field missing from schema");
        assert!(
            props.get("color").is_none(),
            "old 'color' string field must not be present"
        );
        assert!(
            props.get("brightness").is_none(),
            "brightness must not be a color-action field in schema"
        );
    }

    #[test]
    fn build_device_context_shows_device_state() {
        use shared::messages::LightStateReport;
        let devices = vec!["kitchen_light".to_string(), "hallway_light".to_string()];
        let states = vec![
            LightStateReport {
                node_id: "n1".into(),
                device_id: "kitchen_light".into(),
                on: true,
                brightness: Some(200),
                color_xy: None,
                color_temp: Some(370),
                online: true,
            },
            LightStateReport {
                node_id: "n1".into(),
                device_id: "hallway_light".into(),
                on: false,
                brightness: None,
                color_xy: None,
                color_temp: None,
                online: false,
            },
        ];
        let ctx = build_device_context(&devices, &[], &states, &HashMap::new(), &[]);
        assert!(ctx.contains("kitchen_light"), "device name must appear");
        assert!(
            ctx.contains("online"),
            "online device must show online status"
        );
        assert!(ctx.contains("OFFLINE"), "offline device must show OFFLINE");
        assert!(
            ctx.contains("78%"),
            "brightness should be rendered as percent"
        );
    }

    #[test]
    fn extract_color_basic() {
        assert_eq!(extract_color_from_text("set it to blue"), Some("blue"));
        assert_eq!(extract_color_from_text("make it red please"), Some("red"));
        assert_eq!(extract_color_from_text("turn it on"), None);
    }

    #[test]
    fn extract_color_case_insensitive() {
        assert_eq!(extract_color_from_text("colour BLUE"), Some("blue"));
        assert_eq!(extract_color_from_text("Set to RED"), Some("red"));
    }

    #[test]
    fn extract_color_multi_word_beats_single() {
        // Multi-word colours must be checked before single words to avoid
        // "sky blue" matching as just "blue".
        assert_eq!(extract_color_from_text("sky blue please"), Some("sky blue"));
        assert_eq!(
            extract_color_from_text("set it to light blue"),
            Some("light blue")
        );
        assert_eq!(
            extract_color_from_text("light green colour"),
            Some("light green")
        );
    }

    #[test]
    fn build_light_command_color_name_blue() {
        let args =
            serde_json::json!({"target": "test_bulb", "action": "color", "color_name": "blue"});
        let cmd = build_light_command("r20", &args).unwrap().remove(0);
        assert!(
            matches!(cmd.command, LightAction::ColorXY { x, y } if (x - 0.167).abs() < 1e-3 && (y - 0.040).abs() < 1e-3)
        );
    }

    #[test]
    fn build_light_command_color_name_case_insensitive() {
        let args =
            serde_json::json!({"target": "test_bulb", "action": "color", "color_name": "Red"});
        let cmd = build_light_command("r21", &args).unwrap().remove(0);
        assert!(
            matches!(cmd.command, LightAction::ColorXY { x, y: _ } if (x - 0.675).abs() < 1e-3)
        );
    }

    #[test]
    fn build_light_command_color_name_unknown_returns_none() {
        let args = serde_json::json!({"target": "test_bulb", "action": "color", "color_name": "chartreuse"});
        assert!(build_light_command("r22", &args).is_none());
    }

    #[test]
    fn build_light_command_cx_cy_takes_precedence_over_color_name() {
        let args = serde_json::json!({"target": "test_bulb", "action": "color", "cx": 0.5, "cy": 0.4, "color_name": "blue"});
        let cmd = build_light_command("r23", &args).unwrap().remove(0);
        assert!(
            matches!(cmd.command, LightAction::ColorXY { x, y } if (x - 0.5).abs() < 1e-3 && (y - 0.4).abs() < 1e-3)
        );
    }

    #[test]
    fn device_is_offline_returns_true_for_offline_device() {
        use shared::messages::LightStateReport;
        let states = vec![
            LightStateReport {
                node_id: "n1".into(),
                device_id: "bulb_a".into(),
                on: false,
                brightness: None,
                color_xy: None,
                color_temp: None,
                online: false,
            },
            LightStateReport {
                node_id: "n1".into(),
                device_id: "bulb_b".into(),
                on: true,
                brightness: None,
                color_xy: None,
                color_temp: None,
                online: true,
            },
        ];
        assert!(device_is_offline("bulb_a", &states));
        assert!(!device_is_offline("bulb_b", &states));
    }

    #[test]
    fn device_is_offline_unknown_device_is_not_offline() {
        use shared::messages::LightStateReport;
        let states = vec![LightStateReport {
            node_id: "n1".into(),
            device_id: "bulb_a".into(),
            on: true,
            brightness: None,
            color_xy: None,
            color_temp: None,
            online: true,
        }];
        // Unknown target → not in states → treat as online (let it through;
        // the lighting node decides if it actually exists).
        assert!(!device_is_offline("unknown_bulb", &states));
    }

    fn light_record(target: &str, result: &str) -> ToolCallRecord {
        ToolCallRecord {
            tool: "light_command".into(),
            args: serde_json::json!({ "target": target }),
            result: Some(result.into()),
        }
    }

    #[test]
    fn offline_skip_result_round_trips() {
        // Producer and parser must stay in lockstep — this is the contract the
        // proactive summary depends on.
        let msg = offline_skip_result("0x00178801");
        assert_eq!(parse_offline_skip(&msg), Some("0x00178801"));
        assert_eq!(parse_offline_skip("ok"), None);
    }

    #[test]
    fn offline_skip_summary_none_when_nothing_skipped() {
        let records = vec![light_record("bulb_a", "ok")];
        assert!(offline_skip_summary(&records, &HashMap::new()).is_none());
    }

    #[test]
    fn offline_skip_summary_groups_a_single_room() {
        let mut rooms = HashMap::new();
        rooms.insert("bulb_a".to_string(), "Kitchen".to_string());
        rooms.insert("bulb_b".to_string(), "Kitchen".to_string());
        let records = vec![
            light_record("bulb_a", &offline_skip_result("bulb_a")),
            light_record("bulb_b", &offline_skip_result("bulb_b")),
        ];
        let summary = offline_skip_summary(&records, &rooms).unwrap();
        assert!(
            summary.contains("2 Kitchen lights are powered off"),
            "{summary}"
        );
        assert!(summary.contains("skipped them"), "{summary}");
    }

    #[test]
    fn offline_skip_summary_singular_and_only_counts_offline() {
        let mut rooms = HashMap::new();
        rooms.insert("bulb_a".to_string(), "Office".to_string());
        let records = vec![
            light_record("bulb_a", &offline_skip_result("bulb_a")),
            light_record("bulb_b", "ok"), // online — must not be counted
        ];
        let summary = offline_skip_summary(&records, &rooms).unwrap();
        assert!(
            summary.contains("1 Office light is powered off"),
            "{summary}"
        );
        assert!(summary.contains("skipped it"), "{summary}");
    }

    #[test]
    fn offline_skip_summary_mixed_rooms_lists_breakdown() {
        let mut rooms = HashMap::new();
        rooms.insert("bulb_a".to_string(), "Kitchen".to_string());
        rooms.insert("bulb_b".to_string(), "Hall".to_string());
        let records = vec![
            light_record("bulb_a", &offline_skip_result("bulb_a")),
            light_record("bulb_b", &offline_skip_result("bulb_b")),
            light_record("bulb_c", &offline_skip_result("bulb_c")), // no room
        ];
        let summary = offline_skip_summary(&records, &rooms).unwrap();
        assert!(summary.contains("3 lights are powered off"), "{summary}");
        assert!(summary.contains("1 in Kitchen"), "{summary}");
        assert!(summary.contains("1 in Hall"), "{summary}");
        assert!(summary.contains("1 with no room"), "{summary}");
    }

    #[test]
    fn add_track_lua_names_via_gettrack_not_insert_return() {
        let lua = build_add_track_lua("guitar", None, false);
        // The new track handle MUST come from GetTrack, never the (nil) return
        // of InsertTrackAtIndex — that was the original blank-name bug.
        assert!(lua.contains("local t = reaper.GetTrack(0, idx)"));
        assert!(lua.contains("local base = 'Guitar'")); // title-cased
        assert!(lua.contains("reaper.GetSetMediaTrackInfo_String(t, 'P_NAME', name, true)"));
        assert!(!lua.contains("= reaper.InsertTrackAtIndex"));
        // No input requested, not armed → those lines are absent.
        assert!(!lua.contains("I_RECINPUT"));
        assert!(!lua.contains("I_RECARM"));
        assert!(lua.contains("reaper.UpdateArrange()"));
    }

    #[test]
    fn add_track_lua_resolves_unique_name_and_returns_summary() {
        let lua = build_add_track_lua("guitar", None, true);
        // Dedup: scan existing names and append a suffix until unique.
        assert!(lua.contains("local function name_taken(n)"));
        assert!(lua.contains("while name_taken(name) do name = base .. ' ' .. suffix"));
        // Returns a human-readable summary using the *resolved* name + position.
        assert!(lua.contains(
            "return \"Added '\" .. name .. \"' as track \" .. (idx + 1) .. \" (armed)\""
        ));
    }

    #[test]
    fn add_track_lua_unarmed_summary_has_no_armed_suffix() {
        let lua = build_add_track_lua("scratch", None, false);
        assert!(lua.contains("return \"Added '\" .. name .. \"' as track \" .. (idx + 1) .. \"\""));
    }

    #[test]
    fn add_track_lua_input_and_arm() {
        let lua = build_add_track_lua("vox", Some(1), true);
        assert!(lua.contains("reaper.SetMediaTrackInfo_Value(t, 'I_RECINPUT', 1)"));
        assert!(lua.contains("reaper.SetMediaTrackInfo_Value(t, 'I_RECARM', 1)"));
        assert!(lua.contains("reaper.SetMediaTrackInfo_Value(t, 'I_RECMON', 1)"));
    }

    #[test]
    fn add_track_lua_arm_is_exclusive() {
        // Arming the new track must first disarm every other track so only the
        // freshly-added one is record-ready.
        let lua = build_add_track_lua("guitar", None, true);
        assert!(lua.contains("for i = 0, reaper.CountTracks(0) - 1 do"));
        assert!(
            lua.contains("reaper.SetMediaTrackInfo_Value(reaper.GetTrack(0, i), 'I_RECARM', 0)")
        );
        // The disarm loop runs before the new track is armed.
        let disarm = lua.find("'I_RECARM', 0)").unwrap();
        let arm = lua.find("(t, 'I_RECARM', 1)").unwrap();
        assert!(disarm < arm);
    }

    #[test]
    fn add_track_lua_unarmed_leaves_others_untouched() {
        // arm=false must not emit the exclusive record-arm calls.
        let lua = build_add_track_lua("scratch", None, false);
        assert!(!lua.contains("I_RECARM"));
        assert!(!lua.contains("I_RECMON"));
    }

    #[test]
    fn add_track_lua_escapes_name() {
        // A name with a single quote and backslash must not break out of the
        // single-quoted Lua string literal (also title-cased: d → D).
        let lua = build_add_track_lua("d'n\\b", None, false);
        assert!(lua.contains("local base = 'D\\'n\\\\b'"));
    }

    #[test]
    fn title_case_capitalises_each_word() {
        assert_eq!(title_case("vocal"), "Vocal");
        assert_eq!(title_case("vocal track"), "Vocal Track");
        assert_eq!(title_case("DRUM bus"), "Drum Bus");
        assert_eq!(title_case("lead   guitar"), "Lead Guitar"); // collapses runs of spaces
    }

    #[test]
    fn add_track_lua_title_cases_multiword_name() {
        let lua = build_add_track_lua("vocal track", None, true);
        assert!(lua.contains("local base = 'Vocal Track'"));
    }

    #[test]
    fn remove_track_lua_deletes_by_name_case_insensitively() {
        // User types lowercase; tracks are stored Title Case → match on lower().
        let lua = build_remove_track_lua("vocal");
        assert!(lua.contains("local target = 'vocal'"));
        assert!(lua.contains("local display = 'vocal'"));
        assert!(lua.contains("if nm:lower() == target then"));
        assert!(lua.contains("reaper.DeleteTrack(tr)"));
        assert!(lua.contains("if removed then return \"Removed track '\" .. rname .. \"'"));
        // On a miss, the project's track names are listed so the model can self-correct.
        assert!(lua.contains("local names = {}"));
        assert!(lua.contains(
            "else return \"No track named '\" .. display .. \"' found. Tracks: \" .. table.concat(names, \", \") end"
        ));
    }

    #[test]
    fn add_fx_lua_matches_track_by_name_and_adds_by_name() {
        let lua = build_add_fx_lua("vocal", "ValhallaSupermassive");
        // Track resolved case-insensitively by name (same loop as remove_track).
        assert!(lua.contains("local target = 'vocal'"));
        assert!(lua.contains("if nm:lower() == target then tr = t; break end"));
        assert!(
            lua.contains("if not tr then return \"No track named '\" .. display .. \"' found\"")
        );
        // FX added by the bare name with instantiate = -1 (always a new instance).
        assert!(lua.contains("local fx = 'ValhallaSupermassive'"));
        assert!(lua.contains("local idx = reaper.TrackFX_AddByName(tr, fx, false, -1)"));
        // Summary reads the *real* assigned name back, not the requested string.
        assert!(lua.contains("local _, real = reaper.TrackFX_GetFXName(tr, idx, '')"));
        assert!(lua.contains("return \"Added '\" .. real .. \"' to track '\""));
    }

    #[test]
    fn add_fx_lua_reports_unresolved_plugin() {
        // TrackFX_AddByName returns -1 when REAPER can't find/scan the plugin —
        // surface that clearly instead of a silent no-op (the install/scan quirk).
        let lua = build_add_fx_lua("vocal", "Nonexistent");
        assert!(lua.contains("if idx < 0 then return \"FX '\" .. fx .. \"' not found"));
    }

    #[test]
    fn add_fx_lua_does_not_force_a_format_prefix() {
        // The available format varies per machine (Valhalla surfaced VST2-only on
        // OmniLink1), so we must pass the bare name with no VST:/VST3: prefix.
        let lua = build_add_fx_lua("guitar", "ReaComp");
        assert!(lua.contains("local fx = 'ReaComp'"));
        assert!(!lua.contains("VST3:"));
        assert!(!lua.contains("VST:"));
    }

    #[test]
    fn add_fx_lua_escapes_names() {
        // Quotes/backslashes in either name must not break out of the Lua literals.
        let lua = build_add_fx_lua("d'n\\b", "weird'fx");
        assert!(lua.contains("local target = 'd\\'n\\\\b'"));
        assert!(lua.contains("local fx = 'weird\\'fx'"));
    }

    #[test]
    fn list_fx_lua_resolves_track_and_lists_chain() {
        let lua = build_list_fx_lua("vocal");
        // Track resolved case-insensitively by name (same loop as add_fx/remove_track).
        assert!(lua.contains("local target = 'vocal'"));
        assert!(lua.contains("if nm:lower() == target then tr = t; break end"));
        assert!(
            lua.contains("if not tr then return \"No track named '\" .. display .. \"' found\"")
        );
        // Walks the chain by index, emitting name + 1-based slot and a bypass flag.
        assert!(lua.contains("local n = reaper.TrackFX_GetCount(tr)"));
        assert!(lua.contains("local _, fxname = reaper.TrackFX_GetFXName(tr, i, '')"));
        assert!(lua.contains("local on = reaper.TrackFX_GetEnabled(tr, i)"));
        assert!(lua.contains("(on and '' or ' [bypassed]')"));
        // Empty chain reports rather than returning an empty list.
        assert!(lua.contains("if n == 0 then return \"Track '\" .. tn .. \"' has no FX\""));
    }

    #[test]
    fn list_fx_params_lua_resolves_fx_by_name_not_index() {
        // Quirk #1: never trust a raw index — resolve the FX by matching the bare name
        // as a plain substring of the (possibly format-prefixed) real name (quirk #2).
        let lua = build_list_fx_params_lua("vocal", "ValhallaSupermassive");
        assert!(lua.contains("local fx = 'valhallasupermassive'"));
        assert!(lua.contains("if nm:lower():find(fx, 1, true) then fxidx = i; realname = nm"));
        assert!(lua.contains(
            "if fxidx < 0 then return \"No FX matching '\" .. fxdisplay .. \"' on track '\""
        ));
        // Enumerates params: name, formatted value, raw 0–1 value.
        assert!(lua.contains("local np = reaper.TrackFX_GetNumParams(tr, fxidx)"));
        assert!(lua.contains("local _, pname = reaper.TrackFX_GetParamName(tr, fxidx, i, '')"));
        assert!(lua.contains("local val = reaper.TrackFX_GetParam(tr, fxidx, i)"));
        assert!(
            lua.contains("local _, fmt = reaper.TrackFX_GetFormattedParamValue(tr, fxidx, i, '')")
        );
    }

    #[test]
    fn list_fx_params_lua_reports_zero_params_for_lazy_init() {
        // Quirk #3: heavy plugins build their param map lazily on UI init — a 0-param
        // result is reported, not shown as an empty list.
        let lua = build_list_fx_params_lua("vocal", "Kontakt");
        assert!(
            lua.contains("if np == 0 then return \"FX '\" .. realname .. \"' reports 0 parameters")
        );
    }

    #[test]
    fn list_fx_params_lua_escapes_names() {
        let lua = build_list_fx_params_lua("d'n\\b", "weird'fx");
        assert!(lua.contains("local target = 'd\\'n\\\\b'"));
        assert!(lua.contains("local fx = 'weird\\'fx'"));
    }

    #[test]
    fn set_tempo_only_uses_set_current_bpm_no_marker() {
        let lua = build_set_tempo_lua(Some(128.0), None, None);
        assert!(lua.contains("reaper.SetCurrentBPM(0, 128, true)"));
        // No time-signature marker is injected for a tempo-only change.
        assert!(!lua.contains("SetTempoTimeSigMarker"));
        assert!(lua.contains("return 'Set tempo to 128 BPM'"));
    }

    #[test]
    fn set_tempo_with_time_sig_writes_marker() {
        let lua = build_set_tempo_lua(Some(100.0), Some(6), Some(8));
        assert!(lua.contains("local bpm = 100"));
        assert!(lua.contains("local num = 6"));
        assert!(lua.contains("local denom = 8"));
        assert!(
            lua.contains("reaper.SetTempoTimeSigMarker(0, idx, 0, -1, -1, bpm, num, denom, false)")
        );
        // Reuses an existing marker at position 0 rather than always appending.
        assert!(lua.contains("if ok and timepos == 0 then idx = i"));
        // Lock the exact summary concatenation so a stray edit can't emit invalid Lua.
        assert!(lua.contains(
            "return 'Set tempo to ' .. string.format('%.4g', bpm) .. ' BPM, ' .. math.floor(num) .. '/' .. math.floor(denom)"
        ));
    }

    #[test]
    fn set_tempo_time_sig_only_keeps_current_tempo() {
        // No tempo given → bpm falls back to the project's current value.
        let lua = build_set_tempo_lua(None, Some(3), Some(4));
        assert!(
            lua.contains(
                "local cur_num, cur_denom, cur_bpm = reaper.TimeMap_GetTimeSigAtTime(0, 0)"
            )
        );
        assert!(lua.contains("local bpm = cur_bpm"));
        assert!(lua.contains("local num = 3"));
        assert!(lua.contains("local denom = 4"));
    }

    #[test]
    fn project_info_lua_reads_tracks_and_transport() {
        let lua = build_project_info_lua();
        assert!(lua.contains("reaper.CountTracks(0)"));
        assert!(lua.contains("reaper.GetPlayState()"));
        assert!(lua.contains("reaper.GetProjectName(0, '')"));
        assert!(lua.contains("reaper.TimeMap_GetTimeSigAtTime(0, 0)"));
        // Multi-line output joined with newlines (relies on the agent's multi-line read).
        assert!(lua.contains("table.concat(lines, '\\n')"));
    }
}
