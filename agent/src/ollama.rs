use serde::{Deserialize, Serialize};
use std::time::Instant;

fn ollama_base() -> String {
    std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".into())
}

// ── /api/pull ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct PullRequest<'a> {
    model: &'a str,
    stream: bool,
}

/// Pull (download/verify) a model into the local Ollama store.
/// Returns Ok(()) when Ollama reports status "success".
pub async fn pull_model(model_name: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/pull", ollama_base());

    let resp = client
        .post(&url)
        .json(&PullRequest {
            model: model_name,
            stream: false,
        })
        .send()
        .await
        .map_err(|e| format!("pull request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("pull returned HTTP {status}: {body}"));
    }

    // With stream:false Ollama sends a single JSON object. We only care that it
    // arrived without error; any non-error status means the model is available.
    Ok(())
}

// ── /api/generate ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
    #[serde(default)]
    eval_count: u32,
    /// Nanoseconds Ollama spent on token generation.
    #[serde(default)]
    eval_duration: u64,
}

/// Run inference against the local Ollama instance.
/// Returns (output_text, tokens_generated, duration_ms).
pub async fn generate(model_name: &str, prompt: &str) -> Result<(String, u32, u64), String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/generate", ollama_base());

    let wall_start = Instant::now();

    let resp = client
        .post(&url)
        .json(&GenerateRequest {
            model: model_name,
            prompt,
            stream: false,
        })
        .send()
        .await
        .map_err(|e| format!("generate request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("generate returned HTTP {status}: {body}"));
    }

    let resp_body: GenerateResponse = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse generate response: {e}"))?;

    // Prefer Ollama's own eval_duration (nanoseconds → ms); fall back to wall clock.
    let duration_ms = if resp_body.eval_duration > 0 {
        resp_body.eval_duration / 1_000_000
    } else {
        wall_start.elapsed().as_millis() as u64
    };

    Ok((resp_body.response, resp_body.eval_count, duration_ms))
}
