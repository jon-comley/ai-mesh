# ai-mesh

A distributed, hardware-aware mesh for running LLM inference and home
automation across a set of heterogeneous machines — Linux, Windows, ARM
(Raspberry Pi), and macOS (Apple Silicon) — coordinated from a single Rust
workspace.

Point any OpenAI-SDK client at the coordinator and it routes requests to
whichever node actually has the right model loaded, with no manual
orchestration. The same coordinator drives natural-language home automation
(lighting, sensors, music, a DAW), exposed through a real-time web dashboard.

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
- **Live web dashboard** — a real-time, installable PWA (WebSocket-driven)
  covering nodes, health, models, home devices, security, and chat.
- **Music & DAW control** — natural-language playback control over a
  Spotify-backed player, and REAPER integration for audio/DAW work.
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

- **1,300+ tests** across the workspace
- Extensive `docs/` covering architecture, wire protocol, security design,
  and per-crate reference
- An adversarial chaos-test suite (`just chaos`) that actively tries to break
  the security model, not just exercise the happy path

## Workspace layout

```
shared/          Wire protocol, message types, HMAC signing
agent/           Runs on every node — hardware detection, heartbeats, inference
coordinator/     Central orchestrator — TLS server, registry, scheduler, dashboard
cli/             CLI + the adversarial chaos-test binary
capabilities/    Pluggable capabilities: llm, lighting, zigbee, music, voice, audio, art, reaper
frontend/        Real-time dashboard (PWA)
docs/            Architecture, protocol, security, and setup documentation
```

See [`docs/architecture.md`](docs/architecture.md) for the full design, and
[`docs/quickstart.md`](docs/quickstart.md) for day-to-day setup and the
`just` command reference.

## License

MIT — see [`LICENSE`](LICENSE).
