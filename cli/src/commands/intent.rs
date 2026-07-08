use shared::{IntentRequest, MeshMessage};
use uuid::Uuid;

pub async fn run(coordinator: &str, text: String, model: Option<String>) {
    match send_intent(coordinator, text, model).await {
        Ok(()) => {}
        Err(e) => eprintln!("Error: {e}"),
    }
}

async fn send_recv(
    coordinator: &str,
    msg: &MeshMessage,
) -> Result<MeshMessage, Box<dyn std::error::Error>> {
    let mut stream = crate::connection::connect(coordinator).await?;
    Ok(crate::connection::send_recv(&mut stream, msg).await?)
}

async fn lookup_hostname(coordinator: &str, node_id: &str) -> String {
    match send_recv(
        coordinator,
        &MeshMessage::RequestNodeInfo(node_id.to_string()),
    )
    .await
    {
        Ok(MeshMessage::NodeInfo(info)) => info.hostname,
        _ => node_id.chars().take(8).collect(),
    }
}

async fn send_intent(
    coordinator: &str,
    text: String,
    model: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let request_id = Uuid::new_v4().to_string();
    let msg = MeshMessage::IntentRequest(IntentRequest {
        request_id,
        text,
        model_name: model,
        context: vec![],
        source: shared::IntentSource::Cli,
    });

    match send_recv(coordinator, &msg).await? {
        MeshMessage::IntentResponse(resp) => {
            if let Some(err) = resp.error {
                eprintln!("Error: {err}");
                return Ok(());
            }

            // Tool calls
            for call in &resp.tool_calls {
                let args_summary = format_args(&call.args);
                let result = call.result.as_deref().unwrap_or("—");
                println!("[{}] {} → {}", call.tool, args_summary, result);
            }

            // Free-text response
            if let Some(text) = &resp.text {
                println!("{text}");
            }

            // Attribution line
            if !resp.node_id.is_empty() {
                let hostname = lookup_hostname(coordinator, &resp.node_id).await;
                println!("served-by: {hostname}");
            }

            Ok(())
        }
        MeshMessage::Error(err) => {
            eprintln!("Error: {err}");
            Ok(())
        }
        other => Err(format!("Unexpected response: {other:?}").into()),
    }
}

/// Compact single-line summary of tool args for display.
fn format_args(args: &serde_json::Value) -> String {
    let obj = match args.as_object() {
        Some(o) => o,
        None => return args.to_string(),
    };

    obj.iter()
        .map(|(k, v)| {
            let val = v
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| v.to_string());
            format!("{k}={val}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}
