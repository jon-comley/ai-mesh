use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
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

fn ctx_size() -> u32 {
    std::env::var("LLAMA_CTX_SIZE")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(4096)
}

fn n_batch() -> Option<u32> {
    std::env::var("LLAMA_N_BATCH")
        .ok()
        .and_then(|v| v.trim().parse().ok())
}

/// Flash-attention mode passed to llama-server as `--flash-attn <on|off|auto>`.
/// Defaults to `auto`: llama.cpp enables flash-attn where the model + backend support
/// it (≈2× prefill on Qwen/Llama/Mistral on the Vulkan Radeon 780M, no decode cost) and
/// disables it where they don't — notably Gemma-3, which hangs on load when FA is forced
/// `on`. `LLAMA_FLASH_ATTN` overrides; an unrecognised value falls back to `auto`.
fn flash_attn() -> &'static str {
    match std::env::var("LLAMA_FLASH_ATTN")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("on") => "on",
        Some("off") => "off",
        _ => "auto",
    }
}

/// Default health-wait ceiling: 180 s floor, scaled up for larger models
/// (~30 MB/s worst-case cold read + upload). 8.6 GB (14b) → ~287 s. Capped
/// at 900 s so pathological model sizes don't stall a node for 20+ minutes.
fn health_timeout_secs(size_mb: u64) -> u64 {
    180.max(size_mb / 30).min(900)
}

// ── Child process tracking ────────────────────────────────────────────────────

static LLAMA_CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
// Set to true when unload_model is called intentionally; pull_model reads this
// to distinguish "killed by unload" from "crashed" and avoids reporting Failed.
static UNLOAD_REQUESTED: AtomicBool = AtomicBool::new(false);

fn child_lock() -> &'static Mutex<Option<Child>> {
    LLAMA_CHILD.get_or_init(|| Mutex::new(None))
}

/// Fixed port llama-server is launched on (see `pull_model`).
const LLAMA_PORT: u16 = 8080;

async fn kill_existing() {
    let mut guard = child_lock().lock().await;
    if let Some(mut child) = guard.take() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    drop(guard);
    // Belt-and-braces: SIGKILL any *untracked* process still holding the llama
    // port — an orphan from an agent restart that outlived its parent, or a
    // previous run that didn't reap cleanly — so the fresh server can bind it.
    kill_stray_on_port(LLAMA_PORT);
}

/// Socket inodes of LISTEN-state sockets bound to `port`, read from
/// `/proc/net/tcp{,6}`. Pure /proc so it needs no lsof/fuser on minimal nodes.
#[cfg(unix)]
fn listening_inodes(port: u16) -> std::collections::HashSet<u64> {
    const TCP_LISTEN: &str = "0A";
    let mut inodes = std::collections::HashSet::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines().skip(1) {
            // sl local_address rem_address st ... inode
            //  0       1           2        3        9
            let cols: Vec<&str> = line.split_whitespace().collect();
            let (Some(local), Some(st), Some(inode)) = (cols.get(1), cols.get(3), cols.get(9))
            else {
                continue;
            };
            if *st != TCP_LISTEN {
                continue;
            }
            let Some((_ip, hex_port)) = local.rsplit_once(':') else {
                continue;
            };
            if u16::from_str_radix(hex_port, 16).ok() != Some(port) {
                continue;
            }
            if let Ok(i) = inode.parse::<u64>() {
                inodes.insert(i);
            }
        }
    }
    inodes
}

/// `/proc/<pid>/cmdline` rendered as a space-joined string for logging, or the
/// PID as a string if it can't be read (process gone, permission).
#[cfg(unix)]
fn proc_cmdline(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .map(|raw| {
            raw.split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("pid {pid}"))
}

/// Best-effort SIGKILL of every process listening on `port` that we don't
/// otherwise track. Matches `/proc/<pid>/fd` socket links against the listening
/// inodes; skips our own PID; ignores all errors (permission, races). Linux-only
/// — on Windows the agent has no /proc and the deploy script taskkills orphans.
#[cfg(unix)]
fn kill_stray_on_port(port: u16) {
    let inodes = listening_inodes(port);
    if inodes.is_empty() {
        return;
    }
    let self_pid = std::process::id();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in procs.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let matches_socket = target
                .to_str()
                .and_then(|s| s.strip_prefix("socket:["))
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|inode| inodes.contains(&inode));
            if matches_socket {
                tracing::warn!(
                    pid,
                    port,
                    cmdline = %proc_cmdline(pid),
                    "killing stray process holding llama port"
                );
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
                break;
            }
        }
    }
}

