use shared::{messages::WIRE_VERSION, InferenceRequest, MeshMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;

pub async fn run(coordinator: &str, model_name: String, prompt: String) {
    match send_infer(coordinator, model_name, prompt).await {
        Ok(()) => {}
        Err(e) => println!("Error: {}", e),
    }
}

async fn send_infer(coordinator: &str, model_name: String, prompt: String) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(coordinator).await?;

    let msg = MeshMessage::RequestModelInference(InferenceRequest {
        request_id: Uuid::new_v4().to_string(),
        node_id: None,
        model_name,
        prompt,
        max_tokens: 256,
        wire_version: WIRE_VERSION,
    });

    let data = serde_json::to_vec(&msg)?;
    let len = (data.len() as u32).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&data).await?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let msg_len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf).await?;

    match serde_json::from_slice::<MeshMessage>(&buf)? {
        MeshMessage::ModelInferenceResult(res) => {
            if let Some(err) = res.error {
                println!("Error: {}", err);
            } else {
                println!("{}", res.output);
            }
            Ok(())
        }
        MeshMessage::Error(err) => {
            println!("Error: {}", err);
            Ok(())
        }
        other => Err(format!("Unexpected response: {:?}", other).into()),
    }
}
