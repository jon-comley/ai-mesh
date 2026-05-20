use shared::{AdminMessage, MeshMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn run(coordinator: &str) {
    match send_reset(coordinator).await {
        Ok(()) => println!("Registry reset OK"),
        Err(e) => println!("Error: {}", e),
    }
}

async fn send_reset(coordinator: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(coordinator).await?;

    let msg = MeshMessage::Admin(AdminMessage::ResetRegistry);
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
