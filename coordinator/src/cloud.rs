//! Online-AI ("gateway") provider — Phase B.
//!
//! A single OpenAI-compatible chat client. "Pluggable" is achieved through the
//! config-driven `base_url`: the same client reaches OpenRouter (free models),
//! Groq, Cerebras, Mistral, and Gemini's compat endpoint — so we are not locked
//! to any one vendor. Config (including the API key) is persisted in the
//! coordinator's `dashboard_preferences` K/V store under [`GATEWAY_USER`], with
//! environment-variable fallbacks for headless deploys.

use crate::compress::CompressionEngine;
use crate::registry::Registry;
use serde::Deserialize;
use std::time::Duration;

/// Preferences namespace (user_id) under which gateway config is stored.
pub const GATEWAY_USER: &str = "__gateway__";

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// A one-click endpoint preset: a known OpenAI-compatible provider plus the
/// model menu to offer for it. Selecting one fills the endpoint + model in the
/// Gateway tab. The user can always type a custom endpoint/model instead.
pub struct ProviderPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub base_url: &'static str,
    pub models: &'static [&'static str],
    /// Free tier: tried before the paid providers when rotating.
    pub free: bool,
    /// The model used when this provider stands in for another. The cheap one
    /// for a paid provider, never the first menu entry.
    pub fallback_model: &'static str,
}

/// Known OpenAI-compatible providers. Anthropic is reachable via its OpenAI
/// compatibility endpoint (`https://api.anthropic.com/v1/chat/completions`,
/// bearer auth with an `sk-ant-…` key) — so a paid Claude key works through the
/// same client as the free providers.
pub fn provider_presets() -> &'static [ProviderPreset] {
    &[
        ProviderPreset {
            id: "openrouter",
            label: "OpenRouter (free)",
            base_url: "https://openrouter.ai/api/v1",
            // Free slugs rotate often — these are a starting menu; the model box
            // is type-in editable, so any current slug from openrouter.ai/models
            // works too.
            // Refreshed 2026-09-14 after gpt-oss-120b, qwen3-next-80b and
            // llama-3.3-70b all lost their `:free` versions (404, "use the paid
            // slug"). nemotron-3.5-lightning answered a verdict prompt correctly
            // but took 93s against the 60s default timeout — raise
            // CLOUD_TIMEOUT_SECS before relying on it.
            models: &["nvidia/nemotron-3.5-lightning:free"],
            free: true,
            fallback_model: "nvidia/nemotron-3.5-lightning:free",
        },
        ProviderPreset {
            id: "anthropic",
            label: "Anthropic (Claude)",
            base_url: "https://api.anthropic.com/v1",
            models: &["claude-opus-4-8", "claude-sonnet-4-6", "claude-haiku-4-5"],
            free: false,
            fallback_model: "claude-haiku-4-5",
        },
        ProviderPreset {
            id: "openai",
            label: "OpenAI (ChatGPT)",
            base_url: "https://api.openai.com/v1",
            models: &["gpt-4o", "gpt-4o-mini", "gpt-4.1", "o3-mini"],
            free: false,
            fallback_model: "gpt-4o-mini",
        },
        ProviderPreset {
            id: "groq",
            label: "Groq (free)",
            base_url: "https://api.groq.com/openai/v1",
            // Off Groq's own /models, 2026-09-14 — the llama-3.x ids are gone.
            // gpt-oss-120b is what pi1 judges hunts with: correct verdicts in 3s.
            models: &["openai/gpt-oss-120b", "openai/gpt-oss-20b", "qwen/qwen3.6-27b"],
            free: true,
            fallback_model: "openai/gpt-oss-120b",
        },
        ProviderPreset {
            id: "gemini",
            label: "Google Gemini (free)",
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
            models: &["gemini-2.0-flash", "gemini-2.0-flash-lite"],
            free: true,
            fallback_model: "gemini-2.0-flash",
        },
        ProviderPreset {
            id: "mistral",
            label: "Mistral (free)",
            base_url: "https://api.mistral.ai/v1",
            models: &["mistral-small-latest", "mistral-medium-latest"],
            free: true,
            fallback_model: "mistral-small-latest",
        },
    ]
}

fn normalize_url(u: &str) -> &str {
    u.trim_end_matches('/')
}

