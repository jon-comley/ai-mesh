# Frontier local-inference hardware, 2026

Checked 2026-08-16. Answers the hardware gap flagged in
[`non-anthropic-coding-agent.md`](non-anthropic-coding-agent.md) Tier 2: what
to actually buy to run a frontier-class open-weight coding model, fully
locally, at a usable interactive speed, for the least money and power.

**Update, same day: there's already a Mac Studio M4 Max (64GB unified
memory, 40-core GPU) in a box.** That's real Tier-2 hardware, at $0
incremental cost — see "What's already in the house" below before anything
here gets bought. It doesn't reach the biggest MoE tiers, but it comfortably
clears the realistic target (Qwen3-Coder-30B-A3B class).

## The two numbers that decide everything

Not brand, not marketing. For any unified-memory or APU platform, decode
speed is **memory-bandwidth-bound**:

```
tok/s ≈ memory_bandwidth_GBps ÷ active_weight_size_GB
```

Confirmed against `beelink1`'s own measured numbers in
[`beelink-model-guide.md`](../beelink-model-guide.md) (80 GB/s ÷ 5.0 GB
qwen3:8b = 16 tok/s, matches the measured 16.6). Two consequences that matter
more than any spec sheet:

1. **Capacity decides what fits at all.** DeepSeek-class 671B-total MoE
   models need ~400GB+ of memory even at 4-bit — only the biggest Mac Studio
   configs clear that bar at home. Nothing else in this list does.
2. **MoE active-param count decides speed, not total size.** A 30B-A3B model
   (3B active per token) flies on hardware a dense 70B model crawls on, because
   bandwidth-bound decode only touches the active experts' weights. This is
   why "Qwen3-Coder-30B-A3B" and "DeepSeek-V4" behave completely differently
   on the same box despite one being much bigger on disk.

Where a number below is my own bandwidth÷active-weight estimate rather than
someone's measurement, it's marked **(modeled)** — verify before buying on
the strength of it.

## What's already in the house

**Mac Studio, M4 Max — 16-core CPU, 40-core GPU, 64GB unified memory.**
Apple's published bandwidth for M4 Max is 546 GB/s — roughly 7x `beelink1`'s
80 GB/s and comfortably ahead of both Strix Halo (~256 GB/s) and DGX Spark
(273 GB/s) below, though short of the M3 Ultra's 819 GB/s. This is real
Tier-2 hardware, sitting in a box, at $0 marginal cost.

What 64GB total (minus macOS + KV-cache overhead, realistically ~50–56GB
usable for weights) actually fits:

