# A Claude-Code-like experience without Anthropic

Prompted by Anthropic's invisible text watermarking rollout (all Claude models,
2026-08-02+, no opt-out, driven by EU AI Act Article 50). This is the practical
answer: what to run instead, and how to wire it into ai-mesh rather than
standing up something separate.

## Where the other providers actually stand (checked 2026-08-16, not assumed)

| Provider | Text watermarking |
|---|---|
| **Anthropic/Claude** | Yes — invisible, embedded in generation, described as surviving copy/paste and edits. |
| **OpenAI** | No. Provenance signals only on images/audio, not text. |
| **Kimi (Moonshot)** | File-metadata marker, not an embedded text watermark. |
| **DeepSeek** | Platform-level labeling only. No public documentation of a copy-paste-resilient text watermark on V4. Not a signatory of the EU's voluntary Code of Practice either — so "no confirmation" is not the same as "confirmed absence." |

The only way to be *certain* rather than trusting a vendor's current posture is
to run the model yourself. Article 50's marking duty falls on whoever *places
the system on the market* — self-hosting for your own use isn't that, so it
sits structurally outside the obligation rather than just being a provider
who hasn't gotten around to it.

## Three tiers, in order of how much trust you're extending

### Tier 1 — Fully local, today, zero new spend
Already runnable on `beelink1` (Radeon 780M, 16 GB UMA) per
[`beelink-model-guide.md`](../beelink-model-guide.md): `qwen3:14b` or
`deepseek-r1:14b`. Nothing leaves the machine, so watermarking is a
non-question. Ceiling is real, though — solid for day-to-day work, not
competitive with frontier agentic-coding benchmarks.

### Tier 2 — Fully local, frontier quality, needs new hardware
Current strongest genuinely-downloadable coding models (checked 2026-08-16):

| Model | SWE-bench Verified | License | Minimum footprint |
|---|---|---|---|
| DeepSeek-V4-Pro-Max | 80.6% (vendor) | MIT | 80 GB-class multi-GPU |
| Qwen3-Coder-Next | 70.6% | Apache 2.0 | ~46 GB — single Apple M4 Max (128 GB) or 2×48 GB GPU |

Nothing in ai-mesh today clears 46 GB — this tier means a new node (a Mac
Studio, or a 2×48 GB GPU box), registered into the mesh the same way
`beelink1` is. It's the only tier where "no watermark" isn't a claim you're
trusting someone else on.

### Tier 3 — Cloud, open-weight model, someone else's GPUs
DeepSeek's own API: OpenAI-compatible **and** Anthropic-compatible endpoints,
native tool-calling, 1M context, $0.435/$0.87 per M tokens peak (V4 Pro,
cheaper off-peak, half-price V4 Flash) — checked 2026-08-16 via
platform.deepseek.com, reconfirm before relying on it since DeepSeek moved to
peak/off-peak billing that same day. Cheapest and fastest to stand up; the
trust question is the same open one as the table above.

## The constraint that decides which CLI you can actually use

Two things worth knowing before picking a tool:

1. **ai-mesh's own inbound OpenAI endpoint rejects tool-calling.**
   [`docs/openai-api.md`](openai-api.md) is explicit: `tools` / `tool_calls`
   → `400 invalid_messages`. It's a pure-chat surface by design; tool
   execution lives on the dashboard's `/api/chat` intent pipeline instead. Any
   tool-calling-native coding agent pointed **at the mesh's gateway**
   (`http://pi1:9001/v1`) will work fine for chat and die the moment it tries
   to edit a file or run a command.
2. **Aider doesn't use the function-calling API at all.** It edits via
   plain-text SEARCH/REPLACE or whole-file blocks inside the normal chat
   stream — benchmarked by its own author as working *better* than
   function-calling, not just as a fallback. That means Aider is the one
   agent that works through ai-mesh's gateway exactly as it stands today,
   unmodified: local routing, the cloud-gateway fallback, all of it, for free.

| CLI | License | Style | Tool-calling required? | Works through ai-mesh's gateway today? |
|---|---|---|---|---|
| **Aider** | Apache 2.0 | Terminal, diff-based | No | **Yes, as-is** |
| **OpenCode** | MIT | TUI, closest feel to Claude Code | Yes | No — point it directly at DeepSeek instead |
| **Qwen Code** | Apache 2.0 | Alibaba's Claude-Code-style fork, tuned for Qwen3-Coder but provider-flexible | Yes | No |
| **Cline / Continue** | Apache/MIT | VS Code extensions, not terminal | Yes | No |
| Goose, OpenHands | Apache 2.0 | Heavier, multi-agent orchestration | Yes | No |

## Recommended setup: two tracks

**Track A — working in 15 minutes, ai-mesh stays the control plane.**
1. Get a DeepSeek API key at platform.deepseek.com.
2. Set it as the mesh's cloud gateway (dashboard **Online AI** tab, "any
   OpenAI-compatible endpoint" preset, or headless via env):
   `CLOUD_API_KEY=<key>`, `CLOUD_BASE_URL=https://api.deepseek.com/v1`,
   `CLOUD_MODEL=deepseek-chat` — verify the exact base path against DeepSeek's
   current docs at setup time.
3. `pip install aider-chat`, then run it against the mesh:
   `aider --openai-api-base http://pi1:9001/v1 --openai-api-key <MESH_AUTH_TOKEN> --model deepseek-chat`
4. Confirm with a trivial edit before trusting it on real work.

**Track B — full Claude-Code parity (tool-calling, TUI, multi-file agentic
loop).** OpenCode, pointed *directly* at DeepSeek — bypassing ai-mesh's
gateway, since tools aren't passed through there yet:
1. Install OpenCode.
2. `opencode auth login` → **Other** → base URL `https://api.deepseek.com/v1`
   → paste the DeepSeek key.
3. Set default model to `deepseek-chat` or `deepseek-reasoner`.
4. Run `opencode` in a repo — same keybinds and feel as Claude Code.

**Track C — later, once a Tier-2 node exists.** Register the new box into
ai-mesh like `beelink1`, run Qwen3-Coder-Next or DeepSeek-V4-Pro-Max on it via
vLLM/llama.cpp. Aider keeps working through the mesh unmodified; point
OpenCode straight at that node's own `llama-server` (not through the
tool-stripping gateway layer) for the tool-calling path. This is the only
tier that's fully local *and* frontier-quality — the actual end state if the
goal is zero trust extended to anyone.

## One shortcut worth naming, then setting aside

DeepSeek's API also exposes an Anthropic-compatible route at
`api.deepseek.com/anthropic`. That means the Claude Code binary itself —
identical keybinds, identical everything — can be pointed at DeepSeek by
overriding `ANTHROPIC_BASE_URL` / `ANTHROPIC_API_KEY`. It's the literal
shortest path to "the Claude Code interface, DeepSeek underneath." Left out
of the recommendation above because it's still Anthropic's client (their
telemetry, their license terms) — worth it only if the objection is
specifically to the watermark and not to Anthropic's software generally.
OpenCode or Aider are the cleaner call if it's the latter.

## Open items, not yet done

- ai-mesh's inbound OpenAI endpoint doesn't forward `tools`/`tool_calls` to
  the local `llama-server` or the cloud gateway. Adding that (in
  `capabilities/llm`) is the piece that would let OpenCode/Cline/Qwen Code use
  the mesh as the single control plane the same way Aider already can —
  scoped, not started.
- No current ai-mesh node clears the ~46 GB floor for a Tier-2 model.
  `beelink1` tops out at 16 GB. Tier 2 needs a hardware decision, not just
  config.
