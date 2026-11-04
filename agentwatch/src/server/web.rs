//! The page and its API, served at `http://lenovo1/`.

use std::sync::Arc;

use agentwatch::config::Config;
use agentwatch::config::Kind;
use agentwatch::{markdown, memory, reports};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::StreamExt;

use super::{App, actions, poll, run};

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
        .route("/api/agents/{name}/ticket", get(ticket))
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
        .route(
            "/api/agents/{name}/chat",
            get(chat_messages).post(chat_send),
        )
        .route("/api/agents/{name}/chat/new", post(chat_new))
        .route("/api/agents/{name}/term", get(term))
        .route("/api/live", get(live))
        .route("/api/schedule", get(schedule_list))
        .route("/api/schedule/{name}/log", get(schedule_log))
        .route("/api/schedule/{name}/{action}", post(schedule_act))
        .route("/api/agents/{name}/tasks/{id}/retry", post(task_retry))
        .route("/api/memory", get(memory_list))
        .route("/api/memory/file", get(memory_file).put(memory_save))
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

/// The terminal's state, read fresh: the full-screen page polls it.
async fn term(State(app): State<Shared>, Path(name): Path<String>) -> ApiResult {
    let a = agent(&app, &name)?;
    let term = super::poll::term(&app, &a).await;
    Ok(Json(json!({ "term": term, "port": TERM_PORT })))
}

/// What changes from minute to minute, for an open page: each worker's
/// terminal and whether its ticks are paused. Read at most every 10 seconds,
/// however many pages ask.
async fn live(State(app): State<Shared>) -> Json<Value> {
    let mut cache = app.live.lock().await;
    if let Some((at, v)) = cache.as_ref() {
        if at.elapsed() < std::time::Duration::from_secs(10) {
            return Json(v.clone());
        }
    }
    let agents = app
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .agents
        .clone();
    let mut out = serde_json::Map::new();
    for a in agents {
        let term = super::poll::term(&app, &a).await;
        let paused = match (&a.host, &a.pause) {
            (Some(host), Some(pause)) => {
                let script = format!("[ -e {pause} ] && echo yes || echo no");
                match super::run::ssh(host, &script).output().await {
                    Ok(o) => Some(String::from_utf8_lossy(&o.stdout).trim() == "yes"),
                    Err(_) => None,
                }
            }
            _ => None,
        };
        let machine_on = app
            .machine_on
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&a.name)
            .copied();
        let tasks = if a.kind == agentwatch::config::Kind::Queue {
            let mut v = super::poll::blank(&a, chrono::Utc::now());
            super::poll::queue_view(&app, &a, &mut v);
            Some(
                json!({ "tasks": v.tasks, "waiting": v.waiting, "line": v.line, "state": v.state }),
            )
        } else {
            None
        };
        out.insert(
            a.name.clone(),
            json!({ "term": term, "paused": paused, "machine_on": machine_on, "queue": tasks }),
        );
    }
    let v = Value::Object(out);
    *cache = Some((std::time::Instant::now(), v.clone()));
    Json(v)
}

async fn task_retry(
    State(app): State<Shared>,
    Path((name, id)): Path<(String, String)>,
) -> ApiResult {
    let a = agent(&app, &name)?;
    let slug = agentterm::slug(&a.name).ok_or_else(|| fail(StatusCode::BAD_REQUEST, "bad name"))?;
    let t = super::dispatch::retry(&agentterm::term_dir(&app.dir, &slug), &id)
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    *app.live.lock().await = None;
    Ok(Json(json!({ "task": t })))
}

async fn schedule_list(State(app): State<Shared>) -> Json<Value> {
    Json(super::schedule::all(&app).await)
}

fn service(
    app: &App,
    name: &str,
) -> Result<agentwatch::config::Service, (StatusCode, Json<Value>)> {
    app.config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .services
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .cloned()
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, format!("no service called {name}")))
}

async fn schedule_log(State(app): State<Shared>, Path(name): Path<String>) -> ApiResult {
    let s = service(&app, &name)?;
    let text = super::schedule::log(&s)
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(json!({ "log": text })))
}

async fn schedule_act(
    State(app): State<Shared>,
    Path((name, action)): Path<(String, String)>,
) -> ApiResult {
    let s = service(&app, &name)?;
    super::schedule::act(&s, &action)
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(
        json!({ "service": super::schedule::service(&s).await }),
    ))
}

/// ttyd's port on this machine (systemd/agentterm.service).
const TERM_PORT: u16 = 7681;

