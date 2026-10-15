//! The Reviews tab's API (docs/code-review.md).
//!
//! mac1 runs the reviews and owns their data; the coordinator keeps the latest
//! snapshot mac1 sent and passes the dashboard's commands back to it. Every
//! write therefore needs mac1 connected, and says so when it is not. The one
//! setting held here is which models answer home commands and which do
//! review work (`/api/work/roles`), because routing happens here.

use crate::http::auth::Authed;
use crate::http::state::DashboardState;
use crate::registry::Registry;
use crate::work_router::{self, ModelRoles};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use shared::{MeshMessage, ReviewCommand, ReviewRepoSpec, ReviewSnapshot};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Serialize)]
pub struct ReviewsView {
    /// The machine running the reviews is connected right now.
    online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot: Option<ReviewSnapshot>,
    roles: ModelRoles,
    /// Models Ready anywhere, for the roles editor.
    models: Vec<String>,
}

/// The node running the reviews: the one that last sent a snapshot, else any
/// connected node advertising the review feature.
fn review_node(state: &DashboardState, registry: &Arc<Mutex<Registry>>) -> Option<String> {
    let conns = state.connections.lock().unwrap();
    if let Some(snap) = state.review_snapshot()
        && conns.contains_key(&snap.node_id)
    {
        return Some(snap.node_id);
    }
    registry
        .lock()
        .unwrap()
        .nodes_with_feature(shared::Feature::Review)
        .into_iter()
        .map(|n| n.id)
        .find(|id| conns.contains_key(id))
}

fn offline() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({"error": "the review machine (mac1) is offline"})),
    )
        .into_response()
}

async fn send_command(
    state: &DashboardState,
    registry: &Arc<Mutex<Registry>>,
    cmd: ReviewCommand,
) -> Result<(), Response> {
    let node = review_node(state, registry).ok_or_else(offline)?;
    let tx = state.connections.lock().unwrap().get(&node).cloned();
    match tx {
        Some(tx) if tx.send(MeshMessage::ReviewCommand(cmd)).await.is_ok() => Ok(()),
        _ => Err(offline()),
    }
}

pub async fn get_reviews(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> Json<ReviewsView> {
    let online = review_node(&state, &registry).is_some();
    let models = registry.lock().unwrap().ready_llm_models();
    Json(ReviewsView {
        online,
        snapshot: state.review_snapshot(),
        roles: work_router::state().lock().unwrap().roles.clone(),
        models,
    })
}

#[derive(Deserialize)]
pub struct RunNowBody {
    repo: String,
    #[serde(default)]
    sweep: bool,
    /// Review this folder or file instead of new commits.
    #[serde(default)]
    path: Option<String>,
    /// Review what this branch changes compared with the main branch.
    #[serde(default)]
    branch: Option<String>,
}

pub async fn run_now(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<RunNowBody>,
) -> Response {
    match send_command(
        &state,
        &registry,
        ReviewCommand::RunNow {
            repo: body.repo,
            sweep: body.sweep,
            path: body.path,
            branch: body.branch,
        },
    )
    .await
    {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
pub struct AskBody {
    repo: String,
    question: String,
}

/// Ask mac1 a question about a repo. Answers take minutes, so this returns
/// the question's id at once; the answer arrives in the snapshot's
/// `questions` (and so on the Reviews tab, live).
pub async fn ask(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<AskBody>,
) -> Response {
    let question = body.question.trim().to_string();
    if question.is_empty() || question.len() > 2_000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "a question needs some text, and at most 2,000 characters"})),
        )
            .into_response();
    }
    let id = super::gen_request_id();
    match send_command(
        &state,
        &registry,
        ReviewCommand::Ask {
            id: id.clone(),
            repo: body.repo,
            question,
        },
    )
    .await
    {
        Ok(()) => (StatusCode::ACCEPTED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(r) => r,
    }
}

pub async fn upsert_repo(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(spec): Json<ReviewRepoSpec>,
) -> Response {
    match send_command(&state, &registry, ReviewCommand::UpsertRepo { spec }).await {
        // mac1 checks the URL; a refusal comes back as the snapshot's notice.
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(r) => r,
    }
}

