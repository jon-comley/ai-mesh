import { api } from '/static/api.js';

// ── Online-AI gateway tab ─────────────────────────────────────────────────────
// Controls the coordinator-level "forward to an online model" mode: enable/
// disable, compress-or-not, compression engine, model + endpoint + API key, plus
// live token-savings stats. Config is server-side (persisted); this tab only
// ever sees a masked "key set" indicator, never the key itself.

const ENGINES = [
  { id: 'statistical', label: 'Statistical (Rust)', ready: true },
  { id: 'local_llm_distiller', label: 'Local-LLM distiller', ready: false },
  { id: 'llmlingua2', label: 'LLMLingua-2', ready: false },
];

export function init(panel) {
  panel.innerHTML = `
    <h2>Online AI</h2>
    <div class="gw">
      <div class="gw-modes">
        <button id="gw-enable" class="gw-toggle" type="button">Online AI: —</button>
        <button id="gw-compress" class="gw-toggle" type="button" title="Off = swap only the inference backend (send full history)">Compress context: —</button>
      </div>

      <div id="gw-keybanner" class="gw-banner" hidden>⚠ No API key set — cloud requests fall back to local. Add a key below.</div>

      <div class="gw-field">
        <span class="gw-label">Provider</span>
        <div class="gw-btns" id="gw-presets"></div>
      </div>

      <div class="gw-field">
        <label for="gw-model">Online model</label>
        <select id="gw-model"></select>
      </div>

      <div class="gw-field gw-inline">
        <label for="gw-model-custom">…or custom</label>
        <input id="gw-model-custom" type="text" autocomplete="off" placeholder="any model id, e.g. openai/gpt-oss-120b:free">
        <button id="gw-model-custom-save" type="button">Use</button>
      </div>

      <div class="gw-field">
        <span class="gw-label">Compression engine</span>
        <div class="gw-btns" id="gw-engine-btns">
          ${ENGINES.map(e => `
            <button type="button" data-engine="${e.id}" ${e.ready ? '' : 'disabled'}>
              ${e.label}${e.ready ? '' : ' <em>(soon)</em>'}
            </button>`).join('')}
        </div>
      </div>

      <div class="gw-field gw-inline">
        <label for="gw-key">API key</label>
        <input id="gw-key" type="password" autocomplete="off" placeholder="paste key…">
        <button id="gw-key-save" type="button">Save</button>
        <span id="gw-key-status" class="gw-hint"></span>
      </div>

      <div class="gw-field gw-inline">
        <label for="gw-base">Endpoint</label>
        <input id="gw-base" type="text" autocomplete="off" placeholder="https://openrouter.ai/api/v1">
        <button id="gw-base-save" type="button">Save</button>
      </div>

      <div class="gw-field gw-inline">
        <button id="gw-test" type="button">Test cloud call</button>
        <span id="gw-test-result" class="gw-hint"></span>
      </div>

      <div class="gw-stats" id="gw-stats"><p class="placeholder">No calls yet.</p></div>
    </div>
  `;

  panel.querySelector('#gw-enable').addEventListener('click', () =>
    post({ enabled: !state.enabled }));
  panel.querySelector('#gw-compress').addEventListener('click', () =>
    post({ compress: !state.compress }));
  panel.querySelector('#gw-engine-btns').addEventListener('click', (e) => {
    const btn = e.target.closest('button[data-engine]');
    if (btn && !btn.disabled) post({ engine: btn.dataset.engine });
  });
  panel.querySelector('#gw-presets').addEventListener('click', (e) => {
    const btn = e.target.closest('button[data-preset]');
    // Switching provider sets the endpoint and its first model together.
    if (btn) post({ base_url: btn.dataset.baseUrl, selected_model: btn.dataset.model });
  });
  panel.querySelector('#gw-model').addEventListener('change', (e) =>
    post({ selected_model: e.target.value }));
  panel.querySelector('#gw-model-custom-save').addEventListener('click', () => {
    const input = panel.querySelector('#gw-model-custom');
    const v = input.value.trim();
    if (v) post({ selected_model: v }).then(() => { input.value = ''; });
  });
  panel.querySelector('#gw-key-save').addEventListener('click', () => {
    const input = panel.querySelector('#gw-key');
    post({ api_key: input.value }).then(() => { input.value = ''; });
  });
  panel.querySelector('#gw-base-save').addEventListener('click', () =>
    post({ base_url: panel.querySelector('#gw-base').value.trim() }));
  panel.querySelector('#gw-test').addEventListener('click', testCall);

  refresh();
}

let state = {};

async function refresh() {
  try {
    const res = await api('/gateway');
    if (res.ok) render(await res.json());
  } catch { /* dashboard shows disconnected state elsewhere */ }
}

async function post(body) {
  const res = await api('/gateway', { method: 'POST', body });
  if (res.ok) render(await res.json());
}

