use prettytable::{row, Table};
use shared::{MeshMessage, NodeRecordFull, NodeRecordLite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn run() {
    match fetch_nodes_full().await {
        Ok(nodes) => print_table(nodes),
        Err(e) => println!("Error: {}", e),
    }
}

async fn send_recv(msg: &MeshMessage) -> Result<MeshMessage, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect("127.0.0.1:9000").await?;
    let data = serde_json::to_vec(msg)?;
    let len = (data.len() as u32).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&data).await?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let msg_len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

async fn fetch_nodes_full() -> Result<Vec<NodeRecordFull>, Box<dyn std::error::Error>> {
    let lite_list: Vec<NodeRecordLite> = match send_recv(&MeshMessage::RequestNodes).await? {
        MeshMessage::NodeList(nodes) => nodes,
        other => return Err(format!("Unexpected response: {:?}", other).into()),
    };

    let mut full = Vec::with_capacity(lite_list.len());
    for lite in lite_list {
        match send_recv(&MeshMessage::RequestNodeInfo(lite.id)).await? {
            MeshMessage::NodeInfo(info) => full.push(info),
            other => return Err(format!("Unexpected response: {:?}", other).into()),
        }
    }
    Ok(full)
}

fn format_models(node: &NodeRecordFull) -> String {
    if node.models.is_empty() {
        return "-".into();
    }
    node.models
        .iter()
        .map(|m| format!("{} ({:?})", m.model_name, m.state))
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_table(nodes: Vec<NodeRecordFull>) {
    let mut table = Table::new();
    table.add_row(row![
        "ID",
        "Hostname",
        "IP",
        "Role",
        "Last Seen (ms)",
        "Models"
    ]);

    for n in &nodes {
        table.add_row(row![
            n.id,
            n.hostname,
            n.ip,
            format!("{:?}", n.role),
            n.last_heartbeat_ms,
            format_models(n),
        ]);
    }

    table.printstd();
}
