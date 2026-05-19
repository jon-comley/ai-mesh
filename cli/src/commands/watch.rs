use chrono::Local;
use crossterm::{
    cursor, execute,
    terminal::{Clear, ClearType},
};
use prettytable::{row, Table};
use shared::{MeshMessage, NodeRecordFull, NodeRecordLite};
use std::collections::HashMap;
use std::io::{stdout, Write};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, Duration};

const MAX_LOG: usize = 20;

pub async fn run() {
    let mut prev: HashMap<String, NodeRecordFull> = HashMap::new();
    let mut event_log: Vec<String> = Vec::new();
    let mut prev_lines: u16 = 0;
    let mut first = true;

    loop {
        match fetch_nodes_full().await {
            Ok(nodes) => {
                if !first {
                    let events = diff(&prev, &nodes);
                    event_log.extend(events);
                    if event_log.len() > MAX_LOG {
                        event_log.drain(..event_log.len() - MAX_LOG);
                    }
                }
                first = false;
                prev = nodes.iter().cloned().map(|n| (n.id.clone(), n)).collect();
                prev_lines = redraw(&nodes, &event_log, prev_lines);
            }
            Err(e) => {
                if prev_lines > 0 {
                    execute!(stdout(), cursor::MoveUp(prev_lines)).unwrap();
                }
                eprintln!("Error: {e}");
                execute!(stdout(), Clear(ClearType::FromCursorDown)).unwrap();
                prev_lines = 1;
            }
        }
        sleep(Duration::from_secs(1)).await;
    }
}

