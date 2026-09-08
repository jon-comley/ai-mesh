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

## The refresh landed — checked 2026-09-07

**Point 1 below said to check before committing, so this is that check.** Apple
announced the **M5 Max and M5 Ultra Mac Studio on 2026-08-25**, shipping
**22 September**; 512GB follows in late October, so 256GB is the ceiling until
then. **The DRAM cap that shaped every row above is lifted.**

| | Memory | Bandwidth | From |
|---|---|---|---|
| M4 Max (owned) | 64GB | 546 GB/s | — |
| **M5 Max** | up to **128GB** | 614 GB/s | $2,499 base |
| **M5 Ultra** | up to **512GB** (256GB until late Oct) | **1.2 TB/s** | $5,499 base |

**Two rows above are now obsolete and should not be bought.**

- **"A second M4 Max (64GB), ~$2,000–2,500, pool ~120GB"** — an **M5 Max at
  128GB is a better version of this in one box**: more memory than the pair,
  faster, no Thunderbolt hop, no second machine to power or house. Buy that
  instead if the goal is headroom.
- **"A secondhand pre-shortage M3 Ultra, used-market only"** — the frontier tier
  is orderable new again. That row existed only because the shortage made it the
  sole path.

**The prices are base configurations and the memory is the expensive part.**
256GB on the M5 Ultra is **$10,799** with 1TB storage, against the $5,499
headline. Apple does not publish the 128GB M5 Max price on its configurator page
in a form worth quoting, so **get it from the configurator before planning
around it** rather than inferring it from the base.

## The UK configurator, read off the page — 2026-09-08

**Jon pasted the live Apple Store UK page**, so these are actual orderable
options and prices rather than launch-day reporting. **The pound figures equal
the dollar ones** — £2,499 and £5,499 against the $2,499/$5,499 above, which is
Apple's usual UK pricing rather than a conversion.

| Option | M5 Max | M5 Ultra |
|---|---|---|
| From | **£2,499** | **£5,499** |
| Neural Engine | 16-core | 32-core |
| CPU / GPU | — | 30-core/64-core base, **36-core/80-core +£1,300** |
| Memory | — | 96GB base, **256GB +£4,000**; 512GB **late October** |

**Storage ladder, same on both:** 1TB included, 2TB **+£500**, 4TB **+£1,500**,
8TB **+£3,500**, 16TB **+£7,500**.

**So the £10,799 figure above decomposes exactly**: £5,499 base + £1,300 for the
36-core/80-core + £4,000 for 256GB, at 1TB storage. **Memory is 79% of the
upgrade spend.** (The configurator's summary also had a Final Cut Pro licence
selected at £299.99 and Logic Pro at +£199.99 — neither is part of that number
and neither is wanted here.)

**The £1,300 CPU/GPU step is the one worth thinking about rather than reflexively
taking.** Token *generation* on Apple silicon is memory-bandwidth-bound, and
bandwidth is a property of the Ultra die — 1.2 TB/s on both variants — so the
extra 16 GPU cores mostly buy **prefill**: prompt processing, long-context reads,
the first token. For a coding agent chewing through large files that is not
nothing, but it is a different axis from "can it hold the model". **Worth a
benchmark before spending**, and 30-core/64-core with 256GB at **£9,499** is the
same capacity for £1,300 less.

**The number that actually changes planning: dispatch is 16–18 weeks.** Ordered
today that is **late December to mid-January**, not September. Anything that
depends on this machine existing needs to assume the current hardware until then,
and the 512GB option arriving in late October **lands inside that window** — so
ordering 256GB now to get in the queue means committing to 256GB while a 512GB
option becomes orderable before the 256GB machine ships.

**Ports, for completeness.** Front: two Thunderbolt 5, SDXC. Back: four
Thunderbolt 5, two USB-A, HDMI, **10Gb Ethernet**, 3.5mm headphone. Up to eight
external displays. The 10Gb Ethernet is the one that matters here — it is the
link to `pi1` and to any second box in an MLX pairing.

## Two things to know before buying

1. **STALE — the refresh landed and this item is superseded by the two sections
   above.** Kept because the reasoning still explains why the old options looked
   the way they did. **Apple's whole desktop lineup is memory-capped by a DRAM
   shortage right now, not by chip design.** The 128/256/512GB Mac Studio configs were
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
