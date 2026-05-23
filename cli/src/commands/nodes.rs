use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, CellAlignment, ContentArrangement, Table};
use shared::{MeshMessage, NodeRecordFull, NodeRecordLite};

pub async fn run(coordinator: &str) {
    match fetch_nodes_full(coordinator).await {
        Ok(nodes) => print_table(nodes),
        Err(e) => println!("Error: {}", e),
    }
}

async fn send_recv(
    coordinator: &str,
    msg: &MeshMessage,
) -> Result<MeshMessage, Box<dyn std::error::Error>> {
    let mut stream = crate::connection::connect(coordinator).await?;
    Ok(crate::connection::send_recv(&mut stream, msg).await?)
}

async fn fetch_nodes_full(
    coordinator: &str,
) -> Result<Vec<NodeRecordFull>, Box<dyn std::error::Error>> {
    let lite_list: Vec<NodeRecordLite> =
        match send_recv(coordinator, &MeshMessage::RequestNodes).await? {
            MeshMessage::NodeList(nodes) => nodes,
            other => return Err(format!("Unexpected response: {:?}", other).into()),
        };

    let mut full = Vec::with_capacity(lite_list.len());
    for lite in lite_list {
        match send_recv(coordinator, &MeshMessage::RequestNodeInfo(lite.id)).await? {
            MeshMessage::NodeInfo(info) => full.push(info),
            other => return Err(format!("Unexpected response: {:?}", other).into()),
        }
    }
    Ok(full)
}

fn format_last_seen(ms: u128) -> String {
    if ms < 2_000 {
        return format!("{}ms", ms);
    }
    let s = ms / 1_000;
    if s < 60 {
        return format!("{}s", s);
    }
    let m = s / 60;
    let s = s % 60;
    format!("{}m {}s", m, s)
}

fn format_models(node: &NodeRecordFull) -> String {
    if node.models.is_empty() {
        return "-".into();
    }
    node.models
        .iter()
        .map(|m| format!("{} ({:?})", m.model_name, m.state))
        .collect::<Vec<_>>()
        .join("\n")
}

fn print_table(nodes: Vec<NodeRecordFull>) {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Hostname").set_alignment(CellAlignment::Left),
            Cell::new("IP").set_alignment(CellAlignment::Left),
            Cell::new("Role").set_alignment(CellAlignment::Left),
            Cell::new("Last Seen").set_alignment(CellAlignment::Right),
            Cell::new("Models").set_alignment(CellAlignment::Left),
        ]);

    for n in &nodes {
        table.add_row(vec![
            Cell::new(&n.hostname),
            Cell::new(&n.ip),
            Cell::new(format!("{:?}", n.role)),
            Cell::new(format_last_seen(n.last_heartbeat_ms)).set_alignment(CellAlignment::Right),
            Cell::new(format_models(n)),
        ]);
    }

    println!("{table}");
}
