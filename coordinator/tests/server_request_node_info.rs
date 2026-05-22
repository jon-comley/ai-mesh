use coordinator::registry::Registry;
use shared::{MeshMessage, NodeIdentity, NodeRole};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn server_handles_request_node_info() {
    // Start a local test server
    let listener = TcpListener::bind("127.0.0.1:9999").await.unwrap();
    let registry = Arc::new(Mutex::new(Registry::new()));
    let reg_clone = registry.clone();

    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let connections = Arc::new(Mutex::new(HashMap::new()));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let pending_intents = Arc::new(Mutex::new(HashMap::new()));
        coordinator::server::handle_connection(
            socket,
            reg_clone,
            connections,
            pending,
            pending_intents,
        )
        .await
        .unwrap();
    });

    // Insert a node
    {
        let mut reg = registry.lock().unwrap();
        reg.update_heartbeat(NodeIdentity {
            id: "node-1".into(),
            hostname: "OmniBook7".into(),
            ip: "172.20.107.210".into(),
            role: NodeRole::Compute,
        });
    }

    // Connect as client
    let mut stream = TcpStream::connect("127.0.0.1:9999").await.unwrap();

    // Send RequestNodeInfo
    let msg = MeshMessage::RequestNodeInfo("node-1".into());
    let data = serde_json::to_vec(&msg).unwrap();
    let len = (data.len() as u32).to_le_bytes();

    stream.write_all(&len).await.unwrap();
    stream.write_all(&data).await.unwrap();

    // Read response
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.unwrap();
    let msg_len = u32::from_le_bytes(len_buf) as usize;

    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf).await.unwrap();

    match serde_json::from_slice::<MeshMessage>(&buf).unwrap() {
        MeshMessage::NodeInfo(info) => {
            assert_eq!(info.id, "node-1");
            assert_eq!(info.hostname, "OmniBook7");
        }
        other => panic!("Unexpected response: {:?}", other),
    }
}
