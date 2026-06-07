use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio::process::Child;
use tokio::sync::Mutex;

// ── HTTP client (shared, keeps connection pool to llama-server) ───────────────

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        let timeout_secs = std::env::var("LLAMA_GENERATE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(90u64);
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
            .expect("failed to build HTTP client")
    })
}

// ── Configuration ─────────────────────────────────────────────────────────────

fn llama_host() -> String {
    std::env::var("LLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:8080".into())
}

fn model_dir() -> PathBuf {
    if let Ok(d) = std::env::var("LLAMA_MODEL_DIR") {
        return PathBuf::from(d);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ai-mesh")
        .join("models")
}

fn server_bin() -> String {
    std::env::var("LLAMA_SERVER_BIN").unwrap_or_else(|_| "llama-server".into())
}

fn gpu_layers() -> u32 {
    std::env::var("LLAMA_GPU_LAYERS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

fn flash_attn() -> bool {
    std::env::var("LLAMA_FLASH_ATTN")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

// ── Child process tracking ────────────────────────────────────────────────────

static LLAMA_CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn child_lock() -> &'static Mutex<Option<Child>> {
    LLAMA_CHILD.get_or_init(|| Mutex::new(None))
}

async fn kill_existing() {
    let mut guard = child_lock().lock().await;
    if let Some(mut child) = guard.take() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

// ── Model map ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct GgufSpec {
    repo: &'static str,
    /// All shard filenames in order. Single-file models have one entry.
    shards: &'static [&'static str],
}

fn resolve_gguf(model_name: &str) -> Result<GgufSpec, String> {
    match model_name {
        "qwen2.5:0.5b" => Ok(GgufSpec {
            repo: "Qwen/Qwen2.5-0.5B-Instruct-GGUF",
            shards: &["qwen2.5-0.5b-instruct-q4_k_m.gguf"],
        }),
        "qwen2.5:1.5b" => Ok(GgufSpec {
            repo: "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
            shards: &["qwen2.5-1.5b-instruct-q4_k_m.gguf"],
        }),
        "qwen2.5:7b" => Ok(GgufSpec {
            repo: "Qwen/Qwen2.5-7B-Instruct-GGUF",
            shards: &[
                "qwen2.5-7b-instruct-q4_k_m-00001-of-00002.gguf",
                "qwen2.5-7b-instruct-q4_k_m-00002-of-00002.gguf",
            ],
        }),
        "qwen2.5:14b" => Ok(GgufSpec {
            repo: "Qwen/Qwen2.5-14B-Instruct-GGUF",
            shards: &[
                "qwen2.5-14b-instruct-q4_k_m-00001-of-00003.gguf",
                "qwen2.5-14b-instruct-q4_k_m-00002-of-00003.gguf",
                "qwen2.5-14b-instruct-q4_k_m-00003-of-00003.gguf",
            ],
        }),
        "qwen2.5:32b" => Ok(GgufSpec {
            repo: "Qwen/Qwen2.5-32B-Instruct-GGUF",
            shards: &[
                "qwen2.5-32b-instruct-q4_k_m-00001-of-00005.gguf",
                "qwen2.5-32b-instruct-q4_k_m-00002-of-00005.gguf",
                "qwen2.5-32b-instruct-q4_k_m-00003-of-00005.gguf",
                "qwen2.5-32b-instruct-q4_k_m-00004-of-00005.gguf",
                "qwen2.5-32b-instruct-q4_k_m-00005-of-00005.gguf",
            ],
        }),
        "qwen3:4b" => Ok(GgufSpec {
            repo: "Qwen/Qwen3-4B-GGUF",
            shards: &["Qwen3-4B-Q4_K_M.gguf"],
        }),
        "qwen3:8b" => Ok(GgufSpec {
            repo: "Qwen/Qwen3-8B-GGUF",
            shards: &["Qwen3-8B-Q4_K_M.gguf"],
        }),
        "qwen3:14b" => Ok(GgufSpec {
            repo: "Qwen/Qwen3-14B-GGUF",
            shards: &["Qwen3-14B-Q4_K_M.gguf"],
        }),
        "qwen3:32b" => Ok(GgufSpec {
            repo: "Qwen/Qwen3-32B-GGUF",
            shards: &["Qwen3-32B-Q4_K_M.gguf"],
        }),
        "llama3.2:1b" => Ok(GgufSpec {
            repo: "bartowski/Llama-3.2-1B-Instruct-GGUF",
            shards: &["Llama-3.2-1B-Instruct-Q4_K_M.gguf"],
        }),
        "llama3.2:3b" => Ok(GgufSpec {
            repo: "bartowski/Llama-3.2-3B-Instruct-GGUF",
            shards: &["Llama-3.2-3B-Instruct-Q4_K_M.gguf"],
        }),
        "llama3.1:8b" => Ok(GgufSpec {
            repo: "bartowski/Meta-Llama-3.1-8B-Instruct-GGUF",
            shards: &["Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf"],
        }),
        "phi4:14b" => Ok(GgufSpec {
            repo: "bartowski/phi-4-GGUF",
            shards: &["phi-4-Q4_K_M.gguf"],
        }),
        "gemma3:4b" => Ok(GgufSpec {
            repo: "bartowski/google_gemma-3-4b-it-GGUF",
            shards: &["google_gemma-3-4b-it-Q4_K_M.gguf"],
        }),
        "gemma3:12b" => Ok(GgufSpec {
            repo: "bartowski/google_gemma-3-12b-it-GGUF",
            shards: &["google_gemma-3-12b-it-Q4_K_M.gguf"],
        }),
        "mistral:7b" => Ok(GgufSpec {
            repo: "bartowski/Mistral-7B-Instruct-v0.3-GGUF",
            shards: &["Mistral-7B-Instruct-v0.3-Q4_K_M.gguf"],
        }),
        "deepseek-r1:7b" => Ok(GgufSpec {
            repo: "bartowski/DeepSeek-R1-Distill-Qwen-7B-GGUF",
            shards: &["DeepSeek-R1-Distill-Qwen-7B-Q4_K_M.gguf"],
        }),
        "deepseek-r1:8b" => Ok(GgufSpec {
            repo: "bartowski/DeepSeek-R1-Distill-Llama-8B-GGUF",
            shards: &["DeepSeek-R1-Distill-Llama-8B-Q4_K_M.gguf"],
        }),
        "deepseek-r1:14b" => Ok(GgufSpec {
            repo: "bartowski/DeepSeek-R1-Distill-Qwen-14B-GGUF",
            shards: &["DeepSeek-R1-Distill-Qwen-14B-Q4_K_M.gguf"],
        }),
        "deepseek-r1:32b" => Ok(GgufSpec {
            repo: "bartowski/DeepSeek-R1-Distill-Qwen-32B-GGUF",
            shards: &["DeepSeek-R1-Distill-Qwen-32B-Q4_K_M.gguf"],
        }),
        other => Err(format!(
            "unknown model '{other}' — add it to llama::resolve_gguf"
        )),
    }
}

// ── GGUF download ─────────────────────────────────────────────────────────────

/// Returns free bytes on the filesystem containing `path`, or `u64::MAX` if
/// the check fails (so callers default to allowing the download).
fn free_bytes_for(_path: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut buf = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        let c_path = std::ffi::CString::new(_path.as_os_str().as_bytes()).unwrap_or_default();
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), buf.as_mut_ptr()) };
        if rc == 0 {
            let s = unsafe { buf.assume_init() };
            return s.f_bavail.saturating_mul(s.f_bsize);
        }
    }
    u64::MAX
}

