# OpenAI-Compatible API

The coordinator exposes an **inbound OpenAI-compatible API**, so any
OpenAI-SDK client — Continue, LibreChat, LangChain, a Python script, anything
that takes a `base_url` — can use the mesh as its "OpenAI". Requests are
routed to a local llama-server node (or the cloud gateway when the requested
model is the gateway's) and the caller never knows the difference.

This is the **pure chat** surface: the caller's messages (including their
system prompt) go to the model verbatim. No device schemas are injected and
no tool calls are executed — that behaviour stays on the dashboard's
`/api/chat` intent path.

---

## Endpoints

| Endpoint | Method | Purpose |
|---|---|---|
| `/v1/chat/completions` | POST | Chat completion (JSON or SSE streaming) |
| `/v1/models` | GET | Models the mesh can serve right now |

Base URL: `http://pi1:9001/v1` (or `http://10.0.0.10:9001/v1` on the LAN,
Tailscale addresses from anywhere).

## Auth

Send the mesh auth token as a Bearer header — exactly what every OpenAI SDK
does with its `api_key`:

```
Authorization: Bearer <MESH_AUTH_TOKEN>
```

`?token=<MESH_AUTH_TOKEN>` also works as a fallback (parity with the rest of
the HTTP API). The header wins when both are present. Failures return the
OpenAI error envelope with `code: "invalid_api_key"`.

## Model routing

- `model` matches a **Ready local model** → dispatched to a connected node via
  the scheduler (load-spread across nodes serving that model).
- `model` equals the **cloud gateway's selected model** (gateway enabled +
  configured) → forwarded to the cloud provider. No compression is applied on
  this path and there is **no silent cloud↔local fallback** — you asked for a
  specific model, you get that model or an error.
- Anything else → `404` with `code: "model_not_found"`.
- `model` omitted → largest Ready local model, else the gateway model, else
  `503` `no_model_ready`.

`GET /v1/models` lists exactly what routing will accept: local Ready models
(`owned_by: "ai-mesh"`) plus the gateway model when the gateway is enabled.

## Request parameters

| Field | Supported | Notes |
|---|---|---|
| `messages` | ✅ | Roles `system` / `user` / `assistant`; string content only |
| `model` | ✅ | Optional (see routing above) |
| `max_tokens` | ✅ | Default 2048; `max_completion_tokens` accepted as alias |
| `temperature` | ✅ | Default 0.8 local, 0.4 cloud |
| `stream` | ✅ | OpenAI-spec SSE (`chat.completion.chunk` events) — see Streaming below |
| `stream_options.include_usage` | ✅ | Adds the final usage chunk (empty `choices`) before `[DONE]` |
| `tools` / `tool_calls` | ❌ | Rejected via role validation — tool execution lives on `/api/chat` |
| Array-of-parts `content` (vision style) | ❌ | `400 invalid_body` |

Unknown fields (`top_p`, `n`, `stream_options`, …) are ignored, as OpenAI
clients expect.

## Streaming

`"stream": true` returns `text/event-stream` with the standard OpenAI chunk
sequence, on both local and cloud routes:

1. A role-first chunk (`delta: {"role":"assistant","content":""}`).
2. One `chat.completion.chunk` per token batch (`delta.content`).
3. A finish chunk (empty delta, `finish_reason: "stop"` / `"length"`).
4. If `stream_options.include_usage` was set: a usage chunk (`choices: []`,
   real `usage` counts).
5. `data: [DONE]`.

Every chunk shares one `id` and `created`. SSE keep-alive comments are sent
during long prefill so proxies don't drop the idle connection.

**Failure semantics:** if the serving node dies mid-stream, stalls (no data
for 60 s between tokens, 300 s before the first), or the stream buffer
overflows, the client receives one final event —
`data: {"error":{"message":"…","type":"api_error","code":"upstream_error"}}` —
followed by `[DONE]`, and the connection closes. A stream never hangs.
Killing the client mid-stream cancels generation on the node (the whole
chain tears down), freeing the model for the next request.

Note: a node serves one generation at a time, so a long stream holds that
node's inference slot for its full duration; concurrent non-stream requests
for the same model queue behind it.

## Response

Standard `chat.completion` object: `id` (`chatcmpl-<uuid>`), `created`,
`model` (the model that actually served), one `choices[0]` entry with the
assistant message, `finish_reason` (`"stop"`, or `"length"` when the reply
hit `max_tokens`), and real `usage` counts (`prompt_tokens` comes from
llama-server's `usage`, so cost/size accounting is accurate).

### Model-family notes

Serving-layer generation quirks are applied on the agent (they're tuning
knobs like `repeat_penalty`, not content injection):

- **Qwen**: `/no_think` is appended to your system turn (or added as a lone
  system turn if you sent none) to suppress chain-of-thought.
- **DeepSeek-R1**: an empty `<think></think>` assistant prefill is appended —
  unless your last message is already an `assistant` prefill, which is kept.
  `<think>` blocks in the output are returned verbatim on this API.

## Errors

OpenAI envelope everywhere: `{"error": {"message", "type", "code"}}`.

| Status | `code` | When |
|---|---|---|
| 401 | `invalid_api_key` | Missing/wrong Bearer token |
| 400 | `stream_not_supported` | `stream: true` |
| 400 | `invalid_messages` | Empty messages, unsupported role |
| 400 | `invalid_body` / `invalid_max_tokens` | Malformed JSON, `max_tokens: 0` |
| 404 | `model_not_found` | Model neither local-Ready nor the gateway model |
| 503 | `no_model_ready` | No model requested and nothing can serve |
| 503 | `upstream_error` | Node disconnected, dispatch timeout, cloud network error |
| 500 | `upstream_error` | Node accepted but inference failed |
| 500 | `gateway_misconfigured` | Cloud key rejected / missing |
| 429 | `rate_limit_error` | Cloud provider rate limit |

## Examples

curl:

```sh
TOKEN=$(source ~/.config/ai-mesh/coordinator.state && echo $MESH_AUTH_TOKEN)

curl -s http://pi1:9001/v1/models -H "Authorization: Bearer $TOKEN" | jq

curl -s http://pi1:9001/v1/chat/completions \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen2.5:7b",
    "messages": [
      {"role": "system", "content": "You are terse."},
      {"role": "user", "content": "Why is the sky blue?"}
    ]
  }' | jq
```

Or `just openai "why is the sky blue?"` (optional second arg pins a model),
and `just openai-stream "count to 20"` to watch the SSE chunks live.

OpenAI Python SDK:

```python
from openai import OpenAI

client = OpenAI(base_url="http://pi1:9001/v1", api_key="<MESH_AUTH_TOKEN>")
reply = client.chat.completions.create(
    model="qwen2.5:7b",
    messages=[{"role": "user", "content": "Why is the sky blue?"}],
)
print(reply.choices[0].message.content)
print(reply.usage)

# Streaming:
for chunk in client.chat.completions.create(
    model="qwen2.5:7b",
    messages=[{"role": "user", "content": "Count to 20."}],
    stream=True,
):
    if chunk.choices and chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="", flush=True)
```

## Limits

- One `choices` entry per response (`n` is ignored).
- String message content only — no image/vision parts.
- Tool calling is not exposed here yet; the intent pipeline (`/api/chat`)
  remains the tool-calling surface.