async function testCall() {
  const out = document.getElementById('gw-test-result');
  if (out) { out.textContent = 'testing…'; out.dataset.ok = ''; }
  try {
    const res = await api('/gateway/test', { method: 'POST' });
    const data = await res.json();
    if (out) {
      out.dataset.ok = data.ok ? '1' : '0';
      out.textContent = data.ok ? `✓ ${truncate(data.reply, 60)}` : `✗ ${data.error}`;
    }
  } catch (e) {
    if (out) { out.dataset.ok = '0'; out.textContent = `✗ ${e}`; }
  }
}

// Re-render from a GatewayUpdate WS event (same shape as GET /api/gateway).
export function handleGatewayUpdate(evt) { render(evt); }

function render(snap) {
  if (!snap || typeof snap.enabled !== 'boolean') return;
  state = snap;

  const enableBtn = document.getElementById('gw-enable');
  if (enableBtn) {
    enableBtn.textContent = `Online AI: ${snap.enabled ? 'Enabled' : 'Disabled'}`;
    enableBtn.dataset.on = snap.enabled ? '1' : '0';
  }
  const compressBtn = document.getElementById('gw-compress');
  if (compressBtn) {
    compressBtn.textContent = `Compress context: ${snap.compress ? 'On' : 'Off'}`;
    compressBtn.dataset.on = snap.compress ? '1' : '0';
  }

  const banner = document.getElementById('gw-keybanner');
  if (banner) banner.hidden = snap.key_set;

  // Model selection / compression only take effect once Online AI is on —
  // grey them out rather than leave them clickable but inert. API key,
  // endpoint, and the test-call button stay live either way: you need to be
  // able to set them up and verify a key works *before* flipping the switch.
  const offline = !snap.enabled;

  const norm = (u) => (u || '').replace(/\/+$/, '');
  const presetBox = document.getElementById('gw-presets');
  if (presetBox) {
    presetBox.innerHTML = (snap.presets ?? []).map(p =>
      `<button type="button" data-preset="${escapeHtml(p.id)}" data-base-url="${escapeHtml(p.base_url)}" data-model="${escapeHtml(p.models?.[0] ?? '')}" data-active="${norm(p.base_url) === norm(snap.base_url) ? '1' : '0'}" ${offline ? 'disabled' : ''}>${escapeHtml(p.label)}</button>`).join('');
  }

  const sel = document.getElementById('gw-model');
  if (sel) {
    const models = (snap.available_models ?? []).slice();
    // Always include the currently-selected model so it shows even if it isn't
    // in the preset menu (e.g. a custom slug).
    if (snap.selected_model && !models.includes(snap.selected_model)) models.unshift(snap.selected_model);
    sel.innerHTML = models.map(m =>
      `<option value="${escapeHtml(m)}"${m === snap.selected_model ? ' selected' : ''}>${escapeHtml(m)}</option>`).join('');
    sel.disabled = offline;
  }
  const modelCustom = document.getElementById('gw-model-custom');
  const modelCustomSave = document.getElementById('gw-model-custom-save');
  if (modelCustom) modelCustom.disabled = offline;
  if (modelCustomSave) modelCustomSave.disabled = offline;

  if (compressBtn) compressBtn.disabled = offline;
  document.querySelectorAll('#gw-engine-btns button[data-engine]').forEach(btn => {
    btn.dataset.active = btn.dataset.engine === snap.engine ? '1' : '0';
    const engine = ENGINES.find(e => e.id === btn.dataset.engine);
    btn.disabled = offline || !engine?.ready;
  });

  const keyStatus = document.getElementById('gw-key-status');
  if (keyStatus) keyStatus.textContent = snap.key_set ? `key set ${snap.key_hint ?? ''}` : 'no key';

  const base = document.getElementById('gw-base');
  if (base && document.activeElement !== base) base.value = snap.base_url ?? '';

  renderStats(snap);
}

function renderStats(snap) {
  const el = document.getElementById('gw-stats');
  if (!el) return;
  if (!snap.calls) {
    el.innerHTML = `<p class="placeholder">No cloud calls yet${snap.last_error ? ` — last error: ${escapeHtml(snap.last_error)}` : ''}.</p>`;
    return;
  }
  const saved = snap.tokens_saved ?? 0;
  const before = snap.tokens_before ?? 0;
  const pct = before > 0 ? Math.round((saved / before) * 100) : 0;
  const when = snap.last_call_at ? new Date(snap.last_call_at * 1000).toLocaleTimeString() : '—';
  el.innerHTML = `
    <div class="gw-stat"><span>Cloud calls</span><strong>${snap.calls}</strong></div>
    <div class="gw-stat"><span>Context tokens in → out</span><strong>${before} → ${snap.tokens_after ?? 0}</strong></div>
    <div class="gw-stat"><span>Tokens saved</span><strong>${saved} (${pct}%)</strong></div>
    <div class="gw-stat"><span>Last call</span><strong>${when}</strong></div>
    ${snap.last_error ? `<div class="gw-stat gw-err"><span>Last error</span><strong>${escapeHtml(snap.last_error)}</strong></div>` : ''}
  `;
}

function truncate(s, n) { return s && s.length > n ? `${s.slice(0, n)}…` : (s ?? ''); }
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
