use chrono::Local;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Row, Table},
    Frame, Terminal,
};
use shared::{MeshMessage, NodeRecordFull, NodeRecordLite};
use std::collections::HashMap;
use std::io::stdout;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Duration;

const MAX_LOG: usize = 20;

struct AppState {
    nodes: Vec<NodeRecordFull>,
    event_log: Vec<String>,
    prev: HashMap<String, NodeRecordFull>,
    status_line: String,
    last_error: Option<String>,
}

impl AppState {
    fn new() -> Self {
        Self {
            nodes: vec![],
            event_log: vec![],
            prev: HashMap::new(),
            status_line: String::new(),
            last_error: None,
        }
    }

    fn update_status(&mut self) {
        self.status_line = format!(
            "ai-mesh  |  {}  |  {} node(s)  |  Ctrl+C to stop",
            Local::now().format("%H:%M:%S"),
            self.nodes.len(),
        );
    }
}

fn setup_terminal() -> Terminal<CrosstermBackend<std::io::Stdout>> {
    enable_raw_mode().unwrap();
    let mut out = stdout();
    execute!(out, EnterAlternateScreen).unwrap();
    Terminal::new(CrosstermBackend::new(out)).unwrap()
}

fn restore_terminal(mut terminal: Terminal<CrosstermBackend<std::io::Stdout>>) {
    disable_raw_mode().unwrap();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).unwrap();
    terminal.show_cursor().unwrap();
}

fn set_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        original(info);
    }));
}

pub async fn run() {
    set_panic_hook();
    let mut terminal = setup_terminal();

    let mut state = AppState::new();
    state.update_status();
    terminal.draw(|f| ui(f, &state)).unwrap();

    // Spawn a blocking task to watch for quit keys (raw mode swallows SIGINT).
    let (quit_tx, mut quit_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::task::spawn_blocking(move || loop {
        if event::poll(std::time::Duration::from_millis(100)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let quit = key.code == KeyCode::Esc
                    || key.code == KeyCode::Char('q')
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL));
                if quit {
                    let _ = quit_tx.send(());
                    return;
                }
            }
        }
    });

    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = tick.tick() => {
                match fetch_nodes_full().await {
                    Ok(nodes) => {
                        let events = diff(&state.prev, &nodes);
                        state.event_log.extend(events);
                        if state.event_log.len() > MAX_LOG {
                            state.event_log.drain(..state.event_log.len() - MAX_LOG);
                        }
                        state.prev = nodes.iter().cloned().map(|n| (n.id.clone(), n)).collect();
                        state.nodes = nodes;
                        state.last_error = None;
                    }
                    Err(e) => {
                        state.last_error = Some(e.to_string());
                    }
                }
                state.update_status();
                terminal.draw(|f| ui(f, &state)).unwrap();
            }
            _ = &mut quit_rx => {
                break;
            }
        }
    }

    restore_terminal(terminal);
}

fn ui(f: &mut Frame, state: &AppState) {
    let log_height = if state.event_log.is_empty() { 0 } else { 22 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(log_height),
        ])
        .split(f.area());

    // Status bar
    let (status_text, status_style) = match &state.last_error {
        Some(err) => (
            format!(" ai-mesh  |  ERROR: {}", err),
            Style::default().bg(Color::Red).fg(Color::White),
        ),
        None => (
            format!(" {}", state.status_line),
            Style::default().bg(Color::DarkGray).fg(Color::White),
        ),
    };
    f.render_widget(Paragraph::new(status_text).style(status_style), chunks[0]);

    // Node table
    let header = Row::new(["ID", "Hostname", "IP", "Role", "Last Seen (ms)", "Models"])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(1);

    let rows: Vec<Row> = state
        .nodes
        .iter()
        .map(|n| {
            Row::new(vec![
                n.id.clone(),
                n.hostname.clone(),
                n.ip.clone(),
                format!("{:?}", n.role),
                n.last_heartbeat_ms.to_string(),
                format_models(n),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(36),
        Constraint::Length(18),
        Constraint::Length(15),
        Constraint::Length(12),
        Constraint::Length(14),
        Constraint::Min(10),
    ];

    f.render_widget(
        Table::new(rows, widths)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(" Nodes ")),
        chunks[1],
    );

    // Event log
    if log_height > 0 {
        let lines: Vec<Line> = state
            .event_log
            .iter()
            .map(|s| Line::from(s.as_str()))
            .collect();
        f.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(" Events ")),
            chunks[2],
        );
    }
}

// ── data fetching ─────────────────────────────────────────────────────────────

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

// ── diff ──────────────────────────────────────────────────────────────────────

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

// ── tests ─────────────────────────────────────────────────────────────────────

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
