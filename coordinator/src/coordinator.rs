use crate::http::state::DashboardState;
use crate::registry::Registry;
use crate::server::Server;
use crate::tls as coord_tls;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// Generate a cryptographically random 64-character lowercase hex auth token (32 bytes).
pub(crate) fn generate_auth_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).expect("failed to generate auth token");
    buf.iter().map(|b| format!("{:02x}", b)).collect()
}

pub struct CoordinatorConfig {
    pub listen_addr: String,
}

pub struct Coordinator {
    pub config: CoordinatorConfig,
    pub registry: Arc<Mutex<Registry>>,
    /// When false (default for `new()`) TLS is skipped — useful for in-memory tests.
    pub tls_enabled: bool,
}

impl Coordinator {
    /// In-memory registry — suitable for tests. TLS is disabled.
    pub fn new(listen_addr: impl Into<String>) -> Self {
        Self {
            config: CoordinatorConfig {
                listen_addr: listen_addr.into(),
            },
            registry: Arc::new(Mutex::new(Registry::new())),
            tls_enabled: false,
        }
    }

    /// SQLite-backed registry — loads existing state on startup. TLS is enabled.
    pub fn new_persistent(listen_addr: impl Into<String>, db_path: &str) -> Self {
        let registry = Registry::open(db_path)
            .unwrap_or_else(|e| panic!("failed to open registry DB at {db_path}: {e}"));
        Self {
            config: CoordinatorConfig {
                listen_addr: listen_addr.into(),
            },
            registry: Arc::new(Mutex::new(registry)),
            tls_enabled: true,
        }
    }

    pub async fn start(&self) -> (JoinHandle<()>, Arc<DashboardState>) {
        let mut server = Server::new(self.config.listen_addr.clone(), self.registry.clone());

        let insecure = !self.tls_enabled || std::env::var("MESH_INSECURE").as_deref() == Ok("1");

        if insecure {
            if self.tls_enabled {
                warn!("MESH_INSECURE=1 — TLS disabled. Do not use in production.");
            }
        } else {
            let cert_dir = coord_tls::cert_dir();
            let cert_path = cert_dir.join("coordinator.crt");
            let key_path = cert_dir.join("coordinator.key");
            let (cert_der, key_der) = coord_tls::load_or_generate(&cert_path, &key_path);
            let fingerprint = coord_tls::log_fingerprint(&cert_der);
            server.tls = Some(coord_tls::make_acceptor(cert_der, key_der));

            // Collect valid auth tokens from env.
            let mut tokens: Vec<String> = vec![];
            if let Ok(t) = std::env::var("MESH_AUTH_TOKEN") {
                let t = t.trim().to_string();
                if !t.is_empty() {
                    tokens.push(t);
                }
            }
            let next_token: Option<String> =
                std::env::var("MESH_AUTH_TOKEN_NEXT").ok().and_then(|t| {
                    let t = t.trim().to_string();
                    if t.is_empty() { None } else { Some(t) }
                });
            if let Some(ref t) = next_token {
                tokens.push(t.clone());
            }

            // Auto-generate a token when none is configured so the cluster is
            // always authenticated. The generated token is written to
            // coordinator.state and distributed to nodes by restart-coordinator.
            if tokens.is_empty() {
                let token = generate_auth_token();
                info!(
                    "auth token auto-generated — run 'just restart-coordinator' to distribute to nodes"
                );
                tokens.push(token);
            } else {
                info!(
                    "auth token validation enabled ({} token(s) accepted)",
                    tokens.len()
                );
            }
            crate::state::write(&fingerprint, &tokens, next_token.as_deref());
            let tokens = Arc::new(tokens);
            server.auth_tokens = tokens.clone();
            let dashboard = DashboardState::new(tokens, server.connections.clone());
            server.dashboard = Some(dashboard.clone());
            let handle = tokio::spawn(async move {
                let _ = server.run().await;
            });
            return (handle, dashboard);
        }

        // Insecure path — collect tokens without writing state (no fingerprint to record).
        let mut tokens: Vec<String> = vec![];
        if let Ok(t) = std::env::var("MESH_AUTH_TOKEN") {
            let t = t.trim().to_string();
            if !t.is_empty() {
                tokens.push(t);
            }
        }
        if let Ok(t) = std::env::var("MESH_AUTH_TOKEN_NEXT") {
            let t = t.trim().to_string();
            if !t.is_empty() {
                tokens.push(t);
            }
        }
        let tokens = Arc::new(tokens);
        server.auth_tokens = tokens.clone();
        let dashboard = DashboardState::new(tokens, server.connections.clone());
        server.dashboard = Some(dashboard.clone());

        let handle = tokio::spawn(async move {
            let _ = server.run().await;
        });
        (handle, dashboard)
    }

    pub fn node_count(&self) -> usize {
        self.registry.lock().unwrap().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{HeartbeatPayload, MeshMessage, NodeIdentity, NodeRole};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[test]
    fn generated_token_is_64_hex_chars() {
        let token = generate_auth_token();
        assert_eq!(token.len(), 64, "token should be 64 hex chars (32 bytes)");
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit()),
            "token should be lowercase hex"
        );
    }

    #[test]
    fn generated_tokens_are_unique() {
        let a = generate_auth_token();
        let b = generate_auth_token();
        assert_ne!(a, b, "consecutive tokens should differ");
    }

    async fn send_message(addr: &str, msg: &MeshMessage) -> MeshMessage {
        let mut stream = TcpStream::connect(addr).await.unwrap();

        let data = serde_json::to_vec(msg).unwrap();
        let len = (data.len() as u32).to_le_bytes();

        stream.write_all(&len).await.unwrap();
        stream.write_all(&data).await.unwrap();

        // Read ack
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.unwrap();
        let msg_len = u32::from_le_bytes(len_buf) as usize;

        let mut buf = vec![0u8; msg_len];
        stream.read_exact(&mut buf).await.unwrap();

        serde_json::from_slice(&buf).unwrap()
    }

    #[tokio::test]
    async fn test_coordinator_receives_heartbeat() {
        let coord = Coordinator::new("127.0.0.1:9002");
        coord.start().await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let ident = NodeIdentity {
            id: "nodeX".into(),
            hostname: "test-host".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        };

        let ack = send_message(
            "127.0.0.1:9002",
            &MeshMessage::Heartbeat(HeartbeatPayload {
                identity: ident.clone(),
                auth_token: String::new(),
                cpu_usage_pct: 0.0,
                ram_used_gb: 0.0,
                ram_total_gb: 0.0,
                gpu_usage_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
            }),
        )
        .await;

        match ack {
            MeshMessage::Acknowledge => {}
            _ => panic!("Expected Acknowledge"),
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(coord.node_count(), 1);
    }
}