fn diff(prev: &HashMap<String, NodeRecordFull>, current: &[NodeRecordFull]) -> Vec<String> {
    let ts = Local::now().format("%H:%M:%S");
    let mut events = Vec::new();

    for n in current {
        if !prev.contains_key(&n.id) {
            events.push(format!("{ts}  [+] {} joined ({:?})", n.hostname, n.role));
        } else {
            let p = &prev[&n.id];
            let prev_models: HashMap<&str, _> = p
                .models
                .iter()
                .map(|m| (m.model_name.as_str(), &m.state))
                .collect();
            for m in &n.models {
                match prev_models.get(m.model_name.as_str()) {
                    None => events.push(format!(
                        "{ts}  [M] {} appeared on {} ({:?})",
                        m.model_name, n.hostname, m.state
                    )),
                    Some(ps) if *ps != &m.state => events.push(format!(
                        "{ts}  [M] {} → {:?} on {}",
                        m.model_name, m.state, n.hostname
                    )),
                    _ => {}
                }
            }
            for name in prev_models.keys() {
                if !n.models.iter().any(|m| m.model_name == *name) {
                    events.push(format!("{ts}  [M] {} removed from {}", name, n.hostname));
                }
            }
        }
    }

    for (id, n) in prev {
        if !current.iter().any(|c| &c.id == id) {
            events.push(format!("{ts}  [-] {} left the mesh", n.hostname));
        }
    }

    events
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

fn redraw(nodes: &[NodeRecordFull], event_log: &[String], prev_lines: u16) -> u16 {
    let mut out = stdout();

    if prev_lines > 0 {
        execute!(out, cursor::MoveUp(prev_lines)).unwrap();
    }

    let header = format!(
        "ai-mesh  |  {}  |  {} node(s)  |  Ctrl+C to stop",
        Local::now().format("%H:%M:%S"),
        nodes.len(),
    );
    println!("{header}");

    let mut table = Table::new();
    table.add_row(row![
        "ID",
        "Hostname",
        "IP",
        "Role",
        "Last Seen (ms)",
        "Models"
    ]);
    for n in nodes {
        table.add_row(row![
            n.id,
            n.hostname,
            n.ip,
            format!("{:?}", n.role),
            n.last_heartbeat_ms,
            format_models(n),
        ]);
    }
    let table_str = format!("{table}");
    print!("{table_str}");

    let term_width = crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80);
    let mut table_lines: u16 = 0;
    for line in table_str.lines() {
        let len = line.len();
        table_lines += if len == 0 {
            1
        } else {
            len.div_ceil(term_width) as u16
        };
    }
    let mut total_lines = 1 + table_lines;

    if !event_log.is_empty() {
        println!();
        for line in event_log {
            println!("{line}");
            total_lines += line.len().div_ceil(term_width).max(1) as u16;
        }
        total_lines += 1; // spacer println!()
    }

    execute!(out, Clear(ClearType::FromCursorDown)).unwrap();
    out.flush().unwrap();
    total_lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{ModelAllocationFull, ModelLifecycleState, NodeRole};

    fn make_node(id: &str, hostname: &str, models: Vec<ModelAllocationFull>) -> NodeRecordFull {
        NodeRecordFull {
            id: id.into(),
            hostname: hostname.into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
            last_heartbeat_ms: 0,
            hardware: None,
            capabilities: None,
            models,
        }
    }

    fn make_model(name: &str, state: ModelLifecycleState) -> ModelAllocationFull {
        ModelAllocationFull {
            model_name: name.into(),
            size_mb: 100,
            state,
        }
    }

    fn prev(nodes: &[NodeRecordFull]) -> HashMap<String, NodeRecordFull> {
        nodes.iter().cloned().map(|n| (n.id.clone(), n)).collect()
    }

    #[test]
    fn no_change_produces_no_events() {
        let node = make_node("id1", "pi", vec![]);
        let events = diff(&prev(std::slice::from_ref(&node)), &[node]);
        assert!(events.is_empty());
    }

    #[test]
    fn node_join_detected() {
        let node = make_node("id1", "pi", vec![]);
        let events = diff(&HashMap::new(), &[node]);
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("[+]"));
        assert!(events[0].contains("pi"));
    }

    #[test]
    fn node_leave_detected() {
        let node = make_node("id1", "pi", vec![]);
        let events = diff(&prev(&[node]), &[]);
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("[-]"));
        assert!(events[0].contains("pi"));
    }

    #[test]
    fn model_state_change_detected() {
        let old = make_node(
            "id1",
            "pi",
            vec![make_model("qwen", ModelLifecycleState::Loading)],
        );
        let new = make_node(
            "id1",
            "pi",
            vec![make_model("qwen", ModelLifecycleState::Ready)],
        );
        let events = diff(&prev(&[old]), &[new]);
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("[M]"));
        assert!(events[0].contains("qwen"));
        assert!(events[0].contains("Ready"));
    }

    #[test]
    fn new_model_on_existing_node_detected() {
        let old = make_node("id1", "pi", vec![]);
        let new = make_node(
            "id1",
            "pi",
            vec![make_model("qwen", ModelLifecycleState::Loading)],
        );
        let events = diff(&prev(&[old]), &[new]);
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("[M]"));
        assert!(events[0].contains("qwen"));
    }

    #[test]
    fn unchanged_model_produces_no_event() {
        let model = make_model("qwen", ModelLifecycleState::Ready);
        let old = make_node("id1", "pi", vec![model.clone()]);
        let new = make_node("id1", "pi", vec![model]);
        let events = diff(&prev(&[old]), &[new]);
        assert!(events.is_empty());
    }

    #[test]
    fn model_removal_detected() {
        let old = make_node(
            "id1",
            "pi",
            vec![
                make_model("qwen", ModelLifecycleState::Ready),
                make_model("llama", ModelLifecycleState::Ready),
            ],
        );
        let new = make_node(
            "id1",
            "pi",
            vec![make_model("qwen", ModelLifecycleState::Ready)],
        );
        let events = diff(&prev(&[old]), &[new]);
        assert!(events
            .iter()
            .any(|e| e.contains("[M]") && e.contains("llama") && e.contains("removed")));
    }
}
