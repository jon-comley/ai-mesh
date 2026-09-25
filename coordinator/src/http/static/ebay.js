import { api } from '/static/api.js';
import { showToast } from '/static/util.js';

// ── Hunts tab (eBay bargain finder) ─────────────────────────────────────────
// A slim sidebar of saved hunts + an editor (paste URL → analyze → term
// chips → timeslot strip → save), with a reverse-chronological find ticker
// as the primary surface. See plans/ebay-bargain-finder.md.

let state = { hunts: [], finds: [], sortBy: 'newest' };
let editingHunt = null; // null while creating a new hunt
let draftTerms = [];
let draftTimeslots = new Set();
// Category and price ceiling: what keeps a hunt for a car from being answered
// with car parts. All optional; null means "search everything".
let draftFilter = { category_id: null, category_name: null, max_price_minor: null };
// The describe-what-you-want conversation for a NEW hunt. Held here and sent
// whole on every turn, because the server keeps nothing between requests.
let chatMessages = [];
let chatBusy = false;

export function init(panel) {
  panel.innerHTML = `
    <h2>Hunts</h2>
    <div class="ebay-layout">
      <aside class="ebay-sidebar">
        <button id="ebay-new-hunt" type="button">+ New hunt</button>
        <div id="ebay-hunt-list"><p class="placeholder">No hunts yet.</p></div>
        <details class="ebay-settings">
          <summary>eBay &amp; notification settings</summary>
          <div class="gw">
            <div class="gw-field gw-inline">
              <label for="ebay-client-id">Client ID</label>
              <input id="ebay-client-id" type="text" autocomplete="off">
              <button id="ebay-client-id-save" type="button">Save</button>
            </div>
            <div class="gw-field gw-inline">
              <label for="ebay-client-secret">Client secret</label>
              <input id="ebay-client-secret" type="password" autocomplete="off">
              <button id="ebay-client-secret-save" type="button">Save</button>
            </div>
            <span id="ebay-secret-status" class="gw-hint"></span>
            <div class="gw-field gw-inline">
              <label for="ebay-ntfy">ntfy topic</label>
              <input id="ebay-ntfy" type="text" autocomplete="off" placeholder="https://ntfy.sh/your-private-topic">
              <button id="ebay-ntfy-save" type="button">Save</button>
            </div>
          </div>
        </details>
      </aside>
      <section class="ebay-main">
        <div id="ebay-editor" class="ebay-editor" hidden></div>
        <div class="ebay-sortbar">
          <span class="gw-hint">Order</span>
          <button type="button" class="ebay-sort" data-sort="newest">Newest</button>
          <button type="button" class="ebay-sort" data-sort="score">Best fit</button>
        </div>
        <div id="ebay-ticker"><p class="placeholder">No finds yet.</p></div>
      </section>
    </div>
  `;

  panel.querySelector('#ebay-new-hunt').addEventListener('click', () => openEditor(null));
  panel.querySelectorAll('.ebay-sort').forEach(btn => btn.addEventListener('click', () => {
    state.sortBy = btn.dataset.sort;
    syncSortControl();
    sortFinds(state.finds);
    renderTicker();
  }));
  syncSortControl();
  wireSettings(panel);

  refreshConfig();
  refreshHunts();
  refreshFinds();
}

// ── settings ─────────────────────────────────────────────────────────────

function wireSettings(panel) {
  panel.querySelector('#ebay-client-id-save').addEventListener('click', async () => {
    const input = panel.querySelector('#ebay-client-id');
    const res = await api('/ebay/config', { method: 'POST', body: { client_id: input.value.trim() } });
    if (res.ok) { input.value = ''; renderConfig(await res.json()); }
  });
  panel.querySelector('#ebay-client-secret-save').addEventListener('click', async () => {
    const input = panel.querySelector('#ebay-client-secret');
    const res = await api('/ebay/config', { method: 'POST', body: { client_secret: input.value } });
    if (res.ok) { input.value = ''; renderConfig(await res.json()); }
  });
  panel.querySelector('#ebay-ntfy-save').addEventListener('click', async () => {
    const input = panel.querySelector('#ebay-ntfy');
    const res = await api('/ebay/config', { method: 'POST', body: { ntfy_topic_url: input.value.trim() } });
    if (res.ok) renderConfig(await res.json());
  });
}