/// Preference key under which a provider's API key is stored. Keys are kept
/// per-endpoint so switching provider restores the matching key automatically.
pub fn provider_key_name(base_url: &str) -> String {
    format!("api_key:{}", normalize_url(base_url))
}

/// The model menu for a given endpoint — the matching preset's models, or empty
/// for a custom endpoint (the tab still shows the user's chosen model).
pub fn models_for_base_url(base_url: &str) -> Vec<String> {
    let n = normalize_url(base_url);
    provider_presets()
        .iter()
        .find(|p| normalize_url(p.base_url) == n)
        .map(|p| p.models.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

/// Fallback model menu (OpenRouter free) used when no endpoint is configured.
pub fn available_models() -> Vec<String> {
    models_for_base_url(DEFAULT_BASE_URL)
}

/// Errors from a cloud completion. Variants map to the graceful-fallback policy:
/// any of these causes `handle_intent` to fall back to local inference.
#[derive(Debug)]
pub enum CloudError {
    /// No API key configured (neither pref nor env).
    NoKey,
    /// 401/403 — bad or missing credentials.
    Unauthorized,
    /// 429 — rate limited / free-tier quota exhausted.
    RateLimited,
    /// Request timed out.
    Timeout,
    /// Other non-success HTTP status, with the provider's own explanation.
    /// The body is kept because the code alone hides the cause: OpenRouter
    /// answers a retired free slug with a 404 whose body names the paid slug to
    /// use instead, and hunts went unjudged for days logging only "HTTP 404".
    Status(u16, String),
    /// Transport-level failure (DNS, TLS, connection).
    Network(String),
    /// Response could not be parsed / had no content.
    Empty,
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudError::NoKey => write!(f, "no API key configured"),
            CloudError::Unauthorized => write!(f, "unauthorized (check API key)"),
            CloudError::RateLimited => write!(f, "rate limited (free-tier quota?)"),
            CloudError::Timeout => write!(f, "request timed out"),
            CloudError::Status(s, detail) if detail.is_empty() => write!(f, "HTTP {s}"),
            CloudError::Status(s, detail) => write!(f, "HTTP {s}: {detail}"),
            CloudError::Network(e) => write!(f, "network error: {e}"),
            CloudError::Empty => write!(f, "empty or unparseable response"),
        }
    }
}

impl std::error::Error for CloudError {}

/// The provider's error body, trimmed and capped for a log line. Prefers the
/// OpenAI-style `error.message` when the body is that shape.
async fn error_detail(resp: reqwest::Response) -> String {
    let text = resp.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or(text);
    let message = message.trim();
    match message.char_indices().nth(300) {
        Some((cut, _)) => format!("{}…", &message[..cut]),
        None => message.to_string(),
    }
}

/// A successful completion plus the provider-reported token usage.
#[derive(Debug, Clone)]
pub struct CloudReply {
    pub text: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// Resolved gateway configuration (prefs with env fallback). `api_key` is the
/// resolved secret and is **never** serialized back to clients.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub enabled: bool,
    /// When true, compress the conversation history before forwarding. When
    /// false, cloud mode swaps *only* the inference backend (full history sent).
    pub compress: bool,
    pub engine: CompressionEngine,
    pub selected_model: String,
    pub base_url: String,
    pub api_key: Option<String>,
}

impl GatewayConfig {
    /// Load config from the registry's K/V prefs, falling back to env vars.
    pub fn load(reg: &Registry) -> Self {
        let prefs: std::collections::HashMap<String, String> =
            reg.get_all_preferences(GATEWAY_USER).into_iter().collect();
        let pref = |k: &str| prefs.get(k).filter(|v| !v.is_empty()).cloned();

        let enabled = pref("enabled").as_deref() == Some("true");
        // Default ON — compression is the point of the feature; the button lets
        // the user fall back to a pure backend swap.
        let compress = pref("compress").as_deref() != Some("false");
        let engine = match pref("engine").as_deref() {
            Some("local_llm_distiller") => CompressionEngine::LocalLlmDistiller,
            Some("llmlingua2") => CompressionEngine::Llmlingua2,
            _ => CompressionEngine::Statistical,
        };
        let base_url = pref("base_url")
            .or_else(|| std::env::var("CLOUD_BASE_URL").ok())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        // Default the model to the first one for the configured endpoint.
        let selected_model = pref("selected_model")
            .or_else(|| std::env::var("CLOUD_MODEL").ok())
            .or_else(|| models_for_base_url(&base_url).into_iter().next())
            .or_else(|| available_models().into_iter().next())
            .unwrap_or_default();
        // Per-endpoint key first, then a legacy single key, then the env default.
        let api_key = pref(&provider_key_name(&base_url))
            .or_else(|| pref("api_key"))
            .or_else(|| std::env::var("CLOUD_API_KEY").ok());

        Self {
            enabled,
            compress,
            engine,
            selected_model,
            base_url,
            api_key,
        }
    }

