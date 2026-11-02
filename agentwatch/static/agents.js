// The agent watch page (plans/agent-watch.md): a tile per worker, click one
// for its detail. Everything an agent wrote is put in with textContent; the
// only HTML from the server is the report, which it renders with raw HTML
// shown as text.

const QUICK_REPLIES = ['Approved', 'Done', 'No', 'Later'];
const REFRESH_MS = 60_000;

let agents = [];
let checkedAt = null;

// ── Pure helpers (tested in frontend/agentwatch.test.js) ────────────────────

export const STATE_LABELS = {
  ok: 'OK',
  late: 'Late',
  broken: 'Did not finish',
  unreachable: 'Cannot reach it',
  running: 'Running',
  paused: 'Paused',
  new: 'Not run yet',
  checking: 'Checking…',
};

export function stateLabel(state) {
  return STATE_LABELS[state] ?? state;
}

/** "5 min ago", "3 h ago", "2 days ago"; "in 4 h" for the future. */
export function relTime(iso, now = Date.now()) {
  if (!iso) return '';
  const diff = (now - new Date(iso).getTime()) / 1000;
  const ahead = diff < 0;
  const s = Math.abs(diff);
  let text;
  if (s < 90) text = 'a minute';
  else if (s < 3600) text = `${Math.round(s / 60)} min`;
  else if (s < 36 * 3600) text = `${Math.round(s / 3600)} h`;
  else text = `${Math.round(s / 86400)} days`;
  return ahead ? `in ${text}` : `${text} ago`;
}

/** "3 Nov, 07:40" in UK time. */
export function clock(iso) {
  if (!iso) return '';
  return new Date(iso).toLocaleString('en-GB', {
    timeZone: 'Europe/London', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit',
  });
}

/** `#/agent/Desk` → { view: 'agent', name: 'Desk' }. */
export function parseRoute(hash) {
  const parts = (hash || '').replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent);
  if (parts[0] === 'agent' && parts[1]) return { view: 'agent', name: parts[1] };
  if (parts[0] === 'settings') return { view: 'settings' };
  return { view: 'tiles' };
}

/** The "What" of a hub row: the cell headed What, else the longest middle cell. */
export function rowWhat(row) {
  const i = row.headers.findIndex((h) => /^(what|item|idea)$/i.test(h));
  if (i >= 0 && row.cells[i]) return row.cells[i];
  const middle = row.cells.slice(1, -1);
  return middle.reduce((a, b) => (b.length > a.length ? b : a), '');
}

/** Hub cells keep their Markdown; show links as their text. */
export function plain(md) {
  return md.replace(/\[([^\]]+)\]\([^)]*\)/g, '$1').replace(/\*\*([^*]+)\*\*/g, '$1').replace(/`([^`]+)`/g, '$1');
}

// ── DOM helpers ─────────────────────────────────────────────────────────────

function el(tag, attrs = {}, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === undefined || v === null || v === false) continue;
    if (k === 'class') e.className = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined || c === false) continue;
    e.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return e;
}

function toast(text, bad = false) {
  const t = document.getElementById('toast');
  t.textContent = text;
  t.className = bad ? 'bad' : '';
  t.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => { t.hidden = true; }, bad ? 8000 : 3000);
}

async function api(method, path, body) {
  const res = await fetch(path, {
    method,
    headers: body ? { 'Content-Type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `${res.status} ${res.statusText}`);
  return data;
}

function replaceAgent(view) {
  agents = agents.map((a) => (a.name === view.name ? view : a));
}

// ── Views ───────────────────────────────────────────────────────────────────

export function tile(a) {
  return el('a', { class: 'tile', href: `#/agent/${encodeURIComponent(a.name)}` },
    el('span', { class: `light ${a.state}`, title: stateLabel(a.state) }),
    el('h2', {}, a.name),
    el('p', {}, a.purpose),
    a.waiting > 0 ? el('span', { class: 'badge', title: 'Waiting on you' }, a.waiting) : null,
  );
}

function renderTiles(main) {
  if (agents.length === 0) {
    main.append(el('p', { class: 'muted' }, 'No workers yet. Add them in ', el('a', { href: '#/settings' }, 'Settings'), '.'));
    return;
  }
  main.append(el('div', { class: 'tiles' }, agents.map(tile)));
}

