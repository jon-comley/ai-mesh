use shared::{MeshMessage, NodeRecordLite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

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
    let mut stream = TcpStream::connect(coordinator).await?;

    let data = serde_json::to_vec(&MeshMessage::RequestNodes)?;
    let len = (data.len() as u32).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&data).await?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let msg_len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf).await?;

    let nodes: Vec<NodeRecordLite> = match serde_json::from_slice(&buf)? {
        MeshMessage::NodeList(nodes) => nodes,
        _ => return Ok(None),
    };

    Ok(nodes.into_iter().find(|n| n.ip == ip).map(|n| n.id))
}
