use shared::MeshMessage;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn run() {
    println!("Checking coordinator status...");

    match try_status().await {
        Ok(latency_ms) => {
            println!("✔ Coordinator is online");
            println!("  Latency: {} ms", latency_ms);
        }
        Err(e) => {
            println!("✘ Coordinator unreachable");
            println!("  Error: {}", e);
        }
    }
}

async fn try_status() -> Result<u128, Box<dyn std::error::Error>> {
    let start = Instant::now();

    let mut stream = TcpStream::connect("127.0.0.1:9000").await?;

    let msg = MeshMessage::Ping;
    let data = serde_json::to_vec(&msg)?;
    let len = (data.len() as u32).to_le_bytes();

    stream.write_all(&len).await?;
    stream.write_all(&data).await?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let msg_len = u32::from_le_bytes(len_buf) as usize;

    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf).await?;

    let ack: MeshMessage = serde_json::from_slice(&buf)?;

    match ack {
        MeshMessage::Acknowledge => Ok(start.elapsed().as_millis()),
        _ => Err("Unexpected response".into()),
    }
}
