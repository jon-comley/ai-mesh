# eBay Bargain Finder ("Hunts")

## Status (2026-07-12)

**Shipped.** All 9 implementation-order steps landed in one session: the
`ebay` crate (client/schedule/diff/ntfy + unit tests), registry persistence
(`ebay_hunts`/`ebay_seen_listings`/`ebay_finds`), the coordinator HTTP API
(analyze/CRUD/run-now/finds/config), the per-hunt background timer with
startup re-arm, and the Hunts dashboard tab (`ebay.js`). Full workspace test
suite (1234 tests) and `cargo clippy --workspace -- -D warnings` both clean.
Verified live against a running coordinator:
config round-trip, hunt CRUD, `run-now` (genuinely round-tripped to eBay's
OAuth endpoint), `analyze` validation, and static asset/tab serving. Two
review passes (one human-in-the-loop with two independent LLM reviewers)
turned up one real fix — a stale per-hunt generation-counter map entry that
never got cleaned up on delete — applied and tested; everything else raised
was already handled in the code or based on lines that didn't exist in the
diff. User-facing setup doc: `docs/ebay-hunts.md`.

Not yet built (intentionally deferred, not blocking): pruning old
`ebay_seen_listings` rows for a very long-running hunt (low urgency — see
doc's troubleshooting section), and the editor's client-computed "next run"
label.

## Context

The user wants to paste an eBay listing URL for something they want, have an LLM
turn it into good search terms plus the classic misspelling/mis-category bargain
terms, attach one or more daily timeslots, and have the coordinator periodically
search eBay with those terms, flag anything that looks like a bargain, and push a
phone notification. This adds a new "Hunts" tab to the existing dashboard.

Decisions locked in with the user:
- **UI**: Deal ticker — a reverse-chronological feed of finds is the primary
  surface. Saved hunts live in a slim sidebar; each hunt's daily timeslots are
  edited on a 24h tap-to-toggle strip. Hunt creation flow: paste URL → LLM
  analysis → editable/toggleable term chips → timeslot strip → save.
- **eBay data**: official eBay Developer APIs (Browse API, OAuth2
  client-credentials — no user consent flow needed since Browse API's
  `item_summary/search` and item-by-legacy-id lookups are public-data,
  app-token endpoints).
- **LLM**: the existing cloud gateway (`coordinator/src/cloud.rs`
  `GatewayConfig`/`OpenAiCompatProvider`), same path `api/chat.rs` uses — for
  both the one-shot term-generation step and the recurring bargain-verdict step.
- **Notifications**: dashboard WS feed + toast (existing bus) plus a push to
  ntfy.sh (or self-hosted ntfy) via a plain `reqwest` POST.
- **Crate**: a new workspace member, `ebay/` (package `ebay`), holding the eBay
  client, ntfy sender, and pure scheduling/diff logic. Not an agent-side
  `capabilities/*` crate and no new `Feature` variant — this is coordinator-only,
  same shape as the Frame-TV art feature (`coordinator/src/http/api/art.rs`).

## Reference patterns (verified in the repo)

- **Coordinator-hosted external-API feature with a background timer**:
  `coordinator/src/http/api/art.rs`. `spawn_art_rotation_timer` (~line 421) is a
  `tokio::spawn` loop with a generation counter so a superseded timer
  self-cancels instead of fighting a newer one. Hunts need the same shape but
  keyed by hunt id, and must **re-arm at coordinator startup** from persisted
  DB rows (art's timer doesn't persist across restarts — hunts must, since the
  whole point is "check for me while I'm not looking").
- **LLM round-trip**: `coordinator/src/http/api/chat.rs` builds an
  `IntentRequest`/uses the gateway. For Hunts, term-generation is a **one-shot
  structured prompt**, not the tool-calling intent pipeline — model this on
  `art.rs::curate_with_llm` instead: build a `GatewayConfig::load(&registry)`,
  get a provider via `cfg.provider()`, call `provider.complete(&[ChatTurn::user(prompt)], temperature)`,
  and parse JSON out of the reply defensively (see `parse_curated_indices` for
  the "find the first `[...]`/`{...}` substring" trick — small/free models
  wrap JSON in prose despite instructions). Fall back gracefully when
  `cfg.is_configured()` is false (heuristic term generation: just the title,
  no misspellings) exactly like art falls back to raw order when no LLM is
  ready.
- **HTTP client house style**: `capabilities/music/src/web_api.rs`
  `SpotifyClient` — `reqwest::Client`, `Mutex<Option<CachedToken>>` for OAuth
  token caching, a generic private `call()`, typed wrappers, a small `ApiError`
  enum with human-readable messages. eBay's client-credentials flow is
  simpler than Spotify's refresh-token flow (no user auth step, no rotating
  refresh token) — same shape, less state.
