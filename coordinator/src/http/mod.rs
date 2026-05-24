use axum::{Router, http::header, response::Html, routing::get};
use tokio::net::TcpListener;
use tracing::{error, info};

const INDEX_HTML: &str = include_str!("static/index.html");
const STYLE_CSS: &str = include_str!("static/style.css");
const DASHBOARD_JS: &str = include_str!("static/dashboard.js");
const MANIFEST_JSON: &str = include_str!("static/manifest.json");
const SERVICE_WORKER_JS: &str = include_str!("static/service-worker.js");

pub fn router() -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route(
            "/static/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    STYLE_CSS,
                )
            }),
        )
        .route(
            "/static/dashboard.js",
            get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    DASHBOARD_JS,
                )
            }),
        )
        .route(
            "/manifest.json",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/manifest+json")],
                    MANIFEST_JSON,
                )
            }),
        )
        .route(
            "/service-worker.js",
            get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    SERVICE_WORKER_JS,
                )
            }),
        )
}

pub async fn start(port: u16) {
    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind dashboard HTTP server to {addr}: {e}"));
    info!("Dashboard available at http://localhost:{port}");
    if let Err(e) = axum::serve(listener, router()).await {
        error!("Dashboard HTTP server error: {e}");
    }
}
