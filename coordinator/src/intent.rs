use crate::registry::Registry;
use crate::scheduler::Scheduler;
use crate::server::{Connections, PendingInferences};
use shared::{
    InferenceRequest, IntentRequest, IntentResponse, LightAction, LightCommandRequest, LightTarget,
    MeshMessage, SceneLoadRequest, ToolCallRecord, WIRE_VERSION,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

pub type PendingIntents = Arc<Mutex<HashMap<String, oneshot::Sender<MeshMessage>>>>;

const INTENT_INFERENCE_TIMEOUT_SECS: u64 = 60;
const TOOL_RESPONSE_TIMEOUT_SECS: u64 = 10;

pub async fn handle_intent(
    request: IntentRequest,
    registry: Arc<Mutex<Registry>>,
    connections: Connections,
    pending_inferences: PendingInferences,
    pending_intents: PendingIntents,
) -> IntentResponse {
    let fail = |msg: String| IntentResponse {
        request_id: request.request_id.clone(),
        node_id: String::new(),
        text: None,
        tool_calls: vec![],
        error: Some(msg),
    };

    // 1. Collect tool schemas from nodes that advertise a given feature
    let schemas = {
        let reg = registry.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut schemas = Vec::new();
        for feature in ["lighting"] {
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
    let system_prompt = build_system_prompt(&schemas, &known_devices, &known_groups);
    let mut user_prompt = String::new();
    for turn in &request.context {
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

    // 4. Find LLM node
    let llm_node_id = {
        let reg = registry.lock().unwrap();
        Scheduler::new(&reg)
            .select_node_for_inference(&model_name)
            .map(|n| n.id)
    };
    let llm_node_id = match llm_node_id {
        Some(id) => id,
        None => return fail(format!("no node has model '{}' in Ready state", model_name)),
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
        max_tokens: 128,
        temperature: Some(0.0),
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
    let raw = llm_result.output.trim().to_string();
    info!(
        request_id = %request.request_id,
        node_id = %node_id,
        "intent LLM output: {}",
        raw
    );

    // 6. Parse as tool call or return as free text
    if let Some(call) = try_parse_tool_call(&raw) {
        let tool_name = call["tool"].as_str().unwrap_or("").to_string();
        let args = call["args"].clone();

        let result = dispatch_tool(
            &request.request_id,
            &tool_name,
            args.clone(),
            &registry,
            &connections,
            &pending_intents,
        )
        .await;

        return IntentResponse {
            request_id: request.request_id,
            node_id,
            text: None,
            tool_calls: vec![ToolCallRecord {
                tool: tool_name,
                args,
                result: Some(result),
            }],
            error: None,
        };
    }

    IntentResponse {
        request_id: request.request_id,
        node_id,
        text: Some(raw),
        tool_calls: vec![],
        error: None,
    }
}

async fn dispatch_tool(
    request_id: &str,
    tool_name: &str,
    args: serde_json::Value,
    registry: &Arc<Mutex<Registry>>,
    connections: &Connections,
    pending_intents: &PendingIntents,
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
            // Validate target against known devices/groups (if any are registered).
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
                    return format!(
                        "unknown target '{}' — known targets: {}",
                        target,
                        known.join(", ")
                    );
                }
            }
            let cmd = build_light_command(request_id, &args);
            if lighting_tx
                .send(MeshMessage::LightCommand(cmd))
                .await
                .is_err()
            {
                warn!(request_id, "failed to send LightCommand to lighting node");
                return "failed to send LightCommand to lighting node".into();
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
        other => format!("unknown tool: {other}"),
    }
}

fn build_light_command(request_id: &str, args: &serde_json::Value) -> LightCommandRequest {
    let action_str = args.get("action").and_then(|v| v.as_str()).unwrap_or("on");
    let value = args.get("value").and_then(|v| v.as_f64());

    let command = match action_str {
        "off" => LightAction::Off,
        "toggle" => LightAction::Toggle,
        // Zigbee brightness range is 0–254; 255 is reserved by the spec.
        "brightness" => LightAction::Brightness(value.unwrap_or(128.0).clamp(0.0, 254.0) as u8),
        "color_temp" => {
            // Convert Kelvin → mireds (1_000_000 / K)
            let mireds = (1_000_000.0 / value.unwrap_or(4000.0)) as u16;
            LightAction::ColorTemp(mireds)
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

    LightCommandRequest {
        request_id: request_id.to_string(),
        target,
        command,
    }
}

pub fn try_parse_tool_call(output: &str) -> Option<serde_json::Value> {
    let stripped = output
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let parsed = serde_json::from_str::<serde_json::Value>(stripped)
        .or_else(|_| serde_json::from_str::<serde_json::Value>(&format!("{stripped}}}")));

    let v = parsed.ok()?;
    if v.get("tool").is_some() && v.get("args").is_some() {
        Some(v)
    } else {
        None
    }
}

fn tool_schemas_for_feature(feature: &str) -> Vec<serde_json::Value> {
    match feature {
        "lighting" => vec![
            serde_json::json!({
                "name": "light_command",
                "description": "Turn lights on/off, set brightness or colour temperature",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target": {
                            "type": "string",
                            "description": "Room or device name, e.g. 'living_room'"
                        },
                        "action": {
                            "type": "string",
                            "enum": ["on", "off", "toggle", "brightness", "color_temp"]
                        },
                        "value": {
                            "type": "number",
                            "description": "Brightness 0–255 or colour temp in Kelvin"
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
        _ => vec![],
    }
}

pub fn build_system_prompt(
    schemas: &[serde_json::Value],
    known_devices: &[String],
    known_groups: &[String],
) -> String {
    if schemas.is_empty() {
        return "You are a helpful assistant. Answer the user's question directly.".into();
    }

    let schema_json = serde_json::to_string(schemas).unwrap_or_default();

    let device_section = if known_devices.is_empty() && known_groups.is_empty() {
        String::new()
    } else {
        let mut lines = Vec::new();
        if !known_devices.is_empty() {
            lines.push(format!("Known devices: {}", known_devices.join(", ")));
        }
        if !known_groups.is_empty() {
            lines.push(format!(
                "Known groups (control all members at once): {}",
                known_groups.join(", ")
            ));
        }
        format!("\n\n{}", lines.join("\n"))
    };

    format!(
        r#"You are a smart home controller. You control real smart home devices using the tools below.

Tools:
{schema_json}{device_section}

Rules:
- Use the exact device or group name from the known list for the "target" field.
- If the user says "all" or "everything", use a group name if one exists.
- For device control requests, respond with ONLY this exact JSON format (no other text, no tags, no markdown):
{{"tool": "<name>", "args": {{ ... }}}}
- For general questions or conversation, respond normally in plain text.
- Do NOT use XML tags, function_call tags, or any special formatting. Plain JSON only."#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_system_prompt_no_schemas_returns_plain() {
        let p = build_system_prompt(&[], &[], &[]);
        assert!(!p.contains("tool"));
        assert!(p.contains("helpful assistant"));
    }

    #[test]
    fn build_system_prompt_with_schemas_includes_tool_section() {
        let schemas = tool_schemas_for_feature("lighting");
        let p = build_system_prompt(&schemas, &[], &[]);
        assert!(p.contains("light_command"));
        assert!(p.contains("scene_load"));
        assert!(p.contains(r#"{"tool":"#));
    }

    #[test]
    fn build_system_prompt_injects_known_devices_and_groups() {
        let schemas = tool_schemas_for_feature("lighting");
        let devices = vec!["test_bulb".to_string(), "desk_lamp".to_string()];
        let groups = vec!["all".to_string()];
        let p = build_system_prompt(&schemas, &devices, &groups);
        assert!(p.contains("test_bulb"));
        assert!(p.contains("desk_lamp"));
        assert!(p.contains("all"));
        assert!(p.contains("Known devices"));
        assert!(p.contains("Known groups"));
    }

    #[test]
    fn build_system_prompt_no_devices_omits_device_section() {
        let schemas = tool_schemas_for_feature("lighting");
        let p = build_system_prompt(&schemas, &[], &[]);
        assert!(!p.contains("Known devices"));
        assert!(!p.contains("Known groups"));
    }

    #[test]
    fn try_parse_tool_call_valid_json() {
        let raw = r#"{"tool":"light_command","args":{"target":"living_room","action":"on"}}"#;
        let result = try_parse_tool_call(raw).unwrap();
        assert_eq!(result["tool"], "light_command");
        assert_eq!(result["args"]["action"], "on");
    }

    #[test]
    fn try_parse_tool_call_free_text_returns_none() {
        let raw = "The capital of France is Paris.";
        assert!(try_parse_tool_call(raw).is_none());
    }

    #[test]
    fn try_parse_tool_call_strips_code_fence() {
        let raw = "```json\n{\"tool\":\"scene_load\",\"args\":{\"scene\":\"cozy\"}}\n```";
        let result = try_parse_tool_call(raw).unwrap();
        assert_eq!(result["tool"], "scene_load");
    }

    #[test]
    fn try_parse_tool_call_missing_args_returns_none() {
        let raw = r#"{"tool":"light_command"}"#;
        assert!(try_parse_tool_call(raw).is_none());
    }

    #[test]
    fn try_parse_tool_call_truncated_json_repaired() {
        let raw = r#"{"tool":"light_command","args":{"target":"test_bulb","action":"off"}"#;
        let result = try_parse_tool_call(raw).unwrap();
        assert_eq!(result["tool"], "light_command");
        assert_eq!(result["args"]["action"], "off");
    }

    #[test]
    fn try_parse_tool_call_json_without_tool_field_returns_none() {
        let raw = r#"{"action":"on","target":"living_room"}"#;
        assert!(try_parse_tool_call(raw).is_none());
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
        let cmd = build_light_command("r1", &args);
        assert!(matches!(cmd.command, LightAction::On));
        assert_eq!(cmd.request_id, "r1");
        assert!(matches!(cmd.target, LightTarget::Device(ref n) if n == "kitchen"));
    }

    #[test]
    fn build_light_command_no_target_falls_back_to_group() {
        let args = serde_json::json!({"action": "on"});
        let cmd = build_light_command("r0", &args);
        assert!(matches!(cmd.target, LightTarget::Group(ref g) if g == "all"));
    }

    #[test]
    fn build_light_command_off() {
        let args = serde_json::json!({"target": "bedroom", "action": "off"});
        let cmd = build_light_command("r2", &args);
        assert!(matches!(cmd.command, LightAction::Off));
    }

    #[test]
    fn build_light_command_brightness() {
        let args = serde_json::json!({"target": "lounge", "action": "brightness", "value": 200});
        let cmd = build_light_command("r3", &args);
        assert!(matches!(cmd.command, LightAction::Brightness(200)));
    }

    #[test]
    fn build_light_command_brightness_clamped_to_254() {
        let args = serde_json::json!({"target": "lounge", "action": "brightness", "value": 255});
        let cmd = build_light_command("r3b", &args);
        assert!(matches!(cmd.command, LightAction::Brightness(254)));
    }

    #[test]
    fn build_light_command_color_temp_kelvin_to_mireds() {
        // 4000K → 250 mireds
        let args = serde_json::json!({"target": "office", "action": "color_temp", "value": 4000});
        let cmd = build_light_command("r4", &args);
        assert!(matches!(cmd.command, LightAction::ColorTemp(250)));
    }

    #[test]
    fn build_light_command_unknown_action_defaults_to_on() {
        let args = serde_json::json!({"target": "hall", "action": "sparkle"});
        let cmd = build_light_command("r5", &args);
        assert!(matches!(cmd.command, LightAction::On));
    }

    #[test]
    fn build_light_command_toggle() {
        let args = serde_json::json!({"target": "lounge", "action": "toggle"});
        let cmd = build_light_command("r6", &args);
        assert!(matches!(cmd.command, LightAction::Toggle));
    }

    #[test]
    fn build_light_command_empty_target_falls_back_to_group() {
        let args = serde_json::json!({"target": "", "action": "on"});
        let cmd = build_light_command("r7", &args);
        assert!(matches!(cmd.target, LightTarget::Group(ref g) if g == "all"));
    }

    #[test]
    fn build_system_prompt_devices_only_no_groups() {
        let schemas = tool_schemas_for_feature("lighting");
        let devices = vec!["test_bulb".to_string()];
        let p = build_system_prompt(&schemas, &devices, &[]);
        assert!(p.contains("Known devices"));
        assert!(!p.contains("Known groups"));
    }

    #[test]
    fn build_system_prompt_groups_only_no_devices() {
        let schemas = tool_schemas_for_feature("lighting");
        let groups = vec!["all".to_string()];
        let p = build_system_prompt(&schemas, &[], &groups);
        assert!(!p.contains("Known devices"));
        assert!(p.contains("Known groups"));
    }

    #[test]
    fn build_system_prompt_injects_device_list_into_target_description() {
        let schemas = tool_schemas_for_feature("lighting");
        let devices = vec!["test_bulb".to_string()];
        let groups = vec!["all".to_string()];
        let p = build_system_prompt(&schemas, &devices, &groups);
        // LLM is told to use exact names
        assert!(p.contains("exact device or group name"));
        assert!(p.contains("test_bulb"));
        assert!(p.contains("all"));
    }

    #[test]
    fn build_system_prompt_forbids_special_tags() {
        let schemas = tool_schemas_for_feature("lighting");
        let p = build_system_prompt(&schemas, &[], &[]);
        assert!(p.contains("Do NOT use XML tags"));
        assert!(p.contains("Plain JSON only"));
    }

    #[test]
    fn build_system_prompt_schema_is_compact_json() {
        let schemas = tool_schemas_for_feature("lighting");
        let p = build_system_prompt(&schemas, &[], &[]);
        for schema in schemas {
            let s = serde_json::to_string(&schema).unwrap();
            assert!(!s.contains('\n'), "schema JSON must be compact");
            assert!(
                p.contains(&s),
                "system prompt must embed compact schema JSON"
            );
        }
    }
}