- **WS broadcast bus**: `DashboardState.tx: broadcast::Sender<DashboardEvent>`
  (`coordinator/src/http/state.rs:447`/`:80`), `push_*` helper methods on
  `DashboardState`, consumed in `coordinator/src/http/ws.rs`. Browser side:
  `dashboard.js`'s `handlers` map (~line 68) dispatches by `evt.type`; each tab
  module registers itself there. Toasts: `static/util.js::showToast`.
- **Persistence**: rusqlite `Registry` (`coordinator/src/registry/mod.rs`),
  `init_schema` (~line 234) has an established list of
  `CREATE TABLE IF NOT EXISTS` migrations (nodes, rooms, scenes,
  dashboard_preferences, switch_bindings, ...) — add three more the same way.
- **Tab wiring**: `coordinator/src/http/static/index.html` has
  `<button class="tab" data-panel="x">` + `<section class="panel" id="panel-x">`;
  empty-section tabs (chat, reaper, gateway) are rendered by a JS module's
  `init(panel)`, wired at the bottom of `dashboard.js` alongside the imports at
  the top (`import * as gateway from '/static/gateway.js'` etc.). Assets are
  `include_str!` consts + an `ASSETS` table row in
  `coordinator/src/http/mod.rs::static_asset_routes()` (~line 231/269).
