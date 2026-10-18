import { api } from '/static/api.js';
import { esc, showToast } from '/static/util.js';

// ── Songs tab (Ultimate Guitar chord sheets) ────────────────────────────────
// Three views in one panel: the saved library (A–Z or by artist), finding a
// song on Ultimate Guitar (search, or paste a link or the chords), and the
// player — the sheet, a version switcher, transpose, text size, auto-scroll,
// and YouTube / Spotify to play along to. See plans/chords-songbook.md.

const SHARPS = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
const FLATS = ['C', 'Db', 'D', 'Eb', 'E', 'F', 'Gb', 'G', 'Ab', 'A', 'Bb', 'B'];
// Sharps, except the two flats guitarists actually say.
const PREFERRED = ['C', 'C#', 'D', 'Eb', 'E', 'F', 'F#', 'G', 'G#', 'A', 'Bb', 'B'];
const NOTE_INDEX = { Cb: 11, 'B#': 0, Fb: 4, 'E#': 5 };
SHARPS.forEach((n, i) => { NOTE_INDEX[n] = i; });
FLATS.forEach((n, i) => { NOTE_INDEX[n] = i; });

const FONT_MIN = 10, FONT_MAX = 36, FONT_DEFAULT = 16;
const SPEED_MIN = 2, SPEED_MAX = 200, SPEED_DEFAULT = 20, SPEED_STEP = 2;
const KEY_AUTOSCROLL = 'chords.autoscroll';
const KEY_FONT = 'chords.fontsize';
const KEY_LIBMODE = 'chords.libmode';

// localStorage can throw (private windows, blocked storage); a lost
// preference is fine, a dead tab is not.
function load(key, fallback) {
  try { return localStorage.getItem(key) ?? fallback; } catch { return fallback; }
}
function store(key, value) {
  try { localStorage.setItem(key, value); } catch { /* preference only */ }
}

// ── Pure helpers (exported for tests) ───────────────────────────────────────

function transposeNote(note, n) {
  const idx = NOTE_INDEX[note];
  if (idx === undefined) return note;
  const to = ((idx + n) % 12 + 12) % 12;
  return (note.length > 1 && note[1] === 'b' ? FLATS : PREFERRED)[to];
}

