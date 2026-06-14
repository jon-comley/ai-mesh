use crate::http::state::{PendingInferences, PendingIntents};
use crate::registry::Registry;
use crate::scheduler::Scheduler;
use crate::server::Connections;
use shared::{
    InferenceRequest, IntentRequest, IntentResponse, LightAction, LightCommandRequest,
    LightStateReport, LightTarget, MeshMessage, ReaperCommandRequest, SceneLoadRequest,
    ToolCallRecord, WIRE_VERSION,
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

pub async fn handle_intent(
    request: IntentRequest,
    registry: Arc<Mutex<Registry>>,
    connections: Connections,
    pending_inferences: PendingInferences,
    pending_intents: PendingIntents,
    device_states: Vec<LightStateReport>,
) -> IntentResponse {
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

    // 2. Choose model
    let model_name = match request.model_name.clone() {
        Some(m) => m,
        None => match registry.lock().unwrap().any_ready_llm_model() {
            Some(m) => m,
            None => return fail("no LLM model is ready on any node".into()),
        },
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
    let mut user_prompt = device_ctx;
    const MAX_CONTEXT_TURNS: usize = 20;
    let context = if request.context.len() > MAX_CONTEXT_TURNS {
        &request.context[request.context.len() - MAX_CONTEXT_TURNS..]
    } else {
        &request.context[..]
    };
    for turn in context {
        match turn.role {
            shared::IntentRole::User => {
                user_prompt.push_str(&format!("User: {}\n", turn.content));
            }
            shared::IntentRole::Assistant => {
                user_prompt.push_str(&format!("Assistant: {}\n", turn.content));
            }
        }
    }
    user_prompt.push_str(&request.text);

    // 4. Find a connected LLM node — skip any whose TCP channel has gone away.
    let connected: std::collections::HashSet<String> =
        connections.lock().unwrap().keys().cloned().collect();
    let llm_node_id = {
        let reg = registry.lock().unwrap();
        Scheduler::new(&reg)
            .select_node_for_inference(&model_name)
            .filter(|n| connected.contains(&n.id))
            .map(|n| n.id)
    };
    let llm_node_id = match llm_node_id {
        Some(id) => id,
        None => {
            return fail(format!(
                "no connected node has model '{}' in Ready state",
                model_name
            ));
        }
    };

    let agent_tx = connections.lock().unwrap().get(&llm_node_id).cloned();
    let agent_tx = match agent_tx {
        Some(tx) => tx,
        None => return fail(format!("LLM node '{}' is not connected", llm_node_id)),
    };

    // 5. Send inference request, wait for result
    let infer_req_id = format!("intent-{}", request.request_id);
    let infer_req = InferenceRequest {
        request_id: infer_req_id.clone(),
        node_id: None,
        model_name: model_name.clone(),
        system_prompt: Some(system_prompt),
        prompt: user_prompt,
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
        return fail("LLM node channel closed before inference could be sent".into());
    }

    let llm_result = match timeout(Duration::from_secs(INTENT_INFERENCE_TIMEOUT_SECS), orx).await {
        Ok(Ok(MeshMessage::ModelInferenceResult(res))) => res,
        Ok(Ok(MeshMessage::Error(e))) => return fail(format!("LLM error: {e}")),
        Ok(Ok(_)) => return fail("unexpected message from LLM node".into()),
        Err(_) | Ok(Err(_)) => {
            pending_inferences.lock().unwrap().remove(&infer_req_id);
            return fail(format!(
                "LLM inference timed out after {INTENT_INFERENCE_TIMEOUT_SECS}s"
            ));
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

            let tool_result = dispatch_tool(
                &request.request_id,
                &tool_name,
                args.clone(),
                &registry,
                &connections,
                &pending_intents,
                &device_states,
            )
            .await;

            records.push(ToolCallRecord {
                tool: tool_name,
                args,
                result: Some(tool_result),
            });
        }

        return IntentResponse {
            request_id: request.request_id,
            node_id,
            model_name,
            text: None,
            tool_calls: records,
            error: None,
            duration_ms,
            tokens_generated,
            prompt_eval_ms,
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
    }
}

fn device_is_offline(target: &str, states: &[LightStateReport]) -> bool {
    states
        .iter()
        .find(|s| s.device_id == target)
        .is_some_and(|s| !s.online)
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
                return format!("device '{}' is currently offline", final_target);
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
        other => format!("unknown tool: {other}"),
    }
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

pub fn try_parse_tool_calls(output: &str) -> Option<Vec<serde_json::Value>> {
    let stripped = output
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let parsed = serde_json::from_str::<serde_json::Value>(stripped)
        .or_else(|_| serde_json::from_str::<serde_json::Value>(&format!("{stripped}}}")));

    let v = parsed.ok()?;
    if let Some(arr) = v.as_array() {
        let calls: Vec<_> = arr
            .iter()
            .filter(|el| el.get("tool").is_some() && el["args"].is_object())
            .cloned()
            .collect();
        if calls.is_empty() { None } else { Some(calls) }
    } else if v.get("tool").is_some() && v["args"].is_object() {
        Some(vec![v])
    } else {
        None
    }
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
                "description": "Control REAPER DAW transport. Use to start/stop playback or recording.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["play", "stop", "pause", "record", "rewind"],
                            "description": "Transport action to perform"
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
}
