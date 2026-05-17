use agent::agent::Agent;
use agent::config::AgentConfig;
use shared::MeshMessage;
use shared::hardware::NodeRole;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    println!("Agent starting…");

    let role = read_role_from_env();

    let mut stream = match TcpStream::connect("127.0.0.1:9000").await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to connect to coordinator: {}", e);
            return;
        }
    };

    let (tx, mut rx) = mpsc::channel::<MeshMessage>(32);

    let config = AgentConfig {
        role,
        heartbeat_interval_secs: 5,
    };
    let agent = Agent::new_with_config(config, tx);
    tokio::spawn(async move {
        let _ = agent.run().await;
    });

    loop {
        if let Some(msg) = rx.recv().await {
            let data = serde_json::to_vec(&msg).unwrap();
            let len = (data.len() as u32).to_le_bytes();

            if let Err(e) = stream.write_all(&len).await {
                eprintln!("Write error: {}", e);
                break;
            }
            if let Err(e) = stream.write_all(&data).await {
                eprintln!("Write error: {}", e);
                break;
            }
        }
    }
}

fn read_role_from_env() -> NodeRole {
    match std::env::var("AGENT_ROLE").as_deref() {
        Ok("controller") => NodeRole::Controller,
        _ => NodeRole::Compute,
    }
}