/** Move one chord name `n` semitones, slash bass included: D/F# +2 → E/G#. */
export function transposeChord(chord, n) {
  if (!n || n % 12 === 0) return chord;
  return chord.replace(/(^|\/)([A-G][#b]?)/g, (_, pre, note) => pre + transposeNote(note, n));
}

let entityBox = null;
/**
 * Undo HTML entities left in a sheet (`I&rsquo;m`). Sheets saved before the
 * coordinator decoded them still carry them. Each entity is decoded on its
 * own through a <textarea>, which knows every named entity and, holding a
 * single entity and no markup, can't be made to parse anything else.
 */
export function decodeEntities(s) {
  return String(s ?? '').replace(/&(?:#\d{1,7}|#x[0-9a-f]{1,6}|[a-z][a-z0-9]{1,31});/gi, m => {
    entityBox ??= document.createElement('textarea');
    entityBox.innerHTML = m;
    return entityBox.value;
  });
}

/** Ultimate Guitar's sheet markup as HTML, chords moved `transpose` semitones. */
export function renderSheet(content, transpose = 0) {
  return esc(decodeEntities(content))
    .replace(/\[\/?tab\]/g, '')
    .replace(/\[ch\]([^[]*?)\[\/ch\]/g, (_, c) => `<span class="songs-ch">${transposeChord(c, transpose)}</span>`)
    .replace(/^[ \t]*\[([^\]\n]{1,40})\][ \t]*$/gm, (_, s) => `<span class="songs-sec">${s}</span>`);
}

/** A title or artist as the library sorts it: lowercase, no leading "The ". */
export function sortKey(s) {
  const lower = String(s ?? '').trim().replace(/\s+/g, ' ').toLowerCase();
  return lower.startsWith('the ') && lower.length > 4 ? lower.slice(4) : lower;
}

const byKey = (a, b) => a.localeCompare(b, 'en-GB', { numeric: true });

/** Saved songs A–Z by title, then artist. */
export function sortSongs(songs) {
  return [...songs].sort((a, b) =>
    byKey(a.title_key ?? sortKey(a.title), b.title_key ?? sortKey(b.title))
    || byKey(a.artist_key ?? sortKey(a.artist), b.artist_key ?? sortKey(b.artist)));
}

/** Saved songs as [{artist, key, songs}] — artists A–Z, songs A–Z within. */
export function groupByArtist(songs) {
  const groups = new Map();
  for (const s of sortSongs(songs)) {
    const key = s.artist_key ?? sortKey(s.artist);
    if (!groups.has(key)) groups.set(key, { artist: s.artist, key, songs: [] });
    groups.get(key).songs.push(s);
  }
  return [...groups.values()].sort((a, b) => byKey(a.key, b.key));
}

/** The A–Z heading a sort key files under; anything not a letter is '#'. */
export function letterOf(key) {
  const c = (key ?? '').charAt(0).toUpperCase();
  return c >= 'A' && c <= 'Z' ? c : '#';
}

export function clampSpeed(s) {
  const n = Number(s);
  return Number.isFinite(n) ? Math.min(SPEED_MAX, Math.max(SPEED_MIN, n)) : SPEED_DEFAULT;
}

/** The 11-character video id in a YouTube link, or null. */
export function youtubeId(url) {
  let u;
  try { u = new URL(String(url ?? '').trim()); } catch { return null; }
  if (!/^https?:$/.test(u.protocol)) return null;
  const host = u.hostname.toLowerCase().replace(/^(www|m)\./, '');
  let id = null;
  if (host === 'youtu.be') id = u.pathname.split('/')[1];
  else if (['youtube.com', 'music.youtube.com', 'youtube-nocookie.com'].includes(host)) {
    id = u.pathname === '/watch'
      ? u.searchParams.get('v')
      : (u.pathname.match(/^\/(?:shorts|embed|live)\/([^/]+)/) ?? [])[1];
  }
  return id && /^[A-Za-z0-9_-]{11}$/.test(id) ? id : null;
}

function versionLabel(v) {
  const bits = [v.kind ?? 'Chords'];
  if (v.version) bits[0] += ` v${v.version}`;
  if (v.rating) bits.push(`★${Number(v.rating).toFixed(1)}`);
  if (v.votes) bits.push(`(${Number(v.votes).toLocaleString('en-GB')})`);
  return bits.join(' ');
}

// ── State ───────────────────────────────────────────────────────────────────

let panel = null;
let view = 'library';
let songs = [];                 // saved library, summaries
let libMode = 'az';
let libFilter = '';
let results = [];               // last search, grouped by song
let resultsSaved = {};          // UG tab id → saved song id, for the last search
let searchCtl = null;

// The open song. `tab` is a UG sheet or a saved song's fields mapped onto the
// same names; `savedId` is set once it is in the library.
let player = null;
let scroll = { enabled: false, playing: false, held: false, carry: 0, last: 0, frame: null };
let wakeLock = null;
let fontSize = FONT_DEFAULT;
let docListeners = false;

export function init(el) {
  panel = el;
  panel.classList.add('panel-songs');
  scroll.enabled = load(KEY_AUTOSCROLL, '0') === '1';
  fontSize = Math.min(FONT_MAX, Math.max(FONT_MIN, Number(load(KEY_FONT, FONT_DEFAULT)) || FONT_DEFAULT));
  libMode = load(KEY_LIBMODE, 'az') === 'artist' ? 'artist' : 'az';
  panel.innerHTML = `
    <div class="songs-view" id="songs-library"></div>
    <div class="songs-view" id="songs-find" hidden></div>
    <div class="songs-player" id="songs-player" hidden></div>`;
  view = 'library';
  libFilter = '';
  buildLibrary();
  buildFind();
  if (!docListeners) {
    docListeners = true;
    document.addEventListener('keydown', onKey);
    // Songs can arrive while the page is open (the starter songs are fetched
    // after the coordinator starts), so re-read the library on each visit.
    document.querySelector('.tab[data-panel="chords"]')?.addEventListener('click', () => {
      if (view === 'library') refreshLibrary();
    });
    // The browser drops a wake lock whenever the page is hidden.
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'visible' && view === 'player') keepAwake(true);
    });
  }
  refreshLibrary();
}

function show(next) {
  if (view === 'player' && next !== 'player') {
    stopScroll();
    keepAwake(false);
    setFullscreen(false);
  }
  view = next;
  panel.querySelector('#songs-library').hidden = next !== 'library';
  panel.querySelector('#songs-find').hidden = next !== 'find';
  panel.querySelector('#songs-player').hidden = next !== 'player';
  if (next === 'library') renderLibrary();
}

async function errorText(res) {
  try { return (await res.text()) || `HTTP ${res.status}`; } catch { return `HTTP ${res.status}`; }
}

// ── Library ─────────────────────────────────────────────────────────────────

function buildLibrary() {
  const lib = panel.querySelector('#songs-library');
  lib.innerHTML = `
    <div class="songs-head">
      <h2>Songs</h2>
      <button type="button" class="songs-btn songs-primary" id="songs-go-find">+ Find a song</button>
    </div>
    <div class="songs-libbar">
      <input type="search" id="songs-filter" placeholder="Filter saved songs" autocomplete="off">
      <div class="songs-seg" role="group" aria-label="Order">
        <button type="button" class="songs-mode" data-mode="az">A–Z</button>
        <button type="button" class="songs-mode" data-mode="artist">By artist</button>
      </div>
    </div>
    <div id="songs-recent"></div>
    <div id="songs-list"><p class="placeholder">Loading…</p></div>`;
  lib.querySelector('#songs-go-find').addEventListener('click', () => {
    show('find');
    panel.querySelector('#songs-q').focus();
  });
  lib.querySelector('#songs-filter').addEventListener('input', e => {
    libFilter = e.target.value;
    renderLibrary();
  });
  lib.querySelectorAll('.songs-mode').forEach(b => b.addEventListener('click', () => {
    libMode = b.dataset.mode;
    store(KEY_LIBMODE, libMode);
    renderLibrary();
  }));
}

async function refreshLibrary() {
  try {
    const res = await api('/chords/songs');
    if (!res.ok) throw new Error(await errorText(res));
    songs = await res.json();
  } catch (e) {
    songs = [];
    const list = panel.querySelector('#songs-list');
    if (list) list.innerHTML = `<p class="placeholder">Couldn't load saved songs: ${esc(e.message)}</p>`;
    return;
  }
  renderLibrary();
}

