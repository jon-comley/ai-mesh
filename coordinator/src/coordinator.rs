use crate::http::state::{DashboardState, RoomInfo, SceneInfo};
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

fn warm_start_lighting(registry: &Arc<Mutex<Registry>>, dashboard: &Arc<DashboardState>) {
    let reg = registry.lock().unwrap();
    let reports = reg.load_light_states();
    for report in reports {
        dashboard.push_lighting_update(report);
    }

    // Also warm-start placeholders for discovered devices that haven't reported state yet.
    // Suppress internal broadcasts (emit=false) during the loop to prevent massive JSON
    // thundering, then fire a single final update.
    let mut any_new = false;
    let mut lights_by_node: std::collections::HashMap<&String, Vec<String>> = Default::default();
    for (device_id, (node_id, dt)) in reg.all_devices() {
        if *dt == shared::DeviceType::Light {
            lights_by_node
                .entry(node_id)
                .or_default()
                .push(device_id.clone());
        }
    }
    for (node_id, devices) in lights_by_node {
        dashboard.push_device_discovery(node_id, devices, false);
        any_new = true;
    }

    if any_new {
        let devices = dashboard.get_light_snapshot();
        let groups = dashboard.get_group_snapshot();
        let _ = dashboard
            .tx
            .send(crate::http::state::DashboardEvent::LightingUpdate { devices, groups });
    }
}

fn warm_start_sensors(registry: &Arc<Mutex<Registry>>, dashboard: &Arc<DashboardState>) {
    let reports = registry.lock().unwrap().load_sensor_states();
    for report in reports {
        dashboard.push_sensor_update(report);
    }
}

fn warm_start_rooms(registry: &Arc<Mutex<Registry>>, dashboard: &Arc<DashboardState>) {
    let reg = registry.lock().unwrap();
    let room_infos: Vec<RoomInfo> = reg.list_rooms().into_iter().map(RoomInfo::from).collect();
    let names = reg.get_all_device_names();
    drop(reg);
    dashboard.push_rooms_update_with_names(room_infos, names);
}

fn warm_start_scenes(registry: &Arc<Mutex<Registry>>, dashboard: &Arc<DashboardState>) {
    let scenes = registry.lock().unwrap().list_scenes();
    let scene_infos: Vec<SceneInfo> = scenes
        .into_iter()
        .map(|s| {
            let preview_color = s.preview_color();
            SceneInfo {
                id: s.id,
                name: s.name,
                room_id: s.room_id,
                created_at: s.created_at,
                position: s.position,
                preview_color,
                states: s.states,
            }
        })
        .collect();
    dashboard.push_scenes_update(scene_infos);
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

            // Collect valid auth tokens. Precedence:
            //  1. MESH_AUTH_TOKEN env var (explicit override)
            //  2. Token persisted in /var/lib/ai-mesh/coordinator.state from a prior run
            //  3. Freshly generated token (first run, or persisted state missing)
            // Step 2 keeps the token stable across `cargo run` restarts so that
            // bookmarked dashboard URLs (e.g. on a phone) keep working.
            let persisted = crate::state::read().unwrap_or_default();
            let mut tokens: Vec<String> = vec![];
            if let Ok(t) = std::env::var("MESH_AUTH_TOKEN") {
                let t = t.trim().to_string();
                if !t.is_empty() {
                    tokens.push(t);
                }
            }
            let next_token: Option<String> = std::env::var("MESH_AUTH_TOKEN_NEXT")
                .ok()
                .and_then(|t| {
                    let t = t.trim().to_string();
                    if t.is_empty() { None } else { Some(t) }
                })
                .or(persisted.next_token);
            if let Some(ref t) = next_token {
                tokens.push(t.clone());
            }

            if tokens.is_empty() {
                if let Some(prior) = persisted.auth_token {
                    info!("auth token reused from coordinator.state");
                    tokens.push(prior);
                } else {
                    let token = generate_auth_token();
                    info!(
                        "auth token auto-generated — run 'just restart-coordinator' to distribute to nodes"
                    );
                    tokens.push(token);
                }
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
            server.pending_inferences = dashboard.pending_inferences.clone();
            server.pending_intents = dashboard.pending_intents.clone();
            server.pending_streams = dashboard.pending_streams.clone();
            server.dashboard = Some(dashboard.clone());
            warm_start_lighting(&self.registry, &dashboard);
            warm_start_sensors(&self.registry, &dashboard);
            warm_start_rooms(&self.registry, &dashboard);
            warm_start_scenes(&self.registry, &dashboard);
            let handle = tokio::spawn(async move {
                let _ = server.run().await;
            });
            return (handle, dashboard);
        }

        // Insecure path — only collect env tokens when TLS was originally enabled
        // but overridden by MESH_INSECURE=1 (dev override). When tls_enabled is false
        // from the start (i.e. Coordinator::new() in tests), run with no auth so that
        // a MESH_AUTH_TOKEN in the shell environment doesn't silently break tests.
        let mut tokens: Vec<String> = vec![];
        if self.tls_enabled {
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
        }
        let tokens = Arc::new(tokens);
        server.auth_tokens = tokens.clone();
        let dashboard = DashboardState::new(tokens, server.connections.clone());
        server.pending_inferences = dashboard.pending_inferences.clone();
        server.pending_intents = dashboard.pending_intents.clone();
        server.pending_streams = dashboard.pending_streams.clone();
        server.dashboard = Some(dashboard.clone());
        warm_start_lighting(&self.registry, &dashboard);
        warm_start_sensors(&self.registry, &dashboard);
        warm_start_rooms(&self.registry, &dashboard);
        warm_start_scenes(&self.registry, &dashboard);

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

    async fn send_message(addr: &str, msg: &MeshMessage) -> Option<MeshMessage> {
        let mut stream = TcpStream::connect(addr).await.unwrap();

        let data = serde_json::to_vec(msg).unwrap();
        let len = (data.len() as u32).to_le_bytes();

        stream.write_all(&len).await.unwrap();
        stream.write_all(&data).await.unwrap();

        let read_reply = async {
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.ok()?;
            let msg_len = u32::from_le_bytes(len_buf) as usize;
            let mut buf = vec![0u8; msg_len];
            stream.read_exact(&mut buf).await.ok()?;
            serde_json::from_slice(&buf).ok()
        };

        tokio::time::timeout(std::time::Duration::from_millis(200), read_reply)
            .await
            .ok()
            .flatten()
    }

    #[tokio::test]
    #[ignore = "live TCP — run with --include-ignored"]
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
                disk_free_gb: None,
            }),
        )
        .await;

        assert!(ack.is_none(), "heartbeat should produce no reply");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(coord.node_count(), 1);
    }
}