pub async fn remove_repo(
    Path(name): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> Response {
    match send_command(&state, &registry, ReviewCommand::RemoveRepo { name }).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
pub struct FindingStatusBody {
    status: String,
}

pub async fn set_finding_status(
    Path(id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<FindingStatusBody>,
) -> Response {
    if !matches!(body.status.as_str(), "open" | "dismissed" | "fixed") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "status must be open, dismissed or fixed"})),
        )
            .into_response();
    }
    match send_command(
        &state,
        &registry,
        ReviewCommand::SetFindingStatus {
            id,
            status: body.status,
        },
    )
    .await
    {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
pub struct SettingsBody {
    #[serde(default)]
    ntfy_topic_url: Option<String>,
    #[serde(default)]
    max_review_tokens: Option<u32>,
    #[serde(default)]
    evening_max_tokens: Option<u32>,
}

pub async fn set_settings(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SettingsBody>,
) -> Response {
    match send_command(
        &state,
        &registry,
        ReviewCommand::SetSettings {
            ntfy_topic_url: body.ntfy_topic_url,
            max_review_tokens: body.max_review_tokens,
            evening_max_tokens: body.evening_max_tokens,
        },
    )
    .await
    {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(r) => r,
    }
}

/// A run's Markdown report, fetched from mac1.
pub async fn get_report(
    Path(run_id): Path<i64>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> Response {
    let request_id = super::gen_request_id();
    let rx = state.expect_review_reply(&request_id);
    if let Err(r) = send_command(
        &state,
        &registry,
        ReviewCommand::FetchReport {
            request_id: request_id.clone(),
            run_id,
        },
    )
    .await
    {
        state.forget_review_reply(&request_id);
        return r;
    }
    match tokio::time::timeout(Duration::from_secs(15), rx).await {
        Ok(Ok(reply)) => match reply.markdown {
            Some(md) => {
                ([(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], md).into_response()
            }
            None => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": reply.error.unwrap_or_default()})),
            )
                .into_response(),
        },
        _ => {
            state.forget_review_reply(&request_id);
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(serde_json::json!({"error": "mac1 did not send the report"})),
            )
                .into_response()
        }
    }
}

pub async fn get_roles(_: Authed) -> Json<ModelRoles> {
    Json(work_router::state().lock().unwrap().roles.clone())
}

/// Set which models answer home commands and which do review work. Takes
/// effect for the next request.
pub async fn set_roles(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    Json(roles): Json<ModelRoles>,
) -> Json<ModelRoles> {
    let clean = |v: Vec<String>| -> Vec<String> {
        let mut out: Vec<String> = v
            .into_iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty() && !m.contains(','))
            .collect();
        out.dedup();
        out
    };
    let roles = ModelRoles {
        control: clean(roles.control),
        work: clean(roles.work),
    };
    work_router::save_roles(&registry.lock().unwrap(), &roles);
    work_router::state().lock().unwrap().roles = roles.clone();
    Json(roles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::{delete, get, post};
    use tokio::sync::mpsc;

    fn router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route("/api/reviews", get(get_reviews))
            .route("/api/reviews/run-now", post(run_now))
            .route("/api/reviews/ask", post(ask))
            .route("/api/reviews/repos", post(upsert_repo))
            .route("/api/reviews/repos/{name}", delete(remove_repo))
            .route("/api/reviews/findings/{id}", post(set_finding_status))
            .route("/api/reviews/settings", post(set_settings))
            .route("/api/reviews/runs/{id}/report", get(get_report))
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    fn snapshot(node: &str) -> ReviewSnapshot {
        ReviewSnapshot {
            node_id: node.into(),
            hostname: node.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn commands_need_mac1_connected() {
        let state = make_state(vec![], empty_connections());
        let status = send(
            router(state, make_registry()),
            "POST",
            "/api/reviews/run-now?token=",
            r#"{"repo":"dashboard"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn commands_reach_the_node_that_sent_the_snapshot() {
        let conns = empty_connections();
        let (tx, mut rx) = mpsc::channel(4);
        conns.lock().unwrap().insert("mac1".into(), tx);
        let state = make_state(vec![], conns);
        state.set_review_snapshot(snapshot("mac1"));
        let status = send(
            router(state.clone(), make_registry()),
            "POST",
            "/api/reviews/run-now?token=",
            r#"{"repo":"dashboard","sweep":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(
            rx.recv().await,
            Some(MeshMessage::ReviewCommand(ReviewCommand::RunNow {
                repo: "dashboard".into(),
                sweep: true,
                path: None,
                branch: None,
            }))
        );
        let (status, body) = send_with_body(
            router(state, make_registry()),
            "GET",
            "/api/reviews?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["online"], true);
        assert_eq!(v["snapshot"]["node_id"], "mac1");
    }

    #[tokio::test]
    async fn a_question_is_sent_with_an_id_and_the_id_returned() {
        let conns = empty_connections();
        let (tx, mut rx) = mpsc::channel(4);
        conns.lock().unwrap().insert("mac1".into(), tx);
        let state = make_state(vec![], conns);
        state.set_review_snapshot(snapshot("mac1"));
        let (status, body) = send_with_body(
            router(state.clone(), make_registry()),
            "POST",
            "/api/reviews/ask?token=",
            r#"{"repo":"dashboard","question":"  Where is VAT added?  "}"#,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let Some(MeshMessage::ReviewCommand(ReviewCommand::Ask {
            id: sent,
            repo,
            question,
        })) = rx.recv().await
        else {
            panic!("expected an Ask");
        };
        assert_eq!(
            (sent, repo, question.as_str()),
            (id, "dashboard".to_string(), "Where is VAT added?")
        );

        let status = send(
            router(state, make_registry()),
            "POST",
            "/api/reviews/ask?token=",
            r#"{"repo":"dashboard","question":"   "}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn run_now_passes_a_path_or_branch_through() {
        let conns = empty_connections();
        let (tx, mut rx) = mpsc::channel(4);
        conns.lock().unwrap().insert("mac1".into(), tx);
        let state = make_state(vec![], conns);
        state.set_review_snapshot(snapshot("mac1"));
        send(
            router(state, make_registry()),
            "POST",
            "/api/reviews/run-now?token=",
            r#"{"repo":"guv","branch":"feature-x"}"#,
        )
        .await;
        assert_eq!(
            rx.recv().await,
            Some(MeshMessage::ReviewCommand(ReviewCommand::RunNow {
                repo: "guv".into(),
                sweep: false,
                path: None,
                branch: Some("feature-x".into()),
            }))
        );
    }

    #[tokio::test]
    async fn a_bad_finding_status_is_refused_before_it_is_sent() {
        let conns = empty_connections();
        let (tx, mut rx) = mpsc::channel(4);
        conns.lock().unwrap().insert("mac1".into(), tx);
        let state = make_state(vec![], conns);
        state.set_review_snapshot(snapshot("mac1"));
        let status = send(
            router(state, make_registry()),
            "POST",
            "/api/reviews/findings/abc?token=",
            r#"{"status":"deleted"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn the_report_comes_back_from_mac1() {
        let conns = empty_connections();
        let (tx, mut rx) = mpsc::channel(4);
        conns.lock().unwrap().insert("mac1".into(), tx);
        let state = make_state(vec![], conns);
        state.set_review_snapshot(snapshot("mac1"));
        let responder = {
            let state = state.clone();
            tokio::spawn(async move {
                if let Some(MeshMessage::ReviewCommand(ReviewCommand::FetchReport {
                    request_id,
                    run_id,
                })) = rx.recv().await
                {
                    state.resolve_review_reply(shared::ReviewReply {
                        request_id,
                        markdown: Some(format!("# Code review: run {run_id}")),
                        error: None,
                    });
                }
            })
        };
        let (status, body) = send_with_body(
            router(state, make_registry()),
            "GET",
            "/api/reviews/runs/7/report?token=",
            "",
        )
        .await;
        responder.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "# Code review: run 7");
    }
}
