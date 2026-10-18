# Guitar Chords ("Songbook") — Ultimate Guitar search, versions, saved library

## Status (2026-10-18)

**Built, not yet tried against the live site.** The `chords` crate, the
`chord_songs` table, `/api/chords/*` and the Songs tab are in. Rust tests, the
frontend tests and clippy all pass. Ultimate Guitar can't be reached from the
cloud sandbox this was written in, so **the fixtures in `chords/tests/fixtures/`
were built to match UG's documented `js-store` layout, not captured from it.**
The first thing to do on the home network is open the Songs tab, search
"pompeii", and confirm results come back. If they don't, capture a real page
(see `docs/chords.md`, *If Ultimate Guitar changes or blocks*) and fix
`chords/src/ug.rs` against it.

Changes from the plan below, made while building it or asked for during the build:

- **Searching runs when you press Enter or Search**, not as you type. Each
  search is a request to UG's site, and as-you-type would send one per pause.
- **No `html-escape` or `regex` dependency.** Entity decoding and chord-line
  detection are small enough to write by hand in `ug.rs`, and neither crate
  was already in the workspace.
- **The UG client is a `OnceLock` static** in `api/chords.rs`, like
  `model_search.rs`, not an `Extension`, so `router()`'s signature is unchanged.
- **Auto-scroll has an on/off switch** (Jon). It is off by default and
  remembered per device. When it's off, there are no controls and taps do
  nothing. When it's on, play never starts by itself. A tap on the sheet or
  Space pauses and resumes it, and scrolling by hand pauses it.
- **Version switching** (Jon): a dropdown and ‹ › whenever a song has two or
  more versions. A song opened from the library starts with its other saved
  versions, and *More versions* fetches UG's list on request, so a saved song
  opens without touching UG.
- **Listen along** (Jon asked for YouTube, preferably without adverts):
  - **YouTube**: opens a YouTube search, or plays a pasted video in an embedded
    `youtube-nocookie.com` player, remembered per song (`youtube_url`).
    ai-mesh cannot remove YouTube's adverts; YouTube Premium, signed in in that
    browser, does.
  - **Spotify**: plays the song on the house speakers through the existing
    music node (`intent::dispatch_music_command`, now `pub(crate)`). This one
    has no adverts, because librespot already needs Premium.
- **Starter songs** (Jon): the four example versions are fetched into the
  library in the background when the coordinator starts, once each.
  `__chords__` preferences record which were added, so one deleted stays
  deleted, and a blocked fetch is retried at the next start. Only the URLs are
  in the repo. The sheets carry the songs' lyrics, which the public repo must
  not hold, and the test fixtures use invented lyrics for the same reason.

## Context

Jon plays and sings along to chord sheets from Ultimate Guitar (UG), e.g.
`tabs.ultimate-guitar.com/tab/bastille/pompeii-chords-1227805`. He wants this inside
the ai-mesh dashboard: search UG for **any** song, flip between the different
versions UG has of the same song, save a version to play again later, and browse
saved songs **by artist** and as an **A–Z list of song titles**. While playing he
wants **scroll-as-I-play** (auto-scroll).

Defaults chosen (the clarifying questions weren't answered; change any of these at
review): tablet- and phone-first view; **saving stores the whole sheet** (opens
instantly, works offline or if UG blocks or removes the version); a **paste fallback**
(UG URL or raw chord text); auto-scroll, transpose and text size in the song view.
Chord diagrams are deferred. Jon's later messages: he wants scroll-as-I-play, and
**it must be possible to switch auto-scroll on and off**.

UG has no public API, and browsers can't fetch UG directly (CORS). So the **coordinator
fetches the pages server-side**, the same pages a browser loads. Every UG page embeds its
data as HTML-escaped JSON in `<div class="js-store" data-content="…">`. That covers
search results (`store.page.data.results`), the tab (`store.page.data.tab`), the sheet
(`store.page.data.tab_view.wiki_tab.content`, with `[ch]G[/ch]` / `[tab]…[/tab]` markup)
and `tab_view.meta` (capo, tuning). This is for personal use. UG's terms don't welcome
scraping, so the client sends one request per user action, with no crawling or
background polling.