    /// True when a key is present and a model is chosen — i.e. a cloud call could
    /// actually be made.
    pub fn is_configured(&self) -> bool {
        self.api_key.as_deref().is_some_and(|k| !k.is_empty()) && !self.selected_model.is_empty()
    }

    /// Last 4 characters of the key, for a non-revealing "key set" hint.
    pub fn key_hint(&self) -> Option<String> {
        self.api_key.as_ref().map(|k| {
            let tail: String = k
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!("…{tail}")
        })
    }

    /// Build a provider if fully configured.
    pub fn provider(&self) -> Option<OpenAiCompatProvider> {
        if !self.is_configured() {
            return None;
        }
        Some(OpenAiCompatProvider {
            base_url: self.base_url.trim_end_matches('/').to_string(),
            api_key: self.api_key.clone().unwrap_or_default(),
            model: self.selected_model.clone(),
        })
    }
}

/// OpenAI-compatible chat-completions client.
#[derive(Clone)]
pub struct OpenAiCompatProvider {
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: Option<String>,
}
#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}
#[derive(Deserialize, Default)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}
#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: ChatUsage,
}

/// Process-wide client with a connection pool; built once on first use.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

impl OpenAiCompatProvider {
    /// The endpoint this provider talks to (normalized, no trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Provider label for logging / response attribution (the endpoint host).
    pub fn provider_name(&self) -> &str {
        self.base_url
            .split("://")
            .nth(1)
            .and_then(|h| h.split('/').next())
            .unwrap_or("cloud")
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Run a chat completion over a full conversation.
    pub async fn complete(
        &self,
        messages: &[shared::ChatTurn],
        temperature: f32,
    ) -> Result<CloudReply, CloudError> {
        if self.api_key.is_empty() {
            return Err(CloudError::NoKey);
        }
        let timeout = std::env::var("CLOUD_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        // ChatTurn serializes with OpenAI role names, so the array passes straight through.
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "temperature": temperature,
        });

        // OpenRouter throttles/rejects free-tier requests lacking these headers.
        let referer = std::env::var("CLOUD_HTTP_REFERER")
            .unwrap_or_else(|_| "https://github.com/ai-mesh".into());
        let title = std::env::var("CLOUD_X_TITLE").unwrap_or_else(|_| "ai-mesh".into());

        let resp = http_client()
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", referer)
            .header("X-Title", title)
            .timeout(Duration::from_secs(timeout))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    CloudError::Timeout
                } else {
                    CloudError::Network(e.to_string())
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => CloudError::Unauthorized,
                429 => CloudError::RateLimited,
                other => CloudError::Status(other, error_detail(resp).await),
            });
        }

        let parsed: ChatResponse = resp.json().await.map_err(|_| CloudError::Empty)?;
        let text = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or(CloudError::Empty)?;

        Ok(CloudReply {
            text,
            prompt_tokens: parsed.usage.prompt_tokens,
            completion_tokens: parsed.usage.completion_tokens,
        })
    }

    /// Open a streaming chat completion. Returns the raw response after the
    /// status check; the caller consumes `bytes_stream()` with `shared::sse`.
    /// A generous 1h cap replaces the normal request timeout so a wedged
    /// provider still can't pin a connection forever — liveness during the
    /// stream is the caller's per-chunk timeout.
    pub async fn complete_stream(
        &self,
        messages: &[shared::ChatTurn],
        temperature: f32,
    ) -> Result<reqwest::Response, CloudError> {
        if self.api_key.is_empty() {
            return Err(CloudError::NoKey);
        }
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "temperature": temperature,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        let referer = std::env::var("CLOUD_HTTP_REFERER")
            .unwrap_or_else(|_| "https://github.com/ai-mesh".into());
        let title = std::env::var("CLOUD_X_TITLE").unwrap_or_else(|_| "ai-mesh".into());

        let resp = http_client()
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", referer)
            .header("X-Title", title)
            .timeout(Duration::from_secs(3600))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    CloudError::Timeout
                } else {
                    CloudError::Network(e.to_string())
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => CloudError::Unauthorized,
                429 => CloudError::RateLimited,
                other => CloudError::Status(other, error_detail(resp).await),
            });
        }
        Ok(resp)
    }
}