function songRow(s, { showArtist = true } = {}) {
  const b = document.createElement('button');
  b.type = 'button';
  b.className = 'songs-row';
  b.dataset.id = s.id;
  b.innerHTML = `
    <span class="songs-row-title">${esc(s.title)}</span>
    ${showArtist ? `<span class="songs-row-artist">${esc(s.artist)}</span>` : ''}
    <span class="songs-row-meta">${esc(versionLabel(s))}</span>`;
  b.addEventListener('click', () => openSaved(s.id));
  return b;
}

function renderLibrary() {
  const list = panel.querySelector('#songs-list');
  const recent = panel.querySelector('#songs-recent');
  if (!list) return;
  panel.querySelectorAll('.songs-mode').forEach(b =>
    b.classList.toggle('is-active', b.dataset.mode === libMode));

  const f = sortKey(libFilter);
  const shown = f
    ? songs.filter(s => sortKey(s.title).includes(f) || sortKey(s.artist).includes(f))
    : songs;

  recent.innerHTML = '';
  const played = songs.filter(s => s.last_played_ms)
    .sort((a, b) => b.last_played_ms - a.last_played_ms).slice(0, 6);
  if (!f && played.length) {
    recent.innerHTML = '<h3 class="songs-subhead">Recently played</h3>';
    const strip = document.createElement('div');
    strip.className = 'songs-recent';
    for (const s of played) {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'songs-chip';
      b.textContent = `${s.title} · ${s.artist}`;
      b.addEventListener('click', () => openSaved(s.id));
      strip.appendChild(b);
    }
    recent.appendChild(strip);
  }

  list.innerHTML = '';
  if (!songs.length) {
    list.innerHTML = '<p class="placeholder">No saved songs yet — find one on Ultimate Guitar and press Save. (The four starter songs are fetched from Ultimate Guitar when the coordinator starts.)</p>';
    return;
  }
  if (!shown.length) {
    list.innerHTML = '<p class="placeholder">No saved songs match.</p>';
    return;
  }

  if (libMode === 'az') {
    let letter = null;
    for (const s of sortSongs(shown)) {
      const l = letterOf(s.title_key ?? sortKey(s.title));
      if (l !== letter) {
        letter = l;
        const h = document.createElement('h3');
        h.className = 'songs-letter';
        h.textContent = l;
        list.appendChild(h);
      }
      list.appendChild(songRow(s));
    }
    return;
  }

  for (const g of groupByArtist(shown)) {
    const storageKey = `chords.artist-collapsed.${g.key}`;
    // A filter opens everything it matched; otherwise each artist remembers.
    let collapsed = !f && load(storageKey, '0') === '1';
    const head = document.createElement('button');
    head.type = 'button';
    head.className = 'songs-artist';
    const body = document.createElement('div');
    body.className = 'songs-artist-body';
    const paint = () => {
      head.innerHTML = `<span class="songs-chev">${collapsed ? '▸' : '▾'}</span>
        <span>${esc(g.artist)}</span><span class="songs-count">${g.songs.length}</span>`;
      head.setAttribute('aria-expanded', String(!collapsed));
      body.hidden = collapsed;
    };
    head.addEventListener('click', () => {
      collapsed = !collapsed;
      store(storageKey, collapsed ? '1' : '0');
      paint();
    });
    for (const s of g.songs) body.appendChild(songRow(s, { showArtist: false }));
    paint();
    list.append(head, body);
  }
}

// ── Find online ─────────────────────────────────────────────────────────────

function buildFind() {
  const find = panel.querySelector('#songs-find');
  find.innerHTML = `
    <div class="songs-head">
      <button type="button" class="songs-btn" id="songs-find-back">← Library</button>
      <h2>Find a song</h2>
    </div>
    <form class="songs-searchbar" id="songs-search-form">
      <input type="search" id="songs-q" placeholder="Song, artist, or both" autocomplete="off" enterkeyhint="search">
      <button type="submit" class="songs-btn songs-primary">Search</button>
    </form>
    <div id="songs-results"><p class="placeholder">Search Ultimate Guitar for any song.</p></div>
    <details class="songs-paste">
      <summary>Paste a link or the chords instead</summary>
      <form id="songs-url-form" class="songs-searchbar">
        <input type="url" id="songs-url" placeholder="https://tabs.ultimate-guitar.com/tab/…" autocomplete="off">
        <button type="submit" class="songs-btn">Open</button>
      </form>
      <form id="songs-paste-form" class="songs-paste-form">
        <div class="songs-paste-names">
          <input type="text" id="songs-paste-artist" placeholder="Artist" autocomplete="off">
          <input type="text" id="songs-paste-title" placeholder="Song title" autocomplete="off" required>
        </div>
        <textarea id="songs-paste-text" rows="8" placeholder="Paste the chords — chord lines above lyric lines" required></textarea>
        <button type="submit" class="songs-btn">Save pasted song</button>
      </form>
    </details>`;
  find.querySelector('#songs-find-back').addEventListener('click', () => show('library'));
  find.querySelector('#songs-search-form').addEventListener('submit', e => {
    e.preventDefault();
    runSearch(find.querySelector('#songs-q').value);
  });
  find.querySelector('#songs-url-form').addEventListener('submit', e => {
    e.preventDefault();
    const url = find.querySelector('#songs-url').value.trim();
    if (url) openUrl(url, null, 'find');
  });
  find.querySelector('#songs-paste-form').addEventListener('submit', async e => {
    e.preventDefault();
    const paste = {
      artist: find.querySelector('#songs-paste-artist').value,
      title: find.querySelector('#songs-paste-title').value,
      text: find.querySelector('#songs-paste-text').value,
    };
    const res = await api('/chords/songs', { method: 'POST', body: { paste } });
    if (!res.ok) { showToast(await errorText(res), true); return; }
    const song = await res.json();
    find.querySelector('#songs-paste-form').reset();
    showToast(`Saved "${song.title}"`);
    await refreshLibrary();
    openSaved(song.id);
  });
}

