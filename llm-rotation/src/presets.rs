/// A known OpenAI-compatible provider: where it is, which models to offer,
/// whether it has a free tier, and which model to use when it stands in for
/// another provider.
#[derive(Debug)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub base_url: &'static str,
    /// A starting menu of models. Free model names change often, so treat
    /// this as a suggestion rather than a complete list.
    pub models: &'static [&'static str],
    /// Free tier: a rotation tries these before the paid ones.
    pub free: bool,
    /// The model used when standing in: the cheap one for a paid provider,
    /// never simply the first in the menu.
    pub fallback_model: &'static str,
}

/// The known providers, free ones and paid ones in a stable order.
///
/// Anthropic is reached through its OpenAI compatibility endpoint, so a paid
/// Claude key works through the same client as the free providers.
pub fn presets() -> &'static [Preset] {
    &[
        Preset {
            id: "gemini",
            label: "Google Gemini (free)",
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
            // First, so the fallback tries it before the other free ones
            // (Jon, 2026-10-24). The -latest names follow Google's current
            // Flash, as gemini-2.0-flash was retired under them.
            models: &["gemini-flash-latest", "gemini-flash-lite-latest"],
            free: true,
            fallback_model: "gemini-flash-latest",
        },
        Preset {
            id: "openrouter",
            label: "OpenRouter (free)",
            base_url: "https://openrouter.ai/api/v1",
            // Refreshed 2026-09-14 after gpt-oss-120b, qwen3-next-80b and
            // llama-3.3-70b all lost their `:free` versions. This one has
            // taken 93 s on a short prompt, so give it a generous timeout.
            models: &["nvidia/nemotron-3.5-lightning:free"],
            free: true,
            fallback_model: "nvidia/nemotron-3.5-lightning:free",
        },
        Preset {
            id: "anthropic",
            label: "Anthropic (Claude)",
            base_url: "https://api.anthropic.com/v1",
            models: &["claude-opus-4-8", "claude-sonnet-4-6", "claude-haiku-4-5"],
            free: false,
            fallback_model: "claude-haiku-4-5",
        },
        Preset {
            id: "openai",
            label: "OpenAI (ChatGPT)",
            base_url: "https://api.openai.com/v1",
            // Paid only: OpenAI has no free API models.
            models: &["gpt-5.4-mini", "gpt-5.4-nano", "gpt-5.4"],
            free: false,
            fallback_model: "gpt-5.4-nano",
        },
        Preset {
            id: "groq",
            label: "Groq (free)",
            base_url: "https://api.groq.com/openai/v1",
            // Off Groq's own /models, 2026-10-24.
            models: &["openai/gpt-oss-120b", "openai/gpt-oss-20b", "qwen/qwen3.8-27b"],
            free: true,
            fallback_model: "openai/gpt-oss-120b",
        },
        Preset {
            id: "mistral",
            label: "Mistral (free)",
            base_url: "https://api.mistral.ai/v1",
            models: &["mistral-small-latest", "mistral-medium-latest"],
            free: true,
            fallback_model: "mistral-small-latest",
        },
        Preset {
            id: "nvidia",
            label: "NVIDIA (free)",
            base_url: "https://integrate.api.nvidia.com/v1",
            // Free with no card, about 40 requests a minute across the
            // account. /v1/models lists more than a free key can call: on
            // 2026-10-24 nemotron-3-super and kimi-k2.6 answered 404 and
            // deepseek-v4.1-flash never answered, so only these two.
            models: &[
                "nvidia/nemotron-3-ultra-550b-a55b",
                "nvidia/nemotron-3.5-lightning-30b-a3b",
            ],
            free: true,
            fallback_model: "nvidia/nemotron-3-ultra-550b-a55b",
        },
        Preset {
            id: "workersai",
            label: "Cloudflare Workers AI (free)",
            // The account id is filled in by `resolve_base_url`; the key is a
            // Cloudflare API token with Workers AI read. 10,000 neurons a day
            // free, reset at midnight UTC. Off `wrangler ai models list`,
            // 2026-10-24.
            base_url: "https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1",
            models: &[
                "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
                "@cf/openai/gpt-oss-120b",
                "@cf/qwen/qwen3.8-27b",
            ],
            free: true,
            fallback_model: "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
        },
    ]
}

/// A base URL without its trailing slash, so two spellings of one endpoint
/// compare equal.
pub fn normalize_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

/// A preset's base URL with any `{NAME}` in it filled from the environment,
/// or None while one is unset (Workers AI needs `CLOUDFLARE_ACCOUNT_ID`).
pub fn resolve_base_url(base_url: &str) -> Option<String> {
    resolve_with(base_url, |name| std::env::var(name).ok())
}

fn resolve_with(base_url: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::new();
    let mut rest = base_url;
    while let Some(start) = rest.find('{') {
        let end = start + rest[start..].find('}')?;
        let value = lookup(&rest[start + 1..end]).filter(|v| !v.is_empty())?;
        out.push_str(&rest[..start]);
        out.push_str(&value);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// The preset for an endpoint, if it is a known one.
pub fn preset_for(base_url: &str) -> Option<&'static Preset> {
    let n = normalize_url(base_url);
    presets().iter().find(|p| normalize_url(p.base_url) == n)
}

/// The model menu for an endpoint: the matching preset's models, or empty for
/// an endpoint this crate does not know.
pub fn models_for(base_url: &str) -> Vec<String> {
    preset_for(base_url)
        .map(|p| p.models.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fallback_model_is_on_its_own_menu() {
        for p in presets() {
            assert!(p.models.contains(&p.fallback_model), "{}", p.id);
        }
    }

    #[test]
    fn a_template_is_filled_or_refused() {
        let url = "https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1";
        assert_eq!(
            resolve_with(url, |_| Some("abc".into())).as_deref(),
            Some("https://api.cloudflare.com/client/v4/accounts/abc/ai/v1")
        );
        assert_eq!(resolve_with(url, |_| None), None);
        assert_eq!(resolve_with(url, |_| Some(String::new())), None);
        assert_eq!(resolve_with("https://api.mistral.ai/v1", |_| None).as_deref(), Some("https://api.mistral.ai/v1"));
    }

    #[test]
    fn a_trailing_slash_finds_the_same_preset() {
        assert_eq!(preset_for("https://api.groq.com/openai/v1/").unwrap().id, "groq");
        assert!(models_for("https://example.com/v1").is_empty());
    }
}