async fn refresh(State(app): State<Shared>) -> Json<Value> {
    app.refresh_all().await;
    list(State(app)).await
}

#[derive(Deserialize)]
struct ReportQuery {
    /// One of the reports the tile lists; the latest when left out.
    path: Option<String>,
}

async fn report(
    State(app): State<Shared>,
    Path(name): Path<String>,
    Query(q): Query<ReportQuery>,
) -> ApiResult {
    let a = agent(&app, &name)?;
    let Some(view) = app.views().into_iter().find(|v| v.name == a.name) else {
        return Err(fail(StatusCode::NOT_FOUND, "no report yet"));
    };
    let listed: Vec<String> = view
        .report
        .iter()
        .cloned()
        .chain(view.entries.iter().filter_map(|e| e.report.clone()))
        .collect();
    let path = match q.path {
        Some(p) if listed.contains(&p) => p,
        Some(_) => {
            return Err(fail(
                StatusCode::NOT_FOUND,
                "that report is not one this tile lists",
            ));
        }
        None => view
            .report
            .ok_or_else(|| fail(StatusCode::NOT_FOUND, "no report yet"))?,
    };
    let (md, base) = match a.kind {
        Kind::Queue => return Err(fail(StatusCode::NOT_FOUND, "no reports")),
        Kind::Cloud => {
            let _work = app.work.lock().await;
            let md = app
                .repo(&a)
                .read(&path)
                .ok_or_else(|| fail(StatusCode::NOT_FOUND, format!("{path} is not in the repo")))?;
            let folder = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            (md, a.web_url().map(|u| format!("{u}/blob/main/{folder}")))
        }
        Kind::Local => {
            if !reports::safe_path(&path) {
                return Err(fail(StatusCode::BAD_REQUEST, "not a report path"));
            }
            let host = a.host.as_deref().unwrap_or_default();
            let md = run::output(run::ssh(host, &format!("cat {path}")), run::SSH_LIMIT)
                .await
                .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
            (md, None)
        }
    };
    Ok(Json(
        json!({ "path": path, "html": markdown::render(&md, base.as_deref()) }),
    ))
}

/// One of a queue worker's tickets, rendered: read through the laptop's mount.
async fn ticket(
    State(app): State<Shared>,
    Path(name): Path<String>,
    Query(q): Query<ReportQuery>,
) -> ApiResult {
    let a = agent(&app, &name)?;
    let view = app.views().into_iter().find(|v| v.name == a.name);
    let path = q.path.unwrap_or_default();
    let listed = view.is_some_and(|v| v.tickets.iter().any(|t| t.path == path));
    if !listed || !poll::ticket_path_ok(&path) {
        return Err(fail(StatusCode::NOT_FOUND, "that ticket is not one this tile lists"));
    }
    let md = std::fs::read_to_string(poll::ticket_file(&path)).map_err(|_| {
        fail(StatusCode::NOT_FOUND, "can't read it: the work laptop is off or its repos aren't mounted")
    })?;
    Ok(Json(json!({ "path": path, "html": markdown::render(&md, None) })))
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
    // Jon's own Pause or Resume wins over a chat's: the chat no longer resumes it.
    app.chat_paused
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&a.name);
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

fn chat_busy(app: &App, name: &str) -> bool {
    app.chatting
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(name)
}

async fn chat_messages(State(app): State<Shared>, Path(name): Path<String>) -> ApiResult {
    let a = agent(&app, &name)?;
    let messages = app
        .store()
        .messages(&a.name)
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(
        json!({ "messages": messages, "busy": chat_busy(&app, &a.name), "can_chat": a.kind == Kind::Cloud || a.chat_dir.is_some() }),
    ))
}

#[derive(Deserialize)]
struct ChatMessage {
    message: String,
}

/// Sends a message and streams the answer back as it comes, one JSON event
/// per line. The answer carries on, and is kept, if the page is closed.
async fn chat_send(
    State(app): State<Shared>,
    Path(name): Path<String>,
    Json(m): Json<ChatMessage>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let a = agent(&app, &name)?;
    let message = m.message.trim().to_string();
    if message.is_empty() {
        return Err(fail(StatusCode::BAD_REQUEST, "say something first"));
    }
    if !app
        .chatting
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(a.name.clone())
    {
        return Err(fail(
            StatusCode::CONFLICT,
            format!("{} is still answering", a.name),
        ));
    }
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let worker = app.clone();
    tokio::spawn(async move {
        super::chat::talk(&worker, &a, &message, tx).await;
        worker
            .chatting
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&a.name);
    });
    let lines = tokio_stream::wrappers::ReceiverStream::new(rx).map(|event| {
        let mut line = serde_json::to_string(&event).unwrap_or_default();
        line.push('\n');
        Ok::<_, std::convert::Infallible>(line)
    });
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        axum::body::Body::from_stream(lines),
    )
        .into_response())
}