/// Other providers to try if the primary cloud call fails — any preset for
/// which a key was saved at some point (switching endpoints in the Gateway
/// tab leaves the old key in place under its own `api_key:<base_url>` pref),
/// excluding whichever endpoint is primary right now. Each fallback uses its
/// preset's `fallback_model`, since there's no per-provider model preference
/// to restore. Free providers come first, then paid, each in
/// `provider_presets()` order, so a paid key is only spent once the free
/// tiers are used up.
pub fn fallback_providers(reg: &Registry, exclude_base_url: &str) -> Vec<OpenAiCompatProvider> {
    let exclude = normalize_url(exclude_base_url);
    let prefs: std::collections::HashMap<String, String> =
        reg.get_all_preferences(GATEWAY_USER).into_iter().collect();
    let free = provider_presets().iter().filter(|p| p.free);
    let paid = provider_presets().iter().filter(|p| !p.free);
    free.chain(paid)
        .filter(|p| normalize_url(p.base_url) != exclude)
        .filter_map(|p| {
            let key = prefs
                .get(&provider_key_name(p.base_url))
                .filter(|k| !k.is_empty())?;
            Some(OpenAiCompatProvider {
                base_url: normalize_url(p.base_url).to_string(),
                api_key: key.clone(),
                model: p.fallback_model.to_string(),
            })
        })
        .collect()
}

/// How long a provider is skipped after it says it has run out. A rate limit
/// is usually per minute or per day on the free tiers; running out of paid
/// credit lasts until somebody tops it up.
const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(15 * 60);
const NO_CREDIT_COOLDOWN: Duration = Duration::from_secs(6 * 60 * 60);

type Cooldowns = std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>;

/// Providers that have run out, by endpoint, and when to try them again.
/// In memory only: a restart gives every provider another go, which is the
/// right default.
fn cooldowns() -> &'static Cooldowns {
    static C: std::sync::OnceLock<Cooldowns> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// The cooldown this error earns, if it means the provider has run out rather
/// than that this one request went wrong. OpenRouter says 402 for no credit,
/// Anthropic a 400 naming the credit balance, OpenAI a 429. A timeout rests it
/// too: OpenRouter's free models have taken 93 s against the 60 s limit, and
/// every request would otherwise wait that out before moving on. A rejected key
/// stays rejected until somebody changes it.
fn cooldown_for(e: &CloudError) -> Option<Duration> {
    match e {
        CloudError::RateLimited | CloudError::Timeout => Some(RATE_LIMIT_COOLDOWN),
        CloudError::Unauthorized => Some(NO_CREDIT_COOLDOWN),
        CloudError::Status(402, _) => Some(NO_CREDIT_COOLDOWN),
        CloudError::Status(_, detail) => {
            let d = detail.to_ascii_lowercase();
            (d.contains("credit") || d.contains("quota") || d.contains("billing"))
                .then_some(NO_CREDIT_COOLDOWN)
        }
        _ => None,
    }
}

fn cooling(base_url: &str) -> bool {
    let mut c = cooldowns().lock().unwrap();
    match c.get(base_url) {
        Some(until) if *until > std::time::Instant::now() => true,
        Some(_) => {
            c.remove(base_url);
            false
        }
        None => false,
    }
}

/// Every provider worth trying, in order: the one chosen on the Online AI tab,
/// then the others with a saved key (free before paid). Providers that have
/// run out are moved to the back rather than dropped, so a request still has
/// something to try when every one of them is resting.
pub fn provider_rotation(reg: &Registry) -> Vec<OpenAiCompatProvider> {
    let cfg = GatewayConfig::load(reg);
    let mut all: Vec<OpenAiCompatProvider> = cfg.provider().into_iter().collect();
    all.extend(fallback_providers(reg, &cfg.base_url));
    let (resting, ready): (Vec<_>, Vec<_>) =
        all.into_iter().partition(|p| cooling(p.base_url()));
    ready.into_iter().chain(resting).collect()
}

