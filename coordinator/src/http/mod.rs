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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn get(uri: &str) -> (StatusCode, String) {
        let resp = router()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        // Drain the body so the connection closes cleanly.
        let _ = to_bytes(resp.into_body(), usize::MAX).await;
        (status, ct)
    }

    #[tokio::test]
    async fn index_returns_200() {
        let (status, ct) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("text/html"), "expected text/html, got {ct}");
    }

    #[tokio::test]
    async fn css_returns_correct_content_type() {
        let (status, ct) = get("/static/style.css").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("text/css"), "expected text/css, got {ct}");
    }

    #[tokio::test]
    async fn dashboard_js_returns_correct_content_type() {
        let (status, ct) = get("/static/dashboard.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("javascript"),
            "expected application/javascript, got {ct}"
        );
    }

    #[tokio::test]
    async fn manifest_returns_correct_content_type() {
        let (status, ct) = get("/manifest.json").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("manifest+json"),
            "expected application/manifest+json, got {ct}"
        );
    }

    #[tokio::test]
    async fn service_worker_returns_correct_content_type() {
        // Browsers silently reject service workers with a non-JS content-type.
        let (status, ct) = get("/service-worker.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("javascript"),
            "expected application/javascript, got {ct}"
        );
    }
}
