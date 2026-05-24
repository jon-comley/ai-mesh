# Phase 11 — Web Dashboard & PWA

## Overview

A lightweight web dashboard embedded in the coordinator process. No separate service,
no Node.js build step. Serves a Progressive Web App (PWA) that works in any browser,
installs to mobile home screen, and can be wrapped with Capacitor for App Store submission
later without any code changes.

**Stack:** axum HTTP + WebSocket · vanilla HTML/JS · CSS grid (mobile-first) · PWA manifest + service worker

---

## Goals

- Observable mesh at a glance — no SSH required for day-to-day ops
- Real-time updates via WebSocket; DOM patched on each event
- Installable on Android/iOS as a PWA from day one
- Modular: each capability crate owns its own dashboard panel
- Auth: same `MESH_AUTH_TOKEN` as the wire protocol

---

## Coordinator File Layout

```
coordinator/src/
  http/
    mod.rs          # axum Router assembly, server startup
    routes.rs       # GET /, GET /static/*, GET /manifest.json, GET /service-worker.js
    ws.rs           # WebSocket upgrade, auth, broadcast fan-out
    auth.rs         # Bearer token extraction + validation
    api.rs          # POST /api/model/load|unload, GET /api/diag/:node, GET /api/security-report
    state.rs        # DashboardState (ring buffers, security counters, WS broadcast tx)
```