// Searches go out only when asked (Enter or the button) — every one is a
// request to UG's site, so as-you-type would hammer it. The AbortController
// still drops a slow earlier answer that lands after a newer search.
async function runSearch(q) {
  q = q.trim();
  const box = panel.querySelector('#songs-results');
  if (!q) return;
  searchCtl?.abort();
  const ctl = searchCtl = new AbortController();
  box.innerHTML = '<p class="placeholder">Searching Ultimate Guitar…</p>';
  let data;
  try {
    const res = await api(`/chords/search?q=${encodeURIComponent(q)}`);
    if (ctl.signal.aborted) return;
    if (!res.ok) throw new Error(await errorText(res));
    data = await res.json();
  } catch (e) {
    if (ctl.signal.aborted) return;
    box.innerHTML = `<p class="placeholder">Search failed: ${esc(e.message)}</p>`;
    return;
  }
  if (ctl.signal.aborted) return;
  results = data.groups ?? [];
  resultsSaved = data.saved ?? {};
  renderResults();
}

function renderResults() {
  const box = panel.querySelector('#songs-results');
  box.innerHTML = '';
  if (!results.length) {
    box.innerHTML = '<p class="placeholder">No matches with chords on Ultimate Guitar.</p>';
    return;
  }
  results.forEach(g => {
    const card = document.createElement('div');
    card.className = 'songs-result';
    const best = g.versions[0];
    const n = g.versions.length;
    card.innerHTML = `
      <button type="button" class="songs-result-open">
        <span class="songs-row-title">${esc(g.song_name)}</span>
        <span class="songs-row-artist">${esc(g.artist_name)}</span>
        <span class="songs-row-meta">${esc(versionLabel(best))}</span>
      </button>
      ${n > 1 ? `<button type="button" class="songs-result-more" aria-expanded="false">${n} versions ▾</button>` : ''}
      <div class="songs-versions" hidden></div>`;
    card.querySelector('.songs-result-open').addEventListener('click', () => openVersion(best, g.versions, 'find'));
    const more = card.querySelector('.songs-result-more');
    if (more) {
      const list = card.querySelector('.songs-versions');
      more.addEventListener('click', () => {
        list.hidden = !list.hidden;
        more.setAttribute('aria-expanded', String(!list.hidden));
        more.textContent = `${n} versions ${list.hidden ? '▾' : '▴'}`;
      });
      g.versions.forEach(v => {
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'songs-version';
        b.textContent = versionLabel(v) + (resultsSaved[v.tab_id] ? ' · saved' : '');
        b.addEventListener('click', () => openVersion(v, g.versions, 'find'));
        list.appendChild(b);
      });
    }
    box.appendChild(card);
  });
}

// ── Opening a song ──────────────────────────────────────────────────────────

/** A saved song's fields under the names a UG tab uses. */
function savedAsTab(s) {
  return {
    tab_id: s.ug_tab_id, song_name: s.title, artist_name: s.artist, kind: s.kind,
    version: s.version, rating: s.rating, votes: s.votes, capo: s.capo, tuning: s.tuning,
    tonality: s.tonality, url: s.source_url, content: s.content,
  };
}

/** One version from a list: the saved copy if there is one, else from UG. */
function openVersion(v, versions, back) {
  const savedId = player?.saved?.[v.tab_id] ?? resultsSaved[v.tab_id];
  if (savedId) return openSaved(savedId, { versions, back });
  return openUrl(v.tab_url, versions, back);
}

async function openUrl(url, versions, back) {
  showToast('Loading from Ultimate Guitar…');
  const res = await api(`/chords/fetch?url=${encodeURIComponent(url)}`);
  if (!res.ok) { showToast(await errorText(res), true); return; }
  const { tab, saved } = await res.json();
  // UG's page lists the other versions beside the sheet; use them when this
  // wasn't opened from a search that already had the list.
  const list = versions?.length ? versions : [
    { tab_id: tab.tab_id, song_name: tab.song_name, artist_name: tab.artist_name, kind: tab.kind,
      version: tab.version, rating: tab.rating, votes: tab.votes, tab_url: tab.url ?? url },
    ...(tab.versions ?? []),
  ];
  openPlayer({
    tab, savedId: saved?.[tab.tab_id] ?? null, song: null,
    versions: list, saved: { ...resultsSaved, ...(saved ?? {}) }, back,
  });
}