Shape: same as the eBay Hunts feature (`plans/ebay-bargain-finder.md`). A new
coordinator-only workspace crate, registry tables, `api/<name>.rs` handlers, and a
static JS tab. No agent capability and no new `Feature` variant.

## 1. New crate `chords/` (pure, unit-testable)

Add to root `Cargo.toml` members. Deps: `reqwest` (copy `ebay/Cargo.toml:7`
exactly: `default-features=false, ["json","rustls-tls"]`), `serde`, `serde_json`,
`thiserror` if already used by `ebay`, and `html-escape` for entity-decoding the
`data-content` attribute. Don't add a full HTML parser: the attribute is found by a
plain substring search.

- `chords/src/lib.rs`: re-exports.
- `chords/src/ug.rs`: **parsing, no I/O**.
  - `extract_store(html) -> Result<serde_json::Value, UgError>`: find
    `class="js-store"`, take the `data-content="…"` value, decode the entities, then
    `serde_json`. A missing store gives `UgError::Blocked` (likely a Cloudflare page),
    with a message that tells the user to use paste.
  - `parse_search(store) -> Vec<SearchHit>`. `SearchHit { tab_id, song_name,
    artist_name, kind ("Chords"/"Tab"/"Ukulele"/…), version, rating, votes, tab_url }`.
    Drop "Official"/"Pro"/"Video" kinds, which have no free sheet.
  - `parse_tab(store) -> Tab`. `Tab { tab_id, song_name, artist_name, kind, version,
    rating, votes, capo, tuning, tonality, url, content }`, where `content` is the raw
    `[ch]/[tab]` markup, kept as-is so the frontend owns rendering and transposing.
  - `group_versions(hits) -> Vec<SongGroup>`: group by normalised
    (artist, song) and sort the versions by kind (Chords first), then rating×votes.
    This is the "switch between versions" list.
  - `parse_pasted(text) -> Tab`: plain text with chord-over-lyric lines. Detect
    chord-only lines with a chord regex and wrap the chords in `[ch]` so pasted
    sheets also transpose.
  - `is_ug_url(url)`: only `https://tabs.ultimate-guitar.com/tab/…` and
    `https://www.ultimate-guitar.com/…`, so the fetch endpoint can't be used as an
    open proxy (SSRF guard).