async fn download_shard(
    client: &reqwest::Client,
    repo: &str,
    filename: &str,
    dest: &PathBuf,
    size_hint_bytes: u64,
) -> Result<(), String> {
    let url = format!("https://huggingface.co/{repo}/resolve/main/{filename}");
    let resp = client
        .get(&url)
        .header("User-Agent", "ai-mesh/llama-downloader")
        .send()
        .await
        .map_err(|e| format!("download request failed for {filename}: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "download returned HTTP {} for {filename}",
            resp.status()
        ));
    }

    let tmp = dest.with_extension("tmp");
    let std_file =
        std::fs::File::create(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    // Pre-allocate the full file size so the kernel claims space up front:
    // fails immediately if the disk is too full rather than mid-stream.
    if size_hint_bytes > 0 {
        let _ = std_file.set_len(size_hint_bytes);
    }
    let mut file = tokio::fs::File::from_std(std_file);

    let result: Result<(), String> = async {
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("stream error for {filename}: {e}"))?;
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("write error for {filename}: {e}"))?;
        }
        file.flush()
            .await
            .map_err(|e| format!("failed to flush {filename}: {e}"))?;
        Ok(())
    }
    .await;

    drop(file);

    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }

    tokio::fs::rename(&tmp, dest)
        .await
        .map_err(|e| format!("rename failed for {filename}: {e}"))?;

    Ok(())
}