async function openSaved(id, { versions = null, back = 'library' } = {}) {
  const res = await api(`/chords/songs/${encodeURIComponent(id)}`);
  if (!res.ok) { showToast(await errorText(res), true); return; }
  const song = await res.json();
  // Other saved versions of the same song are there at once; UG's full list
  // waits for "More versions", so a saved song opens without touching UG.
  const siblings = songs.filter(s => s.id !== song.id
    && s.artist_key === song.artist_key && s.title_key === song.title_key);
  const saved = { ...resultsSaved };
  for (const s of [song, ...siblings]) if (s.ug_tab_id) saved[s.ug_tab_id] = s.id;
  const list = versions ?? [song, ...siblings].map(s => ({
    tab_id: s.ug_tab_id, saved_id: s.id, song_name: s.title, artist_name: s.artist,
    kind: s.kind, version: s.version, rating: s.rating, votes: s.votes, tab_url: s.source_url,
  }));
  openPlayer({ tab: savedAsTab(song), savedId: song.id, song, versions: list, saved, back });
  // Opening counts as playing; keep "Recently played" current.
  const row = songs.find(s => s.id === song.id);
  if (row) row.last_played_ms = Date.now();
}

// ── Player ──────────────────────────────────────────────────────────────────

function openPlayer(p) {
  stopScroll();
  player = {
    transpose: p.song?.transpose ?? 0,
    speed: clampSpeed(p.song?.scroll_speed ?? SPEED_DEFAULT),
    youtube: p.song?.youtube_url ? youtubeId(p.song.youtube_url) : null,
    ...p,
  };
  buildPlayer();
  show('player');
  keepAwake(true);
}

function currentIndex() {
  const t = player.tab;
  return player.versions.findIndex(v =>
    (t.tab_id != null && v.tab_id === t.tab_id) || (v.saved_id && v.saved_id === player.savedId));
}