async function refreshConfig() {
  try {
    const res = await api('/ebay/config');
    if (res.ok) renderConfig(await res.json());
  } catch { /* dashboard shows disconnected state elsewhere */ }
}

function renderConfig(cfg) {
  const idInput = document.getElementById('ebay-client-id');
  if (idInput) idInput.placeholder = cfg.client_id_set ? 'set' : 'not set';
  const secretStatus = document.getElementById('ebay-secret-status');
  if (secretStatus) {
    secretStatus.textContent = cfg.client_secret_set ? `secret set ${cfg.client_secret_hint ?? ''}` : 'secret not set';
  }
  const ntfyInput = document.getElementById('ebay-ntfy');
  if (ntfyInput && document.activeElement !== ntfyInput) ntfyInput.value = cfg.ntfy_topic_url ?? '';
}

// ── hunts sidebar ────────────────────────────────────────────────────────

async function refreshHunts() {
  try {
    const res = await api('/ebay/hunts');
    if (res.ok) { state.hunts = await res.json(); renderHuntList(); }
  } catch { /* dashboard shows disconnected state elsewhere */ }
}

function renderHuntList() {
  const box = document.getElementById('ebay-hunt-list');
  if (!box) return;
  if (!state.hunts.length) {
    box.innerHTML = '<p class="placeholder">No hunts yet.</p>';
    return;
  }
  // Two buttons per hunt, not one. "Check now" used to exist only inside the
  // editor, so running a hunt meant opening it for editing first and the
  // sidebar looked like it had no run control at all (Jon, 2026-09-14: "I
  // don't see a run button"). A row cannot stay a single <button> with another
  // button inside it — nested buttons are invalid and the inner one's clicks
  // are the outer one's — so the row is a div holding both.
  box.innerHTML = state.hunts.map(h => `
    <div class="ebay-hunt-row${h.enabled ? '' : ' ebay-hunt-off'}">
      <button type="button" class="ebay-hunt-open" data-id="${escapeHtml(h.id)}">
        <span>${escapeHtml(h.name)}</span>
        <span class="gw-hint">${h.enabled ? 'on' : 'off'}</span>
      </button>
      <button type="button" class="ebay-hunt-run" data-id="${escapeHtml(h.id)}" title="Check eBay for this hunt now">Run</button>
    </div>`).join('');
  box.querySelectorAll('.ebay-hunt-open').forEach(btn => btn.addEventListener('click', () => {
    const hunt = state.hunts.find(h => h.id === btn.dataset.id);
    if (hunt) openEditor(hunt);
  }));
  box.querySelectorAll('.ebay-hunt-run').forEach(btn =>
    btn.addEventListener('click', () => runNow(btn.dataset.id, btn)));
}

// ── editor ───────────────────────────────────────────────────────────────

