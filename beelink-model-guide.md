# Beelink SER8 — Model Inference Guide

**Hardware:** AMD Radeon 780M · 16 GB UMA VRAM · ~80 GB/s shared memory bandwidth

---

## Models that fit in 16 GB UMA

Sizes are Q4_K_M quantisation (Ollama default). tok/s estimates are derived from
the measured baseline of **16.6 tok/s on qwen3:8b** (5.0 GB) and scaled by the
bandwidth formula `~80 GB/s ÷ model_size`. Measured values are marked.

| Model | VRAM | Est. tok/s | Quality tier |
|-------|------|-----------|--------------|
| qwen3:0.6b | 0.4 GB | ~90 | Minimal — arithmetic, one-liners |
| qwen3:1.7b | 1.1 GB | ~55 | Basic — short Q&A, simple tasks |
| llama3.2:3b | 2.0 GB | ~35 | Decent — general chat |
| qwen3:4b | 2.5 GB | ~30 | Good — solid reasoning, fast |
| gemma3:4b | 2.8 GB | ~27 | Good — Google instruction-tuned |
| mistral:7b | 4.1 GB | ~19 | Good — strong instruction following |
| qwen3:8b | 5.0 GB | **16.6** (measured) | Better — default pick |
| deepseek-r1:8b | 5.0 GB | **17** (measured) | Strong reasoning — see CoT note below |
| gemma3:12b | 7.8 GB | ~12 | Strong — good coding + analysis |
| phi4:14b | 8.9 GB | ~9 | Strong reasoning, STEM |
| qwen3:14b | 9.0 GB | ~10 | Best quality that fits comfortably |
| deepseek-r1:14b | 9.0 GB | ~10 | Strong reasoning — see CoT note below |
| qwen3:32b | ~20 GB | ✗ won't load | Exceeds 16 GB UMA |

---

## Inference time examples

tok/s numbers from the table above. The times below are dominated by output
generation. Prompt evaluation (prefill) adds a one-off cost on the first request
to a freshly-loaded model; see the Prompt evaluation section for measured values.

---

### Example A — "What is 2+2?"
**Input:** ~6 tokens &nbsp;|&nbsp; **Typical response:** ~12 tokens ("The answer is 4.")

| Model | Output tokens | Time |
|-------|--------------|------|
| qwen3:0.6b | 12 | ~0.1 s |
| qwen3:4b | 12 | ~0.4 s |
| qwen3:8b | 12 | ~0.7 s |
| gemma3:12b | 12 | ~1.0 s |
| phi4:14b | 12 | ~1.3 s |
| qwen3:14b | 12 | ~1.2 s |

---

### Example B — "Turn the kitchen lights off and set the living room to a warm dim glow for movie night."
**Input:** ~24 tokens &nbsp;|&nbsp; **Typical response:** ~60 tokens (intent confirmation + action summary)

| Model | Output tokens | Time |
|-------|--------------|------|
| qwen3:0.6b | 60 | ~0.7 s |
| qwen3:4b | 60 | ~2.0 s |
| qwen3:8b | 60 | ~3.6 s |
| gemma3:12b | 60 | ~5.0 s |
| phi4:14b | 60 | ~6.7 s |
| qwen3:14b | 60 | ~6.0 s |

---

### Example C — "Explain how UMA memory works and why it affects GPU performance."
**Input:** ~16 tokens &nbsp;|&nbsp; **Typical response:** ~300 tokens (technical explanation, 3–4 paragraphs)

| Model | Output tokens | Time |
|-------|--------------|------|
| qwen3:0.6b | 300 | ~3.3 s |
| qwen3:4b | 300 | ~10 s |
| qwen3:8b | 300 | ~18 s |
| gemma3:12b | 300 | ~25 s |
| phi4:14b | 300 | ~33 s |
| qwen3:14b | 300 | ~30 s |

---

## Rule of thumb for quick estimates

```
seconds ≈ output_tokens ÷ tok/s
```

For a typical home assistant reply (~60 tokens):
- **Fast (qwen3:4b):** 2 s
- **Balanced (qwen3:8b):** 4 s
- **Thorough (qwen3:14b / phi4:14b):** 6–7 s

For a detailed code or explanation reply (~400 tokens):
- **Fast (qwen3:4b):** 13 s
- **Balanced (qwen3:8b):** 24 s
- **Thorough (qwen3:14b / phi4:14b):** 40–44 s

---

## Reasoning models and chain-of-thought suppression

Some models in the table are *reasoning* models — they generate a hidden thinking
chain before the visible answer. The thinking tokens are stripped from the response
but still consume generation time.

| Model family | Reasoning | Suppression method |
|---|---|---|
| Qwen2.5, Qwen3 | Optional (off by default on Qwen2.5; opt-in on Qwen3) | `/no_think` appended to system prompt |
| DeepSeek-R1 | Always-on by default | Empty `<think>\n</think>` assistant prefill |
| phi4, gemma3, mistral, llama | None | No action needed |

Without suppression, deepseek-r1:8b spends ~6–8 s generating ~100 hidden thinking
tokens for a simple one-line reply — making a 5 GB model slower end-to-end than
phi4:14b despite running at 17 tok/s. With the empty-think prefill, generation time
matches response length as expected.

ai-mesh applies these suppressions automatically in `capabilities/llm/src/llama.rs`
based on the model name prefix (`qwen` → `/no_think`, `deepseek-r1` → prefill).

---

## Prompt evaluation (prefill) times

Prompt evaluation — processing your input and the system prompt before the first
output token — can be significant on the first request to a freshly-loaded model.

| Hardware | Model | First request | Subsequent |
|---|---|---|---|
| Beelink (GPU) | phi4:14b | ~6.6 s | ~2.3 s |
| Beelink (GPU) | deepseek-r1:8b | ~3.3 s | ~1.0 s |
| pi1 (CPU) | qwen2.5:1.5b | ~22 s | ~1–2 s |

The large first-request cost is a cold KV-cache: llama.cpp must process all system
prompt tokens from scratch. Subsequent requests reuse the cached KV state, so only
the new conversation turn (a few tokens) is evaluated.

ai-mesh keeps the system prompt fully static (device state is sent in the user turn
instead) so `cache_prompt: true` gets a guaranteed cache hit from the second request
onward.

---

## Notes

- All tok/s figures are for **generation** (output).
- UMA bandwidth (~80 GB/s) is shared with the CPU and display. Heavy CPU load or
  screen rendering can reduce effective GPU bandwidth by 10–15%.
- Models larger than ~13 GB (qwen3:32b etc.) will spill to system RAM and drop to
  < 5 tok/s; avoid these on this hardware.
- Token counts use the rough rule of **1 token ≈ 4 characters / 0.75 words**.
  Actual counts vary slightly per model tokeniser.