function buildPlayer() {
  const el = panel.querySelector('#songs-player');
  const t = player.tab;
  const idx = currentIndex();
  const many = player.versions.length >= 2;
  const facts = [
    t.capo ? `Capo ${t.capo}` : null,
    t.tuning && t.tuning !== 'E A D G B E' ? `Tuning ${t.tuning}` : null,
    t.tonality ? `Key ${t.tonality}` : null,
  ].filter(Boolean).join(' · ');
  const rb = (id, label, text, extra = '') =>
    `<button type="button" class="songs-rbtn" id="${id}" aria-label="${label}" title="${label}" ${extra}>${text}</button>`;

  // The sheet gets the whole height; every control sits in a slim rail down
  // the right-hand side, and the title scrolls away with the sheet. The less
  // used controls open as panels over the sheet from the rail.
  el.innerHTML = `
    <div class="songs-stage">
      <div class="songs-sheet" id="songs-sheet" tabindex="0">
        <div class="songs-page">
        <header class="songs-sheethead">
          <strong>${esc(t.song_name)}</strong>
          <span>${esc(t.artist_name)}${facts ? ` · <b>${esc(facts)}</b>` : ''}</span>
          <span class="songs-sheetver">${esc(versionLabel(t))}${many ? ` · version ${idx + 1} of ${player.versions.length}` : ''}${player.savedId ? ' · <span class="songs-saved">Saved ✓</span>' : ''}</span>
        </header>
        <pre></pre>
        </div>
      </div>
      <div class="songs-pop" id="songs-menu" hidden>
        <div class="songs-pophead"><strong>Song</strong>${rb('songs-menu-close', 'Close', '✕')}</div>
        ${many ? `
          <label class="songs-poplabel" for="songs-version">Version</label>
          <select id="songs-version">
            ${player.versions.map((v, i) => `<option value="${i}" ${i === idx ? 'selected' : ''}>${esc(versionLabel(v))}${player.saved[v.tab_id] || v.saved_id ? ' ★' : ''}</option>`).join('')}
          </select>` : ''}
        ${player.song && t.url ? '<button type="button" class="songs-btn" id="songs-more-versions">More versions from Ultimate Guitar</button>' : ''}
        ${player.savedId && t.url ? '<button type="button" class="songs-btn" id="songs-refresh">Refresh from Ultimate Guitar</button>' : ''}
        ${player.savedId ? '<button type="button" class="songs-btn songs-danger" id="songs-delete">Delete from library</button>' : ''}
      </div>
      <div class="songs-pop" id="songs-listen" hidden></div>
    </div>
    <nav class="songs-rail" aria-label="Song controls">
      ${rb('songs-back', 'Back', '←')}
      ${rb('songs-autoscroll', 'Auto-scroll on or off', '<span class="songs-cap">Auto</span>⇣', `aria-pressed="${scroll.enabled}"`)}
      <div class="songs-railgroup" id="songs-scrollctl" ${scroll.enabled ? '' : 'hidden'}>
        ${rb('songs-play', 'Play', '▶', 'class="songs-rbtn songs-play"')}
        ${rb('songs-faster', 'Faster', '+')}
        <span id="songs-speed" class="songs-val" title="Scroll speed"></span>
        ${rb('songs-slower', 'Slower', '−')}
      </div>
      <div class="songs-railgroup">
        ${rb('songs-tup', 'Transpose up a semitone', '♯')}
        <span id="songs-tval" class="songs-val" title="Transpose"></span>
        ${rb('songs-tdown', 'Transpose down a semitone', '♭')}
      </div>
      <div class="songs-railgroup">
        ${rb('songs-fup', 'Larger text', 'A+')}
        ${rb('songs-fdown', 'Smaller text', 'A−')}
      </div>
      ${many ? `<div class="songs-railgroup">
        ${rb('songs-prev', 'Previous version', '‹', idx <= 0 ? 'disabled' : '')}
        <span class="songs-val" title="Version">${idx + 1}/${player.versions.length}</span>
        ${rb('songs-next', 'Next version', '›', idx >= player.versions.length - 1 ? 'disabled' : '')}
      </div>` : ''}
      ${player.savedId
        ? rb('songs-saved', 'Saved in your library', '★', 'disabled')
        : rb('songs-save', 'Save to your library', '☆')}
      ${rb('songs-listen-btn', 'Listen: Spotify or YouTube', '♪')}
      ${rb('songs-menu-btn', 'Versions and more', '⋯')}
      ${rb('songs-full', 'Full screen', '⛶')}
    </nav>`;

  const $ = sel => el.querySelector(sel);
  $('#songs-back').addEventListener('click', () => show(player.back === 'find' ? 'find' : 'library'));
  $('#songs-full').addEventListener('click', () => setFullscreen(!document.body.classList.contains('songs-fullscreen')));
  $('#songs-version')?.addEventListener('change', e => switchVersion(Number(e.target.value)));
  $('#songs-prev')?.addEventListener('click', () => switchVersion(currentIndex() - 1));
  $('#songs-next')?.addEventListener('click', () => switchVersion(currentIndex() + 1));
  $('#songs-more-versions')?.addEventListener('click', loadMoreVersions);
  $('#songs-save')?.addEventListener('click', saveCurrent);
  $('#songs-refresh')?.addEventListener('click', refreshCurrent);
  $('#songs-delete')?.addEventListener('click', deleteCurrent);
  $('#songs-tdown').addEventListener('click', () => setTranspose(player.transpose - 1));
  $('#songs-tup').addEventListener('click', () => setTranspose(player.transpose + 1));
  $('#songs-fdown').addEventListener('click', () => setFont(fontSize - 2));
  $('#songs-fup').addEventListener('click', () => setFont(fontSize + 2));
  $('#songs-autoscroll').addEventListener('click', () => setAutoscroll(!scroll.enabled));
  $('#songs-play').addEventListener('click', () => (scroll.playing ? stopScroll() : startScroll()));
  $('#songs-slower').addEventListener('click', () => setSpeed(player.speed - SPEED_STEP));
  $('#songs-faster').addEventListener('click', () => setSpeed(player.speed + SPEED_STEP));
  $('#songs-menu-btn').addEventListener('click', () => togglePop('songs-menu'));
  $('#songs-menu-close').addEventListener('click', () => togglePop('songs-menu', false));
  $('#songs-listen-btn').addEventListener('click', () => togglePop('songs-listen'));

  const sheet = $('#songs-sheet');
  // With auto-scroll on, a tap on the sheet pauses or resumes. Off, the sheet
  // is just a page you scroll by hand and taps do nothing.
  sheet.addEventListener('click', e => {
    if (!scroll.enabled || e.target.closest('.songs-sheethead')) return;
    scroll.playing ? stopScroll() : startScroll();
  });
  // Scrolling by hand while it plays is how you correct it — ahead when it's
  // slow, back when it's fast — so it keeps playing from wherever you leave
  // it. It only holds still while a finger is on the sheet, so it doesn't
  // fight the drag.
  sheet.addEventListener('touchstart', () => { scroll.held = true; }, { passive: true });
  for (const evt of ['touchend', 'touchcancel']) {
    sheet.addEventListener(evt, () => { scroll.held = false; scroll.carry = 0; }, { passive: true });
  }

  paintSheet();
  paintScrollControls();
  setFont(fontSize);
}

/** Open (or close, or flip) one of the panels over the sheet; one at a time. */
function togglePop(id, open) {
  for (const pop of panel.querySelectorAll('#songs-player .songs-pop')) {
    const show = pop.id === id && (open ?? pop.hidden);
    pop.hidden = !show;
    panel.querySelector(`#${pop.id}-btn`)?.classList.toggle('is-active', show);
    if (show && pop.id === 'songs-listen' && !pop.dataset.built) paintListen(pop);
  }
}

function paintSheet() {
  const pre = panel.querySelector('#songs-sheet pre');
  if (pre) pre.innerHTML = renderSheet(player.tab.content, player.transpose);
  const tval = panel.querySelector('#songs-tval');
  if (tval) tval.textContent = player.transpose ? (player.transpose > 0 ? `+${player.transpose}` : `${player.transpose}`) : '0';
}

function switchVersion(i) {
  const v = player.versions[i];
  if (!v) return;
  if (v.saved_id) return openSaved(v.saved_id, { versions: player.versions, back: player.back });
  return openVersion(v, player.versions, player.back);
}

async function loadMoreVersions() {
  const url = player.tab.url;
  if (!url) return;
  showToast('Fetching other versions from Ultimate Guitar…');
  const res = await api(`/chords/fetch?url=${encodeURIComponent(url)}`);
  if (!res.ok) { showToast(await errorText(res), true); return; }
  const { tab, saved } = await res.json();
  const have = new Set(player.versions.map(v => v.tab_id).filter(id => id != null));
  const more = (tab.versions ?? []).filter(v => !have.has(v.tab_id));
  if (!more.length) { showToast('No other versions on Ultimate Guitar'); return; }
  player.versions = [...player.versions, ...more];
  player.saved = { ...player.saved, ...(saved ?? {}) };
  stopScroll();
  buildPlayer();
  showToast(`${more.length} more version${more.length === 1 ? '' : 's'}`);
}

