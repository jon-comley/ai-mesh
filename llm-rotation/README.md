# llm-rotation

One OpenAI-compatible chat client across many LLM providers. It tries them in
order, free tiers before paid, and when one runs out it rests that provider so
the next request starts elsewhere.

Groq, OpenRouter, Mistral, Gemini, Anthropic and OpenAI all accept the OpenAI
chat-completions request, so one client reaches all of them by base URL.

## Example

```rust
use llm_rotation::{Message, Provider, Rotation};

let providers = vec![
    Provider::new("https://api.groq.com/openai/v1", groq_key, "openai/gpt-oss-120b"),
    Provider::new("https://api.mistral.ai/v1", mistral_key, "mistral-small-latest"),
    Provider::new("https://api.anthropic.com/v1", anthropic_key, "claude-haiku-4-5"),
];

// Keep one Rotation for the life of the program: it remembers who is resting.
let rotation = Rotation::new();

let (reply, used) = rotation
    .complete(&rotation.order(providers), &[Message::user("Say hello")], 0.2)
    .await?;
println!("{} said: {}", used.provider_name(), reply.text);
```

## When a provider is rested

| What the provider said | Rest |
|---|---|
| 429 rate limit, or a timeout | 15 minutes |
| 402, or an error naming credit, quota or billing | 6 hours |
| 401 or 403, the key was rejected | 6 hours |
| Anything else | none, it was this request |

Both lengths can be changed with `Rotation::with_rests`. A resting provider
is moved to the back rather than dropped, so a request still has something to
try when every provider is resting.

## Presets

`presets()` lists the known providers with a model menu, whether each has a
free tier, and a cheap model to use when it stands in for another. Free model
names change often; check each provider's own model list if one starts
failing.

## Licence

MIT or Apache-2.0, at your option.