function openEditor(hunt) {
  editingHunt = hunt;
  draftTerms = hunt ? hunt.terms.map(t => ({ ...t })) : [];
  draftTimeslots = new Set(hunt ? hunt.timeslots : []);
  draftFilter = {
    category_id: hunt?.category_id ?? null,
    category_name: hunt?.category_name ?? null,
    max_price_minor: hunt?.max_price_minor ?? null,
  };
  chatMessages = [];
  chatBusy = false;

  const editor = document.getElementById('ebay-editor');
  editor.hidden = false;
  editor.innerHTML = `
    <h3>${hunt ? 'Edit hunt' : 'New hunt'}</h3>
    ${hunt ? '' : `
    <div class="gw-field ebay-chat">
      <span class="gw-label">Describe what you want</span>
      <div id="ebay-chat-log" class="ebay-chat-log" role="log" aria-live="polite" hidden></div>
      <div class="gw-field gw-inline">
        <input id="ebay-chat-input" type="text" autocomplete="off" placeholder="e.g. a cheap runaround car under £1500">
        <button id="ebay-chat-send" type="button">Send</button>
      </div>
      <span class="gw-hint">The form below fills in as you talk. Change anything before you create the hunt.</span>
    </div>
    <div class="gw-field gw-inline">
      <label for="ebay-url">…or a listing</label>
      <input id="ebay-url" type="text" autocomplete="off" placeholder="paste an eBay listing URL…">
      <button id="ebay-analyze" type="button">Analyze</button>
    </div>`}
    <div class="gw-field">
      <label for="ebay-name">Hunt name</label>
      <input id="ebay-name" type="text" value="${escapeHtml(hunt?.name ?? '')}">
    </div>
    <div class="gw-field">
      <label for="ebay-goal">What it's for</label>
      <textarea id="ebay-goal" rows="2" placeholder="e.g. headless CI runner — core count matters most, then RAM; storage secondary">${escapeHtml(hunt?.goal ?? '')}</textarea>
      <span class="gw-hint">Optional, and the single biggest lever on how good the
        verdicts and ranking are. Without it the LLM only knows the hunt name, so it
        scores similarity to that title rather than fitness for the job.</span>
    </div>
    <div class="gw-field">
      <span class="gw-label">Keep it to</span>
      <div id="ebay-filter-category" class="ebay-chips"></div>
      <div class="gw-field gw-inline">
        <label for="ebay-max-price">Up to (£)</label>
        <input id="ebay-max-price" type="number" inputmode="decimal" min="0" step="any" placeholder="no limit" value="${escapeHtml(minorToInput(draftFilter.max_price_minor))}">
      </div>
      <span class="gw-hint">A category stops a hunt for a car being answered with car parts.</span>
    </div>
    <div class="gw-field">
      <span class="gw-label">Search terms</span>
      <div id="ebay-terms" class="ebay-chips"></div>
      <div class="gw-field gw-inline">
        <input id="ebay-term-add" type="text" placeholder="add a term…">
        <button id="ebay-term-add-btn" type="button">Add</button>
      </div>
    </div>
    <div class="gw-field">
      <span class="gw-label">Daily timeslots</span>
      <div id="ebay-timeslot-chips" class="ebay-chips"></div>
      <div class="gw-field gw-inline">
        <input id="ebay-timeslot-add" type="time" value="00:00">
        <button id="ebay-timeslot-add-btn" type="button">Add</button>
      </div>
    </div>
    <div class="gw-field gw-inline">
      <button id="ebay-save" type="button">${hunt ? 'Save' : 'Create hunt'}</button>
      <button id="ebay-cancel" type="button">Cancel</button>
      ${hunt ? `
      <button id="ebay-run-now" type="button">Check now</button>
      <button id="ebay-rank" type="button" title="Refresh from eBay, then score every live find against 'What it's for'">Rank</button>
      <button id="ebay-toggle-enabled" type="button">${hunt.enabled ? 'Disable' : 'Enable'}</button>
      <button id="ebay-delete" type="button">Delete</button>` : ''}
    </div>
  `;

  editor.querySelector('#ebay-cancel').addEventListener('click', closeEditor);
  editor.querySelector('#ebay-save').addEventListener('click', saveHunt);
  const addTerm = () => {
    const input = editor.querySelector('#ebay-term-add');
    const text = input.value.trim();
    if (text) {
      draftTerms.push({ text, enabled: true, is_misspelling: false });
      input.value = '';
      renderTermChips();
    }
  };
  editor.querySelector('#ebay-term-add-btn').addEventListener('click', addTerm);
  editor.querySelector('#ebay-term-add').addEventListener('keydown', e => {
    if (e.key === 'Enter') addTerm();
  });
  if (!hunt) {
    editor.querySelector('#ebay-analyze').addEventListener('click', analyzeUrl);
    editor.querySelector('#ebay-chat-send').addEventListener('click', sendChat);
    editor.querySelector('#ebay-chat-input').addEventListener('keydown', e => {
      if (e.key === 'Enter') sendChat();
    });
  } else {
    const runBtn = editor.querySelector('#ebay-run-now');
    runBtn.addEventListener('click', () => runNow(hunt.id, runBtn));
    const rankBtn = editor.querySelector('#ebay-rank');
    rankBtn.addEventListener('click', () => rankHunt(hunt, rankBtn));
    editor.querySelector('#ebay-toggle-enabled').addEventListener('click', () => toggleEnabled(hunt));
    editor.querySelector('#ebay-delete').addEventListener('click', () => deleteHunt(hunt.id));
  }
  const addTimeslot = () => {
    const input = editor.querySelector('#ebay-timeslot-add');
    const [h, m] = (input.value || '').split(':').map(Number);
    if (Number.isNaN(h) || Number.isNaN(m)) return;
    draftTimeslots.add(h * 60 + m);
    renderTimeslotChips();
  };
  editor.querySelector('#ebay-timeslot-add-btn').addEventListener('click', addTimeslot);
  editor.querySelector('#ebay-timeslot-add').addEventListener('input', syncTimeslotAddButton);
  editor.querySelector('#ebay-timeslot-add').addEventListener('keydown', e => {
    if (e.key === 'Enter') addTimeslot();
  });

  renderTermChips();
  renderTimeslotChips();
  renderFilterCategory();
}

