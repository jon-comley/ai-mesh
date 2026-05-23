use shared::{messages::WIRE_VERSION, MeshMessage, ModelUnloadRequest};
use uuid::Uuid;

pub async fn run(coordinator: &str, node_id: String, model_name: String) {
    match send_unload(coordinator, node_id, model_name).await {
        Ok(()) => println!("Unload request acknowledged"),
        Err(e) => println!("Error: {}", e),
    }
}

async fn send_unload(
    coordinator: &str,
    node_id: String,
    model_name: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = crate::connection::connect(coordinator).await?;
    let msg = MeshMessage::ModelUnload(ModelUnloadRequest {
        request_id: Uuid::new_v4().to_string(),
        node_id,
        model_name,
        wire_version: WIRE_VERSION,
    });
    match crate::connection::send_recv(&mut stream, &msg).await? {
        MeshMessage::Acknowledge => Ok(()),
        other => Err(format!("Unexpected response: {:?}", other).into()),
    }
}