/// No /proc on Windows; orphan llama-server.exe is reaped by the deploy script.
#[cfg(not(unix))]
fn kill_stray_on_port(_port: u16) {}

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

    // Unique per-attempt temp name (pid + nanos) so two downloads of the same
    // shard can never write the same file and then race on the final rename —
    // the second rename would otherwise fail with ENOENT once the first moved
    // the temp into place. The load guard already serialises loads within a
    // process; this also protects against a stale temp from a crashed attempt.
    let stem = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dest.with_file_name(format!("{stem}.{}.{nonce}.tmp", std::process::id()));
    // create_new: the name is meant to be unique, so refuse rather than silently
    // truncate if a freak nonce collision ever hands us an existing file.
    let std_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
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

/// Delete leftover `*.tmp` download files in `dir` (best-effort). Called before
/// a pull so orphans from a crashed attempt don't pile up — the live download
/// uses a unique name, so nothing in-flight matches.
async fn remove_stale_tmp(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().extension().is_some_and(|e| e == "tmp") {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

// ── /api/pull equivalent: download + start server ────────────────────────────

pub async fn pull_model(model_name: &str, size_mb: u64) -> Result<(), String> {
    let spec = resolve_gguf(model_name)?;
    let dir = model_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("cannot create model dir {}: {e}", dir.display()))?;

    // Sweep stale `*.tmp` left by a crashed/killed earlier download. Unique
    // per-attempt names mean these are never reused, so without this they'd
    // accumulate and eat the disk-space headroom checked below.
    remove_stale_tmp(&dir).await;

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

    UNLOAD_REQUESTED.store(false, Ordering::Relaxed);
    kill_existing().await;

    let layers = gpu_layers();
    let mut cmd = tokio::process::Command::new(server_bin());
    cmd.arg("--model")
        .arg(&model_path)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(LLAMA_PORT.to_string())
        .arg("--ctx-size")
        .arg(ctx_size().to_string());

    if layers > 0 {
        cmd.arg("--n-gpu-layers").arg(layers.to_string());
    }
    cmd.arg("--flash-attn").arg(flash_attn());
    if let Some(batch) = n_batch() {
        cmd.arg("--n-batch").arg(batch.to_string());
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to start llama-server: {e}"))?;

    *child_lock().lock().await = Some(child);

    // Wait for the health endpoint, scaling the ceiling with model size — a
    // cold-cache disk read + VRAM upload of a 14b-class GGUF can exceed a
    // fixed 180 s on iGPU nodes.
    let health_url = format!("{}/health", llama_host());
    let hclient = reqwest::Client::new();
    let max_secs = std::env::var("LLAMA_HEALTH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| health_timeout_secs(size_mb));
    for elapsed in 0..max_secs {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        if UNLOAD_REQUESTED.load(Ordering::Acquire) {
            tracing::info!(model = model_name, "load aborted by unload request");
            return Err("unloaded".into());
        }
        // Fail fast if llama-server exited (port clash, VRAM allocation
        // failure, corrupt GGUF) instead of burning the rest of the timeout.
        // Checked before /health so an orphaned server on the same port can't
        // be mistaken for this one.
        {
            let mut guard = child_lock().lock().await;
            match guard.as_mut() {
                Some(child) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        guard.take();
                        return Err(format!("llama-server exited during startup: {status}"));
                    }
                }
                None => return Err("unloaded".into()),
            }
        }
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

    // Kill the still-loading server so the registry's Failed state matches
    // reality; a retry then starts clean (and benefits from the warm page
    // cache left by this attempt's disk read).
    kill_existing().await;
    Err(format!(
        "llama-server did not become healthy within {max_secs} s"
    ))
}

// ── unload: kill child process ────────────────────────────────────────────────

pub async fn unload_model() -> Result<(), String> {
    UNLOAD_REQUESTED.store(true, Ordering::Release);
    kill_existing().await;
    Ok(())
}

// ── /v1/completions inference ─────────────────────────────────────────────────

#[derive(Serialize)]
struct OwnedChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<OwnedChatMessage>,
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
    #[serde(default)]
    prompt_tokens: u32,
}

#[derive(Deserialize, Default)]
struct CompletionTimings {
    #[serde(default)]
    predicted_ms: f64,
    // llama.cpp exposes prefill time under different names across builds
    #[serde(default, alias = "prompt_ms", alias = "t_prompt_processing")]
    prompt_eval_ms: f64,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: CompletionUsage,
    #[serde(default)]
    timings: CompletionTimings,
}

// ── model-family detection ─────────────────────────────────────────────────────

/// Qwen2.5 / Qwen3 — thinking suppressed via `/no_think` in the system prompt.
fn is_qwen(model_name: &str) -> bool {
    model_name.to_ascii_lowercase().starts_with("qwen")
}

/// DeepSeek-R1 — thinking suppressed via an empty `<think></think>` assistant
/// prefill appended after the user turn.
fn is_deepseek_r1(model_name: &str) -> bool {
    model_name.to_ascii_lowercase().starts_with("deepseek-r1")
}

// ── /v1/chat/completions inference ────────────────────────────────────────────

/// Outcome of a completed generation.
pub struct GenerateResult {
    pub output: String,
    pub tokens_generated: u32,
    pub prompt_tokens: u32,
    pub duration_ms: u64,
    pub prompt_eval_ms: u64,
}

/// Turn the wire conversation into the llama-server message array, applying
/// model-family generation quirks (these are serving-layer controls like
/// `repeat_penalty`, not content injection — the caller's turns pass verbatim).
fn build_messages(model_name: &str, turns: &[shared::ChatTurn]) -> Vec<OwnedChatMessage> {
    let mut messages: Vec<OwnedChatMessage> = Vec::with_capacity(turns.len() + 1);
    let mut has_system = false;
    for turn in turns {
        let role = match turn.role {
            shared::ChatRole::System => "system",
            shared::ChatRole::User => "user",
            shared::ChatRole::Assistant => "assistant",
        };
        let mut content = turn.content.clone();
        // Qwen models recognise /no_think as a special token that disables CoT;
        // it belongs in the (first) system turn.
        if turn.role == shared::ChatRole::System && !has_system && is_qwen(model_name) {
            content = format!("{content}\n\n/no_think");
        }
        if turn.role == shared::ChatRole::System {
            has_system = true;
        }
        messages.push(OwnedChatMessage { role, content });
    }
    if !has_system && is_qwen(model_name) {
        messages.insert(
            0,
            OwnedChatMessage {
                role: "system",
                content: "/no_think".to_string(),
            },
        );
    }
    // DeepSeek-R1 skips its thinking phase when an empty think block is
    // provided as an assistant prefill at the end of the message list — but
    // never clobber a prefill the caller supplied themselves.
    if is_deepseek_r1(model_name) && !matches!(messages.last(), Some(m) if m.role == "assistant") {
        messages.push(OwnedChatMessage {
            role: "assistant",
            content: "<think>\n</think>".to_string(),
        });
    }
    messages
}

/// Dedicated client for streamed generation: connect timeout only, no total
/// timeout — the shared client's `LLAMA_GENERATE_TIMEOUT_SECS` cap would kill
/// any stream longer than it (same reason `pull_model` builds its own client).
/// Liveness is enforced per-chunk via the idle timeout in `generate_stream`.
fn stream_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("failed to build streaming HTTP client")
    })
}

