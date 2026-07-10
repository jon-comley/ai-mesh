# Library Extraction Plan

Status: planning only — no extraction has started. Decision of 2026-07-10: keep this
document current, extract nothing until the Tier 1 crate below is worth launching
properly.

---

## 1. Goal

The point of extracting libraries is **public exposure**, not code hygiene. Exposure is
what can eventually convert into income (reputation → contract work; sponsorship exists
but is rare). That ordering drives every decision below:

- A crate only ships when it can ship **with launch content**: a README with a
  30-second example, a demo recording, and a field-notes writeup posted where the
  audience lives (r/rust, r/LocalLLaMA, HN Show).
- One polished crate beats seven skeletal ones. Publishing to crates.io without a
  launch yields near-zero visibility.
- Anything that can't plausibly attract users as a *library* gets published as
  *writing* instead — a design post costs a fraction of a maintained crate and earns
  comparable exposure.

An earlier draft of this plan proposed seven crates (`mesh-runtime`, `mesh-effects`,
`mesh-gateway`, `mesh-dashboard`, `mesh-wire`, plus the two below). It was written
without reading the code. Most of those seven are not extractable today; see §5.

---

## 2. Tier 1 — llama-server orchestration (extract when ready)

**Source:** `capabilities/llm/src/llama.rs` (~1,200 lines). The most differentiated
code in the repo: everything you learn the hard way running llama.cpp's `llama-server`
unattended on heterogeneous consumer hardware.

What the crate would offer:

- **Curated GGUF registry with escape hatch** — a vetted name → (HF repo, filename,
  size) table for common models, plus `hf:<org>/<repo>:<filename.gguf>` passthrough for
  anything else, so the registry never has to chase the long tail.
- **Process lifecycle** — spawn, health-gate, restart, and tear down `llama-server`;
  reap orphaned listeners by scanning `/proc/net/tcp{,6}` directly (no lsof/fuser
  needed on minimal nodes).
- **Flash-attention heuristics** — default `--flash-attn auto` with a documented
  workaround for Gemma-3, which hangs on load when FA is forced on.
- **Size-scaled health timeouts** — 180 s floor, scaled with model size so a 32 B load
  on slow storage doesn't get killed while a 0.5 B failure is detected fast; early exit
  when the child process dies (OOM, corrupt GGUF) instead of burning the timeout.
- **Streaming with per-chunk liveness** — no total request timeout on generation;
  instead an idle timeout between SSE chunks, so long generations survive but a hung
  server is detected quickly.
- **Per-model CoT suppression** — e.g. injecting Qwen's `/no_think` control token when
  chain-of-thought output isn't wanted.

**Coupling to cut (verified, small):** `llama.rs` imports only
`shared::{ChatTurn, ChatRole}` and `shared::sse::{SseParser, parse_openai_chunk}` from
the mesh. The new crate defines its own chat-turn types and vendors the small SSE
parser. Everything else in the file is already standalone.

**What stays behind:** `capabilities/llm/src/lib.rs` carries all the real mesh coupling
(`InferenceRequest`, `MeshMessage`, heartbeats). After extraction it becomes a thin
adapter over the published crate — one code path, no fork.

---

## 3. Tier 2 — hardware / VRAM detection

**Source:** `agent/src/hardware.rs`, `agent/src/gpu.rs`, `shared/src/hardware.rs`.

Cross-platform GPU/VRAM detection with some genuinely uncommon coverage:

- AMD APU unified-memory VRAM via sysfs `mem_info_vis_vram_{used,total}` — the UMA case
  most tools get wrong.
- Windows VRAM and GPU utilisation via a single-sample `Get-Counter` PowerShell call
  (three counter paths in one one-second window).
- NVIDIA via `nvidia-smi` as the authoritative fast path.

**Blocker before extraction:** the types live in `shared` and the probes live in
`agent`; the crate needs a clean types + detection split first. Smaller audience than
Tier 1, so it goes second — and only if Tier 1 shows the launch playbook works.

---

## 4. Published as writing, not crates

- **Wire protocol** (`shared/src/{frame,messages,tls}.rs`) — HMAC-signed frames, HKDF
  key derivation, TLS setup. A solo hand-rolled security protocol will not attract
  dependents, and *should not*: nobody should depend on unaudited crypto plumbing. But
  the design is a good story. `docs/messages.md` and `docs/phase10-security.md` are
  already most of a blog post.
- **ai-mesh as a whole** — the system (mesh of heterogeneous consumer boxes running
  local AI, lighting, audio) is a showcase, not a library. It earns exposure as the
  demo that the extracted crates link back to.

---

## 5. Deferred, with reasons

`mesh-runtime`, `mesh-effects`, `mesh-gateway`, `mesh-dashboard` — all proposed by the
earlier draft, all deferred:

- They are fused inside the ~35 k-line `coordinator` crate, sharing the
  `DashboardState` and SQLite `Registry` god-objects. The worst knot is
  `intent.rs` ↔ `http/state.rs` ↔ `registry/mod.rs` ↔ `server.rs` (`Connections`).
  Extracting any of them means a major decomposition of the coordinator first.
- Even extracted, they compete with entrenched projects (exo, LiteLLM, ollama, Home
  Assistant) without a differentiator strong enough to overcome the incumbents'
  network effects.

Revisit only on a concrete demand signal (someone asks for it), never speculatively.

---

## 6. Publishing mechanics

When a crate ships (Tier 1 first):

1. **Own GitHub repo** per crate — not a workspace member published in place. The repo
   is the landing page; issues/stars accrue to the crate, not to ai-mesh.
2. Publish to **crates.io** with docs.rs docs building clean; SemVer from day one
   (0.x while the API settles).
3. ai-mesh consumes the crate **via crates.io** — the workspace path dependency is
   dropped in the same change that adopts the published version, so there is never a
   dual-path period.
4. CI in the crate repo: build + test on Linux at minimum; Windows too for Tier 2
   (its value is the cross-platform probes).

---

## 7. Exposure playbook (per crate)

1. **README** — what it does in one sentence, a 30-second copy-paste example, a table
   of the hard-won behaviours (the Gemma-3 hang workaround, UMA VRAM, CoT suppression).
2. **Demo recording** — short terminal capture of the crate doing the thing.
3. **Field-notes writeup** — mined from the code's comments: "what I learned running
   llama-server unattended on mixed consumer hardware". The writeup is the actual
   exposure vehicle; the crate is the proof.
4. **Post** to r/rust (crate announcement), r/LocalLLaMA (the field notes), HN Show.
5. **ai-mesh README pass** — when the first crate ships, ai-mesh's own README gets
   rewritten for a library audience: what it is, what was extracted, links out.

---

## 8. Honest expectations

- Exposure → income is indirect and slow: visible useful work → reputation →
  inbound contract/consulting interest. Sponsorship on a niche crate is a rounding
  error.
- A launch that lands buys attention for days, not months; the compounding asset is
  the sequence of launches and writeups, not any single one.
- No "people will just depend on it" assumptions. Every dependent is earned by the
  README, the docs, and the writeup.