- `chords/src/client.rs`: `UgClient` with a **base URL that can be swapped in tests**,
  like `llm-rotation`'s `Provider::new(base_url, …)`.
  - The builder follows `coordinator/src/http/api/model_search.rs:17-26`
    (`.timeout(..).user_agent(..)`): a normal browser User-Agent (UG, like Wikimedia
    in `capabilities/art/src/lib.rs:695`, is likely to refuse reqwest's default),
    `Accept-Language: en-GB`, and a 15 s timeout.
  - `UgError { Blocked, NotFound, Unreachable(String), Other(String) }` has
    human-readable `Display` messages, like `EbayError` (`ebay/src/client.rs:19-39`).
  - `search(query) -> Vec<SearchHit>`:
    `GET https://www.ultimate-guitar.com/search.php?search_type=title&value=<q>&page=N`.
    Fetch pages 1–2, so popular songs with many versions are covered.
  - `fetch_tab(url) -> Tab`.
- Tests: **saved HTML fixtures** in `chords/tests/fixtures/` (one search page, one
  chords page, one Cloudflare-challenge page), loaded with `include_str!`. Capture
  them once from a real browser, because UG isn't reachable from the cloud dev
  sandbox. Parsing tests run against the fixtures only, the way `ebay/` tests stay
  off-network. `parse_pasted` and `is_ug_url` get unit tests too.
  - One client round-trip test uses a canned-response `TcpListener` on
    `127.0.0.1:0`, the pattern in `llm-rotation/src/rotation.rs:150-161`.

## 2. Persistence (`coordinator/src/registry/chords.rs`)

Add to `init_schema` in `coordinator/src/registry/mod.rs`, next to the `ebay_*`
`CREATE TABLE IF NOT EXISTS` blocks (~line 379):

```sql
CREATE TABLE IF NOT EXISTS chord_songs (
  id TEXT PRIMARY KEY,               -- gen_uuid()
  artist TEXT NOT NULL, title TEXT NOT NULL,
  artist_key TEXT NOT NULL, title_key TEXT NOT NULL,  -- lowercased, leading "The " stripped, for sorting/grouping
  ug_tab_id INTEGER, source_url TEXT, kind TEXT, version INTEGER,
  rating REAL, votes INTEGER, capo INTEGER, tuning TEXT,
  content TEXT NOT NULL,             -- full [ch]/[tab] markup
  transpose INTEGER NOT NULL DEFAULT 0,
  scroll_speed REAL,                 -- per-song auto-scroll speed
  notes TEXT,
  saved_ms INTEGER NOT NULL, last_played_ms INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS chord_songs_ug ON chord_songs(ug_tab_id) WHERE ug_tab_id IS NOT NULL;
```

Several saved versions of the same song are allowed (e.g. one capo version and one
without). The list groups them under the song. `impl Registry` methods follow the
`registry/ebay.rs` style (`gen_uuid`, `now_unix_millis`, `warn!` on errors):
`list_chord_songs()` (summary, without `content`), `get_chord_song(id)`,
`save_chord_song(tab)`, `update_chord_song_prefs(id, transpose, scroll_speed, notes)`,
`touch_chord_song_played(id)`, `delete_chord_song(id)`.

## 3. HTTP API (`coordinator/src/http/api/chords.rs`)

Every handler takes `_: Authed` and `Extension<Arc<Mutex<Registry>>>`, as
`api/ebay.rs:290` does. Routes go in `coordinator/src/http/mod.rs::router()` beside
the `/api/ebay/*` block (~line 217), and the module is declared in `api/mod.rs`.
The `UgClient` is built once and shared with an `Extension`.

| Route | Purpose |
|---|---|
| `GET /api/chords/search?q=` | UG search, grouped into songs with their versions |
| `GET /api/chords/fetch?url=` | One UG version, live (not saved). Returns 400 unless `is_ug_url`, and 502 with a "use paste" hint on `Blocked` |
| `GET /api/chords/songs` | Saved library summary |
| `POST /api/chords/songs` | Save. The body is a fetched `Tab` or `{paste:{artist,title,text}}` |
| `GET /api/chords/songs/{id}` | Full saved sheet. Bumps `last_played_ms` |
| `PATCH /api/chords/songs/{id}` | transpose / scroll_speed / notes |
| `POST /api/chords/songs/{id}/refresh` | Re-fetch from UG and replace the content |
| `DELETE /api/chords/songs/{id}` | Remove |

Don't hold the registry `Mutex` across the UG `.await`: fetch first, then lock to
write, as `ebay.rs:183` does.

Error mapping follows `ebay.rs:166-210`:

| Case | Status |
|---|---|
| `Blocked` / `Unreachable` | 502 (message suggests paste) |
| `NotFound` | 404 |
| Bad or non-UG URL | 400 |

Handler tests use `api/mod.rs` `test_util` (`make_state`, `make_registry`, `send`,
`send_with_body`), like `ebay.rs:1036`.

## 4. Dashboard tab `chords.js` ("Songs")

Wiring, as for Hunts: a `<button class="tab" data-panel="chords">` +
`<section class="panel" id="panel-chords">` in `static/index.html` (after `ebay`,
~line 180). Import it and call `chords.init(...)` in `static/dashboard.js` (like lines
14/192). Add an `include_str!` const and `ASSETS` row in `http/mod.rs` (~lines 54/313).
Bump the service-worker cache name in `static/service-worker.js`. Calls go through
`api()` from `static/api.js`, and errors are shown with `showToast` from `static/util.js`.

Three views inside the panel:

1. **Library** (default). A search box that filters saved songs as you type. A toggle
   switches between **A–Z songs** (alphabetical by `title_key`, with letter
   jump-headings) and **By artist** (collapsible artist groups, sorted by
   `artist_key`, songs A–Z inside each, collapsed state kept in localStorage like
   `devices.js` category sections). Plus a small "Recently played" strip.
   Grouped rendering reuses the `devices.js:905-970` `buildCategorySection` /
   `localeCompare` pattern. The sort toggle reuses the `.ebay-sort` `is-active`
   button style.
2. **Find online**. A debounced search with `AbortController`, so a slow old
   response can't overwrite a newer one (copy `models.js:140-215`, including its
   "Searching…", "No matches" and "Search failed" states). Results are grouped as
   *Song – Artist*, each
   expandable to its versions (`Chords v3 ★4.8 (1,204)`, …). Tapping a version
   opens it in the player. A **Paste** expander takes a UG URL or raw chord text.
