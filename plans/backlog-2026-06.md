# Backlog Audit & Implementation Plan — 2026-06-11

Full sweep of `docs/roadmap.md` deferred sections, review leftovers, and phase
backlog. Every item was verified against the current code; the roadmap has been
corrected in place where it lagged reality. This file is the working list.

## Open items, ordered by effort

| # | Task | Brief | Status | Est. |
|---|------|-------|--------|------|
| 1 | Sparkline fill opacity → CSS var | Replace hardcoded `fill-opacity="0.15"` in `health.js` with `var(--sparkline-fill, 0.15)` so themes can tune it. | **Done** eb8a9f5 | 5 min |
| 2 | SQLite WAL + synchronous=NORMAL | Two PRAGMAs at `Registry::open`. Reduces pi1 SD-card wear, improves write throughput. | **Done** 7e8600b | 15 min |
| 3 | Debounce map pruning | `capabilities/zigbee/src/client.rs:63` — completed debounce tasks leave dead `AbortHandle`s until the same device fires again. Sweep on insert or wrap with completion callback. | **Done** a3bd020 | 20 min |
| 4 | Per-node collapse/expand — Health panel | `▾/▸` per health card; collapsed shows last values only, no sparklines; `localStorage` persistence. Copy the room-card chevron pattern. | **Done** 4f25eee | 30 min |
| 5 | Room CT slider → `buildTempBar` | `rooms.js:499` uses generic `buildSlider`; switch to the warm→cool gradient `buildTempBar` from `lightcontrols.js` to match device cards. | **Done** 998338b | 30 min |
| 6 | Chat context token budget | Backend wiring done; remaining: truncate oldest turns (client `MAX_CONTEXT_TURNS` trim in `chat.js` + server-side guard in `handle_intent`) so long conversations don't overflow the prompt. Visible turn-count limit in UI. | **Done** c1e45d0 | 45 min |
| 7 | `bridge/event` subscription | Add `zigbee2mqtt/bridge/event` to the re-subscribe list in `client.rs` for live pairing announcements. Low priority until live-pair UX exists. | **Done** a3bd020 | 30 min |
| 8 | Log peer addr on oversized frame | Thread `SocketAddr` through `handle_connection` so `read_bounded_frame` rejections name the peer. Forensics only; roadmap rates it low value. | **Done** a3bd020 | 45 min |
| 9 | ProtectHome=true on pi1 unit | `MESH_STATE_DIR` env var in `state.rs` + `tls.rs`; unit sets `MESH_STATE_DIR=/var/lib/ai-mesh` + `StateDirectory=ai-mesh` + `ProtectHome=true`. | **Done** | 1 hr |
| 10 | Offline-device LLM suppression | Mark/suppress offline devices in the intent system prompt; return `device 'x' is currently offline` instead of generic unknown-target. UI half shipped in F-Lighting-UX. | **Done** 677c694 | 1–2 hrs |
| 11 | Layout opening-drag fix | Drag-to-place windows/doors from the sidebar popover unreliable — capture-phase `pointerdown` dismiss handler likely eats the drag start. `layout.js` `openOpeningPopover` / `makeMoveDraggable`. Needs a browser session. | Not started | 1–2 hrs |
| 12 | Deferred chaos scenarios | (1) token rotation mid-WS-session; (2) lagged broadcast receiver under heartbeat flood; (3) `Channel closed` arm in `ws.rs`. Called out post-Phase B as pre-ship requirements for Phase 11. | Not started | 2 hrs |
| 13 | Multi-GPU VRAM selection | `vramData`/`vramPct` use whichever GPU the agent reports; needs a device index once a multi-GPU node exists. Blocked on hardware + agent-side support. | Blocked | 2 hrs |
| 14 | Dashboard preferences persistence | `dashboard_preferences (user_id, key, value)` SQLite table; hybrid optimistic `localStorage` + async server sync. Homes collapse state, panel order, palette visibility. | Not started | 3 hrs |
| 15 | ZigbeeClient lifecycle hardening | `OnceCell` → `ArcSwap` + explicit `shutdown()`. Only matters when a second lighting node exists. | Blocked | 3 hrs |
| 16 | F8.2 — Photo Colour Picker | Client-side Canvas grab-box → average region colour → CIE xy → existing device command endpoint. Zero backend work. | Not started | 1 day |
| 17 | F8.1 — Telemetry Lighting effect | Wire inference + heartbeat events into the registered `Telemetry` effect stub (`tick()` currently returns empty). GPU activity → ambient pulse, node offline → red flash. | Not started | 1 day |
| 18 | Phase E — Error feed | Structured error log tab; `DashboardEvent::ErrorEntry` on inference fail / model-load fail / Zigbee disconnect. Foundation for the diagnostic panel. | Not started | 1 day |
| 19 | Phase G — Security panel | `PeerSecurityStats` counters keyed by `SocketAddr`, `DashboardEvent::SecurityIncident`, dashboard table + `mesh security-report`. Fully spec'd in roadmap. | Not started | 1–2 days |
| 20 | F7 — Switches capability | New crate: Z2M button/remote/motion/contact events → `SwitchEvent` → `DashboardEvent::SwitchEvent` + activity feed. Prerequisite for Reactive Graph and Hot/Cold game. | Not started | 2–3 days |
| 21 | F-Spatial Phase C — Three.js 3D | SVG canvas → Three.js scene, ortho/perspective toggle, `THREE.Shape` rooms, emissive fixture spheres tracking live state. Dimensions schema already landed in Phase E. | Not started | 3–5 days |

## Recommended next slice

Items #7, #8, #9 are done. #11 needs a browser session (WSL2 limitation). #14 is the next substantial feature.

Items 13 and 15 are blocked on hardware/topology and should not be started.

## Deferred from review (2026-06-11, model-load hardening)

- **Serialize concurrent `ModelLoad`s** — `capabilities/llm/src/lib.rs:70` spawns a task per
  `ModelLoad` with no serialization; an overlapping load's `kill_existing` swaps the tracked
  child under the first load's health loop (it then polls the wrong server). Pre-existing,
  near-unreachable with one llama-server per node. Fix: a load mutex so the latest request
  waits (or cancels the prior via the existing `UNLOAD_REQUESTED` path). ~1 hr.
- **Test seam for `pull_model` lifecycle** — the exited-child, timeout-kill, and
  unload-abort paths are untested because the loop drives a real process spawn + real HTTP.
  Needs a process/health-probe abstraction to unit-test with a fake clock. Only worth it if
  this code keeps changing. ~2–3 hrs.

## Also shipped (2026-06-11/12, from review findings)

- **`health_timeout_secs` cap at 900 s** — `capabilities/llm/src/llama.rs` a2f0c2c