/// Run a completion on the first provider in `rotation` that answers. A
/// provider that has run out is rested so the next request starts elsewhere.
/// Returns the reply and the provider that gave it, or the last error.
pub async fn complete_rotating(
    rotation: &[OpenAiCompatProvider],
    messages: &[shared::ChatTurn],
    temperature: f32,
) -> Result<(CloudReply, OpenAiCompatProvider), CloudError> {
    let mut last = CloudError::NoKey;
    for p in rotation {
        match p.complete(messages, temperature).await {
            Ok(reply) => return Ok((reply, p.clone())),
            Err(e) => {
                if let Some(rest) = cooldown_for(&e) {
                    cooldowns()
                        .lock()
                        .unwrap()
                        .insert(p.base_url().to_string(), std::time::Instant::now() + rest);
                }
                tracing::warn!(
                    provider = %p.provider_name(),
                    model = %p.model(),
                    "cloud provider failed: {e}"
                );
                last = e;
            }
        }
    }
    Err(last)
}

/// Persist a single gateway config field (writes through the registry K/V store).
pub fn set_gateway_pref(reg: &Registry, key: &str, value: &str) {
    reg.set_preference(GATEWAY_USER, key, value);
}

/// Everything `handle_intent` needs to route a request to the cloud: the
/// configured provider, the compression engine, and a handle to record stats.
pub struct GatewayInvocation {
    pub provider: OpenAiCompatProvider,
    pub engine: CompressionEngine,
    /// Compress history before forwarding (false = pure backend swap).
    pub compress: bool,
    pub state: std::sync::Arc<crate::http::state::DashboardState>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_key(key: Option<&str>) -> GatewayConfig {
        GatewayConfig {
            enabled: false,
            compress: true,
            engine: CompressionEngine::Statistical,
            selected_model: "some/model".into(),
            base_url: "https://example/api/v1".into(),
            api_key: key.map(|k| k.to_string()),
        }
    }

    #[test]
    fn key_hint_masks_all_but_last_four() {
        let cfg = cfg_with_key(Some("sk-supersecret-9876"));
        assert_eq!(cfg.key_hint().unwrap(), "…9876");
        assert!(cfg.is_configured());
    }

    #[test]
    fn not_configured_without_key() {
        let cfg = cfg_with_key(None);
        assert!(!cfg.is_configured());
        assert!(cfg.provider().is_none());
        assert!(cfg.key_hint().is_none());
    }

    #[test]
    fn load_defaults_compress_on_and_statistical() {
        let reg = Registry::new();
        let cfg = GatewayConfig::load(&reg);
        assert!(!cfg.enabled);
        assert!(cfg.compress, "compression defaults ON");
        assert!(matches!(cfg.engine, CompressionEngine::Statistical));
        assert!(!cfg.selected_model.is_empty());
    }

    #[test]
    fn compress_pref_false_disables() {
        let reg = Registry::new();
        reg.set_preference(GATEWAY_USER, "compress", "false");
        assert!(!GatewayConfig::load(&reg).compress);
    }

    #[test]
    fn keys_are_per_provider_and_restored_on_switch() {
        let reg = Registry::new();
        let openrouter = "https://openrouter.ai/api/v1";
        let anthropic = "https://api.anthropic.com/v1";

        // Save an OpenRouter key while that endpoint is active.
        reg.set_preference(GATEWAY_USER, "base_url", openrouter);
        reg.set_preference(GATEWAY_USER, &provider_key_name(openrouter), "or-key");
        assert_eq!(GatewayConfig::load(&reg).api_key.as_deref(), Some("or-key"));

        // Switch to Anthropic: the OpenRouter key must not leak across.
        reg.set_preference(GATEWAY_USER, "base_url", anthropic);
        assert_ne!(GatewayConfig::load(&reg).api_key.as_deref(), Some("or-key"));

        // Save an Anthropic key, then switching back restores each provider's own.
        reg.set_preference(GATEWAY_USER, &provider_key_name(anthropic), "ant-key");
        assert_eq!(
            GatewayConfig::load(&reg).api_key.as_deref(),
            Some("ant-key")
        );
        reg.set_preference(GATEWAY_USER, "base_url", openrouter);
        assert_eq!(GatewayConfig::load(&reg).api_key.as_deref(), Some("or-key"));
    }

