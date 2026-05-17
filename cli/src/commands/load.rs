use shared::{MeshMessage, ModelLoadRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;

pub async fn run(node_id: String, model_name: String, size_mb: u64) {
    match send_load(node_id, model_name, size_mb).await {
        Ok(()) => println!("Load request acknowledged"),
        Err(e) => println!("Error: {}", e),
    }
}

async fn send_load(
    node_id: String,
    model_name: String,
    size_mb: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect("127.0.0.1:9000").await?;

    let msg = MeshMessage::ModelLoad(ModelLoadRequest {
        request_id: Uuid::new_v4().to_string(),
        node_id,
        model_name,
        model_size_mb: size_mb,
        wire_version: shared::messages::WIRE_VERSION,
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

    match serde_json::from_slice(&buf)? {
        MeshMessage::Acknowledge => Ok(()),
        other => Err(format!("Unexpected response: {:?}", other).into()),
    }
}
