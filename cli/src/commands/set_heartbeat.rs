use shared::{messages::NodeRecordLite, MeshMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{timeout, Duration};

pub async fn run(coordinator: &str, node: String, secs: u64) {
    let node_id = if looks_like_uuid(&node) {
        node.clone()
    } else {
        match resolve_node(coordinator, &node).await {
            Ok(Some(id)) => id,
            Ok(None) => {
                eprintln!(
                    "Error: no connected node found matching '{node}' — use hostname, IP, or UUID"
                );
                return;
            }
            Err(e) => {
                eprintln!("Error: {e}");
                return;
            }
        }
    };

    let host = coordinator.split(':').next().unwrap_or(coordinator);
    let http_port: u16 = std::env::var("MESH_HTTP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9001);
    let token = std::env::var("MESH_AUTH_TOKEN").unwrap_or_default();
    let token = token.trim().to_string();

    match post_interval(host, http_port, &node_id, &token, secs).await {
        Ok(()) => println!("Heartbeat interval set to {secs}s on {node}"),
        Err(e) => eprintln!("Error: {e}"),
    }
}

/// Accept UUID, IP address, or hostname (case-insensitive).
async fn resolve_node(
    coordinator: &str,
    ident: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut stream = crate::connection::connect(coordinator).await?;
    let nodes: Vec<NodeRecordLite> =
        match crate::connection::send_recv(&mut stream, &MeshMessage::RequestNodes).await? {
            MeshMessage::NodeList(nodes) => nodes,
            _ => return Ok(None),
        };
    let ident_lower = ident.to_lowercase();
    Ok(nodes
        .into_iter()
        .find(|n| n.ip == ident || n.hostname.to_lowercase() == ident_lower)
        .map(|n| n.id))
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|&c| c == '-').count() == 4
}

async fn post_interval(
    host: &str,
    http_port: u16,
    node_id: &str,
    token: &str,
    secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = format!(r#"{{"secs":{secs}}}"#);
    let request = format!(
        "POST /api/nodes/{node_id}/heartbeat-interval?token={token} HTTP/1.1\r\n\
         Host: {host}:{http_port}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    let mut stream = tokio::net::TcpStream::connect(format!("{host}:{http_port}")).await?;
    stream.write_all(request.as_bytes()).await?;

    // Read until end-of-headers (\r\n\r\n); don't wait for connection close
    // as Axum keeps connections alive regardless of Connection: close.
    let mut resp = Vec::new();
    let mut chunk = [0u8; 512];
    timeout(Duration::from_secs(5), async {
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            resp.extend_from_slice(&chunk[..n]);
            if resp.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        Ok::<_, std::io::Error>(())
    })
    .await
    .map_err(|_| "timed out waiting for HTTP response")??;

    let resp_str = String::from_utf8_lossy(&resp);
    let status = resp_str
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    match status {
        200 => Ok(()),
        401 => Err("unauthorized — check MESH_AUTH_TOKEN".into()),
        404 => Err(format!("node '{node_id}' not connected").into()),
        400 => Err("invalid interval — must be 1–3600 seconds".into()),
        _ => Err(format!("HTTP {status}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_uuid_accepts_valid_uuid() {
        assert!(looks_like_uuid("550e8400-e29b-41d4-a716-446655440000"));
    }

    #[test]
    fn looks_like_uuid_rejects_hostname() {
        assert!(!looks_like_uuid("beelink1"));
    }

    #[test]
    fn looks_like_uuid_rejects_ip() {
        assert!(!looks_like_uuid("192.168.1.14"));
    }

    #[test]
    fn looks_like_uuid_rejects_short_string() {
        assert!(!looks_like_uuid("abc-def"));
    }
}