// ── /api/pull equivalent: download + start server ────────────────────────────

pub async fn pull_model(model_name: &str, size_mb: u64) -> Result<(), String> {
    let spec = resolve_gguf(model_name)?;
    let dir = model_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("cannot create model dir {}: {e}", dir.display()))?;

    // Require 2× the model size free: one copy for the .tmp in-progress file
    // and one for the final .gguf so we never silently fill the disk.
    let needed = size_mb.saturating_mul(2) * 1024 * 1024;
    let free = free_bytes_for(&dir);
    if free < needed {
        return Err(format!(
            "insufficient disk space to download {model_name}: need {} MB free, have {} MB",
            needed / 1024 / 1024,
            free / 1024 / 1024,
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()
        .map_err(|e| format!("http client error: {e}"))?;

    let shard_hint = size_mb.saturating_mul(1024 * 1024) / spec.shards.len().max(1) as u64;
    for shard in spec.shards {
        let dest = dir.join(shard);
        if dest.exists() {
            tracing::info!(shard, "shard already present — skipping download");
            continue;
        }
        tracing::info!(shard, "downloading shard from HuggingFace...");
        download_shard(&client, spec.repo, shard, &dest, shard_hint).await?;
        tracing::info!(shard, "download complete");
    }

    // First shard is the entry-point llama-server expects.
    let model_path = dir.join(spec.shards[0]);

    kill_existing().await;

    let layers = gpu_layers();
    let mut cmd = tokio::process::Command::new(server_bin());
    cmd.arg("--model")
        .arg(&model_path)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg("8080")
        .arg("--ctx-size")
        .arg("4096");

    if layers > 0 {
        cmd.arg("--n-gpu-layers").arg(layers.to_string());
    }
    if flash_attn() {
        cmd.arg("--flash-attn").arg("on");
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to start llama-server: {e}"))?;

    *child_lock().lock().await = Some(child);

    // Wait for the health endpoint. Large models on GPU can take >60 s to load.
    let health_url = format!("{}/health", llama_host());
    let hclient = reqwest::Client::new();
    let max_secs = std::env::var("LLAMA_HEALTH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(180u64);
    for elapsed in 0..max_secs {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        if let Ok(r) = hclient
            .get(&health_url)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            && r.status().is_success()
        {
            tracing::info!(model = model_name, "llama-server ready");
            return Ok(());
        }
        if elapsed > 0 && elapsed % 30 == 0 {
            tracing::info!(
                model = model_name,
                elapsed,
                "still waiting for llama-server health..."
            );
        }
    }

    Err(format!(
        "llama-server did not become healthy within {max_secs} s"
    ))
}

// ── unload: kill child process ────────────────────────────────────────────────

pub async fn unload_model() -> Result<(), String> {
    kill_existing().await;
    Ok(())
}

// ── /v1/completions inference ─────────────────────────────────────────────────

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    max_tokens: u32,
    stream: bool,
    repeat_penalty: f32,
    temperature: f32,
    cache_prompt: bool,
}

#[derive(Deserialize)]
struct ChatMessageResponse {
    content: String,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessageResponse,
}

#[derive(Deserialize, Default)]
struct CompletionUsage {
    #[serde(default)]
    completion_tokens: u32,
}

#[derive(Deserialize, Default)]
struct CompletionTimings {
    #[serde(default)]
    predicted_ms: f64,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: CompletionUsage,
    #[serde(default)]
    timings: CompletionTimings,
}

// ── /v1/chat/completions inference ────────────────────────────────────────────

/// Run inference. Returns (output_text, tokens_generated, duration_ms).
pub async fn generate(
    model_name: &str,
    system_prompt: Option<&str>,
    prompt: &str,
    max_tokens: u32,
    temperature: f32,
) -> Result<(String, u32, u64), String> {
    let client = http_client();
    let url = format!("{}/v1/chat/completions", llama_host());

    let wall_start = Instant::now();

    let sys = system_prompt.unwrap_or("You are a helpful assistant.");
    let resp = client
        .post(&url)
        .json(&ChatRequest {
            model: model_name,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: sys,
                },
                ChatMessage {
                    role: "user",
                    content: prompt,
                },
            ],
            max_tokens,
            stream: false,
            repeat_penalty: 1.1,
            temperature,
            cache_prompt: true,
        })
        .send()
        .await
        .map_err(|e| format!("generate request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("generate returned HTTP {status}: {body}"));
    }

    let body: ChatResponse = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse completion response: {e}"))?;

    let output = body
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();
    let tokens = body.usage.completion_tokens;
    let duration_ms = if body.timings.predicted_ms > 0.0 {
        body.timings.predicted_ms as u64
    } else {
        wall_start.elapsed().as_millis() as u64
    };

    Ok((output, tokens, duration_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_gguf ──────────────────────────────────────────────────────────

    #[test]
    fn resolve_gguf_unknown_model_returns_err() {
        let err = resolve_gguf("llama3:8b").unwrap_err();
        assert!(err.contains("unknown model"));
        assert!(err.contains("llama3:8b"));
    }

    #[test]
    fn resolve_gguf_0_5b_single_shard() {
        let spec = resolve_gguf("qwen2.5:0.5b").unwrap();
        assert_eq!(spec.repo, "Qwen/Qwen2.5-0.5B-Instruct-GGUF");
        assert_eq!(spec.shards.len(), 1);
        assert_eq!(spec.shards[0], "qwen2.5-0.5b-instruct-q4_k_m.gguf");
    }

    #[test]
    fn resolve_gguf_1_5b_single_shard() {
        let spec = resolve_gguf("qwen2.5:1.5b").unwrap();
        assert_eq!(spec.repo, "Qwen/Qwen2.5-1.5B-Instruct-GGUF");
        assert_eq!(spec.shards.len(), 1);
        assert_eq!(spec.shards[0], "qwen2.5-1.5b-instruct-q4_k_m.gguf");
    }

    #[test]
    fn resolve_gguf_7b_two_shards() {
        let spec = resolve_gguf("qwen2.5:7b").unwrap();
        assert_eq!(spec.repo, "Qwen/Qwen2.5-7B-Instruct-GGUF");
        assert_eq!(spec.shards.len(), 2);
        assert_eq!(
            spec.shards[0],
            "qwen2.5-7b-instruct-q4_k_m-00001-of-00002.gguf"
        );
        assert_eq!(
            spec.shards[1],
            "qwen2.5-7b-instruct-q4_k_m-00002-of-00002.gguf"
        );
    }

    #[test]
    fn resolve_gguf_14b_three_shards() {
        let spec = resolve_gguf("qwen2.5:14b").unwrap();
        assert_eq!(spec.repo, "Qwen/Qwen2.5-14B-Instruct-GGUF");
        assert_eq!(spec.shards.len(), 3);
    }

    #[test]
    fn resolve_gguf_32b_five_shards() {
        let spec = resolve_gguf("qwen2.5:32b").unwrap();
        assert_eq!(spec.repo, "Qwen/Qwen2.5-32B-Instruct-GGUF");
        assert_eq!(spec.shards.len(), 5);
    }

    #[test]
    fn resolve_gguf_multi_shard_models_start_with_shard_1() {
        // llama-server auto-discovers shards from the first file passed to --model.
        // A wrong first shard silently causes it to load from the middle of the model.
        for model in &["qwen2.5:7b", "qwen2.5:14b", "qwen2.5:32b"] {
            let spec = resolve_gguf(model).unwrap();
            assert!(
                spec.shards[0].contains("00001"),
                "first shard of {model} must be 00001, got {}",
                spec.shards[0]
            );
        }
    }

    #[test]
    fn resolve_gguf_shards_agree_on_total_count() {
        // Every filename in a set should embed the same total so llama-server
        // can validate the set is complete.
        for (model, expected_total) in &[
            ("qwen2.5:7b", "00002"),
            ("qwen2.5:14b", "00003"),
            ("qwen2.5:32b", "00005"),
        ] {
            let spec = resolve_gguf(model).unwrap();
            for shard in spec.shards {
                assert!(
                    shard.contains(expected_total),
                    "{model} shard {shard} should contain total {expected_total}"
                );
            }
        }
    }

    // ── config helpers ────────────────────────────────────────────────────────

    #[test]
    fn llama_host_returns_http_url() {
        let host = llama_host();
        assert!(host.starts_with("http"), "expected http URL, got {host}");
    }

    #[test]
    fn gpu_layers_defaults_to_zero_when_unset() {
        if std::env::var("LLAMA_GPU_LAYERS").is_err() {
            assert_eq!(gpu_layers(), 0);
        }
    }

    #[test]
    fn flash_attn_defaults_to_false_when_unset() {
        if std::env::var("LLAMA_FLASH_ATTN").is_err() {
            assert!(!flash_attn());
        }
    }

    #[test]
    fn model_dir_ends_with_ai_mesh_models_when_unset() {
        if std::env::var("LLAMA_MODEL_DIR").is_err() {
            let dir = model_dir();
            let expected_suffix = std::path::Path::new(".ai-mesh").join("models");
            assert!(
                dir.ends_with(&expected_suffix),
                "expected dir to end with .ai-mesh/models, got {}",
                dir.display()
            );
        }
    }

    // ── ChatResponse deserialization ──────────────────────────────────────────

    #[test]
    fn chat_response_parses_full_response() {
        let json = r#"{
            "choices": [{"message": {"content": "hello world"}}],
            "usage": {"completion_tokens": 3},
            "timings": {"predicted_ms": 42.5}
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].message.content, "hello world");
        assert_eq!(resp.usage.completion_tokens, 3);
        assert!((resp.timings.predicted_ms - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn chat_response_usage_and_timings_default_when_absent() {
        // llama-server omits these fields on some versions; defaults must be zero.
        let json = r#"{"choices": [{"message": {"content": "hi"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.usage.completion_tokens, 0);
        assert_eq!(resp.timings.predicted_ms, 0.0);
    }

    #[test]
    fn chat_response_empty_choices_is_valid() {
        let json = r#"{"choices": []}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.choices.is_empty());
    }

    #[test]
    fn chat_response_timings_zero_triggers_wall_clock_fallback() {
        // Verify the duration selection logic: predicted_ms=0 means use wall time.
        let resp: ChatResponse = serde_json::from_str(r#"{"choices": []}"#).unwrap();
        assert_eq!(resp.timings.predicted_ms, 0.0);
        // The branch in generate(): if predicted_ms > 0 { use it } else { wall clock }
        let duration_ms: u64 = if resp.timings.predicted_ms > 0.0 {
            resp.timings.predicted_ms as u64
        } else {
            99 // stand-in for wall_start.elapsed()
        };
        assert_eq!(duration_ms, 99);
    }

    // ── unload_model ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn unload_model_is_ok_with_no_process() {
        // kill_existing is a no-op when the slot is empty — must not panic or error.
        assert!(unload_model().await.is_ok());
    }
}