async function saveCurrent() {
  const tab = { ...player.tab };
  delete tab.versions; // UG's list of other versions isn't part of a save
  const res = await api('/chords/songs', { method: 'POST', body: tab });
  if (!res.ok) { showToast(await errorText(res), true); return; }
  const song = await res.json();
  player.savedId = song.id;
  player.song = song;
  if (song.ug_tab_id != null) player.saved[song.ug_tab_id] = song.id;
  // Keep what was set before saving.
  await patchSong({ transpose: player.transpose, scroll_speed: player.speed,
    ...(player.youtube ? { youtube_url: `https://youtu.be/${player.youtube}` } : {}) });
  showToast(`Saved "${song.title}"`);
  await refreshLibrary();
  const wasPlaying = scroll.playing;
  const top = panel.querySelector('#songs-sheet')?.scrollTop ?? 0;
  buildPlayer();
  panel.querySelector('#songs-sheet').scrollTop = top;
  if (wasPlaying) startScroll();
}

async function refreshCurrent() {
  const res = await api(`/chords/songs/${encodeURIComponent(player.savedId)}/refresh`, { method: 'POST' });
  if (!res.ok) { showToast(await errorText(res), true); return; }
  const song = await res.json();
  player.song = song;
  player.tab = savedAsTab(song);
  paintSheet();
  showToast('Updated from Ultimate Guitar');
}

async function deleteCurrent() {
  if (!window.confirm(`Delete "${player.tab.song_name}" from your library?`)) return;
  const res = await api(`/chords/songs/${encodeURIComponent(player.savedId)}`, { method: 'DELETE' });
  if (!res.ok) { showToast(await errorText(res), true); return; }
  showToast('Deleted');
  await refreshLibrary();
  show('library');
}

async function patchSong(body) {
  if (!player?.savedId) return;
  const res = await api(`/chords/songs/${encodeURIComponent(player.savedId)}`, { method: 'PATCH', body });
  if (!res.ok) showToast(await errorText(res), true);
}

// A run of taps on −/+ saves once, after the last one.
let patchTimer = null;
function patchSoon(body) {
  clearTimeout(patchTimer);
  patchTimer = setTimeout(() => patchSong(body), 600);
}

function setTranspose(n) {
  player.transpose = n % 12;
  paintSheet();
  patchSoon({ transpose: player.transpose });
}

function setFont(px) {
  fontSize = Math.min(FONT_MAX, Math.max(FONT_MIN, px));
  store(KEY_FONT, String(fontSize));
  panel.querySelector('#songs-sheet')?.style.setProperty('--songs-font', `${fontSize}px`);
}

// ── Auto-scroll ─────────────────────────────────────────────────────────────

function setAutoscroll(on) {
  scroll.enabled = on;
  store(KEY_AUTOSCROLL, on ? '1' : '0');
  if (!on) stopScroll();
  paintScrollControls();
}

function setSpeed(s) {
  player.speed = clampSpeed(s);
  paintScrollControls();
  patchSoon({ scroll_speed: player.speed });
}

function paintScrollControls() {
  const ctl = panel.querySelector('#songs-scrollctl');
  if (!ctl) return;
  ctl.hidden = !scroll.enabled;
  const toggle = panel.querySelector('#songs-autoscroll');
  toggle.setAttribute('aria-pressed', String(scroll.enabled));
  toggle.classList.toggle('is-active', scroll.enabled);
  const play = panel.querySelector('#songs-play');
  play.textContent = scroll.playing ? '❚❚' : '▶';
  play.setAttribute('aria-label', scroll.playing ? 'Pause' : 'Play');
  play.title = play.getAttribute('aria-label');
  play.classList.toggle('is-active', scroll.playing);
  panel.querySelector('#songs-speed').textContent = `${player?.speed ?? SPEED_DEFAULT}`;
}

const raf = cb => (globalThis.requestAnimationFrame
  ? globalThis.requestAnimationFrame(cb)
  : setTimeout(() => cb(performance.now()), 16));
const cancelRaf = id => (globalThis.cancelAnimationFrame ? globalThis.cancelAnimationFrame(id) : clearTimeout(id));

export function isScrolling() {
  return scroll.playing;
}

function startScroll() {
  if (!scroll.enabled || !player || scroll.playing) return;
  scroll.playing = true;
  scroll.carry = 0;
  scroll.last = 0;
  scroll.frame = raf(step);
  paintScrollControls();
}

function stopScroll() {
  scroll.playing = false;
  if (scroll.frame != null) cancelRaf(scroll.frame);
  scroll.frame = null;
  if (player) paintScrollControls();
}

