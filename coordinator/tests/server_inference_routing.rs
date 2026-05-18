use coordinator::registry::Registry;
use shared::{
    InferenceRequest, MeshMessage, ModelLifecycleState, ModelStatusReport, NodeCapabilities,
    NodeIdentity, NodeRole, WIRE_VERSION,
};
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
async fn test_coordinator_forwards_inference_request_to_agent() {
    // Shared state — both connections use the same registry and connections map.
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
                let _ = coordinator::server::handle_connection(socket, reg, conns).await;
            }
        });
    }

    // CLI listener
    let cli_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cli_addr = cli_listener.local_addr().unwrap();
    {
        let reg = registry.clone();
        let conns = connections.clone();
        tokio::spawn(async move {
            if let Ok((socket, _)) = cli_listener.accept().await {
                let _ = coordinator::server::handle_connection(socket, reg, conns).await;
            }
        });
    }

    let node_id = "compute-node-inference-test".to_string();
    let mut agent_stream = TcpStream::connect(agent_addr).await.unwrap();

    // Step 1 — Agent registers: Heartbeat registers node in both registry and connections map.
    write_frame(
        &mut agent_stream,
        &MeshMessage::Heartbeat(NodeIdentity {
            id: node_id.clone(),
            hostname: "inference-host".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        }),
    )
    .await;
    // Acknowledge confirms the connections map entry is committed before we proceed.
    assert_eq!(read_frame(&mut agent_stream).await, MeshMessage::Acknowledge);

    // Step 2 — Agent reports capabilities so it is eligible for scheduling.
    write_frame(
        &mut agent_stream,
        &MeshMessage::Capabilities(NodeCapabilities {
            cpu_inference: true,
            gpu_inference: false,
            ane_inference: false,
            max_model_size_gb: 8.0,
        }),
    )
    .await;
    assert_eq!(read_frame(&mut agent_stream).await, MeshMessage::Acknowledge);

    // Step 3 — Agent reports model as Ready; coordinator updates registry.
    write_frame(
        &mut agent_stream,
        &MeshMessage::ModelStatus(ModelStatusReport {
            node_id: node_id.clone(),
            model_name: "llama3".into(),
            size_mb: 4096,
            state: ModelLifecycleState::Ready,
            wire_version: WIRE_VERSION,
        }),
    )
    .await;
    // Acknowledge confirms registry.update_model_status() has run before CLI sends request.
    assert_eq!(read_frame(&mut agent_stream).await, MeshMessage::Acknowledge);

    // Step 4 — CLI sends RequestModelInference; scheduler picks the agent.
    let mut cli_stream = TcpStream::connect(cli_addr).await.unwrap();
    write_frame(
        &mut cli_stream,
        &MeshMessage::RequestModelInference(InferenceRequest {
            request_id: "infer-req-001".into(),
            node_id: None,
            model_name: "llama3".into(),
            prompt: "hello world".into(),
            max_tokens: 64,
            wire_version: WIRE_VERSION,
        }),
    )
    .await;
    assert_eq!(read_frame(&mut cli_stream).await, MeshMessage::Acknowledge);

    // Step 5 — Agent stream receives the forwarded inference request.
    match read_frame(&mut agent_stream).await {
        MeshMessage::RequestModelInference(req) => {
            assert_eq!(req.request_id, "infer-req-001");
            assert_eq!(req.model_name, "llama3");
            assert_eq!(req.prompt, "hello world");
            assert_eq!(req.max_tokens, 64);
        }
        other => panic!("Expected RequestModelInference, got {:?}", other),
    }
}
