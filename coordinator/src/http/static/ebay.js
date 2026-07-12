import { api } from '/static/api.js';
import { showToast } from '/static/util.js';

// ── Hunts tab (eBay bargain finder) ─────────────────────────────────────────
// A slim sidebar of saved hunts + an editor (paste URL → analyze → term
// chips → timeslot strip → save), with a reverse-chronological find ticker
// as the primary surface. See plans/ebay-bargain-finder.md.

let state = { hunts: [], finds: [] };
let editingHunt = null; // null while creating a new hunt
let draftTerms = [];
let draftTimeslots = new Set();

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
        <div id="ebay-ticker"><p class="placeholder">No finds yet.</p></div>
      </section>
    </div>
  `;

  panel.querySelector('#ebay-new-hunt').addEventListener('click', () => openEditor(null));
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
  box.innerHTML = state.hunts.map(h => `
    <button type="button" class="ebay-hunt-row${h.enabled ? '' : ' ebay-hunt-off'}" data-id="${escapeHtml(h.id)}">
      <span>${escapeHtml(h.name)}</span>
      <span class="gw-hint">${h.enabled ? 'on' : 'off'}</span>
    </button>`).join('');
  box.querySelectorAll('.ebay-hunt-row').forEach(btn => btn.addEventListener('click', () => {
    const hunt = state.hunts.find(h => h.id === btn.dataset.id);
    if (hunt) openEditor(hunt);
  }));
}

// ── editor ───────────────────────────────────────────────────────────────

function openEditor(hunt) {
  editingHunt = hunt;
  draftTerms = hunt ? hunt.terms.map(t => ({ ...t })) : [];
  draftTimeslots = new Set(hunt ? hunt.timeslots : []);

  const editor = document.getElementById('ebay-editor');
  editor.hidden = false;
  editor.innerHTML = `
    <h3>${hunt ? 'Edit hunt' : 'New hunt'}</h3>
    ${hunt ? '' : `
    <div class="gw-field gw-inline">
      <label for="ebay-url">Item URL</label>
      <input id="ebay-url" type="text" autocomplete="off" placeholder="paste an eBay listing URL…">
      <button id="ebay-analyze" type="button">Analyze</button>
    </div>`}
    <div class="gw-field">
      <label for="ebay-name">Hunt name</label>
      <input id="ebay-name" type="text" value="${escapeHtml(hunt?.name ?? '')}">
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
      <div id="ebay-timeslots" class="ebay-timeslots"></div>
    </div>
    <div class="gw-field gw-inline">
      <button id="ebay-save" type="button">${hunt ? 'Save' : 'Create hunt'}</button>
      <button id="ebay-cancel" type="button">Cancel</button>
      ${hunt ? `
      <button id="ebay-run-now" type="button">Check now</button>
      <button id="ebay-toggle-enabled" type="button">${hunt.enabled ? 'Disable' : 'Enable'}</button>
      <button id="ebay-delete" type="button">Delete</button>` : ''}
    </div>
  `;

  editor.querySelector('#ebay-cancel').addEventListener('click', closeEditor);
  editor.querySelector('#ebay-save').addEventListener('click', saveHunt);
  editor.querySelector('#ebay-term-add-btn').addEventListener('click', () => {
    const input = editor.querySelector('#ebay-term-add');
    const text = input.value.trim();
    if (text) {
      draftTerms.push({ text, enabled: true, is_misspelling: false });
      input.value = '';
      renderTermChips();
    }
  });
  if (!hunt) {
    editor.querySelector('#ebay-analyze').addEventListener('click', analyzeUrl);
  } else {
    editor.querySelector('#ebay-run-now').addEventListener('click', () => runNow(hunt.id));
    editor.querySelector('#ebay-toggle-enabled').addEventListener('click', () => toggleEnabled(hunt));
    editor.querySelector('#ebay-delete').addEventListener('click', () => deleteHunt(hunt.id));
  }

  renderTermChips();
  renderTimeslotGrid();
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
    box.innerHTML = '<p class="placeholder">No terms yet — paste a URL and Analyze, or add one below.</p>';
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

// 24 hourly cells (minutes-since-midnight) — a good enough default
// granularity; half-hour cells would just double this array.
function renderTimeslotGrid() {
  const box = document.getElementById('ebay-timeslots');
  if (!box) return;
  box.innerHTML = Array.from({ length: 24 }, (_, h) => {
    const minute = h * 60;
    const on = draftTimeslots.has(minute);
    return `<button type="button" class="ebay-slot${on ? ' ebay-slot-on' : ''}" data-minute="${minute}">${String(h).padStart(2, '0')}</button>`;
  }).join('');
  box.querySelectorAll('.ebay-slot').forEach(btn => btn.addEventListener('click', () => {
    const m = +btn.dataset.minute;
    if (draftTimeslots.has(m)) draftTimeslots.delete(m); else draftTimeslots.add(m);
    renderTimeslotGrid();
  }));
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
    const nameInput = document.getElementById('ebay-name');
    if (nameInput && !nameInput.value.trim()) nameInput.value = data.title;
    renderTermChips();
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

async function runNow(id) {
  const res = await api(`/ebay/hunts/${encodeURIComponent(id)}/run-now`, { method: 'POST' });
  if (res.ok) {
    const data = await res.json();
    showToast(`Checked — ${data.new_listings} new listing(s)`);
    refreshFinds();
  } else {
    showToast(`Check failed: ${await res.text()}`, true);
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
  if (res.ok) { closeEditor(); refreshHunts(); }
}

// ── ticker ───────────────────────────────────────────────────────────────

async function refreshFinds() {
  try {
    const res = await api('/ebay/finds');
    if (res.ok) { state.finds = await res.json(); renderTicker(); }
  } catch { /* dashboard shows disconnected state elsewhere */ }
}

// Called from dashboard.js's WS handler map on a live `EbayFind` event.
export function handleFind(evt) {
  state.finds.unshift(evt.find);
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
    <div class="ebay-find${f.reviewed ? ' ebay-find-reviewed' : ''}">
      ${f.image_url ? `<img class="ebay-find-thumb" src="${escapeHtml(f.image_url)}" alt="">` : '<div class="ebay-find-thumb ebay-find-thumb-empty"></div>'}
      <div class="ebay-find-body">
        <a href="${escapeHtml(f.item_web_url)}" target="_blank" rel="noopener noreferrer">${escapeHtml(f.title)}</a>
        <div class="gw-hint">
          ${f.price_minor != null ? `${(f.price_minor / 100).toFixed(2)} ${escapeHtml(f.currency ?? '')}` : 'price unknown'}
          · matched "${escapeHtml(f.matched_term)}"
        </div>
        <div class="ebay-verdict${f.verdict ? '' : ' gw-hint'}">${f.verdict ? escapeHtml(f.verdict) : 'not yet judged'}</div>
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
    renderTicker();
  }
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
