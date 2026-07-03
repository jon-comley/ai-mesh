//! Inbound OpenAI-compatible API: `POST /v1/chat/completions` + `GET /v1/models`.
//!
//! Pure chat semantics — the caller's messages go to the model verbatim; no
//! device-schema injection and no tool execution (that stays on `/api/chat`).
//! Auth accepts `Authorization: Bearer <token>` (what OpenAI SDKs send) with
//! `?token=` as a fallback, both validated against the same mesh token set.
//! `stream: true` returns OpenAI-spec SSE (`chat.completion.chunk` events,
//! `stream_options.include_usage` honoured, `data: [DONE]` sentinel); a
//! mid-stream node death or stall emits an SSE error event and terminates
//! rather than holding the connection open.

use super::auth::{TokenQuery, bearer_token};
use super::state::{DashboardState, PendingStreams};
use crate::registry::Registry;
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use shared::{ChatRole, ChatTurn, MeshMessage};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{info, warn};

// ── Request / response shapes ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ChatCompletionRequest {
    #[serde(default)]
    model: Option<String>,
    messages: Vec<InMessage>,
    #[serde(default, alias = "max_completion_tokens")]
    max_tokens: Option<u32>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    stream_options: Option<StreamOptions>,
}

#[derive(Deserialize)]
struct StreamOptions {
    #[serde(default)]
    include_usage: bool,
}

#[derive(Deserialize)]
struct InMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(Serialize)]
struct Choice {
    index: u32,
    message: OutMessage,
    finish_reason: &'static str,
}

#[derive(Serialize)]
struct OutMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Serialize)]
struct ChatCompletionChunk {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<ChunkChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Usage>,
}

#[derive(Serialize)]
struct ChunkChoice {
    index: u32,
    delta: Delta,
    finish_reason: Option<&'static str>,
}

#[derive(Serialize, Default)]
struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

#[derive(Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<ModelObject>,
}

#[derive(Serialize)]
struct ModelObject {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: String,
}

const DEFAULT_MAX_TOKENS: u32 = 2048;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// A pending OpenAI-envelope error — small enough to sit in `Result::Err`
/// without clippy's large-Err lint; rendered to a `Response` at the edge.
struct ApiError {
    status: StatusCode,
    message: String,
    err_type: &'static str,
    code: &'static str,
}

impl ApiError {
    fn new(
        status: StatusCode,
        message: impl Into<String>,
        err_type: &'static str,
        code: &'static str,
    ) -> Self {
        Self {
            status,
            message: message.into(),
            err_type,
            code,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        openai_error(self.status, &self.message, self.err_type, self.code)
    }
}

/// OpenAI error envelope: `{"error": {"message", "type", "code"}}`.
fn openai_error(status: StatusCode, message: &str, err_type: &str, code: &str) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": { "message": message, "type": err_type, "code": code }
        })),
    )
        .into_response()
}

/// Token from `Authorization: Bearer …` (what OpenAI SDKs send), falling back
/// to the `?token=` query param used by the rest of the HTTP API.
fn request_token(headers: &HeaderMap, q: &TokenQuery) -> String {
    bearer_token(headers).unwrap_or_else(|| q.token.clone())
}

fn unauthorized() -> Response {
    openai_error(
        StatusCode::UNAUTHORIZED,
        "invalid or missing API key (Authorization: Bearer <mesh token>)",
        "invalid_request_error",
        "invalid_api_key",
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn finish_reason(tokens_generated: u32, max_tokens: u32) -> &'static str {
    if tokens_generated >= max_tokens {
        "length"
    } else {
        "stop"
    }
}

/// Map a cloud-provider failure to the OpenAI error envelope, logging it and
/// recording it in the gateway stats.
fn cloud_error_response(
    request_id: &str,
    e: crate::cloud::CloudError,
    state: &DashboardState,
) -> Response {
    warn!(request_id = %request_id, "openai-api: cloud provider failed: {e}");
    state.record_gateway_error(e.to_string());
    let (status, code) = match e {
        crate::cloud::CloudError::RateLimited => {
            (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error")
        }
        crate::cloud::CloudError::Unauthorized | crate::cloud::CloudError::NoKey => {
            (StatusCode::INTERNAL_SERVER_ERROR, "gateway_misconfigured")
        }
        _ => (StatusCode::SERVICE_UNAVAILABLE, "upstream_error"),
    };
    openai_error(
        status,
        &format!("cloud provider error: {e}"),
        "api_error",
        code,
    )
}

/// Build the non-streaming `chat.completion` success body.
fn completion_response(
    request_id: String,
    model: String,
    content: String,
    prompt_tokens: u32,
    completion_tokens: u32,
    max_tokens: u32,
) -> Response {
    Json(ChatCompletionResponse {
        id: request_id,
        object: "chat.completion",
        created: unix_now(),
        model,
        choices: vec![Choice {
            index: 0,
            message: OutMessage {
                role: "assistant",
                content,
            },
            finish_reason: finish_reason(completion_tokens, max_tokens),
        }],
        usage: Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    })
    .into_response()
}

/// Where a request should run. Resolved under a single registry lock so the
/// ready-model list and gateway config are a consistent snapshot.
enum Route {
    Local(String),
    Cloud(crate::cloud::OpenAiCompatProvider, String),
}

fn gateway_not_ready() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "cloud gateway is enabled but not fully configured",
        "api_error",
        "upstream_error",
    )
}