async fn chat_new(State(app): State<Shared>, Path(name): Path<String>) -> ApiResult {
    let a = agent(&app, &name)?;
    if chat_busy(&app, &a.name) {
        return Err(fail(
            StatusCode::CONFLICT,
            format!("{} is still answering", a.name),
        ));
    }
    app.store()
        .clear_chat(&a.name)
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "messages": [] })))
}

fn memory_repo(app: &App) -> Result<super::repo::Repo, (StatusCode, Json<Value>)> {
    app.memory_repo()
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "no memory repo is set in Settings"))
}

/// Every fact in the shared memory, by folder.
async fn memory_list(State(app): State<Shared>) -> ApiResult {
    let repo = memory_repo(&app)?;
    let _work = app.work.lock().await;
    repo.sync_keep()
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
    let index = repo.read("INDEX.md").unwrap_or_default();
    let mut facts = Vec::new();
    for dir in std::fs::read_dir(&repo.dir)
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .flatten()
    {
        let folder = dir.file_name().to_string_lossy().into_owned();
        if folder.starts_with('.') || !dir.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(dir.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = format!("{folder}/{}", f.file_name().to_string_lossy());
            if memory::safe_path(&path)
                && let Ok(text) = std::fs::read_to_string(f.path())
            {
                facts.push(memory::front_matter(&path, &text));
            }
        }
    }
    let count = facts.len();
    let updated = repo.last_commit_time().await.ok().flatten();
    Ok(Json(
        json!({ "folders": memory::folders(&index, facts), "count": count, "updated": updated }),
    ))
}

#[derive(Deserialize)]
struct MemoryPath {
    path: String,
}

async fn memory_file(State(app): State<Shared>, Query(q): Query<MemoryPath>) -> ApiResult {
    if !memory::safe_path(&q.path) {
        return Err(fail(StatusCode::BAD_REQUEST, "not a memory file"));
    }
    let repo = memory_repo(&app)?;
    let text = repo.read(&q.path).ok_or_else(|| {
        fail(
            StatusCode::NOT_FOUND,
            format!("{} is not in the memory", q.path),
        )
    })?;
    let body = text
        .strip_prefix("---\n")
        .and_then(|r| r.split_once("\n---").map(|(_, b)| b))
        .unwrap_or(&text);
    let fact = memory::front_matter(&q.path, &text);
    Ok(Json(
        json!({ "fact": fact, "text": text, "html": markdown::render(body, None) }),
    ))
}

#[derive(Deserialize)]
struct MemorySave {
    path: String,
    text: String,
}

/// Saves Jon's edit to one fact, keeps its index line right, and pushes.
async fn memory_save(State(app): State<Shared>, Json(m): Json<MemorySave>) -> ApiResult {
    if !memory::safe_path(&m.path) {
        return Err(fail(StatusCode::BAD_REQUEST, "not a memory file"));
    }
    let repo = memory_repo(&app)?;
    let _work = app.work.lock().await;
    repo.sync_keep()
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
    let Some(before) = repo.read(&m.path) else {
        return Err(fail(
            StatusCode::NOT_FOUND,
            format!("{} is not in the memory", m.path),
        ));
    };
    let mut text = m.text.replace("\r\n", "\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let fact = memory::front_matter(&m.path, &text);
    if text == before {
        return Ok(Json(json!({ "fact": fact })));
    }
    if fact.description.is_empty() {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "keep the front matter: a description line between the --- lines",
        ));
    }
    let index = memory::set_index_line(
        &repo.read("INDEX.md").unwrap_or_default(),
        &m.path,
        &fact.description,
    );
    let message = format!("Memory: {} edited by Jon", m.path);
    repo.commit_files(
        &[
            (m.path.as_str(), text.as_str()),
            ("INDEX.md", index.as_str()),
        ],
        &message,
    )
    .await
    .map_err(|e| fail(StatusCode::CONFLICT, format!("not saved: {e}")))?;
    Ok(Json(json!({ "fact": fact })))
}
