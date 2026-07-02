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
            models: &[
                "openai/gpt-oss-120b:free",
                "qwen/qwen3-next-80b-a3b-instruct:free",
                "meta-llama/llama-3.3-70b-instruct:free",
                "qwen/qwen3-coder:free",
                "nvidia/nemotron-3-super-120b-a12b:free",
                "google/gemma-4-31b-it:free",
            ],
        },
        ProviderPreset {
            id: "anthropic",
            label: "Anthropic (Claude)",
            base_url: "https://api.anthropic.com/v1",
            models: &["claude-opus-4-8", "claude-sonnet-4-6", "claude-haiku-4-5"],
        },
        ProviderPreset {
            id: "groq",
            label: "Groq (free)",
            base_url: "https://api.groq.com/openai/v1",
            models: &["llama-3.3-70b-versatile", "llama-3.1-8b-instant"],
        },
        ProviderPreset {
            id: "gemini",
            label: "Google Gemini (free)",
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
            models: &["gemini-2.0-flash", "gemini-2.0-flash-lite"],
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
    /// Other non-success HTTP status.
    Status(u16),
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
            CloudError::Status(s) => write!(f, "HTTP {s}"),
            CloudError::Network(e) => write!(f, "network error: {e}"),
            CloudError::Empty => write!(f, "empty or unparseable response"),
        }
    }
}

impl std::error::Error for CloudError {}

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
                other => CloudError::Status(other),
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
                other => CloudError::Status(other),
            });
        }
        Ok(resp)
    }
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
}
