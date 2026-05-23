use shared::{MeshMessage, NodeRecordLite};

/// Print the UUID of the node whose IP matches `ip`, or exit non-zero if not found.
/// Used by justfile recipes that need a machine-readable node ID.
pub async fn run(coordinator: &str, ip: String) {
    match find(coordinator, &ip).await {
        Ok(Some(id)) => print!("{}", id),
        Ok(None) => std::process::exit(1),
        Err(_) => std::process::exit(1),
    }
}

async fn find(coordinator: &str, ip: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut stream = crate::connection::connect(coordinator).await?;
    let nodes: Vec<NodeRecordLite> =
        match crate::connection::send_recv(&mut stream, &MeshMessage::RequestNodes).await? {
            MeshMessage::NodeList(nodes) => nodes,
            _ => return Ok(None),
        };
    Ok(nodes.into_iter().find(|n| n.ip == ip).map(|n| n.id))
}
