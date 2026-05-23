use shared::{MeshMessage, ModelLoadRequest};
use uuid::Uuid;

pub async fn run(coordinator: &str, node_id: Option<String>, model_name: String, size_mb: u64) {
    match send_load(coordinator, node_id, model_name, size_mb).await {
        Ok(()) => println!("Load request acknowledged"),
        Err(e) => println!("Error: {}", e),
    }
}

async fn send_load(
    coordinator: &str,
    node_id: Option<String>,
    model_name: String,
    size_mb: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = crate::connection::connect(coordinator).await?;
    let msg = MeshMessage::ModelLoad(ModelLoadRequest {
        request_id: Uuid::new_v4().to_string(),
        node_id,
        model_name,
        model_size_mb: size_mb,
        wire_version: shared::messages::WIRE_VERSION,
    });
    match crate::connection::send_recv(&mut stream, &msg).await? {
        MeshMessage::Acknowledge => Ok(()),
        other => Err(format!("Unexpected response: {:?}", other).into()),
    }
}