function renderAgent(main, name) {
  const a = agents.find((x) => x.name.toLowerCase() === name.toLowerCase());
  if (!a) {
    main.append(el('p', {}, `No worker called ${name}. `, el('a', { href: '#/' }, 'Back')));
    return;
  }
  const busy = a.state === 'running';
  const detail = el('div', { class: 'detail' },
    el('p', {}, el('a', { href: '#/' }, '← All workers')),
    el('h1', {}, a.name),
    el('p', { class: 'purpose' }, a.purpose),
    el('div', { class: 'facts' },
      fact('State', stateLabel(a.state)),
      fact('Runs every', a.every),
      fact('Last run', a.last_run ? `${clock(a.last_run)} (${relTime(a.last_run)})` : 'never'),
      a.next_due && a.kind === 'cloud' ? fact('Next due', clock(a.next_due)) : null,
      fact('Waiting on you', a.waiting),
    ),
    el('div', { class: 'actions' },
      a.can_run ? el('button', { class: 'primary', disabled: busy, onclick: () => act(a, 'run', 'Started') }, busy ? 'Running…' : 'Run now') : null,
      a.can_pause && !a.paused ? el('button', { onclick: () => act(a, 'pause', 'Paused') }, 'Pause') : null,
      a.can_pause && a.paused ? el('button', { onclick: () => act(a, 'resume', 'Resumed') }, 'Resume') : null,
      a.hub_url ? el('a', { class: 'button', href: a.hub_url, target: '_blank', rel: 'noopener' }, 'Hub on GitHub') : null,
      a.repo_url ? el('a', { class: 'button', href: a.repo_url, target: '_blank', rel: 'noopener' }, 'Repo') : null,
    ),
    a.error ? el('section', {}, el('h3', {}, 'Problem'), el('p', { class: 'error' }, a.error)) : null,
    a.line ? el('section', {}, el('h3', {}, 'Last run'), el('p', {}, a.line)) : null,
    waitingSection(a),
    a.report ? reportSection(a) : null,
    a.entries.length ? el('section', {}, el('h3', {}, a.kind === 'cloud' ? 'Its log' : 'Its steps and reports'),
      el('ul', { class: 'log' }, a.entries.map((e) => el('li', {}, el('span', { class: 'when' }, e.when),
        e.report ? el('a', { href: '#', onclick: (ev) => { ev.preventDefault(); showReport(a, e.report); } }, e.text) : e.text)))) : null,
    a.history.length ? el('section', {}, el('h3', {}, 'What this page has seen'),
      el('ul', { class: 'log' }, a.history.map((h) => el('li', {},
        el('span', { class: 'when' }, clock(h.at)), `${stateLabel(h.state)}, ${h.waiting} waiting. ${h.line}`)))) : null,
  );
  main.append(detail);
}

function fact(label, value) {
  return el('div', {}, el('span', {}, label), String(value));
}

function waitingSection(a) {
  if (!a.can_reply) {
    if (!a.waiting_lines.length) return null;
    return el('section', {}, el('h3', {}, 'Waiting on you'),
      el('ul', { class: 'log' }, a.waiting_lines.map((l) => el('li', {}, l))));
  }
  if (!a.rows.length) return el('section', {}, el('h3', {}, 'Waiting on you'), el('p', { class: 'muted' }, 'Nothing.'));
  return el('section', {}, el('h3', {}, 'Waiting on you'), a.rows.map((r) => replyRow(a, r)));
}