// "1500" → 150000 pence, and "" or nonsense or zero → null (no ceiling).
function poundsToMinor(text) {
  const n = parseFloat(text);
  return Number.isFinite(n) && n > 0 ? Math.round(n * 100) : null;
}

function minorToInput(minor) {
  return minor == null ? '' : String(minor / 100);
}

function renderFilterCategory() {
  const box = document.getElementById('ebay-filter-category');
  if (!box) return;
  if (!draftFilter.category_id) {
    box.innerHTML = '<p class="placeholder">Any category. The chat picks one for you, or Analyze copies the listing\'s.</p>';
    return;
  }
  box.innerHTML = `
    <span class="ebay-chip">
      <span>${escapeHtml(draftFilter.category_name || draftFilter.category_id)}</span>
      <button type="button" class="ebay-chip-remove" aria-label="remove category">×</button>
    </span>`;
  box.querySelector('.ebay-chip-remove').addEventListener('click', () => {
    draftFilter.category_id = null;
    draftFilter.category_name = null;
    renderFilterCategory();
  });
}

function renderChat() {
  const log = document.getElementById('ebay-chat-log');
  if (!log) return;
  log.hidden = chatMessages.length === 0;
  log.innerHTML = chatMessages.map(m =>
    `<div class="ebay-chat-msg ebay-chat-${m.role}${m.error ? ' ebay-chat-error' : ''}">${escapeHtml(m.content)}</div>`
  ).join('');
  log.scrollTop = log.scrollHeight;
}

// A draft from the chat replaces the form: the model sends the WHOLE draft again
// when the person asks for a change, so replacing is right, and it means the last
// thing said is what the form shows. Timeslots are the person's own and stay.
function applyDraft(d) {
  const name = document.getElementById('ebay-name');
  if (name) name.value = d.name ?? '';
  const goal = document.getElementById('ebay-goal');
  if (goal) goal.value = d.goal ?? '';
  draftTerms = (d.terms ?? []).map(t => ({ ...t }));
  draftFilter = {
    category_id: d.category_id ?? null,
    category_name: d.category_name ?? null,
    max_price_minor: d.max_price_minor ?? null,
  };
  const price = document.getElementById('ebay-max-price');
  if (price) price.value = minorToInput(draftFilter.max_price_minor);
  renderTermChips();
  renderFilterCategory();
}

async function sendChat() {
  const input = document.getElementById('ebay-chat-input');
  const text = input?.value.trim();
  if (!text || chatBusy) return;

  chatMessages.push({ role: 'user', content: text });
  input.value = '';
  chatBusy = true;
  const btn = document.getElementById('ebay-chat-send');
  if (btn) { btn.disabled = true; btn.textContent = 'Thinking…'; }
  renderChat();

  try {
    // Error lines are for the person to read, not for the model to be told about.
    const history = chatMessages.filter(m => !m.error).map(({ role, content }) => ({ role, content }));
    const res = await api('/ebay/chat', { method: 'POST', body: { messages: history } });
    if (!res.ok) {
      chatMessages.push({ role: 'assistant', content: (await res.text()) || 'That did not work.', error: true });
      return;
    }
    const data = await res.json();
    chatMessages.push({ role: 'assistant', content: data.reply });
    if (data.draft) applyDraft(data.draft);
  } catch (e) {
    chatMessages.push({ role: 'assistant', content: `That did not work: ${e}`, error: true });
  } finally {
    chatBusy = false;
    const again = document.getElementById('ebay-chat-send');
    if (again) { again.disabled = false; again.textContent = 'Send'; }
    renderChat();
  }
}

