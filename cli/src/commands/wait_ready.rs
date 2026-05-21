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
    widgets::{Block, Borders, Paragraph, Row, Table as RatatuiTable},
    Frame, Terminal,
};
use shared::{MeshMessage, ModelLifecycleState, NodeRecordFull, NodeRecordLite};
use std::collections::HashSet;
use std::io::{stdout, IsTerminal};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Duration;

struct State {
    coordinator: String,
    nodes: Vec<NodeRecordFull>,
    target_ips: HashSet<String>,
    elapsed_secs: u64,
    timeout_secs: u64,
    last_error: Option<String>,
    /// Some(n) = in linger phase with n seconds remaining before exit.
    linger_remaining: Option<u64>,
}

impl State {
    fn ready_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| self.target_ips.contains(&n.ip))
            .filter(|n| {
                n.models
                    .iter()
                    .any(|m| m.state == ModelLifecycleState::Ready)
            })
            .count()
    }

    fn all_ready(&self) -> bool {
        !self.target_ips.is_empty() && self.ready_count() == self.target_ips.len()
    }
}

/// Returns true = all Ready, false = timeout or user-interrupted before ready.
/// Falls back to plain-text progress when stdout is not a TTY (e.g. piped or
/// called from a script) so callers never hit an ENXIO panic.
pub async fn run(coordinator: &str, target_ips: Vec<String>, timeout_secs: u64) -> bool {
    if !std::io::stdin().is_terminal() {
        return run_plain(coordinator, target_ips, timeout_secs).await;
    }
    run_tui(coordinator, target_ips, timeout_secs).await
}

async fn run_plain(coordinator: &str, target_ips: Vec<String>, timeout_secs: u64) -> bool {
    let start = std::time::Instant::now();
    let mut state = State {
        coordinator: coordinator.to_string(),
        nodes: vec![],
        target_ips: target_ips.into_iter().collect(),
        elapsed_secs: 0,
        timeout_secs,
        last_error: None,
        linger_remaining: None,
    };

    let mut poll_tick = tokio::time::interval(Duration::from_secs(3));
    poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        poll_tick.tick().await;
        match fetch_nodes_full(&state.coordinator).await {
            Ok(nodes) => {
                state.nodes = nodes;
                state.last_error = None;
            }
            Err(e) => state.last_error = Some(e.to_string()),
        }
        state.elapsed_secs = start.elapsed().as_secs();
        let ready = state.ready_count();
        let total = state.target_ips.len();
        println!(
            "wait-ready: {ready}/{total} Ready  |  {}s elapsed",
            state.elapsed_secs
        );
        if state.all_ready() {
            println!("wait-ready: all models Ready after {}s", state.elapsed_secs);
            return true;
        }
        if state.elapsed_secs >= state.timeout_secs {
            println!("wait-ready: timeout after {}s", state.elapsed_secs);
            return false;
        }
    }
}

