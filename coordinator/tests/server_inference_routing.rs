use coordinator::registry::Registry;
use shared::{
    InferenceRequest, InferenceResult, MeshMessage, ModelLifecycleState, ModelStatusReport,
    NodeCapabilities, NodeIdentity, NodeRole, WIRE_VERSION,
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
    // Shared state — both connections use the same registry, connections map, and
    // pending_inferences tracker so the oneshot channel bridges the two handlers.
    let registry = Arc::new(Mutex::new(Registry::new()));
    let connections = Arc::new(Mutex::new(HashMap::new()));
    let pending_inferences = Arc::new(Mutex::new(HashMap::new()));

    // Agent listener
    let agent_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agent_addr = agent_listener.local_addr().unwrap();
    {
        let reg = registry.clone();
        let conns = connections.clone();
        let pend = pending_inferences.clone();
        tokio::spawn(async move {
            if let Ok((socket, _)) = agent_listener.accept().await {
                let pending_intents = Arc::new(Mutex::new(HashMap::new()));
                let _ = coordinator::server::handle_connection(
                    socket,
                    reg,
                    conns,
                    pend,
                    pending_intents,
                )
                .await;
            }
        });
    }

    // CLI listener
    let cli_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cli_addr = cli_listener.local_addr().unwrap();
    {
        let reg = registry.clone();
        let conns = connections.clone();
        let pend = pending_inferences.clone();
        tokio::spawn(async move {
            if let Ok((socket, _)) = cli_listener.accept().await {
                let pending_intents = Arc::new(Mutex::new(HashMap::new()));
                let _ = coordinator::server::handle_connection(
                    socket,
                    reg,
                    conns,
                    pend,
                    pending_intents,
                )
                .await;
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
    assert_eq!(
        read_frame(&mut agent_stream).await,
        MeshMessage::Acknowledge
    );

    // Step 2 — Agent reports capabilities so it is eligible for scheduling.
    write_frame(
        &mut agent_stream,
        &MeshMessage::Capabilities(NodeCapabilities {
            cpu_inference: true,
            gpu_inference: false,
            ane_inference: false,
            max_model_size_gb: 8.0,
            features: vec!["llm".into()],
        }),
    )
    .await;
    assert_eq!(
        read_frame(&mut agent_stream).await,
        MeshMessage::Acknowledge
    );

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
    assert_eq!(
        read_frame(&mut agent_stream).await,
        MeshMessage::Acknowledge
    );

    // Step 4 & 5 — CLI sends RequestModelInference and agent responds concurrently.
    //
    // The coordinator blocks the CLI handler on a oneshot receiver after forwarding the
    // request to the agent. The agent side must run concurrently to send the result back
    // and unblock the oneshot, which is why tokio::join! is required here.
    let mut cli_stream = TcpStream::connect(cli_addr).await.unwrap();
    let node_id_clone = node_id.clone();

    let (cli_result, forwarded_req) = tokio::join!(
        // CLI side: send RequestModelInference, await ModelInferenceResult.
        async {
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
            read_frame(&mut cli_stream).await
        },
        // Agent side: receive forwarded request, send ModelInferenceResult back.
        async {
            let req = read_frame(&mut agent_stream).await;
            if let MeshMessage::RequestModelInference(ref r) = req {
                write_frame(
                    &mut agent_stream,
                    &MeshMessage::ModelInferenceResult(InferenceResult {
                        request_id: r.request_id.clone(),
                        node_id: node_id_clone,
                        model_name: r.model_name.clone(),
                        output: "Simulated answer".into(),
                        tokens_generated: 10,
                        duration_ms: 50,
                        error: None,
                        wire_version: WIRE_VERSION,
                    }),
                )
                .await;
            }
            req
        }
    );

    // Verify CLI received ModelInferenceResult with the correct fields.
    match cli_result {
        MeshMessage::ModelInferenceResult(res) => {
            assert_eq!(res.request_id, "infer-req-001");
            assert_eq!(res.model_name, "llama3");
            assert_eq!(res.output, "Simulated answer");
            assert_eq!(res.tokens_generated, 10);
            assert!(res.error.is_none());
        }
        other => panic!("Expected ModelInferenceResult, got {:?}", other),
    }

    // Verify the agent received the correctly forwarded RequestModelInference.
    match forwarded_req {
        MeshMessage::RequestModelInference(req) => {
            assert_eq!(req.request_id, "infer-req-001");
            assert_eq!(req.model_name, "llama3");
            assert_eq!(req.prompt, "hello world");
            assert_eq!(req.max_tokens, 64);
        }
        other => panic!("Expected RequestModelInference, got {:?}", other),
    }
}