function closeEditor() {
  editingHunt = null;
  const editor = document.getElementById('ebay-editor');
  if (editor) { editor.hidden = true; editor.innerHTML = ''; }
}

function renderTermChips() {
  const box = document.getElementById('ebay-terms');
  if (!box) return;
  if (!draftTerms.length) {
    box.innerHTML = '<p class="placeholder">No terms yet. Describe what you want above, paste a listing, or add one below.</p>';
    return;
  }
  box.innerHTML = draftTerms.map((t, i) => `
    <span class="ebay-chip${t.enabled ? '' : ' ebay-chip-off'}">
      <button type="button" class="ebay-chip-toggle" data-idx="${i}">${escapeHtml(t.text)}${t.is_misspelling ? ' <em>typo</em>' : ''}</button>
      <button type="button" class="ebay-chip-remove" data-idx="${i}" aria-label="remove term">×</button>
    </span>`).join('');
  box.querySelectorAll('.ebay-chip-toggle').forEach(btn => btn.addEventListener('click', () => {
    const t = draftTerms[+btn.dataset.idx];
    t.enabled = !t.enabled;
    renderTermChips();
  }));
  box.querySelectorAll('.ebay-chip-remove').forEach(btn => btn.addEventListener('click', () => {
    draftTerms.splice(+btn.dataset.idx, 1);
    renderTermChips();
  }));
}

function renderTimeslotChips() {
  const box = document.getElementById('ebay-timeslot-chips');
  if (!box) return;
  const sorted = Array.from(draftTimeslots).sort((a, b) => a - b);
  if (!sorted.length) {
    box.innerHTML = '<p class="placeholder">No timeslots yet — add one below.</p>';
    return;
  }
  box.innerHTML = sorted.map(m => {
    const hh = String(Math.floor(m / 60)).padStart(2, '0');
    const mm = String(m % 60).padStart(2, '0');
    return `<span class="ebay-chip">
      <span>${hh}:${mm}</span>
      <button type="button" class="ebay-chip-remove" data-minute="${m}" aria-label="remove timeslot">×</button>
    </span>`;
  }).join('');
  box.querySelectorAll('.ebay-chip-remove').forEach(btn => btn.addEventListener('click', () => {
    draftTimeslots.delete(+btn.dataset.minute);
    renderTimeslotChips();
  }));
  syncTimeslotAddButton();
}

// Disabled once the picker's current value is already in draftTimeslots —
// re-enabled by the 'input' listener the moment the value changes, so the
// same time can't be added twice via a stray extra click.
function syncTimeslotAddButton() {
  const input = document.getElementById('ebay-timeslot-add');
  const btn = document.getElementById('ebay-timeslot-add-btn');
  if (!input || !btn) return;
  const [h, m] = (input.value || '').split(':').map(Number);
  const minute = Number.isNaN(h) || Number.isNaN(m) ? null : h * 60 + m;
  btn.disabled = minute === null || draftTimeslots.has(minute);
}

async function analyzeUrl() {
  const input = document.getElementById('ebay-url');
  const url = input?.value.trim();
  if (!url) return;
  const btn = document.getElementById('ebay-analyze');
  if (btn) { btn.disabled = true; btn.textContent = 'Analyzing…'; }
  try {
    const res = await api('/ebay/analyze', { method: 'POST', body: { url } });
    if (!res.ok) { showToast(`Analyze failed: ${await res.text()}`, true); return; }
    const data = await res.json();
    draftTerms = data.terms;
    // The listing's own category, so the hunt searches inside it.
    draftFilter.category_id = data.category_id ?? null;
    draftFilter.category_name = data.category_name ?? null;
    const nameInput = document.getElementById('ebay-name');
    if (nameInput && !nameInput.value.trim()) nameInput.value = data.title;
    renderTermChips();
    renderFilterCategory();
  } catch (e) {
    showToast(`Analyze failed: ${e}`, true);
  } finally {
    if (btn) { btn.disabled = false; btn.textContent = 'Analyze'; }
  }
}