/// Max silence between stream chunks before the generation is declared hung.
/// Generous default because llama-server emits nothing during prefill, which
/// on a 14b model with a long prompt can take minutes.
fn stream_idle_timeout_secs() -> u64 {
    std::env::var("LLAMA_STREAM_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(300)
}

/// Streamed inference: each content delta is sent into `delta_tx` as it
/// arrives; returns the same totals as `generate`. If `delta_tx` closes
/// (mesh connection gone), the response stream is dropped, which closes the
/// llama-server connection and cancels generation.
pub async fn generate_stream(
    model_name: &str,
    turns: &[shared::ChatTurn],
    max_tokens: u32,
    temperature: f32,
    delta_tx: tokio::sync::mpsc::Sender<String>,
) -> Result<GenerateResult, String> {
    let url = format!("{}/v1/chat/completions", llama_host());
    let wall_start = Instant::now();
    let messages = build_messages(model_name, turns);

    let resp = stream_http_client()
        .post(&url)
        .json(&ChatRequest {
            model: model_name,
            messages,
            max_tokens,
            stream: true,
            repeat_penalty: 1.1,
            temperature,
            cache_prompt: true,
        })
        .send()
        .await
        .map_err(|e| format!("stream request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("stream returned HTTP {status}: {body}"));
    }

    let idle = std::time::Duration::from_secs(stream_idle_timeout_secs());
    let mut byte_stream = resp.bytes_stream();
    let mut parser = shared::sse::SseParser::new();
    let mut output = String::new();
    let mut deltas_sent: u32 = 0;
    let mut prompt_tokens: Option<u32> = None;
    let mut completion_tokens: Option<u32> = None;

    'read: loop {
        let chunk = match tokio::time::timeout(idle, byte_stream.next()).await {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(e))) => return Err(format!("stream read failed: {e}")),
            Ok(None) => break 'read, // EOF — treat like [DONE]
            Err(_) => {
                return Err(format!(
                    "llama-server stream stalled for {}s",
                    idle.as_secs()
                ));
            }
        };
        for payload in parser.feed(&chunk) {
            if payload == "[DONE]" {
                break 'read;
            }
            let Some(parsed) = shared::sse::parse_openai_chunk(&payload) else {
                continue;
            };
            if let Some(err) = parsed.error {
                return Err(format!("llama-server stream error: {err}"));
            }
            if parsed.prompt_tokens.is_some() {
                prompt_tokens = parsed.prompt_tokens;
            }
            if parsed.completion_tokens.is_some() {
                completion_tokens = parsed.completion_tokens;
            }
            if let Some(delta) = parsed.delta {
                if delta.is_empty() {
                    continue;
                }
                output.push_str(&delta);
                deltas_sent += 1;
                if delta_tx.send(delta).await.is_err() {
                    // Receiver gone: mesh connection dropped. Dropping the
                    // byte stream closes the socket and cancels generation.
                    return Err("stream cancelled: connection dropped".to_string());
                }
            }
        }
    }

    Ok(GenerateResult {
        output,
        // llama.cpp builds vary on whether the final chunk carries usage —
        // fall back to counting the deltas we actually forwarded.
        tokens_generated: completion_tokens.unwrap_or(deltas_sent),
        prompt_tokens: prompt_tokens.unwrap_or(0),
        duration_ms: wall_start.elapsed().as_millis() as u64,
        prompt_eval_ms: 0,
    })
}

