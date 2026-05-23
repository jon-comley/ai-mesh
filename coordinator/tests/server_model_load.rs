use coordinator::registry::Registry;
use shared::{MeshMessage, ModelLoadRequest, NodeIdentity, NodeRole};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn read_frame(stream: &mut TcpStream) -> MeshMessage {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.unwrap();
    let msg_len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf).await.unwrap();
    serde_json::from_slice(&buf).unwrap()
}

async fn write_frame(stream: &mut TcpStream, msg: &MeshMessage) {
    let data = serde_json::to_vec(msg).unwrap();
    let len = (data.len() as u32).to_le_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(&data).await.unwrap();
}

#[tokio::test]
async fn test_coordinator_forwards_model_load_to_registered_agent() {
    // Shared state — both connections use the same registry and connections map
    // so the Heartbeat from the agent makes it visible to the CLI's ModelLoad handler.
    let registry = Arc::new(Mutex::new(Registry::new()));
    let connections = Arc::new(Mutex::new(HashMap::new()));

    // Agent listener
    let agent_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agent_addr = agent_listener.local_addr().unwrap();
    {
        let reg = registry.clone();
        let conns = connections.clone();
        tokio::spawn(async move {
            if let Ok((socket, _)) = agent_listener.accept().await {
                let pending = Arc::new(Mutex::new(HashMap::new()));
                let pending_intents = Arc::new(Mutex::new(HashMap::new()));
                let _ = coordinator::server::handle_connection(
                    socket,
                    reg,
                    conns,
                    pending,
                    pending_intents,
                    Arc::new(vec![]),
                )
                .await;
            }
        });
    }

    // CLI listener (separate handle_connection sharing the same connections map)
    let cli_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cli_addr = cli_listener.local_addr().unwrap();
    {
        let reg = registry.clone();
        let conns = connections.clone();
        tokio::spawn(async move {
            if let Ok((socket, _)) = cli_listener.accept().await {
                let pending = Arc::new(Mutex::new(HashMap::new()));
                let pending_intents = Arc::new(Mutex::new(HashMap::new()));
                let _ = coordinator::server::handle_connection(
                    socket,
                    reg,
                    conns,
                    pending,
                    pending_intents,
                    Arc::new(vec![]),
                )
                .await;
            }
        });
    }

    // Step 1 — Agent connects and sends Heartbeat to register itself.
    let node_id = "test-pi-node-123".to_string();
    let mut agent_stream = TcpStream::connect(agent_addr).await.unwrap();
    write_frame(
        &mut agent_stream,
        &MeshMessage::Heartbeat(NodeIdentity {
            id: node_id.clone(),
            hostname: "pi-test".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        }),
    )
    .await;
    // The Acknowledge is sent only after the connections map is updated, so by the
    // time we receive it the agent tx is guaranteed to be registered.
    assert_eq!(
        read_frame(&mut agent_stream).await,
        MeshMessage::Acknowledge
    );

    // Step 2 — CLI connects and sends ModelLoad targeting the agent.
    let mut cli_stream = TcpStream::connect(cli_addr).await.unwrap();
    write_frame(
        &mut cli_stream,
        &MeshMessage::ModelLoad(ModelLoadRequest {
            request_id: "req-abc-999".into(),
            node_id: Some(node_id.clone()),
            model_name: "qwen-2.5-7b".into(),
            model_size_mb: 4200,
            wire_version: 1,
        }),
    )
    .await;
    assert_eq!(read_frame(&mut cli_stream).await, MeshMessage::Acknowledge);

    // Step 3 — Agent stream receives the forwarded ModelLoad.
    match read_frame(&mut agent_stream).await {
        MeshMessage::ModelLoad(req) => {
            assert_eq!(req.request_id, "req-abc-999");
            assert_eq!(req.model_name, "qwen-2.5-7b");
            assert_eq!(req.model_size_mb, 4200);
            assert_eq!(req.node_id, Some(node_id));
        }
        other => panic!("Expected ModelLoad, got {:?}", other),
    }
}