fn resolve_route(
    requested: Option<&str>,
    registry: &Arc<Mutex<Registry>>,
) -> Result<Route, ApiError> {
    let (ready, default_local, cfg) = {
        let reg = registry.lock().unwrap();
        (
            reg.ready_llm_models(),
            reg.any_ready_llm_model(),
            crate::cloud::GatewayConfig::load(&reg),
        )
    };
    let gateway_on = cfg.enabled && cfg.is_configured();

    match requested.filter(|m| !m.is_empty()) {
        Some(m) => {
            if ready.iter().any(|r| r == m) {
                Ok(Route::Local(m.to_string()))
            } else if gateway_on && m == cfg.selected_model {
                cfg.provider()
                    .map(|p| Route::Cloud(p, cfg.selected_model))
                    .ok_or_else(gateway_not_ready)
            } else {
                Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    format!(
                        "model '{m}' is not available; GET /v1/models lists what the mesh can serve"
                    ),
                    "invalid_request_error",
                    "model_not_found",
                ))
            }
        }
        None => {
            // No model requested: largest ready local model, else the gateway.
            if let Some(m) = default_local {
                Ok(Route::Local(m))
            } else if gateway_on {
                cfg.provider()
                    .map(|p| Route::Cloud(p, cfg.selected_model))
                    .ok_or_else(gateway_not_ready)
            } else {
                Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no LLM model is ready on any node and the cloud gateway is disabled",
                    "api_error",
                    "no_model_ready",
                ))
            }
        }
    }
}

fn parse_turns(messages: &[InMessage]) -> Result<Vec<ChatTurn>, ApiError> {
    if messages.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "messages must contain at least one entry",
            "invalid_request_error",
            "invalid_messages",
        ));
    }
    messages
        .iter()
        .map(|m| {
            let role = match m.role.as_str() {
                "system" => ChatRole::System,
                "user" => ChatRole::User,
                "assistant" => ChatRole::Assistant,
                other => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        format!(
                            "unsupported message role '{other}' (supported: system, user, assistant)"
                        ),
                        "invalid_request_error",
                        "invalid_messages",
                    ));
                }
            };
            Ok(ChatTurn {
                role,
                content: m.content.clone(),
            })
        })
        .collect()
}

// ── SSE streaming ─────────────────────────────────────────────────────────────

/// Nothing arriving for this long before the first token aborts the stream.
/// Generous because llama-server emits nothing during prefill, which on a 14b
/// model with a long prompt can take minutes.
const FIRST_CHUNK_TIMEOUT_SECS: u64 = 300;
/// Max silence between tokens once generation has started.
const INTER_CHUNK_TIMEOUT_SECS: u64 = 60;

/// Provider-agnostic stream events — the local mesh adapter and the cloud
/// passthrough adapter both reduce to this, so one emitter serves both.
enum StreamItem {
    Delta(String),
    Done {
        finish_reason: &'static str,
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    Error(String),
}

/// Build the SSE response: a spawned emitter task converts `StreamItem`s into
/// OpenAI `chat.completion.chunk` events (single shared `id`/`created`), with
/// the role-first chunk, finish chunk, optional usage chunk, `[DONE]`
/// sentinel, and the failure-semantics guarantee: any error, upstream close,
/// or stall becomes an SSE error event + termination — never a hang.
fn sse_response(
    request_id: String,
    model: String,
    include_usage: bool,
    mut rx: mpsc::Receiver<StreamItem>,
) -> Response {
    let (etx, erx) = mpsc::channel::<Result<Event, Infallible>>(64);
    let created = unix_now();

    tokio::spawn(async move {
        let chunk = |delta: Delta, finish: Option<&'static str>, usage: Option<Usage>| {
            let body = ChatCompletionChunk {
                id: request_id.clone(),
                object: "chat.completion.chunk",
                created,
                model: model.clone(),
                choices: match usage {
                    // The dedicated usage chunk has an empty choices array.
                    Some(_) => vec![],
                    None => vec![ChunkChoice {
                        index: 0,
                        delta,
                        finish_reason: finish,
                    }],
                },
                usage,
            };
            Event::default().data(serde_json::to_string(&body).unwrap_or_default())
        };
        let done_event = Event::default().data("[DONE]");

        // Role-first chunk, per the OpenAI streaming shape.
        if etx
            .send(Ok(chunk(
                Delta {
                    role: Some("assistant"),
                    content: Some(String::new()),
                },
                None,
                None,
            )))
            .await
            .is_err()
        {
            return;
        }

        let mut first = true;
        loop {
            let wait = std::time::Duration::from_secs(if first {
                FIRST_CHUNK_TIMEOUT_SECS
            } else {
                INTER_CHUNK_TIMEOUT_SECS
            });
            let item = match tokio::time::timeout(wait, rx.recv()).await {
                Ok(Some(item)) => item,
                Ok(None) => StreamItem::Error("stream ended without a result".to_string()),
                Err(_) => StreamItem::Error(format!(
                    "stream stalled: no data from the model for {}s",
                    wait.as_secs()
                )),
            };
            first = false;
            match item {
                StreamItem::Delta(text) => {
                    let ev = chunk(
                        Delta {
                            role: None,
                            content: Some(text),
                        },
                        None,
                        None,
                    );
                    if etx.send(Ok(ev)).await.is_err() {
                        return; // client hung up
                    }
                }
                StreamItem::Done {
                    finish_reason,
                    prompt_tokens,
                    completion_tokens,
                } => {
                    let _ = etx
                        .send(Ok(chunk(Delta::default(), Some(finish_reason), None)))
                        .await;
                    if include_usage {
                        let _ = etx
                            .send(Ok(chunk(
                                Delta::default(),
                                None,
                                Some(Usage {
                                    prompt_tokens,
                                    completion_tokens,
                                    total_tokens: prompt_tokens + completion_tokens,
                                }),
                            )))
                            .await;
                    }
                    let _ = etx.send(Ok(done_event)).await;
                    return;
                }
                StreamItem::Error(msg) => {
                    let err = serde_json::json!({
                        "error": { "message": msg, "type": "api_error", "code": "upstream_error" }
                    });
                    let _ = etx.send(Ok(Event::default().data(err.to_string()))).await;
                    let _ = etx.send(Ok(done_event)).await;
                    return;
                }
            }
        }
    });