/// Run inference over a full conversation.
pub async fn generate(
    model_name: &str,
    turns: &[shared::ChatTurn],
    max_tokens: u32,
    temperature: f32,
) -> Result<GenerateResult, String> {
    let client = http_client();
    let url = format!("{}/v1/chat/completions", llama_host());

    let wall_start = Instant::now();

    let messages = build_messages(model_name, turns);

    let resp = client
        .post(&url)
        .json(&ChatRequest {
            model: model_name,
            messages,
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
    let duration_ms = if body.timings.predicted_ms > 0.0 {
        body.timings.predicted_ms as u64
    } else {
        wall_start.elapsed().as_millis() as u64
    };

    Ok(GenerateResult {
        output,
        tokens_generated: body.usage.completion_tokens,
        prompt_tokens: body.usage.prompt_tokens,
        duration_ms,
        prompt_eval_ms: body.timings.prompt_eval_ms as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::ChatTurn;

    // ── build_messages ────────────────────────────────────────────────────────

    #[test]
    fn build_messages_passes_turns_verbatim() {
        let turns = vec![
            ChatTurn::system("You are terse."),
            ChatTurn::user("hi"),
            ChatTurn::assistant("hello"),
            ChatTurn::user("bye"),
        ];
        let msgs = build_messages("llama3:8b", &turns);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[0].content, "You are terse.");
        assert_eq!(msgs[2].role, "assistant");
        assert_eq!(msgs[3].content, "bye");
    }

    #[test]
    fn build_messages_qwen_appends_no_think_to_system() {
        let turns = vec![ChatTurn::system("You are terse."), ChatTurn::user("hi")];
        let msgs = build_messages("qwen2.5:7b", &turns);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[0].content, "You are terse.\n\n/no_think");
        assert_eq!(msgs[1].content, "hi");
    }

    #[test]
    fn build_messages_qwen_without_system_inserts_no_think_turn() {
        let turns = vec![ChatTurn::user("hi")];
        let msgs = build_messages("qwen2.5:7b", &turns);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[0].content, "/no_think");
        assert_eq!(msgs[1].role, "user");
    }

    #[test]
    fn build_messages_no_default_system_for_other_models() {
        let turns = vec![ChatTurn::user("hi")];
        let msgs = build_messages("llama3:8b", &turns);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn build_messages_deepseek_appends_think_prefill() {
        let turns = vec![ChatTurn::user("hi")];
        let msgs = build_messages("deepseek-r1:7b", &turns);
        assert_eq!(msgs.last().unwrap().role, "assistant");
        assert_eq!(msgs.last().unwrap().content, "<think>\n</think>");
    }

    #[test]
    fn build_messages_deepseek_keeps_caller_prefill() {
        let turns = vec![
            ChatTurn::user("hi"),
            ChatTurn::assistant("Sure, the answer is"),
        ];
        let msgs = build_messages("deepseek-r1:7b", &turns);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs.last().unwrap().content, "Sure, the answer is");
    }

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
    fn flash_attn_defaults_to_auto_when_unset() {
        // `auto` lets llama.cpp pick per model — forcing `on` hangs Gemma-3 on the
        // Vulkan backend, so it is not a safe global default.
        if std::env::var("LLAMA_FLASH_ATTN").is_err() {
            assert_eq!(flash_attn(), "auto");
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
    fn health_timeout_floor_is_180s_for_small_models() {
        assert_eq!(health_timeout_secs(500), 180); // 0.5b
        assert_eq!(health_timeout_secs(4096), 180); // 7b
    }

    #[test]
    fn health_timeout_scales_for_large_models() {
        assert_eq!(health_timeout_secs(8635), 287); // 14b-class
        assert_eq!(health_timeout_secs(19456), 648); // 32b-class
        assert_eq!(health_timeout_secs(1_000_000), 900); // cap
    }

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

    // ── model family detection ────────────────────────────────────────────────

    #[test]
    fn is_qwen_matches_qwen2_5_and_qwen3_variants() {
        for name in &[
            "qwen2.5:0.5b",
            "qwen2.5:7b",
            "qwen3:8b",
            "qwen3:14b",
            "QWEN3:4b",
        ] {
            assert!(is_qwen(name), "{name} should be detected as Qwen");
        }
    }

    #[test]
    fn is_qwen_does_not_match_other_families() {
        for name in &[
            "phi4:14b",
            "gemma3:4b",
            "llama3.2:3b",
            "mistral:7b",
            "deepseek-r1:8b",
        ] {
            assert!(!is_qwen(name), "{name} should not be detected as Qwen");
        }
    }

    #[test]
    fn is_deepseek_r1_matches_all_r1_variants() {
        for name in &[
            "deepseek-r1:7b",
            "deepseek-r1:8b",
            "deepseek-r1:14b",
            "deepseek-r1:32b",
            "DeepSeek-R1:8b",
        ] {
            assert!(
                is_deepseek_r1(name),
                "{name} should be detected as DeepSeek-R1"
            );
        }
    }

    #[test]
    fn is_deepseek_r1_does_not_match_other_deepseek_or_families() {
        for name in &["deepseek-v3:7b", "qwen2.5:7b", "phi4:14b", "llama3.1:8b"] {
            assert!(
                !is_deepseek_r1(name),
                "{name} should not be detected as DeepSeek-R1"
            );
        }
    }

    // ── unload_model ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn unload_model_is_ok_with_no_process() {
        // kill_existing is a no-op when the slot is empty — must not panic or error.
        assert!(unload_model().await.is_ok());
    }
}
