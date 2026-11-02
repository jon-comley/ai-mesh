//! The page and its API, served at `http://lenovo1/`.

use std::sync::Arc;

use agentwatch::config::Config;
use agentwatch::markdown;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{App, actions};

const INDEX_HTML: &str = include_str!("../../static/index.html");
const AGENTS_JS: &str = include_str!("../../static/agents.js");
const AGENTS_CSS: &str = include_str!("../../static/agents.css");

type Shared = Arc<App>;
type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

pub fn router(app: Shared) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }),
        )
        .route(
            "/agents.js",
            get(|| async { asset("text/javascript; charset=utf-8", AGENTS_JS) }),
        )
        .route(
            "/agents.css",
            get(|| async { asset("text/css; charset=utf-8", AGENTS_CSS) }),
        )
        .route("/api/agents", get(list))
        .route("/api/agents/refresh", post(refresh))
        .route("/api/agents/{name}/report", get(report))
        .route("/api/agents/{name}/run", post(run_now))
        .route("/api/agents/{name}/reply", post(reply))
        .route(
            "/api/agents/{name}/pause",
            post(|s, p| set_paused(s, p, true)),
        )
        .route(
            "/api/agents/{name}/resume",
            post(|s, p| set_paused(s, p, false)),
        )
        .route("/api/config", get(get_config).put(put_config))
        .with_state(app)
}

fn asset(kind: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, kind),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

fn fail(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": msg.into() })))
}

fn agent(app: &App, name: &str) -> Result<agentwatch::config::Agent, (StatusCode, Json<Value>)> {
    app.agent(name)
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, format!("no agent called {name}")))
}

async fn list(State(app): State<Shared>) -> Json<Value> {
    let ntfy = app
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .ntfy
        .is_some();
    Json(json!({ "agents": app.views(), "ntfy": ntfy }))
}

async fn refresh(State(app): State<Shared>) -> Json<Value> {
    app.refresh_all().await;
    list(State(app)).await
}

async fn report(State(app): State<Shared>, Path(name): Path<String>) -> ApiResult {
    let a = agent(&app, &name)?;
    let view = app.views().into_iter().find(|v| v.name == a.name);
    let Some(path) = view.and_then(|v| v.report) else {
        return Err(fail(StatusCode::NOT_FOUND, "no report yet"));
    };
    let _work = app.work.lock().await;
    let md = app
        .repo(&a)
        .read(&path)
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, format!("{path} is not in the repo")))?;
    let folder = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let base = a.web_url().map(|u| format!("{u}/blob/main/{folder}"));
    Ok(Json(
        json!({ "path": path, "html": markdown::render(&md, base.as_deref()) }),
    ))
}

async fn run_now(State(app): State<Shared>, Path(name): Path<String>) -> ApiResult {
    let a = agent(&app, &name)?;
    let _work = app.work.lock().await;
    actions::run_now(&app, &a)
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(json!({ "agent": app.refresh(&a).await })))
}

#[derive(Deserialize)]
struct Reply {
    number: String,
    text: String,
    #[serde(default)]
    run_now: bool,
}

async fn reply(
    State(app): State<Shared>,
    Path(name): Path<String>,
    Json(r): Json<Reply>,
) -> ApiResult {
    let a = agent(&app, &name)?;
    let _work = app.work.lock().await;
    actions::reply(&app, &a, &r.number, &r.text)
        .await
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    let run_error = if r.run_now {
        actions::run_now(&app, &a).await.err()
    } else {
        None
    };
    Ok(Json(
        json!({ "agent": app.refresh(&a).await, "run_error": run_error }),
    ))
}

async fn set_paused(
    State(app): State<Shared>,
    Path(name): Path<String>,
    paused: bool,
) -> ApiResult {
    let a = agent(&app, &name)?;
    let _work = app.work.lock().await;
    actions::set_paused(&a, paused)
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(json!({ "agent": app.refresh(&a).await })))
}

async fn get_config(State(app): State<Shared>) -> Json<Value> {
    let text = std::fs::read_to_string(app.config_path()).unwrap_or_default();
    Json(json!({ "text": text }))
}

#[derive(Deserialize)]
struct ConfigText {
    text: String,
}

/// Saves `agents.toml` as Jon typed it (comments kept), once it parses.
async fn put_config(State(app): State<Shared>, Json(c): Json<ConfigText>) -> ApiResult {
    let config = Config::parse(&c.text).map_err(|e| fail(StatusCode::BAD_REQUEST, e.0))?;
    std::fs::write(app.config_path(), &c.text)
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *app.config.write().unwrap_or_else(|p| p.into_inner()) = config;
    app.refresh_all().await;
    Ok(Json(json!({ "agents": app.views() })))
}
