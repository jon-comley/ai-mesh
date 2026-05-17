use crossterm::{
    execute,
    terminal::{Clear, ClearType},
};
use prettytable::{row, Table};
use shared::{MeshMessage, NodeRecordLite};
use std::io::stdout;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, Duration};

pub async fn run() {
    loop {
        match fetch_nodes().await {
            Ok(nodes) => {
                redraw(nodes);
            }
            Err(e) => {
                println!("Error: {}", e);
            }
        }

        sleep(Duration::from_secs(1)).await;
    }
}

async fn fetch_nodes() -> Result<Vec<NodeRecordLite>, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect("127.0.0.1:9000").await?;

    let msg = MeshMessage::RequestNodes;
    let data = serde_json::to_vec(&msg)?;
    let len = (data.len() as u32).to_le_bytes();

    stream.write_all(&len).await?;
    stream.write_all(&data).await?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let msg_len = u32::from_le_bytes(len_buf) as usize;

    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf).await?;

    match serde_json::from_slice(&buf)? {
        MeshMessage::NodeList(nodes) => Ok(nodes),
        other => Err(format!("Unexpected response: {:?}", other).into()),
    }
}

fn redraw(nodes: Vec<NodeRecordLite>) {
    // Clear screen
    let mut out = stdout();
    execute!(out, Clear(ClearType::All)).unwrap();

    println!("Watching mesh… (Ctrl+C to stop)\n");

    let mut table = Table::new();
    table.add_row(row!["ID", "Hostname", "IP", "Last Seen (ms)"]);

    for n in nodes {
        table.add_row(row![n.id, n.hostname, n.ip, n.last_heartbeat_ms,]);
    }

    table.printstd();
}
