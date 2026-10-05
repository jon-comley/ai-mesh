/// Why a completion failed.
#[derive(Debug)]
pub enum Error {
    /// No API key configured.
    NoKey,
    /// 401/403: bad or missing credentials.
    Unauthorized,
    /// 429: rate limited, or the free-tier quota is used up.
    RateLimited,
    /// The request timed out.
    Timeout,
    /// Any other non-success status, with the provider's own explanation.
    /// The body is kept because the code alone hides the cause: OpenRouter
    /// answers a retired free model with a 404 whose body names the paid one.
    Status(u16, String),
    /// Transport failure (DNS, TLS, connection).
    Network(String),
    /// The response had no content or could not be parsed.
    Empty,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoKey => write!(f, "no API key configured"),
            Error::Unauthorized => write!(f, "unauthorized (check API key)"),
            Error::RateLimited => write!(f, "rate limited (free-tier quota?)"),
            Error::Timeout => write!(f, "request timed out"),
            Error::Status(s, detail) if detail.is_empty() => write!(f, "HTTP {s}"),
            Error::Status(s, detail) => write!(f, "HTTP {s}: {detail}"),
            Error::Network(e) => write!(f, "network error: {e}"),
            Error::Empty => write!(f, "empty or unparseable response"),
        }
    }
}

impl std::error::Error for Error {}