async function saveHunt() {
  const name = document.getElementById('ebay-name')?.value.trim();
  if (!name) { showToast('Hunt name is required', true); return; }
  const body = {
    name,
    terms: draftTerms,
    timeslots: Array.from(draftTimeslots).sort((a, b) => a - b),
    goal: document.getElementById('ebay-goal')?.value.trim() ?? '',
    // Empty and zero mean "none": on an edit that is how a filter is cleared.
    category_id: draftFilter.category_id ?? '',
    category_name: draftFilter.category_name ?? '',
    max_price_minor: poundsToMinor(document.getElementById('ebay-max-price')?.value) ?? 0,
  };
  let res;
  if (editingHunt) {
    res = await api(`/ebay/hunts/${encodeURIComponent(editingHunt.id)}`, { method: 'PATCH', body });
  } else {
    body.source_url = document.getElementById('ebay-url')?.value.trim() ?? '';
    body.marketplace = 'EBAY_GB';
    res = await api('/ebay/hunts', { method: 'POST', body });
  }
  if (!res.ok) { showToast(`Save failed: ${await res.text()}`, true); return; }
  closeEditor();
  refreshHunts();
}

// A run is several seconds of eBay search plus an LLM verdict per new
// listing. Without a busy state the button did nothing visible for that whole
// time and read as broken (Jon, 2026-09-14: "the button doesn't do the pressed
// change colour thing and doesn't look like it has been pressed"). Disabling
// it also makes double-clicking a hunt impossible, which previously fired two
// concurrent searches against a rate-limited API.
//
// `btn` is optional so a caller without one still works; the toast is the
// feedback in that case.
// Hold a button in a visible working state for the whole request. Everything
// here is slow enough (an eBay round-trip, an LLM call) that without it the
// control reads as dead.
async function whileBusy(btn, label, work) {
  const restore = btn ? btn.textContent : null;
  if (btn) {
    btn.disabled = true;
    btn.classList.add('is-busy');
    btn.textContent = label;
  }
  try {
    return await work();
  } finally {
    // In `finally` so a thrown request cannot leave the button stuck on its
    // busy label with no way back short of a reload.
    if (btn) {
      btn.disabled = false;
      btn.classList.remove('is-busy');
      btn.textContent = restore;
    }
  }
}

// Score every live find for this hunt against its goal, in one LLM call, and
// switch the ticker to best-fit order so the answer is visible immediately —
// ranking that leaves the list in date order has not told anyone anything.
async function rankHunt(hunt, btn) {
  if (!hunt.goal?.trim() && !window.confirm(
    `"${hunt.name}" has no "What it's for" set, so the ranking can only go on the hunt name. Rank anyway?`)) {
    return;
  }
  await whileBusy(btn, 'Ranking…', async () => {
    const res = await api(`/ebay/hunts/${encodeURIComponent(hunt.id)}/rank`, { method: 'POST' });
    if (!res.ok) { showToast(`Ranking failed: ${await res.text()}`, true); return; }
    const data = await res.json();
    if (data.refresh_error) {
      // Said out loud rather than swallowed: a ranking over a set that could
      // not be refreshed may be led by something already sold.
      showToast(`Ranked ${data.scored} of ${data.considered} — but eBay refresh failed (${data.refresh_error}), so this may include sold listings`, true);
    } else {
      showToast(`Ranked ${data.scored} of ${data.considered} find(s)`);
    }
    state.sortBy = 'score';
    syncSortControl();
    await refreshFinds();
  });
}

async function runNow(id, btn) {
  const restore = btn ? btn.textContent : null;
  if (btn) {
    btn.disabled = true;
    btn.classList.add('is-busy');
    btn.textContent = 'Checking…';
  }
  try {
    const res = await api(`/ebay/hunts/${encodeURIComponent(id)}/run-now`, { method: 'POST' });
    if (res.ok) {
      const data = await res.json();
      showToast(`Checked — ${data.new_listings} new listing(s)`);
      refreshFinds();
    } else {
      showToast(`Check failed: ${await res.text()}`, true);
    }
  } catch (err) {
    showToast(`Check failed: ${err}`, true);
  } finally {
    // In `finally` so a thrown request cannot leave the button stuck on
    // "Checking…" with no way back short of a reload.
    if (btn) {
      btn.disabled = false;
      btn.classList.remove('is-busy');
      btn.textContent = restore;
    }
  }
}