3. **Player** (the core of the feature).
   - Renders `content`: `[ch]X[/ch]` becomes a highlighted chord span placed above the
     lyrics in a monospace `pre`, `[tab]` blocks keep their alignment, and
     `[Verse]`/`[Chorus]` headings are styled.
   - **Version switcher**: a dropdown or prev/next arrows across all versions of the
     song (from the search group, or re-run `search?q=artist title` when the song was
     opened from the library), so another version is one tap away. Saved versions are
     marked ★.
   - **Save** / **Saved ✓** (with a "save another version" option), **Refresh from
     UG**, **Delete**.
   - **Auto-scroll** (scroll as I play), which **can be switched on and off**:
     - An **Auto-scroll on/off switch** in the player toolbar, remembered in
       localStorage (`chords.autoscroll`), **off by default**. When it's off, the
       sheet only scrolls by hand: there are no scroll controls and taps on the
       sheet do nothing. Switching it off mid-song stops scrolling immediately.
     - When it's on, a big play/pause button and speed − / + appear.
       `requestAnimationFrame` scrolls the sheet at px/s. Scrolling never starts
       on its own; you press play when you start the song.
     - Tapping the sheet pauses or resumes, and Space does the same on a keyboard.
       Scrolling by hand while it's playing pauses it, so you can jump back to a
       verse. It stops at the end of the sheet.
     - The speed is saved per song (`PATCH scroll_speed`).
     - The Screen Wake Lock API keeps the screen awake while a song is open, and
       is ignored where it's unsupported.
   - **Transpose** −/+ semitone (shows the capo from meta) and is saved per song.
   - **Text size** A−/A+, saved per device in localStorage.
   - **Full screen** mode hides the tab bar and header for the music stand.

Keep the pure helpers exported for tests: `transposeChord`, `renderSheet`,
`sortSongs`/`groupByArtist`, and the scroll-speed maths.

## 5. Docs

`docs/chords.md` covers usage, the paste fallback, and what to do if UG starts
blocking (capture a new fixture and re-check the `js-store` path). Add one line to the
README feature list and an entry in `HISTORY.md` when it ships.

## Critical files

- New: `chords/` crate, `coordinator/src/registry/chords.rs`,
  `coordinator/src/http/api/chords.rs`, `coordinator/src/http/static/chords.js`,
  `frontend/chords.test.js`, `docs/chords.md`.
- Edit: root `Cargo.toml`, `coordinator/Cargo.toml`, `coordinator/src/registry/mod.rs`
  (schema + `mod chords`), `coordinator/src/http/api/mod.rs`,
  `coordinator/src/http/mod.rs` (routes, asset const, ASSETS row, Extension),
  `static/index.html`, `static/dashboard.js`, `static/style.css`,
  `static/service-worker.js`.

## Verification

1. `cargo test -p chords` (fixture parsing for search, tab, blocked page, paste, URL
   guard) and `cargo test -p coordinator`, with registry round-trip tests using the
   in-memory Registry the existing ebay registry tests use.
2. `cargo clippy --workspace -- -D warnings`.
3. `cd frontend && npx vitest run`, with `chords.test.js` mocking `/static/api.js` as
   `ebay.test.js` does: transpose (incl. sharps/flats, slash chords `D/F#`), A–Z and
   by-artist ordering ("The Beatles" under B), and the version switcher. Auto-scroll
   tests cover:
   - the switch defaults to off, with the controls hidden and taps doing nothing;
   - turning it on shows play/pause and speed;
   - turning it off mid-scroll stops it;
   - the setting persists across `init`.
4. Live, on a coordinator that can reach UG (Jon's network; the cloud sandbox can't).
   Search "pompeii" and switch between at least 2 versions. Open each of the 4 example
   URLs via paste. Save one, restart the coordinator, and confirm it's still in both
   A–Z and By artist. Then check auto-scroll and transpose on a phone.
