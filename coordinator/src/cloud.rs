//! Online-AI ("gateway") settings: which providers the coordinator may use and
//! with which keys.
//!
//! The client itself, the provider presets and the rotation that rests a
//! provider when it runs out live in the `llm-rotation` crate. This module
//! reads the coordinator's side of it: config (including the API keys) is
//! persisted in the `dashboard_preferences` K/V store under [`GATEWAY_USER`],
//! with environment-variable fallbacks for headless deploys.

use crate::compress::CompressionEngine;
use crate::registry::Registry;
use std::time::Duration;

pub use llm_rotation::{
    Error as CloudError, Preset as ProviderPreset, Provider as OpenAiCompatProvider,
    Reply as CloudReply,
};

/// Preferences namespace (user_id) under which gateway config is stored.
pub const GATEWAY_USER: &str = "__gateway__";

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Known OpenAI-compatible providers.
pub fn provider_presets() -> &'static [ProviderPreset] {
    llm_rotation::presets()
}

/// Preference key under which a provider's API key is stored. Keys are kept
/// per-endpoint so switching provider restores the matching key automatically.
pub fn provider_key_name(base_url: &str) -> String {
    format!("api_key:{}", llm_rotation::normalize_url(base_url))
}

/// The model menu for a given endpoint: the matching preset's models, or empty
/// for a custom endpoint (the tab still shows the user's chosen model).
pub fn models_for_base_url(base_url: &str) -> Vec<String> {
    llm_rotation::models_for(base_url)
}

/// Fallback model menu (OpenRouter free) used when no endpoint is configured.
pub fn available_models() -> Vec<String> {
    models_for_base_url(DEFAULT_BASE_URL)
}

/// A provider with this coordinator's timeout (`CLOUD_TIMEOUT_SECS`) and the
/// attribution headers OpenRouter wants (`CLOUD_HTTP_REFERER`, `CLOUD_X_TITLE`).
fn provider(base_url: &str, api_key: &str, model: &str) -> OpenAiCompatProvider {
    let timeout = std::env::var("CLOUD_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let referer = std::env::var("CLOUD_HTTP_REFERER")
        .unwrap_or_else(|_| "https://github.com/ai-mesh".into());
    let title = std::env::var("CLOUD_X_TITLE").unwrap_or_else(|_| "ai-mesh".into());
    OpenAiCompatProvider::new(base_url, api_key, model)
        .with_timeout(Duration::from_secs(timeout))
        .with_attribution(referer, title)
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
        Some(provider(
            &self.base_url,
            self.api_key.as_deref().unwrap_or_default(),
            &self.selected_model,
        ))
    }
}

/// Other providers to try if the primary cloud call fails: any preset for
/// which a key was saved at some point (switching endpoints in the Gateway
/// tab leaves the old key in place under its own `api_key:<base_url>` pref),
/// excluding whichever endpoint is primary right now. Each fallback uses its
/// preset's `fallback_model`, since there's no per-provider model preference
/// to restore. Free providers come first, then paid, each in
/// `provider_presets()` order, so a paid key is only spent once the free
/// tiers are used up.
pub fn fallback_providers(reg: &Registry, exclude_base_url: &str) -> Vec<OpenAiCompatProvider> {
    let exclude = llm_rotation::normalize_url(exclude_base_url);
    let prefs: std::collections::HashMap<String, String> =
        reg.get_all_preferences(GATEWAY_USER).into_iter().collect();
    let free = provider_presets().iter().filter(|p| p.free);
    let paid = provider_presets().iter().filter(|p| !p.free);
    free.chain(paid)
        .filter(|p| llm_rotation::normalize_url(p.base_url) != exclude)
        .filter_map(|p| {
            let key = prefs
                .get(&provider_key_name(p.base_url))
                .filter(|k| !k.is_empty())?;
            Some(provider(p.base_url, key, p.fallback_model))
        })
        .collect()
}

/// The one rotation for this process, so every caller sees the same rests.
fn rotation() -> &'static llm_rotation::Rotation {
    static R: std::sync::OnceLock<llm_rotation::Rotation> = std::sync::OnceLock::new();
    R.get_or_init(llm_rotation::Rotation::new)
}

/// Every provider worth trying, in order: the one chosen on the Online AI tab,
/// then the others with a saved key (free before paid), with any that have run
/// out moved to the back.
pub fn provider_rotation(reg: &Registry) -> Vec<OpenAiCompatProvider> {
    let cfg = GatewayConfig::load(reg);
    let mut all: Vec<OpenAiCompatProvider> = cfg.provider().into_iter().collect();
    all.extend(fallback_providers(reg, &cfg.base_url));
    rotation().order(all)
}

/// `providers` with any that are resting moved to the back, for a caller that
/// walks the list itself (chat, which records every provider's failure).
pub fn order_by_rest(providers: Vec<OpenAiCompatProvider>) -> Vec<OpenAiCompatProvider> {
    rotation().order(providers)
}

/// Rest `provider` if `e` says it has run out (rate limit, timeout, no credit,
/// bad key), the same as [`complete_rotating`] does for its own failures.
pub fn rest_if_out(provider: &OpenAiCompatProvider, e: &CloudError) {
    if let Some(d) = rotation().rest_for(e) {
        rotation().rest(provider.base_url(), d);
    }
}

/// Run a completion on the first provider in `providers` that answers, resting
/// any that have run out. Returns the reply and the provider that gave it.
pub async fn complete_rotating(
    providers: &[OpenAiCompatProvider],
    messages: &[shared::ChatTurn],
    temperature: f32,
) -> Result<(CloudReply, OpenAiCompatProvider), CloudError> {
    rotation().complete(providers, messages, temperature).await
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
        assert_eq!(fb[0].model(), "nvidia/nemotron-3.5-lightning:free");
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
        assert_eq!(fb[0].model(), "claude-haiku-4-5");
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

        rotation().rest(groq, Duration::from_secs(60));
        assert_eq!(order(&reg), vec![mistral, groq]);
        rotation().rest(groq, Duration::ZERO);
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
