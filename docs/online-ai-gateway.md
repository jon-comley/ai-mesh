# Online AI Gateway

The **Online AI** tab lets the mesh forward a chat to a hosted model (OpenRouter,
Anthropic Claude, Groq, Google Gemini, or any OpenAI-compatible endpoint) instead
of running it on a local node. The conversation history is **compressed** before it
is sent, so you get a big-model answer for fewer tokens. Tool-calling (lights,
REAPER) still works — the online model returns the commands and the coordinator
executes them locally. If the cloud call fails, the request quietly falls back to
local inference.

It is **off by default**. Nothing leaves your network until you enable it.

---

## 1. Get an API key

Pick a provider. The first three are free; Anthropic is paid (your own account).

| Provider | Cost | Get a key | Notes |
|---|---|---|---|
| **OpenRouter** | Free tier, no card | <https://openrouter.ai/keys> | One key, many `:free` models. Easiest start. |
| **Groq** | Free tier, no card | <https://console.groq.com/keys> | Very fast (Llama models). |
| **Google Gemini** | Free tier, no card | <https://aistudio.google.com/apikey> | Generous daily quota. |
| **Anthropic (Claude)** | **Paid** (your account) | <https://platform.claude.com/settings/keys> | Highest quality. Uses the OpenAI-compatibility endpoint. |

Steps are the same everywhere:

1. Sign in / sign up (OpenRouter, Groq, and Gemini need no credit card).
2. Open the **API keys** page above and create a key.
3. Copy it — you'll paste it into the tab in the next section. Keys look like
   `sk-or-v1-…` (OpenRouter), `gsk_…` (Groq), `AIza…` (Gemini), `sk-ant-…` (Anthropic).

> Free models can rate-limit (HTTP 429) or briefly go offline — that's expected,
> and it's exactly what the local fallback covers.

---

## 2. Turn it on (dashboard)

Open the dashboard (`http://pi1:9001`, or over Tailscale) and choose the **Online AI**
tab.

1. **Provider** — click a preset: **OpenRouter (free)**, **Anthropic (Claude)**,
   **Groq (free)**, or **Google Gemini (free)**. This fills the endpoint and the
   model list for you.
2. **Online model** — pick one from the pulldown (or type any model id the provider
   supports).
3. **API key** — paste your key and press **Save**. The key is stored on the
   coordinator and **never shown back** — you'll only ever see a `key set …1234`
   hint. Keys are saved **per provider**: switch the provider preset and each
   one's key is restored automatically, so you only paste a key the first time
   (or when changing it).
4. **Test cloud call** — press it; you should get `✓ pong`.
5. **Online AI: Enabled** — press the toggle. From now on, chat goes to the cloud
   model. Press it again (**Disabled**) to return to local-only.

That's it. Use the **Chat** tab as normal.

### The two extra toggles

- **Compress context: On / Off** — On (default) compresses the conversation history
  before sending (fewer tokens). Off forwards the full history unchanged — useful if
  you ever want the online model to see every word verbatim.
- **Compression engine** — **Statistical (Rust)** is the live engine. *Local-LLM
  distiller* and *LLMLingua-2* are on the roadmap and shown greyed-out.

### Live stats

The tab shows cumulative usage since the coordinator last started: cloud calls,
context tokens in → out, tokens saved (with %), last call time, and the last error
(if any). They update live as you chat.

---

## 3. What "pass" looks like

- **Test cloud call** returns `✓ pong`.
- A general question in **Chat** comes back from the cloud model; the Online AI tab
  shows **calls: 1** and **tokens saved** climbing.
- A command like *"turn the kitchen lights blue"* still changes the light — the
  online model produced the tool call and the mesh ran it.
- If you enter a bad key, chats keep working (answered **locally**) and the tab's
  **last error** explains why.

---

## Suggested models per provider

| Provider | Endpoint | Good first model |
|---|---|---|
| OpenRouter | `https://openrouter.ai/api/v1` | `meta-llama/llama-3.3-70b-instruct:free` (fast, strong tool use) |
| Anthropic | `https://api.anthropic.com/v1` | `claude-opus-4-8` (best), `claude-sonnet-4-6`, `claude-haiku-4-5` |
| Groq | `https://api.groq.com/openai/v1` | `llama-3.3-70b-versatile` |
| Google Gemini | `https://generativelanguage.googleapis.com/v1beta/openai` | `gemini-2.0-flash` |

Any OpenAI-compatible provider works — just set the **endpoint** box to its base URL
and type the model id.

---

## Headless / deploy configuration (optional)

Everything above can also be set with environment variables on the coordinator, so a
fresh deploy starts pre-configured. Values entered in the tab take precedence; env
vars are the fallback.

| Variable | Purpose | Example |
|---|---|---|
| `CLOUD_API_KEY` | API key | `sk-or-v1-…` |
| `CLOUD_BASE_URL` | Endpoint base URL | `https://api.anthropic.com/v1` |
| `CLOUD_MODEL` | Default model id | `claude-opus-4-8` |
| `PROMPT_COMPRESS_RATIO` | Target keep-ratio, 0.1–0.95 | `0.5` (keep ~50%) |
| `CLOUD_TIMEOUT_SECS` | Cloud request timeout | `60` |
| `CLOUD_HTTP_REFERER` / `CLOUD_X_TITLE` | OpenRouter free-tier headers | `https://github.com/ai-mesh` / `ai-mesh` |

Measure compression savings on a sample corpus at any time:

```sh
just measure-compression
```

---

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Banner: *"No API key set"* | Paste a key and press **Save**. |
| Test call: `✗ unauthorized` | Wrong/expired key, or the model isn't available on that endpoint. |
| Test call: `✗ rate limited` | Free-tier quota hit — wait, switch model, or use another provider. |
| Chats answer locally even when enabled | No key configured (it falls back) — check `key set …` shows in the tab. |
| Lights/REAPER stop responding in cloud mode | They shouldn't — the online model returns the commands and the mesh runs them. If a command is ignored, try a stronger model (e.g. `claude-opus-4-8` or a 70B Llama). |

---

## Notes & limits

- The API key is stored server-side and is **never** sent back to the browser — the
  tab only sees a masked `key set …1234` hint.
- Anthropic is reached via its **OpenAI-compatibility** endpoint
  (<https://platform.claude.com/docs/en/api/openai-sdk>). Prompt caching isn't
  available over that endpoint, and `temperature` is capped at 1 (the mesh uses
  0.4) — neither affects normal use.
- Compression only meaningfully helps on **long** conversations; short one-line
  commands pass through unchanged.
- Free providers are free; **Anthropic bills your own account** — compression is what
  keeps that bill small.