    Sse::new(ReceiverStream::new(erx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Adapt the mesh streaming channel (chunks + terminal result) to
/// `StreamItem`s. Removes the `pending_streams` entry on exit so the TCP
/// demux stops forwarding and the agent's sends fail (cancelling generation).
fn spawn_local_stream_adapter(
    request_id: String,
    max_tokens: u32,
    mut mesh_rx: mpsc::Receiver<MeshMessage>,
    item_tx: mpsc::Sender<StreamItem>,
    pending_streams: PendingStreams,
) {
    tokio::spawn(async move {
        let mut saw_delta = false;
        loop {
            let msg = tokio::select! {
                // Emitter gone (client hung up / stream ended) — stop consuming.
                _ = item_tx.closed() => break,
                msg = mesh_rx.recv() => msg,
            };
            match msg {
                Some(MeshMessage::ModelInferenceChunk(c)) => {
                    saw_delta = true;
                    if item_tx.send(StreamItem::Delta(c.delta)).await.is_err() {
                        break;
                    }
                }
                Some(MeshMessage::ModelInferenceResult(res)) => {
                    let item = if let Some(e) = res.error {
                        StreamItem::Error(format!("inference failed on node: {e}"))
                    } else {
                        // Old (pre-v4) agents ignore the unknown `stream`
                        // field and reply non-streamed: surface the whole
                        // output as one delta so the client still gets a
                        // valid stream.
                        if !saw_delta && !res.output.is_empty() {
                            let _ = item_tx.send(StreamItem::Delta(res.output)).await;
                        }
                        StreamItem::Done {
                            finish_reason: finish_reason(res.tokens_generated, max_tokens),
                            prompt_tokens: res.prompt_tokens,
                            completion_tokens: res.tokens_generated,
                        }
                    };
                    let _ = item_tx.send(item).await;
                    break;
                }
                Some(MeshMessage::Error(e)) => {
                    let _ = item_tx.send(StreamItem::Error(e)).await;
                    break;
                }
                Some(_) => {} // unrelated message type — ignore
                None => {
                    // Demux dropped the sender (buffer overflow kill) or the
                    // entry was removed — emitter turns this into an error.
                    break;
                }
            }
        }
        pending_streams.lock().unwrap().remove(&request_id);
    });
}

/// Adapt a cloud provider's OpenAI SSE stream to `StreamItem`s and record
/// gateway stats. Per-read stalls are caught by the emitter's timeouts; the
/// request itself is capped at 1h in `complete_stream`.
fn spawn_cloud_stream_adapter(
    resp: reqwest::Response,
    max_tokens: u32,
    item_tx: mpsc::Sender<StreamItem>,
    state: Arc<DashboardState>,
) {
    tokio::spawn(async move {
        let mut byte_stream = resp.bytes_stream();
        let mut parser = shared::sse::SseParser::new();
        let mut prompt_tokens: u32 = 0;
        let mut completion_tokens: Option<u32> = None;
        let mut deltas_sent: u32 = 0;
        let mut finish: Option<&'static str> = None;

        'read: loop {
            let bytes = tokio::select! {
                _ = item_tx.closed() => return,
                chunk = byte_stream.next() => match chunk {
                    Some(Ok(b)) => b,
                    Some(Err(e)) => {
                        state.record_gateway_error(e.to_string());
                        let _ = item_tx
                            .send(StreamItem::Error(format!("cloud stream read failed: {e}")))
                            .await;
                        return;
                    }
                    // EOF without [DONE] — some providers just close.
                    None => break 'read,
                },
            };
            for payload in parser.feed(&bytes) {
                if payload == "[DONE]" {
                    break 'read;
                }
                let Some(parsed) = shared::sse::parse_openai_chunk(&payload) else {
                    continue;
                };
                if let Some(err) = parsed.error {
                    state.record_gateway_error(err.clone());
                    let _ = item_tx
                        .send(StreamItem::Error(format!("cloud provider error: {err}")))
                        .await;
                    return;
                }
                if let Some(pt) = parsed.prompt_tokens {
                    prompt_tokens = pt;
                }
                if let Some(ct) = parsed.completion_tokens {
                    completion_tokens = Some(ct);
                }
                if let Some(fr) = parsed.finish_reason {
                    finish = Some(if fr == "length" { "length" } else { "stop" });
                }
                if let Some(delta) = parsed.delta {
                    if delta.is_empty() {
                        continue;
                    }
                    deltas_sent += 1;
                    if item_tx.send(StreamItem::Delta(delta)).await.is_err() {
                        return;
                    }
                }
            }
        }

        if deltas_sent == 0 {
            state.record_gateway_error("cloud stream produced no output".to_string());
            let _ = item_tx
                .send(StreamItem::Error(
                    "cloud stream produced no output".to_string(),
                ))
                .await;
        } else {
            state.record_gateway_call(0, 0);
            let _ = item_tx
                .send(StreamItem::Done {
                    finish_reason: finish.unwrap_or_else(|| finish_reason(deltas_sent, max_tokens)),
                    prompt_tokens,
                    completion_tokens: completion_tokens.unwrap_or(deltas_sent),
                })
                .await;
        }
    });
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn chat_completions(
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    body: Result<Json<ChatCompletionRequest>, JsonRejection>,
) -> Response {
    if !state.auth_ok(&request_token(&headers, &q)) {
        return unauthorized();
    }
    let Json(req) = match body {
        Ok(b) => b,
        Err(rej) => {
            return openai_error(
                StatusCode::BAD_REQUEST,
                &format!("invalid request body: {rej}"),
                "invalid_request_error",
                "invalid_body",
            );
        }
    };
    if req.max_tokens == Some(0) {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "max_tokens must be at least 1",
            "invalid_request_error",
            "invalid_max_tokens",
        );
    }
    let turns = match parse_turns(&req.messages) {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    let route = match resolve_route(req.model.as_deref(), &registry) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let request_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let streaming = req.stream.unwrap_or(false);
    let include_usage = req.stream_options.as_ref().is_some_and(|o| o.include_usage);

    match route {
        Route::Local(model) if streaming => {
            info!(request_id = %request_id, model = %model, turns = turns.len(),
                  "openai-api: local streaming inference");
            let mesh_rx = match crate::inference::dispatch_local_inference_stream(
                &request_id,
                &model,
                turns,
                max_tokens,
                req.temperature,
                &registry,
                &state.connections,
                &state.pending_streams,
            )
            .await
            {
                Ok(rx) => rx,
                // No SSE bytes sent yet — a plain JSON error is correct here.
                Err(msg) => {
                    return openai_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &msg,
                        "api_error",
                        "upstream_error",
                    );
                }
            };
            let (item_tx, item_rx) = mpsc::channel::<StreamItem>(64);
            spawn_local_stream_adapter(
                request_id.clone(),
                max_tokens,
                mesh_rx,
                item_tx,
                state.pending_streams.clone(),
            );
            sse_response(request_id, model, include_usage, item_rx)
        }
        Route::Cloud(provider, model) if streaming => {
            info!(request_id = %request_id, model = %model, turns = turns.len(),
                  "openai-api: cloud streaming inference");
            let resp = match provider
                .complete_stream(&turns, req.temperature.unwrap_or(0.4))
                .await
            {
                Ok(resp) => resp,
                Err(e) => return cloud_error_response(&request_id, e, &state),
            };
            let (item_tx, item_rx) = mpsc::channel::<StreamItem>(64);
            spawn_cloud_stream_adapter(resp, max_tokens, item_tx, state.clone());
            sse_response(request_id, model, include_usage, item_rx)
        }
        Route::Local(model) => {
            info!(request_id = %request_id, model = %model, turns = turns.len(),
                  "openai-api: local inference");
            let res = crate::inference::dispatch_local_inference(
                &request_id,
                &model,
                turns,
                max_tokens,
                req.temperature,
                &registry,
                &state.connections,
                &state.pending_inferences,
            )
            .await;
            match res {
                Ok(res) => {
                    if let Some(e) = res.error {
                        return openai_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &format!("inference failed on node: {e}"),
                            "api_error",
                            "upstream_error",
                        );
                    }
                    completion_response(
                        request_id,
                        res.model_name,
                        res.output,
                        res.prompt_tokens,
                        res.tokens_generated,
                        max_tokens,
                    )
                }
                Err(msg) => openai_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &msg,
                    "api_error",
                    "upstream_error",
                ),
            }
        }
        Route::Cloud(provider, model) => {
            info!(request_id = %request_id, model = %model, turns = turns.len(),
                  "openai-api: cloud inference");
            match provider
                .complete(&turns, req.temperature.unwrap_or(0.4))
                .await
            {
                Ok(reply) => {
                    // No compression on this path — record the call for the
                    // Online AI tab's counters without a tokens-saved delta.
                    state.record_gateway_call(0, 0);
                    completion_response(
                        request_id,
                        model,
                        reply.text,
                        reply.prompt_tokens,
                        reply.completion_tokens,
                        max_tokens,
                    )
                }
                Err(e) => cloud_error_response(&request_id, e, &state),
            }
        }
    }
}