- **API routes**: handlers live in `coordinator/src/http/api/<name>.rs`,
  declared in `coordinator/src/http/api/mod.rs`, routed in
  `coordinator/src/http/mod.rs::router()`. Every handler takes `_: Authed`
  (`coordinator/src/http/auth.rs`) unless there's a documented reason not to
  (there isn't one here).
- **Secrets/deploy**: gateway API keys and Spotify creds are handled two
  different ways in this repo — gateway keys live in `dashboard_preferences`
  (settable from a UI tab, no deploy step); Spotify uses a `just spotify-auth`
  one-time OAuth helper bin (`capabilities/music/src/bin/spotify_auth.rs`) plus
  a creds-push deploy step. eBay client-credentials needs neither OAuth dance
  nor a bin — it's just an app ID + cert ID (client secret). **Store eBay
  `client_id`/`client_secret` and the ntfy topic URL in `dashboard_preferences`**
  (new namespace, e.g. `__ebay__`), following `GatewayConfig::load`'s
  pref-with-env-fallback pattern — this avoids a new deploy recipe entirely and
  lets the user configure it from a small settings block in the Hunts tab
  itself, matching how the Gateway tab configures its own API key.

## New crate: `ebay/`

Workspace member `ebay` (add to root `Cargo.toml` `[workspace] members`).
Dependencies: `reqwest` (json, rustls-tls), `serde`, `serde_json`, `tokio`,
`chrono` (for timeslot math — check if already a workspace dep; if not,
time-of-day math can be done with `std::time` + a small day-seconds helper to
avoid adding a new dependency — prefer that if straightforward).

Modules:
- `ebay/src/lib.rs` — re-exports.
- `ebay/src/client.rs` — `EbayClient` (client-credentials OAuth like
  `SpotifyClient` but simpler: `POST https://api.ebay.com/identity/v1/oauth2/token`
  with `grant_type=client_credentials&scope=https://api.ebay.com/oauth/api_scope`,
  Basic auth of `client_id:client_secret`, cache token with expiry same as
  Spotify's `CachedToken`). Methods:
  - `async fn search(&self, terms: &[String], marketplace: &str) -> Result<Vec<Listing>, EbayError>`
    — calls Browse API `GET /buy/browse/v1/item_summary/search?q=<terms joined>`
    with header `X-EBAY-C-MARKETPLACE-ID: EBAY_GB` (or configurable).
  - `async fn lookup_item(&self, url: &str) -> Result<ItemDetail, EbayError>` — parse
    the legacy item id out of the pasted URL. Handle both path forms
    (`.../itm/<title-slug>/<id>`, `.../itm/<id>`) **and** the query-string form
    eBay also emits (`...?hash=item<id>:g:<rest>` — extract the digits between
    `item` and `:`), then call Browse API's
    `GET /buy/browse/v1/item/get_item_by_legacy_id?legacy_item_id=<id>` to get
    title/category/price/image/condition for the LLM prompt. If the URL can't
    be parsed, return an error the handler surfaces as 400 — do not fall back
    to scraping.
  - `Listing { item_id, title, price, currency, image_url, item_web_url, condition }`
    — `condition` (e.g. "For parts or not working", "Used", "New") is carried
    through to the bargain-verdict prompt so the LLM doesn't flag a
    suspiciously-cheap "for parts" listing as a steal.
  - `EbayError` enum mirroring `ApiError` (NotConfigured, Unauthorized, Other(String)).
- `ebay/src/ntfy.rs` — `async fn send_ntfy(topic_url: &str, title: &str, body: &str, click_url: Option<&str>) -> Result<(), String>`
  — one `reqwest::Client::post(topic_url).header("Title", title).header("Click", click_url).body(body)`.
  ntfy's HTTP contract is simple enough not to need a struct.
- `ebay/src/schedule.rs` — pure, unit-testable:
  - `Timeslot { hour: u8, minute: u8 }` (or minutes-since-midnight `u16`,
    simpler to serialize/compare — prefer this).
  - `fn next_wakeup(now_secs_since_midnight: u32, slots: &[u16]) -> Duration` —
    returns time until the next slot today, or the earliest slot tomorrow if
    all of today's have passed. Takes "now" as a param (not `SystemTime::now()`
    directly) so it's trivially unit-testable.
  - A thin wrapper used only by the coordinator's spawn loop that supplies
    real wall-clock time via `chrono::Local::now()` (**local** time, not
    UTC) so the OS timezone database handles BST/GMT transitions correctly —
    a naive UTC-based wrapper would silently drift hunts by an hour twice a
    year. The pure function itself stays UTC-agnostic and deterministic;
    only the wrapper cares about the zone.
- `ebay/src/diff.rs` — `fn new_listings(seen_ids: &HashSet<String>, results: &[Listing]) -> Vec<Listing>`
  — filters out already-seen item ids. Trivial but keeping it here keeps the
  coordinator handler thin and this function unit-testable in isolation.
- `ebay/src/lib.rs` also defines `HuntSpec { id, name, source_url, terms: Vec<TermEntry>, timeslots: Vec<u16>, marketplace, enabled }`
  and `TermEntry { text: String, enabled: bool, is_misspelling: bool }` as the
  shared shape between DB rows, API JSON, and the timer loop.

Unit tests in this crate: `next_wakeup` (today-slot-remaining, all-slots-passed
→ tomorrow, empty slots, exact-boundary), `new_listings` diffing, URL→item-id
parsing (`.../itm/foo-bar/123456789012`, `.../itm/123456789012`, malformed
URL → None). No network in these tests — `EbayClient`/`ntfy` need a live
integration smoke test instead (see Verification).

## Registry schema (`coordinator/src/registry/mod.rs`)

Add to `init_schema`, following the existing `CREATE TABLE IF NOT EXISTS` style:

```sql
CREATE TABLE IF NOT EXISTS ebay_hunts (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    source_url TEXT NOT NULL,
    terms_json TEXT NOT NULL,      -- Vec<TermEntry> as JSON
    timeslots_json TEXT NOT NULL,  -- Vec<u16> minutes-since-midnight, as JSON
    marketplace TEXT NOT NULL DEFAULT 'EBAY_GB',
    enabled INTEGER NOT NULL DEFAULT 1,
    created_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ebay_seen_listings (
    hunt_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    first_seen_ms INTEGER NOT NULL,
    PRIMARY KEY (hunt_id, item_id)
);
CREATE TABLE IF NOT EXISTS ebay_finds (
    id TEXT PRIMARY KEY,
    hunt_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    title TEXT NOT NULL,
    price_minor INTEGER,
    currency TEXT,
    image_url TEXT,
    item_web_url TEXT NOT NULL,
    matched_term TEXT NOT NULL,
    verdict TEXT,          -- LLM's bargain reasoning, nullable (heuristic-only mode, or item omitted from the LLM's reply)
    found_ms INTEGER NOT NULL,
    reviewed INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_ebay_finds_hunt_found ON ebay_finds(hunt_id, found_ms);
```

`Registry` accessor methods (new `coordinator/src/registry/ebay.rs` submodule,
following how other domains split out — check whether `rooms`/`scenes` live in
submodules or inline in `mod.rs` and match that): `list_hunts`, `get_hunt`,
`create_hunt`, `update_hunt`, `delete_hunt`, `mark_listing_seen`,
`has_seen_listing`, `insert_find`, `list_finds(limit)`, `mark_find_reviewed`.

## Coordinator wiring

- `ebay/` crate becomes a coordinator dependency in `coordinator/Cargo.toml`.
- New `coordinator/src/http/api/ebay.rs`, declared in `api/mod.rs`, routed in
  `http/mod.rs`:
  - `POST /api/ebay/analyze` — body `{ url }`. Calls `EbayClient::lookup_item`,
    then the one-shot LLM term-generation prompt, returns
    `{ item_id, title, terms: [{text, enabled, is_misspelling}], marketplace }`
    (the parsed `item_id` included for the frontend to display/debug with) for
    the frontend to render as editable chips. Does not persist anything.
  - `POST /api/ebay/hunts` — create a hunt from `{name, source_url, terms, timeslots, marketplace}`;
    persists, arms its timer, returns the created `HuntSpec`.
  - `GET /api/ebay/hunts` — list hunts (for the sidebar).
  - `PATCH /api/ebay/hunts/{id}` — update terms/timeslots/enabled; re-arms or
    cancels the timer as needed (bump a per-hunt generation counter, same
    self-cancel trick as art's rotation timer).
  - `DELETE /api/ebay/hunts/{id}` — delete + cancel timer.
  - `POST /api/ebay/hunts/{id}/run-now` — trigger one search cycle immediately
    (manual "check now" button), reusing the same run-cycle function the timer
    calls.
  - `GET /api/ebay/finds?since=&limit=&hunt_id=` — feed for the ticker;
    `hunt_id` optional, filters to one hunt's history.
  - `POST /api/ebay/finds/{id}/reviewed` — mark as seen/dismissed in the UI.
  - `GET/POST /api/ebay/config` — read/write eBay client_id/secret + ntfy topic
    (stored via `dashboard_preferences` under a new `__ebay__` namespace,
    mirroring `gateway.rs`'s pattern for its own settings tab; secret values
    masked on read the same way `GatewayConfig::key_hint` masks the API key).
- **Background timers**: one `tokio::spawn` loop per enabled hunt, each with a
  `generation: u64` cell (an `Arc<AtomicU64>` per hunt, or a shared map on
  `DashboardState` keyed by hunt id — simplest: a `HashMap<String, Arc<AtomicU64>>`
  guarded by a `Mutex` on `DashboardState`, incremented on any update/delete so
  a stale loop iteration no-ops and exits). Loop body: sleep until
  `schedule::next_wakeup(...)` (local-time wrapper), run the search — on a
  `429`/rate-limited response, `tracing::warn!` and skip this cycle rather
  than retrying immediately, letting the normal next-timeslot sleep act as
  backoff — diff against `ebay_seen_listings` for new listings. For each new
  listing: LLM bargain-verdict via the **cloud gateway** (same
  `GatewayConfig`/`OpenAiCompatProvider` path as term-generation — batch all
  new listings from this cycle into one prompt, including price + title +
  **condition** (to stop a "for parts/not working" listing's low price
  reading as a steal) + the hunt's original target item context, ask for a
  JSON array of `{item_id, is_bargain, reason}`). Any item the LLM's reply
  omits is inserted into `ebay_finds` with `verdict = NULL` rather than
  dropped — still visible in the ticker, just unjudged. Insert all new
  listings into `ebay_finds`, `push_ebay_find` over the WS bus, and if
  `is_bargain` (or LLM unconfigured/omitted → heuristic: any new match at all)
  POST to ntfy. Mark all new listings seen regardless of verdict, so they're
  not re-evaluated next cycle.
  - **Heuristic fallback** (gateway not configured, request failed, or item
    omitted from the reply): flag as interesting rather than silently doing
    nothing — a user who hasn't set up a cloud key still gets the core
    "notify me on new matches" behavior, just without bargain-vs-not
    judgement. Note this fallback explicitly in the UI (e.g. a small "AI
    verdicts off — showing all new matches" note) so it's not a silent
    quality drop.
  - **Startup re-arming**: in `coordinator/src/main.rs` (wherever
    `DashboardState`/`Registry` are constructed and other startup spawns
    happen, e.g. near the `effects::runner` spawn), after loading the
    registry, iterate `registry.list_hunts()` for `enabled` ones and spawn
    their timers — this is the one piece with no art-timer precedent, since
    art's rotation is deliberately session-only.
- `DashboardState` (`coordinator/src/http/state.rs`): add
  `DashboardEvent::EbayFind { hunt_id, hunt_name, find: EbayFindInfo }` variant
  and a `push_ebay_find(&self, ...)` method (mirrors existing `push_*`
  patterns e.g. `push_gateway_update`/`push_join_event`). Add the hunt-timer
  generation map as a new field on `DashboardState`.

## Frontend

- `index.html`: add `<button class="tab" data-panel="ebay">Hunts</button>` and
  `<section class="panel" id="panel-ebay"></section>`.
- New `coordinator/src/http/static/ebay.js`, `include_str!`'d as `EBAY_JS` +
  an `ASSETS` row in `mod.rs`, imported in `dashboard.js`
  (`import * as ebay from '/static/ebay.js'`), `ebay.init(panel)` called
  alongside `chat.init`/`reaper.init`/`gateway.init`, and a
  `EbayFind: evt => ebay.handleFind(evt)` entry added to the `handlers` map.
  Structure inside `ebay.js`:
  - Ticker: list of find cards (thumbnail, title, price, hunt name, link out,
    dismiss/mark-reviewed button), newest first, populated from
    `GET /api/ebay/finds` on init and prepended-to live via the WS handler;
    `showToast` (from `util.js`) on a live bargain find while the tab isn't
    active.
  - Sidebar: hunt list from `GET /api/ebay/hunts`, each row togglable
    (enabled/disabled) and clickable to open its editor.
  - Hunt editor: URL input → `POST /api/ebay/analyze` → render returned terms
    as removable/toggleable chips (plus an "add term" free-text input) → a 24h
    strip (24 or 48 tappable cells for hour/half-hour granularity) that
    toggles timeslots on/off, reflecting/mutating a `Set<number>` of
    minutes-since-midnight → Save button hits `POST /api/ebay/hunts` (create)
    or `PATCH /api/ebay/hunts/{id}` (edit). Once timeslots are set, show a
    client-computed "Next run: HH:MM" label (pure JS date math over the
    selected slots against the browser's local clock) — no new endpoint
    needed for this.
  - Small settings block (or reuse the existing `gateway.js` settings-panel
    convention) for eBay client_id/secret + ntfy topic, backed by
    `GET/POST /api/ebay/config`.

## Config / credentials

No new deploy recipe needed — eBay client_id/secret and the ntfy topic are
entered once via the Hunts tab's settings block and persisted in
`dashboard_preferences`, same operational model as the Gateway tab's API key
(survives redeploys since the SQLite DB isn't part of the deployed binary).
Env-var fallback (`EBAY_CLIENT_ID`, `EBAY_CLIENT_SECRET`, `NTFY_TOPIC_URL`) for
headless bring-up, matching `GatewayConfig::load`'s pref-then-env pattern.

## Implementation order (each step should compile/pass tests on its own)

1. `ebay/` crate: `schedule.rs` + `diff.rs` + tests (no network, no coordinator
   dependency yet). Add to workspace members.
2. `ebay/src/client.rs` (`EbayClient`, `Listing`, `EbayError`, URL parsing +
   its unit tests) and `ebay/src/ntfy.rs`.
3. Registry: `ebay_hunts`/`ebay_seen_listings`/`ebay_finds` tables in
   `init_schema` + accessor methods + their own tests (in-memory `Registry::new()`
   pattern already used elsewhere in that file).
4. `coordinator/src/http/state.rs`: `DashboardEvent::EbayFind` variant,
   `push_ebay_find`, hunt-timer generation map field.
5. `coordinator/src/http/api/ebay.rs`: CRUD + analyze + config handlers, plus
   the run-cycle as one directly-callable async fn (used by both `run-now` and
   the timer, written once). Exercise it against a fake/mock `EbayClient`
   (trait-object or a test-only stub returning canned `Listing`s) before
   wiring any real timer, so the diff → verdict → insert → push logic is
   proven independent of network timing and eBay's actual API.
6. Wire routes into `api/mod.rs` + `http/mod.rs::router()`.
7. Background timer spawn (per-hunt loop using the proven run-cycle fn +
   startup re-arm in `main.rs`).
8. Frontend: `ebay.js`, `index.html` tab, `dashboard.js` wiring, `mod.rs`
   asset entry.
9. End-to-end pass: create a hunt via curl, confirm a search cycle runs,
   confirm a WS event and (if configured) an ntfy push fire.

## Verification

- `cargo test -p ebay` for the pure logic (`next_wakeup`, `new_listings`, URL
  parsing).
- `cargo test -p coordinator` for the new registry methods and handler
  validation-path tests (400/503 cases), following `art.rs`'s test style —
  the *live* eBay/ntfy happy path is explicitly not unit-tested, same
  precedent as `search_art`'s live Met-API call.
- No browser available in this WSL2 environment (per existing project note) —
  drive the REST contract with `curl` against the running coordinator:
  `POST /api/ebay/analyze` with a real eBay item URL, `POST /api/ebay/hunts`,
  `POST /api/ebay/hunts/{id}/run-now`, `GET /api/ebay/finds`, and watch the
  coordinator logs / `wscat` the `/ws` endpoint for the `EbayFind` event.
- `cargo clippy --workspace -- -D warnings` (pre-commit hook enforces this
  anyway).

## Open risks / questions to confirm before/while building

- **eBay production keys**: Browse API needs a production (not sandbox) app
  registered at developer.ebay.com, with `X-EBAY-C-MARKETPLACE-ID` set
  correctly (e.g. `EBAY_GB` for UK) — sandbox data is fake/useless for real
  bargain-hunting, so this must be a production keyset from the start.
- **Rate limits**: Browse API's free tier has a daily call cap (varies by
  applied-for limit, commonly 5,000/day) — with multiple hunts × multiple
  timeslots this is very unlikely to be hit, but worth a log line if a 429
  ever comes back so it's visible rather than silently dropped.
- **Misspelling-term matching**: eBay's own search likely already fuzzy-matches
  common typos to some extent, which may mean some LLM-generated misspelling
  terms return the *same* results as the correct spelling — not a correctness
  problem, just worth knowing so duplicate finds across terms within one hunt
  get deduped by item_id (the `ebay_seen_listings` table already does this
  per-hunt).
- **ntfy topic privacy**: ntfy.sh public topics are guessable-name-based with
  no auth by default — mention self-hosting or a long random topic name as
  the private-by-obscurity baseline when the settings UI is built.