    #[test]
    fn fallback_providers_only_returns_endpoints_with_a_saved_key() {
        let reg = Registry::new();
        assert!(fallback_providers(&reg, "https://api.groq.com/openai/v1").is_empty());

        reg.set_preference(
            GATEWAY_USER,
            &provider_key_name("https://openrouter.ai/api/v1"),
            "or-key",
        );
        let fb = fallback_providers(&reg, "https://api.groq.com/openai/v1");
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].base_url(), "https://openrouter.ai/api/v1");
        assert_eq!(fb[0].api_key, "or-key");
        assert_eq!(fb[0].model, "nvidia/nemotron-3.5-lightning:free");
    }

    #[test]
    fn paid_fallbacks_use_the_cheap_model() {
        let reg = Registry::new();
        reg.set_preference(
            GATEWAY_USER,
            &provider_key_name("https://api.anthropic.com/v1"),
            "ant-key",
        );
        let fb = fallback_providers(&reg, "https://api.groq.com/openai/v1");
        assert_eq!(fb[0].model, "claude-haiku-4-5");
    }

    #[test]
    fn running_out_rests_a_provider_and_a_bad_request_does_not() {
        assert_eq!(cooldown_for(&CloudError::RateLimited), Some(RATE_LIMIT_COOLDOWN));
        assert_eq!(
            cooldown_for(&CloudError::Status(402, String::new())),
            Some(NO_CREDIT_COOLDOWN)
        );
        assert_eq!(
            cooldown_for(&CloudError::Status(400, "Your credit balance is too low".into())),
            Some(NO_CREDIT_COOLDOWN)
        );
        assert_eq!(cooldown_for(&CloudError::Status(400, "bad model".into())), None);
        assert_eq!(cooldown_for(&CloudError::Timeout), Some(RATE_LIMIT_COOLDOWN));
        assert_eq!(cooldown_for(&CloudError::Unauthorized), Some(NO_CREDIT_COOLDOWN));
        assert_eq!(cooldown_for(&CloudError::Empty), None);
    }

    #[test]
    fn rotation_puts_a_resting_provider_last() {
        let reg = Registry::new();
        let groq = "https://api.groq.com/openai/v1";
        let mistral = "https://api.mistral.ai/v1";
        reg.set_preference(GATEWAY_USER, "base_url", groq);
        reg.set_preference(GATEWAY_USER, "selected_model", "openai/gpt-oss-120b");
        reg.set_preference(GATEWAY_USER, &provider_key_name(groq), "groq-key");
        reg.set_preference(GATEWAY_USER, &provider_key_name(mistral), "mistral-key");

        let order = |reg: &Registry| -> Vec<String> {
            provider_rotation(reg)
                .iter()
                .map(|p| p.base_url().to_string())
                .collect()
        };
        assert_eq!(order(&reg), vec![groq, mistral]);

        cooldowns()
            .lock()
            .unwrap()
            .insert(groq.to_string(), std::time::Instant::now() + RATE_LIMIT_COOLDOWN);
        assert_eq!(order(&reg), vec![mistral, groq]);
        cooldowns().lock().unwrap().remove(groq);
    }

    #[test]
    fn fallback_providers_excludes_the_primary_endpoint() {
        let reg = Registry::new();
        let groq = "https://api.groq.com/openai/v1";
        reg.set_preference(GATEWAY_USER, &provider_key_name(groq), "groq-key");
        reg.set_preference(
            GATEWAY_USER,
            &provider_key_name("https://openrouter.ai/api/v1"),
            "or-key",
        );

        // Excluding groq (the primary) leaves openrouter as the only fallback,
        // even though groq itself also has a stored key.
        let fb = fallback_providers(&reg, groq);
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].base_url(), "https://openrouter.ai/api/v1");
    }

    #[test]
    fn fallback_providers_try_free_before_paid() {
        let reg = Registry::new();
        for preset in provider_presets() {
            reg.set_preference(
                GATEWAY_USER,
                &provider_key_name(preset.base_url),
                "some-key",
            );
        }
        let fb = fallback_providers(&reg, "https://api.groq.com/openai/v1");
        let free = provider_presets().iter().filter(|p| p.free);
        let paid = provider_presets().iter().filter(|p| !p.free);
        let expected: Vec<&str> = free
            .chain(paid)
            .map(|p| p.base_url)
            .filter(|&u| u != "https://api.groq.com/openai/v1")
            .collect();
        let got: Vec<&str> = fb.iter().map(|p| p.base_url()).collect();
        assert_eq!(got, expected);
    }
}