function replyRow(a, row) {
  const meta = row.headers.map((h, i) => [h, row.cells[i]])
    .filter(([h, c]) => c && !/^(#|what|item|idea|your reply)$/i.test(h))
    .map(([h, c]) => `${h}: ${plain(c)}`).join(' · ');
  const input = el('input', { type: 'text', placeholder: 'Your reply', value: row.reply || '' });
  const runNow = el('input', { type: 'checkbox' });
  const send = (text) => sendReply(a, row.number, text, a.can_run && runNow.checked);
  return el('div', { class: 'row' },
    el('p', { class: 'what' }, `${row.number}. ${plain(rowWhat(row))}`),
    meta ? el('p', { class: 'meta' }, meta) : null,
    row.reply ? el('p', { class: 'answered' }, `You replied: ${row.reply}`) : null,
    el('div', { class: 'reply' },
      QUICK_REPLIES.map((q) => el('button', { type: 'button', onclick: () => send(q) }, q)),
      input,
      el('button', { type: 'button', class: 'primary', onclick: () => send(input.value) }, 'Send'),
      a.can_run ? el('label', {}, runNow, ' and run now') : null,
    ),
  );
}

function reportSection(a) {
  const body = el('div', { class: 'report', id: 'report-body' }, el('p', { class: 'muted' }, 'Loading…'));
  loadReport(a, null, body);
  return el('section', { id: 'report' }, el('h3', { id: 'report-title' }, 'Latest report'), body);
}

function loadReport(a, path, body) {
  const q = path ? `?path=${encodeURIComponent(path)}` : '';
  api('GET', `/api/agents/${encodeURIComponent(a.name)}/report${q}`)
    .then((r) => { body.innerHTML = r.html; })
    .catch((e) => { body.textContent = e.message; });
}

/** Shows one of the listed reports in the report section and scrolls to it. */
function showReport(a, path) {
  const body = document.getElementById('report-body');
  if (!body) return;
  document.getElementById('report-title').textContent = path === a.report ? 'Latest report' : `Report: ${path.split('/').pop()}`;
  body.replaceChildren(el('p', { class: 'muted' }, 'Loading…'));
  loadReport(a, path, body);
  document.getElementById('report').scrollIntoView({ behavior: 'smooth' });
}

async function renderSettings(main) {
  const area = el('textarea', { class: 'config', spellcheck: 'false' });
  main.append(
    el('p', {}, el('a', { href: '#/' }, '← All workers')),
    el('h1', {}, 'Settings'),
    el('p', { class: 'muted' }, 'agents.toml on lenovo1. One [[agent]] per worker; see plans/agent-watch.md for the fields.'),
    area,
    el('div', { class: 'actions' }, el('button', { class: 'primary', onclick: async () => {
      try {
        const r = await api('PUT', '/api/config', { text: area.value });
        agents = r.agents;
        toast('Saved');
      } catch (e) { toast(e.message, true); }
    } }, 'Save')),
  );
  try { area.value = (await api('GET', '/api/config')).text; } catch (e) { toast(e.message, true); }
}

// ── Actions ─────────────────────────────────────────────────────────────────

async function act(a, action, done) {
  try {
    const r = await api('POST', `/api/agents/${encodeURIComponent(a.name)}/${action}`);
    replaceAgent(r.agent);
    toast(done);
  } catch (e) { toast(e.message, true); }
  render();
}

async function sendReply(a, number, text, runNow) {
  if (!text.trim()) return;
  try {
    const r = await api('POST', `/api/agents/${encodeURIComponent(a.name)}/reply`, { number, text, run_now: runNow });
    replaceAgent(r.agent);
    if (r.run_error) toast(`Reply saved, but Run now failed: ${r.run_error}`, true);
    else toast(runNow ? 'Reply saved, run started' : 'Reply saved');
  } catch (e) { toast(e.message, true); }
  render();
}

// ── Page ────────────────────────────────────────────────────────────────────

function render() {
  const main = document.getElementById('main');
  // Leave a half-typed reply or config alone.
  if (main.contains(document.activeElement) && document.activeElement.matches('input[type=text], textarea')) return;
  main.replaceChildren();
  const route = parseRoute(location.hash);
  if (route.view === 'agent') renderAgent(main, route.name);
  else if (route.view === 'settings') renderSettings(main);
  else renderTiles(main);
  document.getElementById('checked').textContent = checkedAt ? `checked ${relTime(checkedAt)}` : '';
}

async function load(path = '/api/agents', method = 'GET') {
  try {
    const r = await api(method, path);
    agents = r.agents;
    checkedAt = agents.map((a) => a.checked_at).sort().at(-1) || null;
  } catch (e) { toast(e.message, true); }
  if (parseRoute(location.hash).view !== 'settings') render();
}

if (typeof window !== 'undefined' && document.getElementById('main')) {
  window.addEventListener('hashchange', render);
  document.getElementById('refresh').addEventListener('click', () => load('/api/agents/refresh', 'POST'));
  load();
  setInterval(load, REFRESH_MS);
  render();
}
