use shared::{MeshMessage, NodeRecordFull};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn run(coordinator: &str, id: String) {
    match fetch_info(coordinator, id).await {
        Ok(info) => print_info(info),
        Err(e) => println!("Error: {}", e),
    }
}

async fn fetch_info(
    coordinator: &str,
    id: String,
) -> Result<NodeRecordFull, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(coordinator).await?;

    let msg = MeshMessage::RequestNodeInfo(id);
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
        MeshMessage::NodeInfo(info) => Ok(info),
        other => Err(format!("Unexpected response: {:?}", other).into()),
    }
}

fn print_info(n: NodeRecordFull) {
    println!("Node ID: {}", n.id);
    println!("Hostname: {}", n.hostname);
    println!("IP: {}", n.ip);
    println!("Role: {:?}", n.role);
    println!("Last Heartbeat: {} ms ago", n.last_heartbeat_ms);

    println!("\nHardware:");
    match n.hardware {
        Some(hw) => println!("{:#?}", hw),
        None => println!("  (no hardware report)"),
    }

    println!("\nCapabilities:");
    match n.capabilities {
        Some(c) => println!("{:#?}", c),
        None => println!("  (no capabilities report)"),
    }
}
