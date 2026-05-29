use shared::{messages::WIRE_VERSION, InferenceRequest, MeshMessage};
use tokio::time::{timeout, Duration};
use uuid::Uuid;

const INFER_TIMEOUT_SECS: u64 = 30;

pub async fn run(
    coordinator: &str,
    model_name: String,
    prompt: String,
    system_prompt: Option<String>,
) {
    match send_infer(coordinator, model_name, prompt, system_prompt).await {
        Ok(()) => {}
        Err(e) => eprintln!("Error (coordinator={}): {}", coordinator, e),
    }
}

async fn send_recv(
    coordinator: &str,
    msg: &MeshMessage,
) -> Result<MeshMessage, Box<dyn std::error::Error>> {
    let mut stream = crate::connection::connect(coordinator).await?;
    let fut = crate::connection::send_recv(&mut stream, msg);
    match timeout(Duration::from_secs(INFER_TIMEOUT_SECS), fut).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(format!(
            "no response from coordinator '{}' after {INFER_TIMEOUT_SECS}s",
            coordinator
        )
        .into()),
    }
}

async fn lookup_hostname(coordinator: &str, node_id: &str) -> Option<String> {
    match send_recv(
        coordinator,
        &MeshMessage::RequestNodeInfo(node_id.to_string()),
    )
    .await
    {
        Ok(MeshMessage::NodeInfo(info)) => Some(info.hostname),
        _ => None,
    }
}

async fn send_infer(
    coordinator: &str,
    model_name: String,
    prompt: String,
    system_prompt: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = MeshMessage::RequestModelInference(InferenceRequest {
        request_id: Uuid::new_v4().to_string(),
        node_id: None,
        model_name,
        system_prompt,
        prompt,
        max_tokens: 256,
        temperature: None,
        wire_version: WIRE_VERSION,
    });

    match send_recv(coordinator, &msg).await? {
        MeshMessage::ModelInferenceResult(res) => {
            if let Some(err) = res.error {
                eprintln!("Error: {}", err);
            } else {
                println!("{}", res.output);
                let hostname = lookup_hostname(coordinator, &res.node_id)
                    .await
                    .unwrap_or_else(|| res.node_id.chars().take(8).collect());
                println!(
                    "served-by: {} | {} | {} tokens | {}ms",
                    hostname, res.model_name, res.tokens_generated, res.duration_ms
                );
            }
            Ok(())
        }
        MeshMessage::Error(err) => {
            eprintln!("Error (coordinator={}): {}", coordinator, err);
            Ok(())
        }
        other => Err(format!("Unexpected response: {:?}", other).into()),
    }
}