async function toggleEnabled(hunt) {
  const res = await api(`/ebay/hunts/${encodeURIComponent(hunt.id)}`, {
    method: 'PATCH',
    body: { enabled: !hunt.enabled },
  });
  if (res.ok) { closeEditor(); refreshHunts(); }
}

async function deleteHunt(id) {
  if (!window.confirm('Delete this hunt?')) return;
  const res = await api(`/ebay/hunts/${encodeURIComponent(id)}`, { method: 'DELETE' });
  // The finds go with the hunt server-side (`ebay_finds.hunt_id` is ON DELETE
  // CASCADE), so the ticker has to be re-fetched too — refreshing only the
  // sidebar left every find of a just-deleted hunt sitting on screen until
  // some unrelated event happened to reload them, which reads as the delete
  // having half worked.
  if (res.ok) { closeEditor(); refreshHunts(); refreshFinds(); }
}

// ── ticker ───────────────────────────────────────────────────────────────

async function refreshFinds() {
  try {
    const res = await api('/ebay/finds');
    if (res.ok) { state.finds = sortFinds(await res.json()); renderTicker(); }
  } catch { /* dashboard shows disconnected state elsewhere */ }
}

// Newest first, with dismissed finds below every live one. The server orders
// the same way (`list_finds`), so a reload and a live push agree.
//
// **This used to sort bargains to the top and that was actively hiding the
// newest finds** — the server applies a LIMIT after ordering, and once the
// bargain count passed the limit nothing recent came back at all. The verdict
// still matters, so it moved from position to a badge: see `renderTicker`.
// `process_hunt_results` writes verdicts as "bargain: …" / "not a bargain: …".
function isBargain(f) {
  return typeof f.verdict === 'string' && f.verdict.startsWith('bargain:');
}

// How much of the term that matched actually appears in the title, 0..1.
//
// eBay's search is fuzzy and joins a hunt's terms with " OR ", which is why an
// M920q hunt returns M900s and M710qs at all. The API gives no relevance score
// back, so this recomputes one from what we do have: the words of
// `matched_term` against the words of the title. Punctuation-insensitive
// because "i5-8500T" and "i5 8500T" are the same machine, and set-based
// because word order in an eBay title means nothing.
//
// Deliberately not a substring test on the whole term: no real title contains
// "Lenovo ThinkCentre M920q Tiny PC i5 8500T 16GB RAM 512GB SSD WiFi Warranty"
// verbatim, so that would rank everything equally at zero.
function termWords(s) {
  return String(s ?? '').toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
}

export function keywordCoverage(find) {
  const wanted = termWords(find?.matched_term);
  if (!wanted.length) return 0;
  const have = new Set(termWords(find?.title));
  return wanted.filter(w => have.has(w)).length / wanted.length;
}

// A title carrying every keyword of the term that found it. Jon, 2026-09-14:
// "lets have matches that match the keywords exactly at the top."
function isExactMatch(f) {
  return keywordCoverage(f) === 1;
}

// Exact keyword matches, then newest first, with dismissed below everything.
//
// **Only the exact tier is allowed to jump the queue, and that is on purpose.**
// The previous attempt at a quality-first sort (bargains) put a large,
// ever-growing group above recency and buried every recent find behind it. An
// exact match is rare by construction — 2 of 212 finds on the day this went in
// — so it is a handful of rows at the top rather than a wall. Everything below
// stays newest-first, and the near-misses carry a visible match percentage
// instead of a position, the same trade the bargain badge makes.
function sortFinds(finds) {
  // Dismissed is below everything in both modes — it means "I have dealt with
  // this", which outranks any opinion about quality or recency.
  //
  // Best fit falls back to the newest ordering for anything unscored, rather
  // than treating a missing score as zero: a find the model never mentioned,
  // or one stored before ranking existed, has not been judged badly. Unscored
  // rows sit below scored ones so the ranking is not interleaved with noise.
  if (state.sortBy === 'score') {
    return finds.sort((a, b) =>
      (!!a.reviewed - !!b.reviewed)
      || ((a.score == null) - (b.score == null))
      || ((b.score ?? 0) - (a.score ?? 0))
      || (b.found_ms - a.found_ms));
  }
  return finds.sort((a, b) =>
    (!!a.reviewed - !!b.reviewed)
    || (isExactMatch(b) - isExactMatch(a))
    || (b.found_ms - a.found_ms));
}

