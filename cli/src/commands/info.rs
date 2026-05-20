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
    println!("  ID:             {}", n.id);
    println!("  Hostname:       {}", n.hostname);
    println!("  IP:             {}", n.ip);
    println!("  Role:           {:?}", n.role);
    println!("  Last heartbeat: {} ms ago", n.last_heartbeat_ms);

    println!("\n  Hardware:");
    match n.hardware {
        Some(hw) => {
            println!(
                "    CPU:   {} ({} cores / {} threads)",
                hw.cpu_model, hw.cpu_cores, hw.cpu_threads
            );
            println!("    RAM:   {:.1} GB", hw.ram_gb);
            println!("    OS:    {} ({})", hw.os, hw.arch);
            println!("    GPU:   {}", hw.gpu.unwrap_or_else(|| "none".into()));
        }
        None => println!("    (no hardware report)"),
    }

    println!("\n  Capabilities:");
    match n.capabilities {
        Some(c) => {
            println!("    CPU inference:  {}", c.cpu_inference);
            println!("    GPU inference:  {}", c.gpu_inference);
            println!("    ANE inference:  {}", c.ane_inference);
            println!("    Max model:      {:.1} GB", c.max_model_size_gb);
        }
        None => println!("    (no capabilities report)"),
    }

    if !n.models.is_empty() {
        println!("\n  Models:");
        for m in &n.models {
            println!("    {} ({} MB) — {:?}", m.model_name, m.size_mb, m.state);
        }
    }
}