pub async fn list_models(
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    State(state): State<Arc<DashboardState>>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> Response {
    if !state.auth_ok(&request_token(&headers, &q)) {
        return unauthorized();
    }
    let (ready, cfg) = {
        let reg = registry.lock().unwrap();
        (
            reg.ready_llm_models(),
            crate::cloud::GatewayConfig::load(&reg),
        )
    };
    let mut data: Vec<ModelObject> = ready
        .into_iter()
        .map(|id| ModelObject {
            id,
            object: "model",
            created: 0,
            owned_by: "ai-mesh".to_string(),
        })
        .collect();
    if cfg.enabled && cfg.is_configured() && !data.iter().any(|m| m.id == cfg.selected_model) {
        let owned_by = cfg
            .provider()
            .map(|p| p.provider_name().to_string())
            .unwrap_or_else(|| "cloud".to_string());
        data.push(ModelObject {
            id: cfg.selected_model,
            object: "model",
            created: 0,
            owned_by,
        });
    }
    Json(ModelList {
        object: "list",
        data,
    })
    .into_response()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::state::NodeConnections;
    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::routing::{get, post};
    use shared::{
        InferenceRequest, InferenceResult, MeshMessage, ModelLifecycleState, NodeCapabilities,
        NodeIdentity, NodeRole, WIRE_VERSION,
    };
    use std::collections::HashMap;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    fn make_state(tokens: Vec<String>, connections: NodeConnections) -> Arc<DashboardState> {
        DashboardState::new(Arc::new(tokens), connections)
    }

    fn empty_connections() -> NodeConnections {
        Arc::new(Mutex::new(HashMap::new()))
    }

    /// Registry with one llm-feature Compute node serving `model` in Ready state.
    fn ready_registry(node_id: &str, model: &str) -> Arc<Mutex<Registry>> {
        let mut reg = Registry::new();
        reg.update_heartbeat(NodeIdentity {
            id: node_id.into(),
            hostname: "test-node".into(),
            ip: "10.0.0.1".into(),
            role: NodeRole::Compute,
        });
        reg.update_capabilities(
            node_id,
            NodeCapabilities {
                features: vec!["llm".into()],
                ..NodeCapabilities::default()
            },
        );
        reg.update_model_status(node_id, model, 4096, ModelLifecycleState::Ready);
        Arc::new(Mutex::new(reg))
    }

    fn v1_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .route("/v1/models", get(list_models))
            .layer(Extension(registry))
            .with_state(state)
    }

    async fn send(
        router: Router,
        method: &str,
        uri: &str,
        bearer: Option<&str>,
        body: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = bearer {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let req = match body {
            Some(b) => builder
                .header("content-type", "application/json")
                .body(axum::body::Body::from(b.to_owned()))
                .unwrap(),
            None => builder.body(axum::body::Body::empty()).unwrap(),
        };
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    /// Spawn a fake agent on `node_id`: receives the forwarded
    /// `RequestModelInference`, stores it for later assertions, and resolves
    /// the pending oneshot with a canned result.
    fn fake_agent(
        connections: &NodeConnections,
        state: &Arc<DashboardState>,
        node_id: &str,
        tokens_generated: u32,
    ) -> Arc<Mutex<Option<InferenceRequest>>> {
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert(node_id.into(), tx);
        let seen: Arc<Mutex<Option<InferenceRequest>>> = Arc::new(Mutex::new(None));
        let seen2 = seen.clone();
        let pending = state.pending_inferences.clone();
        let node_id = node_id.to_string();
        tokio::spawn(async move {
            if let Some(MeshMessage::RequestModelInference(req)) = rx.recv().await {
                let result = InferenceResult {
                    request_id: req.request_id.clone(),
                    node_id,
                    model_name: req.model_name.clone(),
                    output: "hello from the mesh".into(),
                    tokens_generated,
                    prompt_tokens: 7,
                    duration_ms: 5,
                    prompt_eval_ms: 1,
                    error: None,
                    wire_version: WIRE_VERSION,
                };
                *seen2.lock().unwrap() = Some(req.clone());
                if let Some((otx, _)) = pending.lock().unwrap().remove(&req.request_id) {
                    let _ = otx.send(MeshMessage::ModelInferenceResult(result));
                }
            }
        });
        seen
    }

    const CHAT_BODY: &str = r#"{"model":"qwen2.5:7b","messages":[{"role":"user","content":"hi"}]}"#;

    #[tokio::test]
    async fn missing_token_returns_401_envelope() {
        let router = v1_router(
            make_state(vec!["secret".into()], empty_connections()),
            ready_registry("node-1", "qwen2.5:7b"),
        );
        let (status, json) = send(
            router,
            "POST",
            "/v1/chat/completions",
            None,
            Some(CHAT_BODY),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["error"]["code"], "invalid_api_key");
    }

    #[tokio::test]
    async fn wrong_bearer_returns_401() {
        let router = v1_router(
            make_state(vec!["secret".into()], empty_connections()),
            ready_registry("node-1", "qwen2.5:7b"),
        );
        let (status, _) = send(
            router,
            "POST",
            "/v1/chat/completions",
            Some("wrong"),
            Some(CHAT_BODY),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn stream_with_no_connected_node_returns_json_503() {
        // Dispatch fails before any SSE bytes are sent, so the error must be
        // a plain JSON envelope, not a stream.
        let router = v1_router(
            make_state(vec![], empty_connections()),
            ready_registry("node-1", "qwen2.5:7b"),
        );
        let body = r#"{"messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let (status, json) = send(router, "POST", "/v1/chat/completions", None, Some(body)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["error"]["code"], "upstream_error");
    }

    #[tokio::test]
    async fn empty_messages_returns_400() {
        let router = v1_router(
            make_state(vec![], empty_connections()),
            ready_registry("node-1", "qwen2.5:7b"),
        );
        let body = r#"{"messages":[]}"#;
        let (status, json) = send(router, "POST", "/v1/chat/completions", None, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_messages");
    }

    #[tokio::test]
    async fn tool_role_returns_400() {
        let router = v1_router(
            make_state(vec![], empty_connections()),
            ready_registry("node-1", "qwen2.5:7b"),
        );
        let body = r#"{"messages":[{"role":"tool","content":"result"}]}"#;
        let (status, json) = send(router, "POST", "/v1/chat/completions", None, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_messages");
    }

    #[tokio::test]
    async fn unknown_model_returns_404() {
        let router = v1_router(
            make_state(vec![], empty_connections()),
            ready_registry("node-1", "qwen2.5:7b"),
        );
        let body = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#;
        let (status, json) = send(router, "POST", "/v1/chat/completions", None, Some(body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"]["code"], "model_not_found");
    }

    #[tokio::test]
    async fn ready_model_without_connection_returns_503() {
        // Model is Ready in the registry, but the node's TCP channel is gone.
        let router = v1_router(
            make_state(vec![], empty_connections()),
            ready_registry("node-1", "qwen2.5:7b"),
        );
        let (status, json) = send(
            router,
            "POST",
            "/v1/chat/completions",
            None,
            Some(CHAT_BODY),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["error"]["code"], "upstream_error");
    }

    #[tokio::test]
    async fn no_model_and_no_gateway_returns_503() {
        let router = v1_router(
            make_state(vec![], empty_connections()),
            Arc::new(Mutex::new(Registry::new())),
        );
        let body = r#"{"messages":[{"role":"user","content":"hi"}]}"#;
        let (status, json) = send(router, "POST", "/v1/chat/completions", None, Some(body)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["error"]["code"], "no_model_ready");
    }

    #[tokio::test]
    async fn happy_path_returns_chat_completion_and_forwards_messages_verbatim() {
        let connections = empty_connections();
        let state = make_state(vec!["secret".into()], connections.clone());
        let seen = fake_agent(&connections, &state, "node-1", 3);
        let router = v1_router(state, ready_registry("node-1", "qwen2.5:7b"));

        let body = r#"{
            "model": "qwen2.5:7b",
            "messages": [
                {"role": "system", "content": "You are terse."},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": "bye"}
            ],
            "max_tokens": 99,
            "temperature": 0.2
        }"#;
        let (status, json) = send(
            router,
            "POST",
            "/v1/chat/completions",
            Some("secret"),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["object"], "chat.completion");
        assert!(json["id"].as_str().unwrap().starts_with("chatcmpl-"));
        assert_eq!(json["model"], "qwen2.5:7b");
        assert_eq!(json["choices"][0]["message"]["role"], "assistant");
        assert_eq!(
            json["choices"][0]["message"]["content"],
            "hello from the mesh"
        );
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        assert_eq!(json["usage"]["prompt_tokens"], 7);
        assert_eq!(json["usage"]["completion_tokens"], 3);
        assert_eq!(json["usage"]["total_tokens"], 10);

        // The wire request carried the caller's turns verbatim plus the tuning knobs.
        let req = seen.lock().unwrap().take().expect("agent saw the request");
        assert_eq!(
            req.messages,
            vec![
                ChatTurn::system("You are terse."),
                ChatTurn::user("hi"),
                ChatTurn::assistant("hello"),
                ChatTurn::user("bye"),
            ]
        );
        assert_eq!(req.max_tokens, 99);
        assert_eq!(req.temperature, Some(0.2));
        assert!(req.request_id.starts_with("chatcmpl-"));
    }

    #[tokio::test]
    async fn token_query_param_accepted_as_fallback() {
        let connections = empty_connections();
        let state = make_state(vec!["secret".into()], connections.clone());
        let _seen = fake_agent(&connections, &state, "node-1", 3);
        let router = v1_router(state, ready_registry("node-1", "qwen2.5:7b"));

        let (status, _) = send(
            router,
            "POST",
            "/v1/chat/completions?token=secret",
            None,
            Some(CHAT_BODY),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn finish_reason_length_when_at_max_tokens() {
        let connections = empty_connections();
        let state = make_state(vec![], connections.clone());
        let _seen = fake_agent(&connections, &state, "node-1", 99);
        let router = v1_router(state, ready_registry("node-1", "qwen2.5:7b"));

        let body = r#"{"messages":[{"role":"user","content":"hi"}],"max_tokens":99}"#;
        let (status, json) = send(router, "POST", "/v1/chat/completions", None, Some(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["choices"][0]["finish_reason"], "length");
    }

    #[tokio::test]
    async fn omitted_model_routes_to_ready_model() {
        let connections = empty_connections();
        let state = make_state(vec![], connections.clone());
        let seen = fake_agent(&connections, &state, "node-1", 3);
        let router = v1_router(state, ready_registry("node-1", "qwen2.5:7b"));

        let body = r#"{"messages":[{"role":"user","content":"hi"}]}"#;
        let (status, json) = send(router, "POST", "/v1/chat/completions", None, Some(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["model"], "qwen2.5:7b");
        let req = seen.lock().unwrap().take().expect("agent saw the request");
        assert_eq!(req.model_name, "qwen2.5:7b");
    }

    #[tokio::test]
    async fn list_models_returns_ready_models() {
        let router = v1_router(
            make_state(vec![], empty_connections()),
            ready_registry("node-1", "qwen2.5:7b"),
        );
        let (status, json) = send(router, "GET", "/v1/models", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["object"], "list");
        assert_eq!(json["data"][0]["id"], "qwen2.5:7b");
        assert_eq!(json["data"][0]["object"], "model");
        assert_eq!(json["data"][0]["owned_by"], "ai-mesh");
    }

    #[tokio::test]
    async fn list_models_includes_gateway_model_when_enabled() {
        let registry = ready_registry("node-1", "qwen2.5:7b");
        {
            let reg = registry.lock().unwrap();
            reg.set_preference(crate::cloud::GATEWAY_USER, "enabled", "true");
            reg.set_preference(
                crate::cloud::GATEWAY_USER,
                "selected_model",
                "cloud-model-x",
            );
            reg.set_preference(crate::cloud::GATEWAY_USER, "api_key", "sk-test");
            reg.set_preference(
                crate::cloud::GATEWAY_USER,
                "base_url",
                "https://example.com/v1",
            );
        }
        let router = v1_router(make_state(vec![], empty_connections()), registry);
        let (status, json) = send(router, "GET", "/v1/models", None, None).await;
        assert_eq!(status, StatusCode::OK);
        let ids: Vec<&str> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"qwen2.5:7b"));
        assert!(ids.contains(&"cloud-model-x"));
    }

    #[tokio::test]
    async fn list_models_requires_auth() {
        let router = v1_router(
            make_state(vec!["secret".into()], empty_connections()),
            ready_registry("node-1", "qwen2.5:7b"),
        );
        let (status, json) = send(router, "GET", "/v1/models", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["error"]["code"], "invalid_api_key");
    }

    // ── Streaming ────────────────────────────────────────────────────────────

    /// What the fake streaming agent should send after the deltas.
    enum StreamEnd {
        Result(InferenceResult),
        MeshError(String),
    }

    /// Spawn a fake agent that, on receiving a streaming RequestModelInference,
    /// pushes `deltas` + the chosen terminal through `state.pending_streams`
    /// exactly like the TCP demux would.
    fn fake_stream_agent(
        connections: &NodeConnections,
        state: &Arc<DashboardState>,
        node_id: &str,
        deltas: Vec<&'static str>,
        end: StreamEnd,
    ) -> Arc<Mutex<Option<InferenceRequest>>> {
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert(node_id.into(), tx);
        let seen: Arc<Mutex<Option<InferenceRequest>>> = Arc::new(Mutex::new(None));
        let seen2 = seen.clone();
        let pending = state.pending_streams.clone();
        let node_id = node_id.to_string();
        tokio::spawn(async move {
            if let Some(MeshMessage::RequestModelInference(req)) = rx.recv().await {
                *seen2.lock().unwrap() = Some(req.clone());
                // The dispatch inserts the entry before sending the request,
                // so it is guaranteed to be present here.
                let stx = pending
                    .lock()
                    .unwrap()
                    .get(&req.request_id)
                    .map(|(s, _)| s.clone())
                    .expect("stream entry registered");
                for d in deltas {
                    let _ = stx
                        .send(MeshMessage::ModelInferenceChunk(shared::InferenceChunk {
                            request_id: req.request_id.clone(),
                            node_id: node_id.clone(),
                            delta: d.to_string(),
                            wire_version: WIRE_VERSION,
                        }))
                        .await;
                }
                match end {
                    StreamEnd::Result(mut res) => {
                        res.request_id = req.request_id.clone();
                        let _ = stx.send(MeshMessage::ModelInferenceResult(res)).await;
                    }
                    StreamEnd::MeshError(e) => {
                        let _ = stx.send(MeshMessage::Error(e)).await;
                    }
                }
            }
        });
        seen
    }

    fn ok_result(output: &str, tokens_generated: u32) -> InferenceResult {
        InferenceResult {
            request_id: String::new(), // filled by the fake agent
            node_id: "node-1".into(),
            model_name: "qwen2.5:7b".into(),
            output: output.into(),
            tokens_generated,
            prompt_tokens: 7,
            duration_ms: 5,
            prompt_eval_ms: 1,
            error: None,
            wire_version: WIRE_VERSION,
        }
    }

    /// POST a streaming request and return (content_type, data payloads).
    async fn send_stream(router: Router, body: &str) -> (String, Vec<String>) {
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let mut parser = shared::sse::SseParser::new();
        let payloads = parser.feed(text.as_bytes());
        (content_type, payloads)
    }

    const STREAM_BODY: &str =
        r#"{"model":"qwen2.5:7b","messages":[{"role":"user","content":"hi"}],"stream":true}"#;

    #[tokio::test]
    async fn stream_happy_path_emits_openai_chunks() {
        let connections = empty_connections();
        let state = make_state(vec![], connections.clone());
        let seen = fake_stream_agent(
            &connections,
            &state,
            "node-1",
            vec!["hel", "lo"],
            StreamEnd::Result(ok_result("hello", 2)),
        );
        let router = v1_router(state.clone(), ready_registry("node-1", "qwen2.5:7b"));

        let (content_type, payloads) = send_stream(router, STREAM_BODY).await;
        assert!(content_type.starts_with("text/event-stream"));

        // role-first chunk, two deltas, finish chunk, [DONE]
        assert_eq!(payloads.len(), 5, "payloads: {payloads:?}");
        let first: serde_json::Value = serde_json::from_str(&payloads[0]).unwrap();
        assert_eq!(first["object"], "chat.completion.chunk");
        assert_eq!(first["choices"][0]["delta"]["role"], "assistant");
        let d1: serde_json::Value = serde_json::from_str(&payloads[1]).unwrap();
        assert_eq!(d1["choices"][0]["delta"]["content"], "hel");
        let d2: serde_json::Value = serde_json::from_str(&payloads[2]).unwrap();
        assert_eq!(d2["choices"][0]["delta"]["content"], "lo");
        let fin: serde_json::Value = serde_json::from_str(&payloads[3]).unwrap();
        assert_eq!(fin["choices"][0]["finish_reason"], "stop");
        assert_eq!(payloads[4], "[DONE]");

        // Every chunk shares one id, and the wire request was a stream.
        assert_eq!(first["id"], d1["id"]);
        assert_eq!(d1["id"], fin["id"]);
        let req = seen.lock().unwrap().take().expect("agent saw request");
        assert!(req.stream);
        // Adapter cleaned up its entry.
        assert!(state.pending_streams.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stream_include_usage_adds_usage_chunk() {
        let connections = empty_connections();
        let state = make_state(vec![], connections.clone());
        let _seen = fake_stream_agent(
            &connections,
            &state,
            "node-1",
            vec!["hi"],
            StreamEnd::Result(ok_result("hi", 1)),
        );
        let router = v1_router(state, ready_registry("node-1", "qwen2.5:7b"));

        let body = r#"{"model":"qwen2.5:7b","messages":[{"role":"user","content":"hi"}],"stream":true,"stream_options":{"include_usage":true}}"#;
        let (_, payloads) = send_stream(router, body).await;
        // role, delta, finish, usage, [DONE]
        assert_eq!(payloads.len(), 5, "payloads: {payloads:?}");
        let usage: serde_json::Value = serde_json::from_str(&payloads[3]).unwrap();
        assert_eq!(usage["usage"]["prompt_tokens"], 7);
        assert_eq!(usage["usage"]["completion_tokens"], 1);
        assert_eq!(usage["usage"]["total_tokens"], 8);
        assert_eq!(usage["choices"], serde_json::json!([]));
        assert_eq!(payloads[4], "[DONE]");
    }

    #[tokio::test]
    async fn stream_error_terminal_emits_error_event() {
        let connections = empty_connections();
        let state = make_state(vec![], connections.clone());
        let mut failed = ok_result("", 0);
        failed.error = Some("GPU exploded".into());
        let _seen = fake_stream_agent(
            &connections,
            &state,
            "node-1",
            vec!["par"],
            StreamEnd::Result(failed),
        );
        let router = v1_router(state, ready_registry("node-1", "qwen2.5:7b"));

        let (_, payloads) = send_stream(router, STREAM_BODY).await;
        // role, one delta, error event, [DONE]
        let err: serde_json::Value = serde_json::from_str(&payloads[2]).unwrap();
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap()
                .contains("GPU exploded")
        );
        assert_eq!(err["error"]["code"], "upstream_error");
        assert_eq!(payloads.last().unwrap(), "[DONE]");
    }

    #[tokio::test]
    async fn stream_node_death_emits_error_event() {
        // The disconnect teardown sends MeshMessage::Error into the channel.
        let connections = empty_connections();
        let state = make_state(vec![], connections.clone());
        let _seen = fake_stream_agent(
            &connections,
            &state,
            "node-1",
            vec![],
            StreamEnd::MeshError("compute node 'node-1' disconnected during inference".into()),
        );
        let router = v1_router(state, ready_registry("node-1", "qwen2.5:7b"));

        let (_, payloads) = send_stream(router, STREAM_BODY).await;
        let err: serde_json::Value = serde_json::from_str(&payloads[1]).unwrap();
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap()
                .contains("disconnected")
        );
        assert_eq!(payloads.last().unwrap(), "[DONE]");
    }

    #[tokio::test]
    async fn stream_terminal_only_result_degrades_to_single_delta() {
        // A pre-v4 agent ignores the unknown `stream` field and replies
        // non-streamed: the full output must still reach the client.
        let connections = empty_connections();
        let state = make_state(vec![], connections.clone());
        let _seen = fake_stream_agent(
            &connections,
            &state,
            "node-1",
            vec![],
            StreamEnd::Result(ok_result("full answer", 3)),
        );
        let router = v1_router(state, ready_registry("node-1", "qwen2.5:7b"));

        let (_, payloads) = send_stream(router, STREAM_BODY).await;
        // role, degradation delta, finish, [DONE]
        assert_eq!(payloads.len(), 4, "payloads: {payloads:?}");
        let d: serde_json::Value = serde_json::from_str(&payloads[1]).unwrap();
        assert_eq!(d["choices"][0]["delta"]["content"], "full answer");
        let fin: serde_json::Value = serde_json::from_str(&payloads[2]).unwrap();
        assert_eq!(fin["choices"][0]["finish_reason"], "stop");
    }

    #[tokio::test]
    async fn stream_finish_reason_length_at_max_tokens() {
        let connections = empty_connections();
        let state = make_state(vec![], connections.clone());
        let _seen = fake_stream_agent(
            &connections,
            &state,
            "node-1",
            vec!["a", "b"],
            StreamEnd::Result(ok_result("ab", 2)),
        );
        let router = v1_router(state, ready_registry("node-1", "qwen2.5:7b"));

        let body = r#"{"model":"qwen2.5:7b","messages":[{"role":"user","content":"hi"}],"stream":true,"max_tokens":2}"#;
        let (_, payloads) = send_stream(router, body).await;
        let fin: serde_json::Value = serde_json::from_str(&payloads[3]).unwrap();
        assert_eq!(fin["choices"][0]["finish_reason"], "length");
    }
}