function syncSortControl() {
  document.querySelectorAll('.ebay-sort').forEach(btn =>
    btn.classList.toggle('is-active', btn.dataset.sort === state.sortBy));
}

// "14 Sep, 06:00" — short enough for the hint line, unambiguous about which
// day, and local time because that is the clock the person reading it is on.
// A find with no usable timestamp says so rather than rendering "Invalid Date".
function formatFound(ms) {
  if (typeof ms !== 'number' || !Number.isFinite(ms)) return 'date unknown';
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return 'date unknown';
  return d.toLocaleString(undefined, {
    day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit',
  });
}

// Called from dashboard.js's WS handler map on a live `EbayFind` event.
export function handleFind(evt) {
  state.finds.unshift(evt.find);
  sortFinds(state.finds);
  renderTicker();
  if (!document.getElementById('panel-ebay')?.classList.contains('active')) {
    showToast(`eBay: new find for "${evt.hunt_name}" — ${evt.find.title}`);
  }
}

function renderTicker() {
  const box = document.getElementById('ebay-ticker');
  if (!box) return;
  if (!state.finds.length) {
    box.innerHTML = '<p class="placeholder">No finds yet.</p>';
    return;
  }
  box.innerHTML = state.finds.map(f => `
    <div class="ebay-find${f.reviewed ? ' ebay-find-reviewed' : ''}${isBargain(f) ? ' ebay-find-bargain' : ''}${isExactMatch(f) ? ' ebay-find-exact' : ''}">
      ${f.image_url ? `<img class="ebay-find-thumb" src="${escapeHtml(f.image_url)}" alt="">` : '<div class="ebay-find-thumb ebay-find-thumb-empty"></div>'}
      <div class="ebay-find-body">
        <a href="${escapeHtml(f.item_web_url)}" target="_blank" rel="noopener noreferrer">${escapeHtml(f.title)}</a>
        <div class="gw-hint">
          <time datetime="${escapeHtml(new Date(f.found_ms).toISOString?.() ?? '')}" class="ebay-find-when">${escapeHtml(formatFound(f.found_ms))}</time>
          · ${f.price_minor != null ? `${(f.price_minor / 100).toFixed(2)} ${escapeHtml(f.currency ?? '')}` : 'price unknown'}
          · ${isExactMatch(f)
            ? '<span class="ebay-badge ebay-badge-exact">exact</span>'
            : `<span class="ebay-match-pct">${Math.round(keywordCoverage(f) * 100)}% match</span>`}
          · matched "${escapeHtml(f.matched_term)}"
          ${f.score != null ? `· <span class="ebay-score" title="Fitness for this hunt's goal, 0-100">${f.score}/100</span>` : ''}
        </div>
        <div class="ebay-verdict${f.verdict ? '' : ' gw-hint'}">${isBargain(f) ? '<span class="ebay-badge">bargain</span> ' : ''}${f.verdict ? escapeHtml(f.verdict) : 'not yet judged'}</div>
      </div>
      <button type="button" class="ebay-dismiss" data-id="${escapeHtml(f.id)}" ${f.reviewed ? 'disabled' : ''}>${f.reviewed ? '✓' : 'Dismiss'}</button>
    </div>`).join('');
  box.querySelectorAll('.ebay-dismiss').forEach(btn => btn.addEventListener('click', () => markReviewed(btn.dataset.id)));
}

async function markReviewed(id) {
  const res = await api(`/ebay/finds/${encodeURIComponent(id)}/reviewed`, { method: 'POST' });
  if (res.ok) {
    const f = state.finds.find(x => x.id === id);
    if (f) f.reviewed = true;
    // Re-sort before rendering, so a dismissed find drops to the bottom on the
    // press rather than only after the next reload.
    sortFinds(state.finds);
    renderTicker();
  }
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
