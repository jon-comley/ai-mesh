use shared::{AdminMessage, MeshMessage};

pub async fn run(coordinator: &str) {
    match send_reset(coordinator).await {
        Ok(()) => println!("Registry reset OK"),
        Err(e) => println!("Error: {}", e),
    }
}

async fn send_reset(coordinator: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = crate::connection::connect(coordinator).await?;
    let msg = MeshMessage::Admin(AdminMessage::ResetRegistry);
    match crate::connection::send_recv(&mut stream, &msg).await? {
        MeshMessage::Acknowledge => Ok(()),
        other => Err(format!("Unexpected response: {:?}", other).into()),
    }
}
