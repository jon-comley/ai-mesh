use crate::registry::Registry;
use crate::server::Server;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

pub struct CoordinatorConfig {
    pub listen_addr: String,
}

pub struct Coordinator {
    pub config: CoordinatorConfig,
    pub registry: Arc<Mutex<Registry>>,
}

impl Coordinator {
    pub fn new(listen_addr: impl Into<String>) -> Self {
        Self {
            config: CoordinatorConfig {
                listen_addr: listen_addr.into(),
            },
            registry: Arc::new(Mutex::new(Registry::new())),
        }
    }

    pub async fn start(&self) -> JoinHandle<()> {
        let server = Server::new(self.config.listen_addr.clone(), self.registry.clone());

        tokio::spawn(async move {
            let _ = server.run().await;
        })
    }

    pub fn node_count(&self) -> usize {
        self.registry.lock().unwrap().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{MeshMessage, NodeIdentity, NodeRole};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

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

        let ack = send_message("127.0.0.1:9002", &MeshMessage::Heartbeat(ident.clone())).await;

        match ack {
            MeshMessage::Acknowledge => {}
            _ => panic!("Expected Acknowledge"),
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(coord.node_count(), 1);
    }
}
