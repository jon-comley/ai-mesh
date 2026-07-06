//! Hugging Face model search — a self-populating alternative to typing an
//! exact `hf:<org>/<repo>:<filename.gguf>` custom-model string by hand (see
//! `capability-llm::llama::resolve_gguf`'s own doc comment: curated names
//! cover ~20 models, the leaderboards list thousands). Two steps, mirroring
//! how you'd actually pick a model on huggingface.co itself: search for a
//! repo, then pick one of its GGUF files. Coordinator-side proxy rather than
//! a direct browser fetch — the coordinator already makes outbound HTTPS
//! calls to huggingface.co (see `capability-llm::llama::download_shard`), so
//! this reuses an already-proven network path instead of relying on HF's CORS
//! policy from the browser.

use axum::{Json, extract::Query, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};

use crate::http::auth::Authed;

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("ai-mesh-coordinator")
            .build()
            .unwrap_or_default()
    })
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}

#[derive(Deserialize)]
struct HfSearchHit {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
}

#[derive(Serialize)]
pub struct ModelSearchHit {
    repo: String,
    downloads: u64,
    likes: u64,
}

/// `GET /api/models/search?q=<text>` — repo-level search, gguf-tagged only.
/// Returns the plain array HF's own API returns, reshaped to just the fields
/// the picker needs.
pub async fn search_models(_: Authed, Query(q): Query<SearchQuery>) -> impl IntoResponse {
    let query = q.q.trim();
    if query.is_empty() {
        return (StatusCode::BAD_REQUEST, "q must not be empty").into_response();
    }
    let resp = http_client()
        .get("https://huggingface.co/api/models")
        .query(&[
            ("search", query),
            ("filter", "gguf"),
            ("sort", "downloads"),
            ("direction", "-1"),
            ("limit", "20"),
        ])
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "model search: request to huggingface.co failed");
            return (StatusCode::BAD_GATEWAY, "could not reach huggingface.co").into_response();
        }
    };
    if !resp.status().is_success() {
        return (StatusCode::BAD_GATEWAY, "huggingface.co search failed").into_response();
    }
    let hits: Vec<HfSearchHit> = match resp.json().await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "model search: unexpected response shape");
            return (
                StatusCode::BAD_GATEWAY,
                "unexpected response from huggingface.co",
            )
                .into_response();
        }
    };
    let out: Vec<ModelSearchHit> = hits
        .into_iter()
        .map(|h| ModelSearchHit {
            repo: h.id,
            downloads: h.downloads,
            likes: h.likes,
        })
        .collect();
    Json(out).into_response()
}

#[derive(Deserialize)]
pub struct FilesQuery {
    repo: String,
}

#[derive(Deserialize)]
struct HfTreeEntry {
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
    #[serde(default)]
    size: u64,
}

#[derive(Serialize)]
pub struct ModelFileHit {
    filename: String,
    size_mb: u64,
}

/// `GET /api/models/search/files?repo=<org>/<repo>` — single-file GGUFs only
/// (filenames containing "-of-" are one shard of a multi-file set;
/// `resolve_gguf`'s `hf:` path can't load those, so they're filtered out here
/// rather than surfaced as a choice that would fail on load).
pub async fn search_model_files(_: Authed, Query(q): Query<FilesQuery>) -> impl IntoResponse {
    let repo = q.repo.trim();
    if repo.is_empty() || !repo.contains('/') {
        return (StatusCode::BAD_REQUEST, "repo must look like org/repo").into_response();
    }
    let url = format!("https://huggingface.co/api/models/{repo}/tree/main");
    let resp = http_client().get(&url).send().await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "model file list: request to huggingface.co failed");
            return (StatusCode::BAD_GATEWAY, "could not reach huggingface.co").into_response();
        }
    };
    if !resp.status().is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            "huggingface.co file listing failed",
        )
            .into_response();
    }
    let entries: Vec<HfTreeEntry> = match resp.json().await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "model file list: unexpected response shape");
            return (
                StatusCode::BAD_GATEWAY,
                "unexpected response from huggingface.co",
            )
                .into_response();
        }
    };
    let mut files: Vec<ModelFileHit> = entries
        .into_iter()
        .filter(|e| e.entry_type == "file" && e.path.ends_with(".gguf") && !e.path.contains("-of-"))
        .map(|e| ModelFileHit {
            filename: e.path,
            size_mb: e.size / (1024 * 1024),
        })
        .collect();
    files.sort_by_key(|f| f.size_mb);
    Json(files).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use crate::http::state::DashboardState;
    use axum::Router;
    use axum::routing::get;
    use std::sync::Arc;

    fn search_router(state: Arc<DashboardState>) -> Router {
        Router::new()
            .route("/api/models/search", get(search_models))
            .route("/api/models/search/files", get(search_model_files))
            .with_state(state)
    }

    #[tokio::test]
    async fn search_models_returns_400_for_empty_query() {
        let state = make_state(vec![], empty_connections());
        let status = send(
            search_router(state),
            "GET",
            "/api/models/search?q=&token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn search_models_returns_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            search_router(state),
            "GET",
            "/api/models/search?q=qwen&token=wrong",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn search_model_files_returns_400_for_repo_without_slash() {
        let state = make_state(vec![], empty_connections());
        let status = send(
            search_router(state),
            "GET",
            "/api/models/search/files?repo=notarepo&token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn search_model_files_returns_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            search_router(state),
            "GET",
            "/api/models/search/files?repo=org/repo&token=wrong",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn multi_shard_gguf_filenames_are_excluded() {
        let entries = [
            "model-q4_k_m-00001-of-00002.gguf",
            "model-q4_k_m.gguf",
            "README.md",
        ];
        let kept: Vec<&&str> = entries
            .iter()
            .filter(|p| p.ends_with(".gguf") && !p.contains("-of-"))
            .collect();
        assert_eq!(kept, vec![&"model-q4_k_m.gguf"]);
    }
}