| Model | Fits on 64GB? | Modeled tok/s (546 GB/s ÷ active weight) |
|---|---|---|
| Qwen3-Coder-30B-A3B (3B active, ~18GB total at 4-bit) | **Yes, comfortably** | ~290 (modeled) — measured MLX figures for this model on comparable Apple Silicon landed ~130 tok/s, so treat 290 as a ceiling, not an expectation |
| Qwen3-Coder-Next (~46GB reported footprint) | **Tight** — fits at 4-bit only with a reduced context window; the "needs 128GB M4 Max" claim some sources give doesn't match Apple's current lineup (the 128GB *M4 Max* Studio configuration doesn't exist — that capacity is only on M3 Ultra). Worth trying on what's owned before assuming it needs new hardware. | (modeled) ~40–60, unverified |
| 235B+-class MoE, DeepSeek-V4/671B-class | **No** — total footprint exceeds 64GB regardless of active-param count | — |

**Practical next step, no purchase required:** install LM Studio or
`mlx-lm`/`llama.cpp` (Metal backend) on the Studio, pull a Qwen3-Coder-30B-A3B
GGUF or MLX quant, serve it on an OpenAI-compatible port, and either register
it into `ai-mesh` as a node the same way `beelink1` is, or point Aider/OpenCode
straight at it. This is the thing to actually benchmark before spending
anything below.

## Pairing the Studio instead of replacing it

If more hardware is on the table, the highest-leverage purchase isn't a
different box — it's a **second Mac**, linked to the M4 Max Studio over
Thunderbolt 5 to pool memory. This matters because it's the one upgrade path
that can plausibly reach actual DeepSeek-V4/671B-class territory without
jumping straight to a $14k single machine.

**The mechanism:** Apple shipped **JACCL** with macOS 26.2 — an MLX
distributed backend that runs collectives over **RDMA on Thunderbolt 5**,
reported at 50–60 Gbps with sub-50µs latency (roughly 6–7.5 GB/s of
inter-node bandwidth, which is what makes this fast enough to matter — pure
network clustering, e.g. over Ethernet, is bottlenecked far below that). The
M4 Max Studio already has four Thunderbolt 5 ports, so no new I/O hardware is
needed, just a second Mac and a cable. Reported results (checked
2026-08-16, WWDC26 demo + community writeups, not independently verified):
two Mac Studios linked this way can host a 400B-parameter MoE "with
respectable throughput"; a four-Mac cluster ran the 1-trillion-parameter Kimi
2.6 at 28+ tok/s.

**What a second Mac buys, concretely — corrected 2026-08-16 after actually
checking Apple's current pricing (see the hardware table above):** the
256GB/512GB M3 Ultra path is **currently a dead end for a new purchase.**
Apple pulled 512GB in March 2026 and 256GB shortly after — global DRAM
shortage — and as of today **96GB is the only memory tier Apple sells on
M3 Ultra**, at $5,299/£5,299. The bigger-pool numbers below only apply if a
pre-shortage 256GB/512GB unit turns up secondhand.

| Add | Combined pool (approx, minus overhead) | Gets you to |
|---|---|---|
| A second M4 Max 64GB (same spec as owned) | ~120GB | Qwen3-Coder-Next with real headroom; a squeeze on 235B-class MoE at aggressive quant — not full 671B |
| A **new** M3 Ultra 96GB (the only config Apple currently sells, $5,299/£5,299) | ~150–155GB | Qwen3-Coder-Next comfortably, some room into bigger MoE tiers — still well short of 400GB-class pooling |
| A **secondhand, pre-shortage** M3 Ultra 256GB or 512GB | ~320–576GB | The 400B-MoE-class territory the JACCL reports describe — but this now depends on finding used stock from before Apple discontinued the config, not on a straightforward order |

The DRAM shortage that killed the 256/512GB configs is an industry-wide,
ongoing constraint (also showing up in the Strix Halo/DGX Spark pricing
above) — worth checking whether it's eased before assuming the used-market
route is the only one, rather than taking this as a permanent state of the
market.

**Across the whole current Apple lineup (checked 2026-08-16): 128GB is the
most unified memory orderable right now, and it's on the M5 Pro/M5 Max
MacBook Pro — not on any desktop.** Mac Studio tops out lower (96GB on M3
Ultra, 64GB on M4 Max) purely because of the shortage. There's a reported
M5 Max/M5 Ultra Mac Studio refresh in the pipeline, with the M5 Ultra
rumoured as high as 768GB — but unconfirmed whether Apple can actually ship
that much memory given the same shortage, and no ship date pinned down. If
the pairing purchase isn't urgent, this is worth watching before buying the
current 96GB M3 Ultra — a refreshed Studio could make the whole
"pair two Macs" workaround unnecessary.

**Mixed M4 Max + M3 Ultra specifically — checked 2026-08-16:**

- **Heterogeneous clustering works.** MLX/JACCL doesn't require matching
  chips — MPI won't complain about an M4 Max paired with an M3 Ultra. But
  every collective operation waits on the slowest node, so the pair runs at
  **M4 Max speed (546 GB/s), not M3 Ultra speed (819 GB/s)**, on the portion
  of work that has to synchronize. Buying the M3 Ultra buys pooled
  *capacity*, not the M3 Ultra's own throughput throughout.
- **Setup, concretely:** both Macs need macOS 26.2+; write a JSON hostfile,
  run `mlx.distributed_config --hosts <a>,<b> --over thunderbolt --dot` to
  confirm the topology, then `mlx.launch --hostfile hosts.txt -n 2 <script>`.
  One direct Thunderbolt 5 cable between the two machines is enough for a
  2-node pair (JACCL requires a direct cable between every pair of nodes,
  which only matters once you go past 2).
- **A real published number, not a model:** Qwen3-235B-A22B over RDMA
  (Exo framework, comparable mechanism) measured **19.5 tok/s on one node,
  scaling to 31.9 tok/s across four** — sub-linear, because collective
  communication overhead eats into the gain. A 2-node pair would land
  somewhere between those, plausibly low-to-mid 20s tok/s — past the ~15-30
  tok/s "feels responsive" bar, but nowhere near a 4x speedup from doubling
  the boxes.

**The non-Mac alternative:** llama.cpp's RPC backend does the same job
(pipeline-splits model layers across machines) but works across *any*
hardware — a documented setup pairs a Mac Studio with a DGX Spark over direct
10-gigabit Ethernet, model weights only need to live on one machine. It's the
more flexible option (mix in a Strix Halo or a GPU box instead of a second
Mac) but 10GbE (~1.25 GB/s) is a fraction of Thunderbolt 5/JACCL's bandwidth,
so cross-node speed on a large model will be noticeably worse than the
all-Mac/JACCL path above. Worth it only if a second whole Mac isn't the
preferred spend.

## Hardware options — if the Studio's 64GB turns out to be the ceiling

| Option | Price (2026) | Memory | Bandwidth | Power: idle / load |
|---|---|---|---|---|
| *`beelink1` (owned, baseline)* | — | 16 GB UMA | ~80 GB/s | low (mobile APU) |
| *Mac Studio M4 Max (owned)* | — | 64 GB unified | 546 GB/s | ~30W / well under 200W |
| **AMD Strix Halo 128GB** (GMKtec EVO-X2, Framework Desktop, similar) | $2,600–4,000 | 128 GB unified | ~256 GB/s (spec) | ~32W / 55–140W (config TDP) |
| **NVIDIA DGX Spark** (GB10) | $3,999–4,699 | 128 GB unified | 273 GB/s | ~32–37W / up to 240W system |
| **Mac Studio M3 Ultra 96GB** | $5,299 / £5,299 (28-core CPU/60-core GPU/1TB base — checked 2026-08-16) | 96 GB unified | 819 GB/s | ~32–34W / well under 200W |
| ~~Mac Studio M3 Ultra 256/512GB~~ | **Not orderable from Apple as of 2026-08-16.** Apple pulled 512GB in March 2026 and has since dropped 256GB too — DRAM shortage. **96GB is currently the only M3 Ultra memory option Apple sells.** The $14,099/512GB figure earlier in this doc was correct when a prior search surfaced it but is now stale — corrected below. | — | 819 GB/s | — |
| **4× used RTX 3090** | $2,000–3,200 (cards) + ~$800–1,000 (PSU/board/case) | 96 GB VRAM (split, not unified) | 936 GB/s *per card* | ~1,000–1,200W under load |
| **2× RTX 5090** | $4,000–4,400 (cards) + ~$800–1,000 rest | 64 GB VRAM (split) | 1,792 GB/s *per card* | ~1,150W+ under load |

## What each can actually run, and how fast

| Model class | Active weight size (4-bit, approx) | `beelink1` | **M4 Max Studio (owned)** | Strix Halo | DGX Spark | M3 Ultra 96GB | 4×3090 |
|---|---|---|---|---|---|---|---|
| Qwen3-Coder-30B-A3B (3B active) | ~1.9 GB | ~43 tok/s (measured, llama.cpp) | **~130 tok/s (measured MLX, comparable Apple Silicon) — try this first** | ~100 tok/s (vendor-claimed) / (modeled: ~135) | (modeled: ~140) | ~130 tok/s (measured, MLX) | fast — VRAM bandwidth is overkill here, tensor-parallel overhead dominates instead |
| Dense 70B (e.g. Llama 3.3 70B) | ~44 GB | ✗ won't fit | (modeled: ~12, tight on 64GB with context) | 3.7–3.8 tok/s (measured, GMKtec) | ~2.7 tok/s (measured, batch) | (modeled: ~19) | fits across 4 cards, fast |
| Qwen3-235B-A22B-class MoE (22B active) | ~14 GB | ✗ won't fit | ✗ — 235B total won't fit in 64GB regardless of active params | ~11 tok/s (measured, GMKtec, heavily quantized to fit 128GB) | (modeled: ~19) | (modeled: ~58) | ✗ needs >96GB at reasonable quant |
| DeepSeek-V4-Flash (MoE, quantized to fit 128GB = IQ2, quality-degraded) | fits at 2-bit only | ✗ | ✗ (not enough capacity headroom for context too) | ✗ | 27–34 tok/s (community-measured, **but 2-bit — real quality loss**) | ✗ |
| DeepSeek-V3/R1-class 671B (37B active), proper 4-bit quality | ~400GB total, ~22GB active | ✗ | ✗ — doesn't fit | ✗ — doesn't fit | ✗ — 96GB doesn't fit either | ✗ |
| DeepSeek-V3/R1-class 671B, **512GB Mac only** | 448GB reported working set | — | — | — | **only the 512GB config**: fits, community-reported "under 200W," no explicit tok/s found — treat as usable-but-unverified until you've run it | — |

Rows with ✗ mean the model's total footprint (not just active weights)
doesn't fit that box's memory at a quality-preserving quant — this is the
capacity ceiling from the section above, separate from the speed question.

## Reading it

- **"Reasonable interactive rate" for an agentic coding loop is roughly
  15–30 tok/s single-stream** — enough that a multi-step tool-calling
  exchange doesn't feel like waiting on a modem. Below ~10 tok/s it's usable
  but you'll feel every step.
- On that bar, **every platform above is fine for the Qwen3-Coder-30B-A3B
  class** — it's the model that actually matches home hardware, not the
  671B one. That's the realistic Tier-2 target, not DeepSeek-V4 itself.
- **DeepSeek-V4/671B-class "fully local" is a Mac-Studio-512GB-only
  proposition**, and even then only one blog-level report exists, no hard
  tok/s number — this is the one row in the table you should not trust
  without running it yourself first.
- **Multi-GPU (3090s/5090s) wins on raw tok/s-per-dollar for models that fit
  in its VRAM**, but that VRAM (64–96GB, split across cards, not unified)
  is smaller than a single Strix Halo or DGX Spark box, and power draw is
  4–6× higher for a build that still can't touch the 671B tier. It only
  makes sense if the GPU horsepower has a second use (video/3D/other ML)
  beyond this.

## Running cost, not just purchase price

At UK domestic rates (~£0.28/kWh) and a generous 4 hours/day of active
inference:

| Option | Load power | Annual electricity (4h/day) |
|---|---|---|
| Strix Halo | ~90W avg | ~£37/yr |
| DGX Spark | ~150W avg | ~£61/yr |
| Mac Studio M3 Ultra | ~150W avg | ~£61/yr |
| 4× RTX 3090 | ~1,100W avg | ~£450/yr |

The purchase-price gap between Strix Halo and Mac Studio (~$2,500) buys
roughly a decade of the 3090 rig's electricity difference — running cost is
real but it's the multi-GPU build's problem, not a reason to avoid the
pricier unified-memory boxes.

## Bottom line

- **Zero-cost first move: run Qwen3-Coder-30B-A3B on the Mac Studio already
  owned.** 546 GB/s bandwidth, comfortable capacity headroom on 64GB, and
  measured MLX numbers on comparable Apple Silicon land around 130 tok/s —
  well past the 15–30 tok/s bar for a coding agent to feel responsive. Try
  this before buying anything on this page. Also worth testing:
  Qwen3-Coder-Next, which may fit at 4-bit with a smaller context window
  despite one source's claim that it needs 128GB (that capacity tier doesn't
  actually exist on M4 Max — only on M3 Ultra — so the real ceiling here is
  unverified until it's tried).
- **If more hardware is genuinely on the table, pair rather than replace:** a
  second Mac linked over Thunderbolt 5 (JACCL/MLX) pools memory with the
  existing Studio instead of stranding it. A second 64GB M4 Max, or the
  96GB M3 Ultra Apple currently sells ($5,299/£5,299), both land Qwen3-
  Coder-Next comfortably. **The actual DeepSeek-V4/671B-class pooling target
  is currently blocked at the new-purchase level** — Apple stopped selling
  M3 Ultra above 96GB in 2026 (DRAM shortage) — so that tier now depends on
  finding a pre-shortage 256GB/512GB unit secondhand, not a straightforward
  order. See "Pairing the Studio instead of replacing it" above.
- **If a Mac isn't the preferred next spend**, a Strix Halo 128GB box is the
  cheaper, hardware-agnostic option — $2,600–4,000, ~90W average, fits the
  existing `ai-mesh` node pattern (register it the same way as `beelink1`) —
  but it runs standalone (Strix Halo isn't part of the JACCL/Thunderbolt
  pooling trick), so it doesn't combine with the Studio's memory the way a
  second Mac would.
- **If the goal shifts to "actually run something DeepSeek-V4-shaped, not
  just Qwen3-Coder":** Mac Studio M3 Ultra 96GB — best bandwidth-per-watt of
  anything here, silent, ~£61/yr to run, but ~$5,300 up front and even then
  it's the 22B-active-param MoE tier, not the full 671B one.
- **512GB Mac Studio ($14k) only if the actual full-size DeepSeek model is
  the point**, not a smaller open-weight coding model — and go in expecting
  to be the one generating the first real tok/s number for that
  configuration, since the current sourcing doesn't have one.
- **Skip the multi-GPU rig for this specific goal** unless the raw compute
  has another job to do — power draw and build complexity buy you a VRAM
  pool still smaller than a single Strix Halo box, at 10x the running cost.