function step(now) {
  if (!scroll.playing) return;
  const sheet = panel.querySelector('#songs-sheet');
  // Leaving the tab stops the song rather than scrolling a page nobody sees.
  if (!sheet || !panel.classList.contains('active') || view !== 'player') { stopScroll(); return; }
  if (scroll.last && !scroll.held) {
    // scrollTop drops fractions, so carry them — at slow speeds a frame
    // moves less than a pixel and would otherwise never move at all.
    scroll.carry += (player.speed * Math.min(now - scroll.last, 100)) / 1000;
    const whole = Math.floor(scroll.carry);
    if (whole) {
      sheet.scrollTop += whole;
      scroll.carry -= whole;
    }
    if (sheet.scrollTop + sheet.clientHeight >= sheet.scrollHeight - 1) { stopScroll(); return; }
  }
  scroll.last = now;
  scroll.frame = raf(step);
}

function onKey(e) {
  if (view !== 'player' || !panel.classList.contains('active')) return;
  if (e.target.closest?.('input, textarea, select, summary, [contenteditable]')) return;
  if (e.key === ' ' && scroll.enabled) {
    e.preventDefault();
    scroll.playing ? stopScroll() : startScroll();
  } else if (e.key === 'Escape' && document.body.classList.contains('songs-fullscreen')) {
    setFullscreen(false);
  }
}

// ── Screen, full screen ─────────────────────────────────────────────────────

async function keepAwake(on) {
  try {
    if (on && !wakeLock && navigator.wakeLock) {
      wakeLock = await navigator.wakeLock.request('screen');
      wakeLock.addEventListener?.('release', () => { wakeLock = null; });
    } else if (!on && wakeLock) {
      await wakeLock.release();
      wakeLock = null;
    }
  } catch { /* unsupported or refused: the screen may sleep, nothing else breaks */ }
}

function setFullscreen(on) {
  document.body.classList.toggle('songs-fullscreen', on);
  try {
    if (on && !document.fullscreenElement) document.documentElement.requestFullscreen?.()?.catch?.(() => {});
    else if (!on && document.fullscreenElement) document.exitFullscreen?.()?.catch?.(() => {});
  } catch { /* the class alone still hides the dashboard chrome */ }
}

// ── Listen along: YouTube and Spotify ───────────────────────────────────────

function songQuery() {
  return `${player.tab.song_name} ${player.tab.artist_name}`.trim();
}

// Spotify and YouTube share one panel. It is built once and then only hidden
// and shown, so closing it doesn't stop a video that's playing.
function paintListen(box) {
  box.dataset.built = '1';
  box.innerHTML = `
    <div class="songs-pophead"><strong>Listen along</strong>
      <button type="button" class="songs-rbtn" id="songs-listen-close" aria-label="Close" title="Close">✕</button></div>
    <div class="songs-listen-row">
      <button type="button" class="songs-btn songs-primary" id="songs-sp-play">▶ Spotify</button>
      <button type="button" class="songs-btn" id="songs-sp-pause">❚❚ Pause</button>
    </div>
    <p class="songs-hint">Plays "${esc(songQuery())}" on the house speakers through the mesh's Spotify player — no adverts with Spotify Premium.</p>
    <div id="songs-yt-box"></div>`;
  const send = async body => {
    const res = await api('/chords/spotify', { method: 'POST', body });
    if (!res.ok) { showToast(await errorText(res), true); return; }
    const { message } = await res.json();
    showToast(message);
  };
  box.querySelector('#songs-listen-close').addEventListener('click', () => togglePop('songs-listen', false));
  box.querySelector('#songs-sp-play').addEventListener('click', () => send({ action: 'play', query: songQuery() }));
  box.querySelector('#songs-sp-pause').addEventListener('click', () => send({ action: 'pause' }));
  paintYoutube(box.querySelector('#songs-yt-box'));
}

function paintYoutube(box) {
  const id = player.youtube;
  const search = `https://www.youtube.com/results?search_query=${encodeURIComponent(songQuery())}`;
  box.innerHTML = `
    ${id ? `<div class="songs-video"><iframe src="https://www.youtube-nocookie.com/embed/${id}?rel=0&modestbranding=1"
      title="YouTube video" allow="autoplay; encrypted-media; picture-in-picture; fullscreen" allowfullscreen
      referrerpolicy="strict-origin-when-cross-origin"></iframe></div>` : ''}
    <div class="songs-listen-row">
      <a class="songs-btn" href="${esc(search)}" target="_blank" rel="noopener noreferrer">Find on YouTube ↗</a>
    </div>
    <div class="songs-listen-row">
      <input type="url" id="songs-yt-url" placeholder="Paste a YouTube video link" autocomplete="off">
      <button type="button" class="songs-btn" id="songs-yt-use">${id ? 'Change' : 'Use'}</button>
      ${id ? '<button type="button" class="songs-btn" id="songs-yt-clear">Remove</button>' : ''}
    </div>
    <p class="songs-hint">YouTube shows its adverts unless you're signed in to YouTube Premium in this browser.
      ${player.savedId ? 'The video you choose is remembered for this song.' : 'Save the song to remember the video.'}</p>`;
  box.querySelector('#songs-yt-use').addEventListener('click', () => {
    const vid = youtubeId(box.querySelector('#songs-yt-url').value);
    if (!vid) { showToast("That doesn't look like a YouTube video link", true); return; }
    player.youtube = vid;
    patchSong({ youtube_url: `https://youtu.be/${vid}` });
    paintYoutube(box);
  });
  box.querySelector('#songs-yt-clear')?.addEventListener('click', () => {
    player.youtube = null;
    patchSong({ youtube_url: '' });
    paintYoutube(box);
  });
}
