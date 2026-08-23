# What the Mac Studio can run, and what buying more gets you

Checked 2026-08-16. Clean summary — see
[`frontier-hardware-2026.md`](frontier-hardware-2026.md) for the full research
trail and sourcing detail behind these numbers.

## What's owned today

**Mac Studio — Apple M4 Max, 16-core CPU, 40-core GPU, 64GB unified memory,
2TB SSD.**

| Spec | Value |
|---|---|
| Memory bandwidth | 546 GB/s |
| Power | ~30W idle, well under 200W at load |
| Cost to run 4h/day at UK rates (~£0.28/kWh) | ~£61/yr |
| Purchase cost from here | $0 — already owned |

## What it can run right now, no purchase

| Model | Fits in 64GB? | Speed |
|---|---|---|
| Qwen3-Coder-30B-A3B (3B active params) | Yes, comfortably | ~130 tok/s (measured on comparable Apple Silicon) — well past the ~15–30 tok/s bar for a coding agent to feel responsive |
| Qwen3-Coder-Next (~46GB footprint) | Tight — fits at 4-bit with a reduced context window | ~40–60 tok/s (modeled, unverified — worth trying before assuming it needs new hardware) |
| 235B+-class MoE (Qwen3-235B, DeepSeek-V4, Kimi K2.6, GLM-5.2) | No — total footprint exceeds 64GB regardless of active-param count | — |

**Do this first, before spending anything:** install LM Studio or
`mlx-lm`/`llama.cpp` (Metal backend), pull a Qwen3-Coder-30B-A3B quant, serve
it on an OpenAI-compatible port, and point Aider or OpenCode at it (see
[`non-anthropic-coding-agent.md`](non-anthropic-coding-agent.md)). This is
already a genuinely fast, capable local coding model — the honest baseline
everything below is measured against.

## What buying more gets you

The lever that matters is **pairing a second Mac over Thunderbolt 5**
(Apple's JACCL backend, macOS 26.2+), not buying a bigger single box — it
pools memory with the Studio you already have instead of stranding it.

| Buy | Price | Combined pool (approx, minus overhead) | Unlocks |
|---|---|---|---|
| Nothing | $0 | 64GB (current) | Qwen3-Coder-30B-A3B at ~130 tok/s |
| A second M4 Max Studio (64GB) | ~$2,000–2,500 (est., same tier as owned) | ~120GB | Qwen3-Coder-Next with real headroom |
| A new M3 Ultra Studio (96GB — **the only memory config Apple currently sells** on M3 Ultra) | $5,299 / £5,299 | ~130–155GB | Qwen3-Coder-Next comfortably; a squeeze into 235B-class MoE — low-to-mid 20s tok/s on a comparable published 2-node benchmark |
| A secondhand, **pre-shortage** M3 Ultra (256GB or 512GB — no longer orderable new) | Used-market only, price not established | ~320–576GB | The actual DeepSeek-V4/Kimi-K2.6/GLM-5.2 "frontier" tier — the only home-hardware path that gets there |
| AMD Strix Halo 128GB mini PC (standalone, not part of the Thunderbolt pool) | $2,600–4,000 | N/A — separate node, doesn't combine with the Studio | Its own Qwen3-Coder-30B-A3B-class inference (~90–135 tok/s), useful as an `ai-mesh` node but not a memory-pooling partner |

## Two things to know before buying

1. **Apple's whole desktop lineup is memory-capped by a DRAM shortage right
   now, not by chip design.** The 128/256/512GB Mac Studio configs were
   pulled in 2026. The single highest-RAM Mac Apple sells today is actually
   a *laptop* — the M5 Pro/M5 Max MacBook Pro at 128GB — which is backwards
   from normal. A Mac Studio refresh (M5 Max/M5 Ultra) is reported in the
   pipeline, with the M5 Ultra rumoured as high as 768GB, but no ship date
   and no certainty the shortage lets Apple actually ship that much memory.
   Worth checking before committing to the 96GB M3 Ultra now.
2. **Pairing works with mismatched chips, but runs at the slower one's
   speed.** MLX/JACCL doesn't require both Macs to match — an M4 Max and an
   M3 Ultra cluster fine — but every synchronized operation waits on the
   slower node, so the pair runs at the **M4 Max's 546 GB/s pace**, not the
   M3 Ultra's 819 GB/s. Buying the M3 Ultra buys pooled *capacity*, not its
   own throughput throughout.

## Bottom line

1. **Run Qwen3-Coder-30B-A3B on the Studio tonight.** Zero cost, already
   past the speed bar that matters.
2. **If Qwen3-Coder-Next feels worth the upgrade once tested**, the 96GB
   M3 Ultra ($5,299/£5,299) is the only new-purchase pairing option right
   now and gets there comfortably, plus a squeeze into 235B-class MoE.
3. **True "frontier" (DeepSeek-V4/Kimi-K2.6-class) needs ~400GB+ pooled**,
   which currently means either a secondhand pre-shortage M3 Ultra or
   waiting on the rumoured M5 Ultra Studio refresh — not a straightforward
   order today.
