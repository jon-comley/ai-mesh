//! One OpenAI-compatible chat client across many LLM providers.
//!
//! Groq, OpenRouter, Mistral, Gemini, Anthropic and OpenAI all speak the
//! OpenAI chat-completions protocol, so a single [`Provider`] reaches any of
//! them by base URL. A [`Rotation`] tries a list of providers in order and,
//! when one says it has run out (rate limited, out of credit, timed out, key
//! rejected), rests it for a while so the next request starts elsewhere.
//!
//! ```no_run
//! use llm_rotation::{Message, Provider, Rotation};
//!
//! # async fn run() -> Result<(), llm_rotation::Error> {
//! let providers = vec![
//!     Provider::new("https://api.groq.com/openai/v1", "gsk-...", "openai/gpt-oss-120b"),
//!     Provider::new("https://api.mistral.ai/v1", "...", "mistral-small-latest"),
//! ];
//! let rotation = Rotation::new();
//! let (reply, used) = rotation
//!     .complete(&providers, &[Message::user("Say hello")], 0.2)
//!     .await?;
//! println!("{} said: {}", used.provider_name(), reply.text);
//! # Ok(())
//! # }
//! ```

mod error;
mod message;
mod presets;
mod provider;
mod rotation;

pub use error::Error;
pub use message::{Message, Role};
pub use presets::{models_for, normalize_url, preset_for, presets, Preset};
pub use provider::{Provider, Reply};
pub use rotation::Rotation;
