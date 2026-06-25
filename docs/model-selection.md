# Model Selection — Command Generation (REAPER + Lights)

Which model to run for **command/control generation** — i.e. turning natural language
("set tempo to 120", "warm dim glow in the living room") into reliable **tool calls**
for the REAPER and lighting capabilities.

This is a **different axis** from [`beelink-model-guide.md`](../beelink-model-guide.md),
which ranks models by general quality + raw throughput. For *control*, the dominant
factor is **tool-calling reliability** — a model that "narrates" a tool call as plain
text instead of emitting a structured call is useless no matter how clever or fast it is.

## Scope & caveats

- **Inference runs on pi1, beelink1, and the Mac.** OmniLink1 is **controller-only**
  (`nodes/omnilink1.env` → `NODE_FEATURES=reaper`, no `llm`); the project never schedules
  inference on it.
- **Mac node is not yet configured.** The repo plans a **Mac mini M4 (48 GB unified)**
  (~end July 2026). A Mac Studio (M-Max/Ultra, 32–192 GB unified) would have equal-or-greater
  capacity — figures below assume a 48 GB+ Apple-Silicon / Metal node. Confirm the real
  chip/RAM when it joins.
- **tok/s are beelink1 (Radeon 780M) decode figures** from the model guide. The Mac (Metal)
  is materially faster; pi1 (CPU-only Pi 5) is far slower.

## Ranking — best first for command generation

| # | Model | Why (for tool/command gen) | Size Q4 | ~tok/s¹ | pi1 | beelink1 | Mac |
|---|-------|----------------------------|---------|---------|-----|----------|-----|
| 1 | **qwen2.5:7b** | Gold standard — native function-calling, reliable for REAPER control. Best reliability-vs-speed balance. ⭐ | 4.7 GB | 17.9 | ❌ | ✅ | ✅ |
| 2 | **qwen3:8b** | Excellent tool-calling + better intent disambiguation; run with thinking off for snappy commands | 5.0 GB | 16.6 | ❌ | ✅ | ✅ |
| 3 | **qwen3:4b** | Reliable Qwen tool-calling **and** fast — snappiest solid option, ideal for simple lights commands | 2.5 GB | 30 | ⚠️ slow | ✅ | ✅ |
| 4 | **qwen2.5:14b / qwen3:14b** | Most accurate on ambiguous / multi-step commands; slower. Overkill for "lights off" | 9 GB | ~10 | ❌ | ✅ | ✅ |
| 5 | **phi4:14b** | Strong, decent tools, but stricter-JSON tool format less reliable than Qwen, and slow | 8.9 GB | 9.0 | ❌ | ✅ | ✅ |
| 6 | **mistral:7b** | Has tool support, fast-ish; less consistent than Qwen on edge cases | 4.1 GB | 18.6 | ❌ | ✅ | ✅ |
| 7 | **qwen2.5:1.5b** | The **pi1 pick** — small/fast, reliable enough for *simple* lights; weak on complex REAPER | ~1 GB | fast | ✅ | ✅ | ✅ |
| 8 | **llama3.2:3b** | Tool-capable + very fast, but small → flakier on anything non-trivial | 2.0 GB | 36.7 | ⚠️ slow | ✅ | ✅ |
| 9 | **gemma3:4b** | ⚠️ **Avoid for commands** — no native function-calling; observed emitting tool calls as plain text rather than structured calls (2026-06-25). Fine for chat, bad for control | 2.8 GB | 29.5 | ⚠️ | ✅ | ✅ |
| 10 | **deepseek-r1:8b / 14b** | Reasoning/CoT model — slow, verbose, not tool-optimised. Wrong tool for snappy commands | 5–9 GB | ~10–17 | ❌ | ✅ | ✅ |
| — | **qwen2.5:32b** | Best accuracy, but **only the Mac fits it** (20 GB > beelink's 16 GB UMA) | ~20 GB | — | ❌ | ❌ | ✅ |

¹ decode tok/s on beelink1's 780M (see `beelink-model-guide.md`). ✅ runs well · ⚠️ runs but slow/marginal · ❌ won't fit / impractical.

## Practical picks per machine

- **beelink1** (main compute) → **`qwen2.5:7b`** for control. Drop to **`qwen3:4b`** for
  snappier lights; bump to a **14b Qwen** for trickier multi-step intents.
- **pi1** → **`qwen2.5:1.5b`** — fine for lights, not for complex REAPER.
- **Mac** (when it lands) → **`qwen2.5:14b`** daily driver, or **`qwen2.5:32b`** for max
  accuracy (only node that fits 32b; Metal makes the big Qwens genuinely fast).

## Why Qwen dominates here

The Qwen2.5 / Qwen3 instruct families are explicitly trained for function-calling and emit
clean, schema-valid tool calls consistently. Phi-4, Mistral and Llama-3.x support tools but
are less consistent on strict JSON tool-call format. Gemma has no native function-calling
(prompt-coaxed only) and slips into prose. Reasoning models (DeepSeek-R1) add chain-of-thought
latency and verbosity that hurt snappy command turnaround. For *control*, reliability of the
structured call beats every other property — hence the ranking above departs from the
general-quality ordering in `beelink-model-guide.md`.
