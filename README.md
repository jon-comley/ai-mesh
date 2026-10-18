# ai-mesh

A distributed, hardware-aware mesh for running LLM inference and home
automation across a set of heterogeneous machines — Linux, Windows, ARM
(Raspberry Pi), and macOS (Apple Silicon) — coordinated from a single Rust
workspace.

Point any OpenAI-SDK client at the coordinator and it routes requests to
whichever node actually has the right model loaded, with no manual
orchestration. The same coordinator drives natural-language home automation
(lighting, sensors, music, a DAW, voice), exposed through a real-time web
dashboard.

## What it does

- **Hardware-aware inference routing** — each node's agent reports its own
  hardware and loaded models; the coordinator schedules requests to whichever
  node can actually serve them, and auto-selects the best-fit model for a
  node's hardware.
- **OpenAI-compatible gateway** — any existing OpenAI-SDK client works
  unmodified, backed by local nodes or a cloud gateway as needed.
- **Natural-language home automation** — plain-English intents ("turn all
  lights off") are routed through the LLM and executed as real device
  commands (Zigbee lighting, sensors), with target validation before dispatch
  rather than a silent no-op on typos.
- **Live web dashboard** — a real-time, installable PWA (WebSocket-driven),
  organised into five categories on its bottom bar:
  - **Home:** rooms and lighting, Devices
  - **Chat**
  - **Music:** Songs, REAPER
  - **Tools:** Hunts, Reviews
  - **System:** Nodes, Health, Models, Online AI, Security, Errors

  New errors and security events put a dot on System.
- **Music, DAW & voice** — natural-language playback control over a
  Spotify-backed player, REAPER integration for audio/DAW work, and a Home
  Assistant Voice PE puck for speaking to the house.
- **Chord sheets to play along to** — search Ultimate Guitar for any song,
  switch between its versions, save favourites into a library (A–Z or by
  artist), and play with transpose, auto-scroll and YouTube or Spotify
  alongside ([`docs/chords.md`](docs/chords.md)).
- **Online AI, off by default** — forward a chat to a hosted model (OpenRouter,
  Anthropic, Groq, Gemini or any OpenAI-compatible endpoint) with the history
  compressed first. Tool calls still run locally, and a failed call falls back
  to local inference. `llm-rotation` tries free tiers before paid ones
  ([`docs/online-ai-gateway.md`](docs/online-ai-gateway.md)).
- **Code reviews on the mesh** — mac1 reviews GitHub repos on a schedule or on
  demand, and answers questions about their code, sharing the work across the
  mesh's machines using local models only. Home commands always come first
  ([`docs/code-review.md`](docs/code-review.md)).
- **eBay Hunts** — describe something you want (or paste a listing). An LLM
  suggests search terms, including the misspellings sellers use, and saved
  hunts search on a daily schedule. Matches arrive in a live feed and as a
  phone push via ntfy ([`docs/ebay-hunts.md`](docs/ebay-hunts.md)).
- **A real security model, not an afterthought** — TLS with trust-on-first-
  contact fingerprint pinning, a shared auth token with zero-downtime
  rotation, and HMAC-signed messages on top of that (closing the window where
  a leaked token alone would be enough to inject traffic). An adversarial
  chaos-test suite fires real attack scenarios at a live coordinator to prove
  it holds.
- **Cross-platform by design** — the same agent binary cross-compiles and
  runs as a proper OS service on Linux (systemd), Windows (NSSM), and macOS
  (launchd), plus native ARM64 builds for Raspberry Pi and Apple Silicon.

## Why

Off-the-shelf tools solve pieces of this (service meshes handle discovery,
various LLM servers handle inference), but nothing coordinates *inference
placement across genuinely mismatched home hardware* — a Pi, a gaming PC, a
laptop — while also driving real-world home automation from the same
control plane, over a wire protocol built to withstand disconnects, restarts,
and (adversarially tested) tampering rather than only good weather.

## Quality bar

- **1,500+ Rust tests** across the workspace, plus a Vitest suite for the
  dashboard's modules
- Extensive `docs/` covering architecture, wire protocol, security design,
  and per-crate reference
- An adversarial chaos-test suite (`just chaos`) that actively tries to break
  the security model, not just exercise the happy path

## Workspace layout

```
shared/          Wire protocol, message types, HMAC signing
agent/           Runs on every node — hardware detection, heartbeats, inference
coordinator/     Central orchestrator — TLS server, registry, scheduler, and the
                 dashboard itself (coordinator/src/http/static)
cli/             CLI + the adversarial chaos-test binary
capabilities/    Pluggable capabilities: core, llm, lighting, sensors, zigbee,
                 music, voice, audio, art, reaper, review
chords/          Ultimate Guitar chord sheets (Songs tab)
ebay/            eBay Browse API client and hunt scheduling (Hunts tab)
codereview/      Review logic: chunking, prompts, findings, reports (Reviews tab)
llm-rotation/    One OpenAI-compatible client across many hosted LLM providers
frontend/        Vitest unit tests for the dashboard's modules
nodes/           Per-machine settings for deploys (nodes/<name>.env)
systemd/         The coordinator's service unit
scripts/         Node installers and helper scripts behind the justfile
plans/           Design notes for features, before and as they were built
docs/            Architecture, protocol, security, and setup documentation
```

See [`docs/architecture.md`](docs/architecture.md) for the full design, and
[`docs/quickstart.md`](docs/quickstart.md) for day-to-day setup and the
`just` command reference. Outstanding work lives in [`ROADMAP.md`](ROADMAP.md);
finished work and the reasoning behind it is in [`HISTORY.md`](HISTORY.md).

## License

MIT or Apache-2.0, at your option: see [`LICENSE-MIT`](LICENSE-MIT) and
[`LICENSE-APACHE`](LICENSE-APACHE).