### HTTP Endpoints

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/` | Serve `index.html` |
| GET | `/static/*` | JS, CSS, icons (embedded via `include_str!`) |
| GET | `/manifest.json` | PWA manifest |
| GET | `/service-worker.js` | PWA service worker |
| GET | `/ws` | WebSocket upgrade — auth via `Authorization: Bearer <token>` |
| GET | `/api/security-report` | Snapshot of `PeerSecurityStats` per peer |
| GET | `/api/diag/:node` | Trigger `DiagRequest` → return last N log lines |
| POST | `/api/model/load` | Load a model on a node (body: `{node, model, size_mb}`) |
| POST | `/api/model/unload` | Unload a model from a node |

---

## DashboardModule Trait (extensibility)

Lives in `capability-core`. Each capability crate implements it to opt into dashboard presence.
The coordinator collects registered modules at startup and builds the nav dynamically — only
tabs for capabilities actually running on at least one node appear.

```rust
pub trait DashboardModule: Send + Sync {
    fn slug(&self) -> &'static str;           // "llm", "lighting" — used in JS dispatch
    fn nav_label(&self) -> &'static str;      // "Models", "Lighting"
    fn js_module(&self) -> &'static str;      // include_str!("dashboard/models.js")
    fn html_fragment(&self) -> &'static str;  // include_str!("dashboard/models.html")
}
```

**Capability → panel mapping:**

| Crate | slug | Nav label |
|-------|------|-----------|
| coordinator (built-in) | `health` | Health |
| coordinator (built-in) | `topology` | Nodes |
| capability-llm | `llm` | Models |
| capability-lighting | `lighting` | Lighting |
| capability-security | `security` | Security |

Adding a future capability = implement the trait + drop in a JS file. Coordinator shell unchanged.

---

## DashboardState

Owned by the coordinator alongside `Registry`. Passed into the HTTP module at startup.

```rust
pub struct DashboardState {
    pub health_ring: Arc<Mutex<HashMap<String, RingBuffer<HealthSample>>>>, // keyed by node_id
    pub error_feed: Arc<Mutex<VecDeque<ErrorEntry>>>,                       // capped at 500
    pub security_stats: Arc<Mutex<HashMap<SocketAddr, PeerSecurityStats>>>, // evicted on disconnect
    pub ws_tx: broadcast::Sender<DashboardEvent>,                           // fan-out to all WS clients
}
```

Ring buffer size: 300 samples per node (5 min at 1 Hz).

---

## DashboardEvent Enum

```rust
#[derive(Serialize, Clone)]
#[serde(tag = "type")]
pub enum DashboardEvent {
    TopologyUpdate { nodes: Vec<NodeRecordLite> },
    HealthUpdate { node: String, cpu: f32, gpu: f32, ram: f32, ts: u64 },
    ErrorFeedAppend { entry: ErrorEntry },
    SecurityUpdate { peer: String, stats: PeerSecurityStats },
    ModelStatus { node: String, loaded: Vec<ModelAllocationFull>, vram_mb: u64, ram_mb: u64 },
    DiagResponse { node: String, lines: Vec<String> },
    ModuleList { modules: Vec<ModuleInfo> },   // sent on WS connect so JS builds nav
}
```

Event frequency:

| Event | Trigger |
|-------|---------|
| `TopologyUpdate` | On heartbeat, node join, node leave |
| `HealthUpdate` | Every heartbeat — throttled to 1/sec per node |
| `ErrorFeedAppend` | Whenever coordinator logs a structured error |
| `SecurityUpdate` | On every `PeerSecurityStats` counter increment |
| `ModelStatus` | On model load/unload/heartbeat |
| `DiagResponse` | On demand via `/api/diag/:node` |
| `ModuleList` | Once, immediately after WS upgrade |

---

## Front-End File Layout

```
coordinator/src/http/static/
  index.html
  style.css
  dashboard.js       # WS loop, dispatch table, nav rendering
  topology.js        # node list / topology panel
  health.js          # sparkline health timeline
  errors.js          # error feed + diag inline expand
  diag.js            # diagnostic log renderer
  models.js          # load/unload UI + VRAM bars
  security.js        # HMAC failure table
  lighting.js        # device/group state (capability-lighting panel)
  icon-192.png
  icon-512.png
  manifest.json
  service-worker.js
```

Each JS module exposes:

```js
export function init(container) { ... }
export function handleEvent(evt) { ... }
```

The dispatch table in `dashboard.js` routes events by `evt.type` to the correct module.

---

## Mobile-First Layout

**Mobile (default):**
- Tab bar fixed at the bottom: Nodes · Health · Models · Lighting · Security · Errors
- One panel visible at a time (CSS `display: none` on inactive panels)
- Topology collapses to a vertical list of nodes with coloured health badges
- No SVG graph on mobile — too dense on small screens

**Desktop (min-width: 900px, CSS grid):**
- Left sidebar: node list with health badges
- Main area: topology SVG graph + health sparklines
- Right rail: error feed + security panel
- Bottom: model management

---

## PWA Components

### manifest.json

```json
{
  "name": "AI-Mesh Dashboard",
  "short_name": "MeshDash",
  "start_url": "/",
  "display": "standalone",
  "background_color": "#0d1117",
  "theme_color": "#0d1117",
  "icons": [
    { "src": "/static/icon-192.png", "sizes": "192x192", "type": "image/png" },
    { "src": "/static/icon-512.png", "sizes": "512x512", "type": "image/png" }
  ]
}
```

### service-worker.js

Cache-first for static assets; network-first for API/WS. Shows offline shell when
coordinator is unreachable.

```js
const CACHE = "mesh-v1";
const PRECACHE = ["/", "/static/style.css", "/static/dashboard.js",
                  "/static/topology.js", "/static/health.js",
                  "/static/errors.js", "/static/models.js",
                  "/static/security.js", "/static/lighting.js",
                  "/manifest.json"];

self.addEventListener("install", e =>
  e.waitUntil(caches.open(CACHE).then(c => c.addAll(PRECACHE))));

self.addEventListener("fetch", e => {
  if (e.request.url.includes("/ws") || e.request.url.includes("/api/")) return;
  e.respondWith(caches.match(e.request).then(r => r || fetch(e.request)));
});
```

---

## Security Model

- WebSocket upgrade requires `Authorization: Bearer <MESH_AUTH_TOKEN>` header
- Coordinator rejects upgrade (401) if token missing or wrong
- No login page for MVP — user sets token via env var or config; JS reads from
  `localStorage` (set once on first visit via a simple token-entry prompt)
- `PeerSecurityStats` tracked per `SocketAddr` in `DashboardState.security_stats`;
  evicted when connection closes cleanly

---

## Future: App Store (Capacitor)

When store submission is desired:

1. `npm init @capacitor/app` in the static folder
2. `npx cap add ios` + `npx cap add android`
3. Add a first-launch config screen for coordinator address (replaces hardcoded IP)
4. Submit — no changes to dashboard logic

Prerequisite: a domain / DDNS entry pointing at the coordinator (App Store reviewers
cannot access `192.168.1.x`).

---

## Implementation Phases

### Phase A — Axum shell + static file serving
- Add `axum` + `tokio` HTTP listener to coordinator (separate port, default 9001)
- Serve `index.html`, `style.css`, `manifest.json`, `service-worker.js` via `include_str!`
- Bare-bones `index.html` with tab bar (mobile-first CSS)
- Verify PWA install prompt appears on Android Chrome

### Phase B — WebSocket + DashboardEvent broadcaster
- `DashboardState` struct with `ws_tx: broadcast::Sender<DashboardEvent>`
- WS upgrade with Bearer token auth
- `coordinator/src/server.rs` pushes `TopologyUpdate` + `HealthUpdate` on every heartbeat
- JS WS loop + dispatch table wired up; `topology.js` renders node list

### Phase C — Health timeline
- `RingBuffer<HealthSample>` per node in `DashboardState`
- `HealthUpdate` events feed the ring buffer and push to WS
- `health.js` renders per-node sparklines (canvas)
- Send full ring buffer to new WS clients on connect (`HealthSnapshot` event)

### Phase D — Model management panel
- `capability-llm` implements `DashboardModule`
- `ModelStatus` events pushed on model load/unload
- `models.js` renders VRAM/RAM bars + load/unload buttons
- `POST /api/model/load` + `/api/model/unload` wired to existing coordinator logic

### Phase E — Error feed + diagnostic panel
- Structured `ErrorEntry` type; coordinator populates on inference failure, Zigbee disconnect, etc.
- `errors.js` appends rows; click → fetch `/api/diag/:node` → inline `<pre>` expand
- `DiagRequest` mesh message + agent handler (returns last 100 lines of stderr)

### Phase F — Lighting panel
- `capability-lighting` implements `DashboardModule`
- `lighting.js` shows device list, online/offline badges, last known state

### Phase G — Security panel
- `PeerSecurityStats` incremented in existing `FrameVerifyError` match arms
- `SecurityUpdate` events pushed on each increment
- `security.js` renders table: amber = stale frame, red = downgrade attempt

### Phase H — Polish + PWA icons + mobile layout pass
- Real icons (192 + 512 PNG)
- CSS grid desktop layout
- Offline shell ("Coordinator unreachable") in service worker
- `mesh security-report` CLI command

---

## Deferred

- Alert rules (node offline > 60s → webhook/email)
- Historical inference latency per model
- Live intent log (query, routed-to node, latency, response preview)
- Capacitor app store wrapper
