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
    let system_prompt = build_system_prompt(&schemas);
    let mut conversation = String::new();
    for turn in &request.context {
        match turn.role {
            shared::IntentRole::User => {
                conversation.push_str(&format!("User: {}\n", turn.content));
            }
            shared::IntentRole::Assistant => {
                conversation.push_str(&format!("Assistant: {}\n", turn.content));
            }
        }
    }
    conversation.push_str(&format!("User: {}", request.text));
    let full_prompt = format!("{}\n\n{}", system_prompt, conversation);

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
        prompt: full_prompt,
        max_tokens: 512,
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
        "brightness" => LightAction::Brightness(value.unwrap_or(128.0) as u8),
        "color_temp" => {
            // Convert Kelvin → mireds (1_000_000 / K)
            let mireds = (1_000_000.0 / value.unwrap_or(4000.0)) as u16;
            LightAction::ColorTemp(mireds)
        }
        _ => LightAction::On,
    };

    // All commands target group 1 (the whole house) until Phase 6 adds room resolution.
    LightCommandRequest {
        request_id: request_id.to_string(),
        target: LightTarget::Group(1),
        command,
    }
}

pub fn try_parse_tool_call(output: &str) -> Option<serde_json::Value> {
    let stripped = output
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let v: serde_json::Value = serde_json::from_str(stripped).ok()?;
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

pub fn build_system_prompt(schemas: &[serde_json::Value]) -> String {
    if schemas.is_empty() {
        return "You are a helpful assistant. Answer the user's question directly.".into();
    }

    let schema_json = serde_json::to_string_pretty(schemas).unwrap_or_default();
    format!(
        r#"You are a smart home and general-purpose assistant.

You have access to the following tools:
{schema_json}

If the user's request maps to a tool, respond with ONLY a JSON object:
{{"tool": "<name>", "args": {{ ... }}}}

If the user's request is a general question or conversation, respond normally in free text.
Do not explain your reasoning. Do not use markdown for tool calls."#
    )
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
}
