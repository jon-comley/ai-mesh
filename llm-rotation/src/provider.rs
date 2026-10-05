use crate::{presets::normalize_url, Error};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A completion plus the token usage the provider reported.
#[derive(Debug, Clone)]
pub struct Reply {
    pub text: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// One OpenAI-compatible endpoint, with a key and a model.
#[derive(Clone)]
pub struct Provider {
    base_url: String,
    api_key: String,
    model: String,
    timeout: Duration,
    referer: Option<String>,
    title: Option<String>,
}

// The key never appears in logs or panics.
impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}
#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}
#[derive(Deserialize, Default)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}
#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Usage,
}

/// One client and connection pool for the whole process.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// The provider's error body, trimmed and capped for a log line. Prefers the
/// OpenAI-style `error.message` when the body has that shape.
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

impl Provider {
    /// A provider at `base_url` (for example `https://api.groq.com/openai/v1`),
    /// with a 60 s timeout.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: normalize_url(&base_url.into()).to_string(),
            api_key: api_key.into(),
            model: model.into(),
            timeout: DEFAULT_TIMEOUT,
            referer: None,
            title: None,
        }
    }

    /// How long a non-streaming completion may take.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The `HTTP-Referer` and `X-Title` headers. OpenRouter throttles or
    /// rejects free-tier requests that do not name the calling app.
    pub fn with_attribution(mut self, referer: impl Into<String>, title: impl Into<String>) -> Self {
        self.referer = Some(referer.into());
        self.title = Some(title.into());
        self
    }

    /// The endpoint, without a trailing slash.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The endpoint's host, for logs and attribution.
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

    async fn post(&self, body: &serde_json::Value, timeout: Duration) -> Result<reqwest::Response, Error> {
        if self.api_key.is_empty() {
            return Err(Error::NoKey);
        }
        let mut req = http_client()
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .timeout(timeout)
            .json(body);
        if let Some(r) = &self.referer {
            req = req.header("HTTP-Referer", r);
        }
        if let Some(t) = &self.title {
            req = req.header("X-Title", t);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                Error::Timeout
            } else {
                Error::Network(e.to_string())
            }
        })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => Error::Unauthorized,
                429 => Error::RateLimited,
                other => Error::Status(other, error_detail(resp).await),
            });
        }
        Ok(resp)
    }

    /// Run a chat completion over a whole conversation. `messages` is any
    /// type that serializes as `{ "role": ..., "content": ... }`, such as
    /// [`crate::Message`].
    pub async fn complete<M: Serialize>(&self, messages: &[M], temperature: f32) -> Result<Reply, Error> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "temperature": temperature,
        });
        let resp = self.post(&body, self.timeout).await?;
        let parsed: ChatResponse = resp.json().await.map_err(|_| Error::Empty)?;
        let text = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or(Error::Empty)?;
        Ok(Reply {
            text,
            prompt_tokens: parsed.usage.prompt_tokens,
            completion_tokens: parsed.usage.completion_tokens,
        })
    }

    /// Open a streaming completion and return the response once its status
    /// has been checked; read it with `bytes_stream()` as server-sent events.
    /// The request may run for up to an hour, so a wedged provider cannot hold
    /// a connection for ever. Watching for stalls between chunks is the
    /// caller's job.
    pub async fn complete_stream<M: Serialize>(
        &self,
        messages: &[M],
        temperature: f32,
    ) -> Result<reqwest::Response, Error> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "temperature": temperature,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        self.post(&body, Duration::from_secs(3600)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_leaves_the_key_out() {
        let p = Provider::new("https://api.groq.com/openai/v1/", "gsk-secret", "m");
        let shown = format!("{p:?}");
        assert!(!shown.contains("gsk-secret"));
        assert_eq!(p.base_url(), "https://api.groq.com/openai/v1");
        assert_eq!(p.provider_name(), "api.groq.com");
    }

    #[tokio::test]
    async fn no_key_fails_before_any_request() {
        let p = Provider::new("http://127.0.0.1:9", "", "m");
        assert!(matches!(p.complete(&[crate::Message::user("hi")], 0.0).await, Err(Error::NoKey)));
    }
}
