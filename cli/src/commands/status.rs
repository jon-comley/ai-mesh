use shared::MeshMessage;
use std::time::Instant;

pub async fn run(coordinator: &str) {
    println!("Checking coordinator status...");

    match try_status(coordinator).await {
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

async fn try_status(coordinator: &str) -> Result<u128, Box<dyn std::error::Error>> {
    let start = Instant::now();
    let mut stream = crate::connection::connect(coordinator).await?;
    match crate::connection::send_recv(&mut stream, &MeshMessage::Ping).await? {
        MeshMessage::Acknowledge => Ok(start.elapsed().as_millis()),
        _ => Err("Unexpected response".into()),
    }
}
