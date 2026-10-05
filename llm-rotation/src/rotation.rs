use crate::{Error, Provider, Reply};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Tries providers in order and rests the ones that have run out.
///
/// Rest state is kept in memory, by endpoint, for as long as the `Rotation`
/// lives. Keep one for the life of the process: a restart gives every
/// provider another go, which is the right default.
#[derive(Debug)]
pub struct Rotation {
    resting: Mutex<HashMap<String, Instant>>,
    short_rest: Duration,
    long_rest: Duration,
}

impl Default for Rotation {
    fn default() -> Self {
        Self::new()
    }
}

impl Rotation {
    /// Rests a provider for 15 minutes after a rate limit or a timeout, and
    /// for 6 hours after it runs out of credit or rejects the key.
    pub fn new() -> Self {
        Self::with_rests(Duration::from_secs(15 * 60), Duration::from_secs(6 * 60 * 60))
    }

    /// `short` follows a rate limit or a timeout; `long` follows no credit or
    /// a rejected key, which last until somebody does something about them.
    pub fn with_rests(short: Duration, long: Duration) -> Self {
        Self { resting: Mutex::new(HashMap::new()), short_rest: short, long_rest: long }
    }

    /// How long this error rests a provider, or `None` when it is about this
    /// one request rather than the provider. OpenRouter says 402 for no
    /// credit, Anthropic a 400 naming the credit balance, OpenAI a 429. A
    /// timeout rests it too, or every request would wait it out again.
    pub fn rest_for(&self, e: &Error) -> Option<Duration> {
        match e {
            Error::RateLimited | Error::Timeout => Some(self.short_rest),
            Error::Unauthorized | Error::Status(402, _) => Some(self.long_rest),
            Error::Status(_, detail) => {
                let d = detail.to_ascii_lowercase();
                (d.contains("credit") || d.contains("quota") || d.contains("billing"))
                    .then_some(self.long_rest)
            }
            _ => None,
        }
    }

    /// Rest an endpoint by hand, for `duration` from now.
    pub fn rest(&self, base_url: &str, duration: Duration) {
        self.resting
            .lock()
            .unwrap()
            .insert(crate::normalize_url(base_url).to_string(), Instant::now() + duration);
    }

    /// True while an endpoint is resting.
    pub fn is_resting(&self, base_url: &str) -> bool {
        let key = crate::normalize_url(base_url);
        let mut r = self.resting.lock().unwrap();
        match r.get(key) {
            Some(until) if *until > Instant::now() => true,
            Some(_) => {
                r.remove(key);
                false
            }
            None => false,
        }
    }

    /// `providers` with the resting ones moved to the back. They are kept
    /// rather than dropped, so a request still has something to try when
    /// every provider is resting.
    pub fn order(&self, providers: Vec<Provider>) -> Vec<Provider> {
        let (resting, ready): (Vec<_>, Vec<_>) =
            providers.into_iter().partition(|p| self.is_resting(p.base_url()));
        ready.into_iter().chain(resting).collect()
    }

    /// Run a completion on the first provider that answers, in the order
    /// given (call [`Rotation::order`] first to put resting ones last). Each
    /// provider that has run out is rested. Returns the reply and the
    /// provider that gave it, or the last error when none did.
    pub async fn complete<M: Serialize>(
        &self,
        providers: &[Provider],
        messages: &[M],
        temperature: f32,
    ) -> Result<(Reply, Provider), Error> {
        let mut last = Error::NoKey;
        for p in providers {
            match p.complete(messages, temperature).await {
                Ok(reply) => return Ok((reply, p.clone())),
                Err(e) => {
                    if let Some(d) = self.rest_for(&e) {
                        self.rest(p.base_url(), d);
                    }
                    tracing::warn!(
                        provider = %p.provider_name(),
                        model = %p.model(),
                        "LLM provider failed: {e}"
                    );
                    last = e;
                }
            }
        }
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn running_out_rests_a_provider_and_a_bad_request_does_not() {
        let r = Rotation::new();
        let short = Some(Duration::from_secs(15 * 60));
        let long = Some(Duration::from_secs(6 * 60 * 60));
        assert_eq!(r.rest_for(&Error::RateLimited), short);
        assert_eq!(r.rest_for(&Error::Timeout), short);
        assert_eq!(r.rest_for(&Error::Unauthorized), long);
        assert_eq!(r.rest_for(&Error::Status(402, String::new())), long);
        assert_eq!(r.rest_for(&Error::Status(400, "Your credit balance is too low".into())), long);
        assert_eq!(r.rest_for(&Error::Status(400, "bad model".into())), None);
        assert_eq!(r.rest_for(&Error::Empty), None);
    }

    #[test]
    fn a_resting_provider_goes_last_and_comes_back() {
        let r = Rotation::new();
        let a = Provider::new("https://a.example/v1", "k", "m");
        let b = Provider::new("https://b.example/v1", "k", "m");
        let names = |v: Vec<Provider>| v.iter().map(|p| p.base_url().to_string()).collect::<Vec<_>>();

        r.rest("https://a.example/v1/", Duration::from_secs(60));
        assert_eq!(names(r.order(vec![a.clone(), b.clone()])), ["https://b.example/v1", "https://a.example/v1"]);

        r.rest("https://a.example/v1", Duration::ZERO);
        assert_eq!(names(r.order(vec![a, b])), ["https://a.example/v1", "https://b.example/v1"]);
    }

    /// A one-shot HTTP server that answers every connection with `response`.
    async fn serve(response: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(response.as_bytes()).await;
            }
        });
        url
    }

    #[tokio::test]
    async fn moves_on_from_a_spent_provider_and_rests_it() {
        let spent = serve("HTTP/1.1 429 Too Many Requests\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
        let body = r#"{"choices":[{"message":{"content":"hello"}}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#;
        let ok = serve(Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        ))
        .await;

        let r = Rotation::new();
        let providers = vec![Provider::new(&spent, "k", "m"), Provider::new(&ok, "k", "m")];
        let (reply, used) = r.complete(&providers, &[crate::Message::user("hi")], 0.0).await.unwrap();

        assert_eq!(reply.text, "hello");
        assert_eq!(used.base_url(), ok);
        assert!(r.is_resting(&spent));
        assert!(!r.is_resting(&ok));
    }
}