async fn run_tui(coordinator: &str, target_ips: Vec<String>, timeout_secs: u64) -> bool {
    set_panic_hook();
    let mut terminal = setup_terminal();

    let start = std::time::Instant::now();
    let mut state = State {
        coordinator: coordinator.to_string(),
        nodes: vec![],
        target_ips: target_ips.into_iter().collect(),
        elapsed_secs: 0,
        timeout_secs,
        last_error: None,
        linger_remaining: None,
    };

    // Keyboard input task: signals quit_tx when the user presses q / Ctrl+C / Esc.
    // stop_flag tells it to exit cleanly when we're done so the runtime can shut down.
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_clone = stop_flag.clone();
    let (quit_tx, mut quit_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::task::spawn_blocking(move || {
        while !stop_clone.load(Ordering::Relaxed) {
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
        }
    });

    let mut poll_tick = tokio::time::interval(Duration::from_secs(3));
    poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // ── polling loop ──────────────────────────────────────────────────────────
    let all_ready = loop {
        tokio::select! {
            _ = poll_tick.tick() => {
                match fetch_nodes_full(&state.coordinator).await {
                    Ok(nodes) => { state.nodes = nodes; state.last_error = None; }
                    Err(e)    => state.last_error = Some(e.to_string()),
                }
                state.elapsed_secs = start.elapsed().as_secs();
                terminal.draw(|f| ui(f, &state)).unwrap();

                if state.all_ready() { break true; }
                if state.elapsed_secs >= state.timeout_secs { break false; }
            }
            _ = &mut quit_rx => break false,
        }
    };

    // ── linger phase: 5-second live countdown ────────────────────────────────
    if all_ready {
        let mut linger_tick = tokio::time::interval(Duration::from_secs(1));
        linger_tick.tick().await; // consume the immediate first tick
        let mut remaining: u64 = 5;

        loop {
            state.linger_remaining = Some(remaining);
            terminal.draw(|f| ui(f, &state)).unwrap();

            if remaining == 0 {
                break;
            }

            tokio::select! {
                _ = linger_tick.tick() => remaining -= 1,
                _ = &mut quit_rx => { remaining = 0; }
            }
        }
    }

    // ── clean up ──────────────────────────────────────────────────────────────
    stop_flag.store(true, Ordering::Relaxed); // unblock the keyboard polling thread
    restore_terminal(terminal);

    all_ready
}

// ── ratatui UI ────────────────────────────────────────────────────────────────

fn ui(f: &mut Frame, state: &State) {
    let ready = state.ready_count();
    let total = state.target_ips.len();

    let (status_text, status_style) = match &state.last_error {
        Some(err) => (
            format!(" ai-mesh  |  {}  |  ERROR: {}", Local::now().format("%H:%M:%S"), err),
            Style::default().bg(Color::Red).fg(Color::White),
        ),
        None if state.linger_remaining.is_some() => (
            format!(
                " ai-mesh  |  {}  |  All models Ready ({elapsed}s)  |  exiting in {n}s…  |  q to skip",
                Local::now().format("%H:%M:%S"),
                elapsed = state.elapsed_secs,
                n = state.linger_remaining.unwrap(),
            ),
            Style::default().bg(Color::Green).fg(Color::Black),
        ),
        None if state.all_ready() => (
            format!(
                " ai-mesh  |  {}  |  All models Ready ({elapsed}s)",
                Local::now().format("%H:%M:%S"),
                elapsed = state.elapsed_secs,
            ),
            Style::default().bg(Color::Green).fg(Color::Black),
        ),
        None => (
            format!(
                " ai-mesh  |  {}  |  {ready}/{total} Ready  |  {}s elapsed  |  q to abort",
                Local::now().format("%H:%M:%S"),
                state.elapsed_secs,
            ),
            Style::default().bg(Color::DarkGray).fg(Color::White),
        ),
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(5)])
        .split(f.area());

    f.render_widget(Paragraph::new(status_text).style(status_style), chunks[0]);

    let header = Row::new(["Hostname", "IP", "Role", "Last Seen", "Models"])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(1);

    let rows: Vec<Row> = state
        .nodes
        .iter()
        .map(|n| {
            let is_target = state.target_ips.contains(&n.ip);
            let row_style = if is_target
                && n.models
                    .iter()
                    .any(|m| m.state == ModelLifecycleState::Ready)
            {
                Style::default().fg(Color::Green)
            } else if is_target
                && n.models
                    .iter()
                    .any(|m| m.state == ModelLifecycleState::Loading)
            {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            Row::new(vec![
                n.hostname.clone(),
                n.ip.clone(),
                format!("{:?}", n.role),
                format_last_seen(n.last_heartbeat_ms),
                format_models(n),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Length(14),
        Constraint::Length(15),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Min(10),
    ];

    f.render_widget(
        RatatuiTable::new(rows, widths)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(" Nodes ")),
        chunks[1],
    );
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn format_last_seen(ms: u128) -> String {
    if ms < 2_000 {
        return format!("{}ms", ms);
    }
    let s = ms / 1_000;
    if s < 60 {
        return format!("{}s", s);
    }
    format!("{}m {}s", s / 60, s % 60)
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

// ── coordinator comms ─────────────────────────────────────────────────────────

async fn send_recv(
    coordinator: &str,
    msg: &MeshMessage,
) -> Result<MeshMessage, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(coordinator).await?;
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

async fn fetch_nodes_full(
    coordinator: &str,
) -> Result<Vec<NodeRecordFull>, Box<dyn std::error::Error>> {
    let lite_list: Vec<NodeRecordLite> =
        match send_recv(coordinator, &MeshMessage::RequestNodes).await? {
            MeshMessage::NodeList(nodes) => nodes,
            other => return Err(format!("Unexpected: {other:?}").into()),
        };
    let mut tasks = tokio::task::JoinSet::new();
    for lite in lite_list {
        let coord = coordinator.to_string();
        tasks.spawn(async move {
            match send_recv(&coord, &MeshMessage::RequestNodeInfo(lite.id)).await {
                Ok(MeshMessage::NodeInfo(info)) => Some(info),
                _ => None,
            }
        });
    }
    let mut full = Vec::new();
    while let Some(res) = tasks.join_next().await {
        if let Ok(Some(info)) = res {
            full.push(info);
        }
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{ModelAllocationFull, ModelLifecycleState, NodeRole};

    fn make_state(target_ips: Vec<&str>, nodes: Vec<NodeRecordFull>) -> State {
        State {
            coordinator: "127.0.0.1:9000".into(),
            nodes,
            target_ips: target_ips.into_iter().map(|s| s.to_string()).collect(),
            elapsed_secs: 0,
            timeout_secs: 60,
            last_error: None,
            linger_remaining: None,
        }
    }

    fn ready_node(ip: &str) -> NodeRecordFull {
        NodeRecordFull {
            id: "id".into(),
            hostname: "host".into(),
            ip: ip.to_string(),
            role: NodeRole::Compute,
            hardware: None,
            capabilities: None,
            last_heartbeat_ms: 0,
            models: vec![ModelAllocationFull {
                model_name: "m".into(),
                size_mb: 0,
                state: ModelLifecycleState::Ready,
            }],
        }
    }

    fn loading_node(ip: &str) -> NodeRecordFull {
        let mut n = ready_node(ip);
        n.models[0].state = ModelLifecycleState::Loading;
        n
    }

    #[test]
    fn format_last_seen_ms() {
        assert_eq!(format_last_seen(0), "0ms");
        assert_eq!(format_last_seen(1999), "1999ms");
    }

    #[test]
    fn format_last_seen_seconds() {
        assert_eq!(format_last_seen(2000), "2s");
        assert_eq!(format_last_seen(59_000), "59s");
    }

    #[test]
    fn format_last_seen_minutes() {
        assert_eq!(format_last_seen(60_000), "1m 0s");
        assert_eq!(format_last_seen(90_000), "1m 30s");
        assert_eq!(format_last_seen(3_661_000), "61m 1s");
    }

    #[test]
    fn ready_count_counts_ready_targets() {
        let state = make_state(
            vec!["1.1.1.1", "2.2.2.2"],
            vec![ready_node("1.1.1.1"), loading_node("2.2.2.2")],
        );
        assert_eq!(state.ready_count(), 1);
    }

    #[test]
    fn ready_count_ignores_non_target_nodes() {
        let state = make_state(
            vec!["1.1.1.1"],
            vec![ready_node("1.1.1.1"), ready_node("9.9.9.9")],
        );
        assert_eq!(state.ready_count(), 1);
    }

    #[test]
    fn all_ready_false_when_no_targets() {
        let state = make_state(vec![], vec![ready_node("1.1.1.1")]);
        assert!(!state.all_ready());
    }

    #[test]
    fn all_ready_true_when_all_ready() {
        let state = make_state(
            vec!["1.1.1.1", "2.2.2.2"],
            vec![ready_node("1.1.1.1"), ready_node("2.2.2.2")],
        );
        assert!(state.all_ready());
    }

    #[test]
    fn all_ready_false_when_one_loading() {
        let state = make_state(
            vec!["1.1.1.1", "2.2.2.2"],
            vec![ready_node("1.1.1.1"), loading_node("2.2.2.2")],
        );
        assert!(!state.all_ready());
    }
}
