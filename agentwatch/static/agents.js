// The agent watch page (plans/agent-watch.md): a tile per worker, click one
// for its detail. Everything an agent wrote is put in with textContent; the
// only HTML from the server is a report or ticket, which it renders with raw HTML
// shown as text.

const REFRESH_MS = 60_000;
// The tiles' terminal and paused lines, while the page is in view. No AI: files and one ssh.
const LIVE_MS = 10_000;

let agents = [];
let checkedAt = null;
// The open worker's chat: { name, messages: [{role, text}], busy, loaded }.
let chat = null;

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
  if (parts[0] === 'term' && parts[1]) return { view: 'term', name: parts[1] };
  if (parts[0] === 'settings') return { view: 'settings' };
  if (parts[0] === 'schedule') return { view: 'schedule' };
  if (parts[0] === 'ticket' && parts.length >= 3) return { view: 'ticket', name: parts[1], path: parts.slice(2).join('/') };
  if (parts[0] === 'memory' && parts.length === 3) return { view: 'fact', path: `${parts[1]}/${parts[2]}` };
  if (parts[0] === 'memory') return { view: 'memory' };
  return { view: 'tiles' };
}

export const PHASE_LABELS = { working: 'Working', done: 'Done', needs_you: 'Needs you' };

/** One line about a worker's terminal: "Done · £0.40 today · £3.10 this month". */
export function termText(t) {
  if (!t) return '';
  const phase = t.state ? PHASE_LABELS[t.state.phase] ?? t.state.phase : (t.open ? 'Open' : 'Closed');
  // Costs live on the Schedule tab; the tile keeps only a limit warning.
  const parts = [t.node && t.open ? `${phase} on ${t.node}` : phase];
  if (t.over_session) parts.push('session over its limit');
  else if (t.over_day) parts.push('over the day limit');
  return parts.join(' · ');
}

/** A queue worker's machine light: "wl on", "wl off", or nothing before the first check. */
export function machineText(a) {
  if (!a.machine || a.machine_on === null || a.machine_on === undefined) return '';
  return `${a.machine} ${a.machine_on ? 'on' : 'off'}`;
}

export const TASK_LABELS = {
  queued: 'Queued', running: 'Running', done: 'Done', needs_you: 'Needs you', interrupted: 'Interrupted', failed: 'Failed',
};

/**
 * What the terminal page shows above Claude: where the worker stands, from what
 * its tile already knows (no model call). Lines of { label, text }.
 */
export function briefing(a) {
  const out = [];
  // Its own STATUS.md first: State and NEXT, as the agent wrote them.
  for (const l of a.status_lines || []) {
    const [label, ...rest] = l.split(':');
    out.push({ label, text: rest.join(':').trim() });
  }
  if (a.last_run) out.push({ label: 'Last run', text: `${relTime(a.last_run)}${a.line ? `: ${a.line}` : ''}` });
  else if (a.line) out.push({ label: 'Now', text: a.line });
  const waiting = a.can_reply
    ? (a.rows || []).filter((r) => !r.reply).map((r) => `${r.number}. ${plain(rowWhat(r))}`)
    : (a.waiting_lines || []);
  waiting.slice(0, 5).forEach((w) => out.push({ label: 'Needs you', text: w }));
  if (waiting.length > 5) out.push({ label: 'Needs you', text: `and ${waiting.length - 5} more` });
  if (a.term?.state?.line) out.push({ label: 'Terminal said', text: a.term.state.line });
  return out;
}

/** Where an agent runs, for the Schedule tab. */
export function agentWhere(a) {
  // A cloud tile with no routine is a chat page that writes to its repo (Marketing comms).
  if (a.kind === 'cloud') return a.can_run === false ? 'its chat page, on demand' : 'Anthropic cloud';
  if (a.kind === 'queue') return `lenovo1, for ${a.machine ?? 'its machine'}`;
  return a.machine ?? 'mac1';
}

/** How often, as people say it: "every 20m", "daily 07:30". */
export function agentWhen(a) {
  if (a.kind === 'queue') return 'when work is queued';
  if (a.kind === 'cloud' && a.can_run === false) return `when used, checked every ${a.every}`;
  return `every ${a.every}`;
}

const gbp = (n) => `£${(n ?? 0).toFixed(2)}`;

/**
 * Today's and this month's spend across every agent's terminal and tasks.
 * `books` leaves out the work laptop: ThingCo work is not a Glebefish cost.
 */
export function costTotals(list, books = false) {
  return (list || []).filter((a) => !books || a.kind !== 'queue').reduce((t, a) => ({
    today: t.today + (a.term?.today_gbp ?? 0),
    month: t.month + (a.term?.month_gbp ?? 0),
  }), { today: 0, month: 0 });
}

export const SERVICE_LABELS = { running: 'Running', waiting: 'Waiting', stopped: 'Stopped', failed: 'Failed', unreachable: 'Cannot reach it' };

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
  // A worker with a terminal opens straight into it; its details are a button there.
  const view = a.term ? 'term' : 'agent';
  const phase = a.term?.state?.phase;
  return el('a', { class: 'tile', href: `#/${view}/${encodeURIComponent(a.name)}` },
    el('span', { class: `light ${a.state}`, title: stateLabel(a.state) }),
    el('h2', {}, a.name),
    el('p', {}, a.purpose),
    machineText(a) ? el('span', { class: `machine ${a.machine_on ? 'on' : 'off'}` }, machineText(a)) : null,
    a.term ? el('p', { class: `term-line ${phase ?? ''}` }, termText(a.term)) : null,
    a.term?.state?.line ? el('p', { class: 'term-said' }, a.term.state.line) : null,
    a.waiting > 0 ? el('span', { class: 'badge', title: 'Waiting on you' }, a.waiting) : null,
  );
}

function renderTiles(main) {
  if (agents.length === 0) {
    main.append(el('p', { class: 'muted' }, 'No workers yet. Add them in ', el('a', { href: '#/settings' }, 'Settings'), '.'));
    return;
  }
  main.append(el('div', { class: 'tiles' }, agents.map(tile), memoryTile()));
}

// ── Memory ──────────────────────────────────────────────────────────────────

let memory = null; // { folders, count, updated } once loaded

function loadMemory() {
  return api('GET', '/api/memory').then((m) => { memory = m; return m; });
}

export function memoryTileText(m) {
  if (!m) return 'What every worker knows, by subject';
  return `What every worker knows: ${m.count} facts in ${m.folders.length} folders`;
}

function memoryTile() {
  const p = el('p', {}, memoryTileText(memory));
  if (!memory) loadMemory().then((m) => { p.textContent = memoryTileText(m); }).catch(() => {});
  return el('a', { class: 'tile memory-tile', href: '#/memory' }, el('h2', {}, 'Memory'), p);
}

function renderMemory(main) {
  const body = el('div', {}, el('p', { class: 'muted' }, 'Loading…'));
  main.append(
    el('div', { class: 'detail' },
      el('h1', {}, 'Memory'),
      el('p', { class: 'purpose' }, 'Shared by every worker. Each starts with the index and jon/, and opens a folder only when the talk is about it. Tell any worker something lasting and it lands here.'),
      body),
  );
  loadMemory().then((m) => {
    body.replaceChildren(
      el('p', { class: 'muted' }, `${m.count} facts${m.updated ? `, last changed ${clock(m.updated)}` : ''}.`),
      ...m.folders.map((f) => el('section', {},
        el('h3', {}, `${f.name}/`, f.title ? el('span', { class: 'muted' }, `  ${f.title}`) : null),
        el('ul', { class: 'log' }, f.facts.map((x) => el('li', {},
          el('a', { href: `#/memory/${x.path.split('/').map(encodeURIComponent).join('/')}` }, x.name),
          el('p', { class: 'meta' }, x.description)))))),
    );
  }).catch((e) => { body.textContent = e.message; });
}

/** `#/ticket/Work laptop/thingco/TICKET-x.md`. */
export function ticketHref(a, path) {
  return `#/ticket/${encodeURIComponent(a.name)}/${path.split('/').map(encodeURIComponent).join('/')}`;
}

/** A queue worker's ticket, rendered from the laptop's copy. */
function renderTicket(main, name, path) {
  const body = el('div', { class: 'report' }, el('p', { class: 'muted' }, 'Loading…'));
  main.append(el('div', { class: 'detail' },
    el('p', {}, el('a', { href: `#/term/${encodeURIComponent(name)}` }, `← ${name}`)),
    el('p', { class: 'muted' }, path),
    el('section', {}, body)));
  api('GET', `/api/agents/${encodeURIComponent(name)}/ticket?path=${encodeURIComponent(path)}`)
    .then((r) => { body.innerHTML = r.html; })
    .catch((e) => { body.textContent = e.message; });
}

function renderFact(main, path) {
  const body = el('div', { class: 'report' }, el('p', { class: 'muted' }, 'Loading…'));
  const title = el('h1', {}, path);
  const actions = el('div', { class: 'actions' });
  main.append(el('div', { class: 'detail' },
    el('p', {}, el('a', { href: '#/memory' }, '← Memory')), title, actions, el('section', {}, body)));
  api('GET', `/api/memory/file?path=${encodeURIComponent(path)}`).then((r) => {
    title.textContent = r.fact.name;
    const show = () => {
      body.innerHTML = r.html;
      actions.replaceChildren(
        el('p', { class: 'muted' }, `${r.fact.description}${r.fact.updated ? ` (updated ${r.fact.updated})` : ''}`),
        el('button', { type: 'button', onclick: edit }, 'Edit'));
    };
    const edit = () => {
      const area = el('textarea', { class: 'config' });
      area.value = r.text;
      body.replaceChildren(area);
      actions.replaceChildren(
        el('button', { type: 'button', class: 'primary', onclick: async () => {
          try {
            await api('PUT', '/api/memory/file', { path, text: area.value });
            toast('Saved and pushed');
            memory = null;
            const fresh = await api('GET', `/api/memory/file?path=${encodeURIComponent(path)}`);
            Object.assign(r, fresh);
            show();
          } catch (e) { toast(e.message, true); }
        } }, 'Save'),
        el('button', { type: 'button', onclick: show }, 'Cancel'));
      area.focus();
    };
    show();
  }).catch((e) => { body.textContent = e.message; });
}

function renderAgent(main, name) {
  const a = agents.find((x) => x.name.toLowerCase() === name.toLowerCase());
  if (!a) {
    main.append(el('p', {}, `No worker called ${name}. `, el('a', { href: '#/' }, 'Back')));
    return;
  }
  const busy = a.state === 'running';
  const detail = el('div', { class: 'detail' },
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
    a.kind === 'queue' ? tasksSection(a) : null,
    chatSection(a),
    a.line ? el('section', {}, el('h3', {}, 'Last run'), el('p', {}, a.line)) : null,
    waitingSection(a),
    a.report ? reportSection(a) : null,
    a.entries.length ? el('section', {}, el('h3', {}, a.kind === 'cloud' ? 'Its log' : 'Its steps and reports'),
      el('ul', { class: 'log' }, a.entries.map((e) => el('li', {}, el('span', { class: 'when' }, e.when),
        e.report ? el('a', { href: '#', onclick: (ev) => { ev.preventDefault(); showReport(a, e.report); } }, e.text) : e.text)))) : null,
    a.history.length ? el('section', {}, el('h3', {}, 'What this page has seen'),
      el('ul', { class: 'log' }, a.history.map((h) => el('li', {},
        el('span', { class: 'when' }, clock(h.at)), `${stateLabel(h.state)}, ${h.waiting} waiting. ${h.line}`)))) : null,
    el('p', {}, el('a', { href: '#/', class: 'button' }, '← All workers')),
  );
  main.append(detail);
}

function fact(label, value) {
  return el('div', {}, el('span', {}, label), String(value));
}

function waitingSection(a) {
  const items = a.can_reply
    ? a.rows.map((r) => waitingRow(r))
    : a.waiting_lines.map((l) => el('li', {}, l));
  if (!items.length) return el('section', {}, el('h3', {}, 'Waiting on you'), el('p', { class: 'muted' }, 'Nothing.'));
  return el('section', {}, el('h3', {}, 'Waiting on you'),
    el('p', { class: 'muted' }, `To answer, tell ${a.name} in the chat above.`),
    el('ul', { class: 'log' }, items));
}

function waitingRow(row) {
  const meta = row.headers.map((h, i) => [h, row.cells[i]])
    .filter(([h, c]) => c && !/^(#|what|item|idea|your reply)$/i.test(h))
    .map(([h, c]) => `${h}: ${plain(c)}`).join(' · ');
  return el('li', {},
    el('p', { class: 'what' }, `${row.number}. ${plain(rowWhat(row))}`),
    meta ? el('p', { class: 'meta' }, meta) : null,
    row.reply ? el('p', { class: 'answered' }, `You replied: ${row.reply}`) : null,
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

// ── Chat ────────────────────────────────────────────────────────────────────

function chatSection(a) {
  if (!a.can_chat) return null;
  if (!chat || chat.name !== a.name) {
    chat = { name: a.name, messages: [], busy: false, loaded: false };
    api('GET', `/api/agents/${encodeURIComponent(a.name)}/chat`).then((r) => {
      if (chat.name !== a.name) return;
      chat.messages = r.messages;
      chat.busy = r.busy;
      chat.loaded = true;
      drawChat();
    }).catch((e) => toast(e.message, true));
  }
  const box = el('textarea', { id: 'chat-input', rows: '3', placeholder: `Talk to ${a.name}…` });
  box.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); sendChat(a, box); }
  });
  return el('section', { class: 'chat' },
    el('div', { class: 'chat-head' }, el('h3', {}, `Talk to ${a.name}`),
      el('button', { type: 'button', onclick: () => newChat(a) }, 'New conversation')),
    el('div', { id: 'chat-log', class: 'chat-log' }),
    el('div', { class: 'chat-send' }, box,
      el('button', { id: 'chat-send', type: 'button', class: 'primary', onclick: () => sendChat(a, box) }, 'Send')),
    el('p', { class: 'muted' }, 'Enter sends, Shift+Enter for a new line. It can change its own repo while you talk, and keeps what you agree for its next run.'),
  );
}

/** Redraws the chat log from `chat`, keeping the view at the bottom. */
function drawChat() {
  const log = document.getElementById('chat-log');
  if (!log || !chat) return;
  const items = chat.messages.map((m) => el('div', { class: `msg ${m.role}` }, m.text));
  if (chat.busy) items.push(el('div', { class: 'msg tool' }, 'Thinking…'));
  if (!items.length) items.push(el('p', { class: 'muted' }, chat.loaded ? 'No conversation yet.' : 'Loading…'));
  log.replaceChildren(...items);
  log.scrollTop = log.scrollHeight;
  const send = document.getElementById('chat-send');
  if (send) send.disabled = chat.busy;
}

async function sendChat(a, box) {
  const text = box.value.trim();
  if (!text || chat.busy) return;
  box.value = '';
  // Keep writing into this conversation even if Jon opens another worker meanwhile.
  const state = chat;
  state.messages.push({ role: 'jon', text });
  state.busy = true;
  drawChat();
  try {
    const res = await fetch(`/api/agents/${encodeURIComponent(a.name)}/chat`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ message: text }),
    });
    if (!res.ok) throw new Error((await res.json().catch(() => ({}))).error || res.statusText);
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let nl;
      while ((nl = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, nl).trim();
        buffer = buffer.slice(nl + 1);
        if (line) chatEvent(JSON.parse(line), state);
      }
    }
  } catch (e) {
    state.messages.push({ role: 'error', text: e.message });
  }
  state.busy = false;
  drawChat();
  load();
}

export function chatEvent(event, state = chat) {
  if (event.kind === 'text') state.messages.push({ role: 'worker', text: event.text });
  else if (event.kind === 'tool') state.messages.push({ role: 'tool', text: event.text });
  else if (event.kind === 'error') state.messages.push({ role: 'error', text: event.text });
  if (state === chat) drawChat();
}

async function newChat(a) {
  if (chat?.busy) return;
  try {
    await api('POST', `/api/agents/${encodeURIComponent(a.name)}/chat/new`);
    chat = { name: a.name, messages: [], busy: false, loaded: true };
    drawChat();
  } catch (e) { toast(e.message, true); }
}

// ── Actions ─────────────────────────────────────────────────────────────────

// ── Queue ───────────────────────────────────────────────────────────────────

function tasksSection(a) {
  const head = el('h3', {}, 'Tasks', machineText(a) ? el('span', { class: `machine ${a.machine_on ? 'on' : 'off'}` }, machineText(a)) : null);
  if (!a.tasks?.length) return el('section', {}, head, el('p', { class: 'muted' }, 'Nothing queued. Give it work in its terminal.'));
  return el('section', {}, head, el('ul', { class: 'tasks' }, a.tasks.map((t) => el('li', { class: `task ${t.state}` },
    el('span', { class: 'when' }, `${TASK_LABELS[t.state] ?? t.state} · ${relTime(t.created)}`),
    el('p', {}, t.task),
    el('p', { class: 'muted' }, t.dir),
    t.result ? el('p', { class: 'result' }, t.result) : null,
    ['interrupted', 'failed', 'needs_you'].includes(t.state)
      ? el('button', { onclick: () => retryTask(a, t) }, 'Retry') : null,
  ))));
}

async function retryTask(a, t) {
  try {
    await api('POST', `/api/agents/${encodeURIComponent(a.name)}/tasks/${encodeURIComponent(t.id)}/retry`);
    toast('Queued again');
    load();
  } catch (e) { toast(e.message, true); }
}

// ── Schedule ────────────────────────────────────────────────────────────────

async function renderSchedule(main) {
  main.append(el('h1', {}, 'Schedule'),
    el('p', { class: 'purpose' }, 'Every background job, where it runs and when. Read from the schedulers themselves: no AI.'));
  const box = el('div', {}, el('p', { class: 'muted' }, 'Reading the schedulers…'));
  main.append(box);
  let data;
  try { data = await api('GET', '/api/schedule'); } catch (e) { box.replaceChildren(el('p', { class: 'error' }, e.message)); return; }
  const totals = costTotals(data.agents);
  const books = costTotals(data.agents, true);
  const agentRows = data.agents.map((a) => el('tr', {},
    el('td', {}, el('a', { href: `#/agent/${encodeURIComponent(a.name)}` }, a.name)),
    el('td', {}, agentWhere(a)),
    el('td', {}, agentWhen(a)),
    el('td', {}, el('span', { class: `light ${a.state}` }), ' ', stateLabel(a.state)),
    el('td', {}, a.last_run ? relTime(a.last_run) : 'never'),
    el('td', {}, a.next_due ? clock(a.next_due) : ''),
    el('td', {}, a.term ? `${gbp(a.term.today_gbp)} / ${gbp(a.term.month_gbp)}` : ''),
    el('td', { class: 'acts' },
      a.can_run ? el('button', { onclick: () => scheduleAgent(a, 'run', 'Started') }, 'Run now') : null,
      a.can_pause && !a.paused ? el('button', { onclick: () => scheduleAgent(a, 'pause', 'Paused') }, 'Pause') : null,
      a.can_pause && a.paused ? el('button', { onclick: () => scheduleAgent(a, 'resume', 'Resumed') }, 'Resume') : null),
  ));
  const serviceRows = data.services.map((s) => el('tr', {},
    el('td', {}, el('strong', {}, s.name), el('br'), el('span', { class: 'muted' }, s.purpose)),
    el('td', {}, s.host),
    el('td', {}, s.unit.endsWith('.timer') ? 'timer' : 'always on'),
    el('td', {}, el('span', { class: `svc ${s.state}` }, SERVICE_LABELS[s.state] ?? s.state),
      s.restarts ? el('span', { class: 'muted' }, ` · restarted ${s.restarts}×`) : null),
    el('td', {}, s.last || s.since || ''),
    el('td', {}, s.next || ''),
    el('td', { class: 'acts' },
      el('button', { onclick: () => serviceLog(s) }, 'Log'),
      s.can_run ? el('button', { onclick: () => serviceAct(s, 'run', 'Started') }, 'Run now')
        : el('button', { onclick: () => serviceAct(s, 'restart', 'Restarted') }, 'Restart')),
  ));
  const head = (cols) => el('thead', {}, el('tr', {}, cols.map((c) => el('th', {}, c))));
  box.replaceChildren(
    el('div', { class: 'cost-total' },
      el('strong', {}, `${gbp(totals.today)} today`), ` · ${gbp(totals.month)} this month`,
      el('span', { class: 'muted' }, ' (terminals and wl tasks; the cloud routines and the orchestrator bill to the Claude account and are not counted here)'),
      el('br'),
      el('span', {}, `For the books, without wl: ${gbp(books.month)} this month (${gbp(books.today)} today). `),
      el('span', { class: 'muted' }, 'Usage inside the Claude plan, priced at API rates: recorded on the 1st to compare with what the plan costs (ventures/cost-capture-checklist.md). The costs pages show the subscription.')),
    el('h3', {}, 'Agents'),
    el('table', { class: 'schedule' }, head(['Agent', 'Runs on', 'When', 'State', 'Last run', 'Next', 'Spend today / month', '']), el('tbody', {}, agentRows)),
    el('h3', {}, 'Services'),
    el('table', { class: 'schedule' }, head(['Service', 'Machine', 'Kind', 'State', 'Last / since', 'Next', '']), el('tbody', {}, serviceRows)),
    el('pre', { id: 'service-log', class: 'service-log', hidden: true }),
  );
}

async function scheduleAgent(a, action, done) {
  try { await api('POST', `/api/agents/${encodeURIComponent(a.name)}/${action}`); toast(done); } catch (e) { toast(e.message, true); }
  render();
}

async function serviceAct(s, action, done) {
  try { await api('POST', `/api/schedule/${encodeURIComponent(s.name)}/${action}`); toast(done); } catch (e) { toast(e.message, true); }
  render();
}

async function serviceLog(s) {
  const pre = document.getElementById('service-log');
  pre.hidden = false;
  pre.textContent = `${s.name}: reading its log…`;
  try { pre.textContent = `${s.name} (${s.host}, ${s.unit})\n\n${(await api('GET', `/api/schedule/${encodeURIComponent(s.name)}/log`)).log}`; }
  catch (e) { pre.textContent = e.message; }
  pre.scrollIntoView({ behavior: 'smooth' });
}

// ── Terminal ────────────────────────────────────────────────────────────────

let termTimer = null;
// lenovo1's Tailscale address: the name needs MagicDNS, which not every browser has.
const TERM_HOST = '100.82.51.25';
// Over https (Tailscale serve, 8443) the terminal comes through serve on 7682 too.
function termBase() {
  return location.protocol === 'https:' ? 'https://lenovo1.tail3c4ee3.ts.net:7682' : `http://${TERM_HOST}:7681`;
}

function renderTerm(main, name) {
  const a = agents.find((x) => x.name.toLowerCase() === name.toLowerCase());
  if (!a) {
    main.append(el('p', {}, `No worker called ${name}. `, el('a', { href: '#/' }, 'Back')));
    return;
  }
  const slug = a.name.toLowerCase();
  const status = el('span', { class: 'term-status' }, termText(a.term));
  const frame = el('iframe', {
    class: 'term-frame',
    title: `${a.name} terminal`,
    // ttyd listens on Tailscale only (systemd/agentterm.service).
    src: `${termBase()}/?arg=${encodeURIComponent(slug)}`,
  });
  // Where it stands, before Jon types: hidden with × and remembered per worker.
  const hideKey = `agentwatch-brief-hidden-${slug}`;
  const lines = briefing(a);
  const brief = lines.length && localStorage.getItem(hideKey) !== '1'
    ? el('div', { class: 'term-brief' },
      el('button', { class: 'term-brief-close', title: 'Hide (Details shows it all)', onclick: () => { localStorage.setItem(hideKey, '1'); brief.remove(); } }, '×'),
      el('ul', {}, lines.map((l) => el('li', {}, el('strong', {}, `${l.label}: `), l.text)),
        (a.tickets || []).map((t) => el('li', {}, el('strong', {}, 'Ticket: '), el('a', { href: ticketHref(a, t.path) }, t.title))),
        (a.kanban || []).map((u) => el('li', {}, el('strong', {}, 'Kanban: '), el('a', { href: u, target: '_blank', rel: 'noopener' }, u)))),
      a.report ? el('a', { href: `#/agent/${encodeURIComponent(a.name)}` }, 'Latest report and everything else in Details') : null)
    : null;
  main.append(el('div', { class: 'term' },
    el('div', { class: 'term-bar' },
      el('a', { class: 'button', href: '#/' }, '← All workers'),
      el('strong', {}, a.name),
      status,
      el('span', { class: 'spacer' }),
      // The browser never asks for the terminal's login inside a frame: log in once on its own page.
      el('a', { class: 'button', href: frame.src, target: '_blank', rel: 'noopener', title: 'Opens the terminal on its own page, where the browser asks for the login' }, 'Log in'),
      // Inside the ai-mesh dashboard's frame, Full screen leaves the frame for this page on its own.
      window.top !== window
        ? el('a', { class: 'button', href: `${location.origin}/#/term/${encodeURIComponent(a.name)}`, target: '_top' }, 'Full screen')
        : (document.fullscreenEnabled ? el('button', { onclick: () => (document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen()) }, 'Full screen') : null),
      lines.length ? el('button', { title: 'Show where it stands', onclick: () => { localStorage.removeItem(hideKey); render(true); } }, 'Brief') : null,
      el('a', { class: 'button', href: `#/agent/${encodeURIComponent(a.name)}` }, 'Details'),
    ),
    brief,
    frame,
  ));
  document.body.classList.add('fullscreen');
  clearInterval(termTimer);
  termTimer = setInterval(async () => {
    if (parseRoute(location.hash).view !== 'term') { clearInterval(termTimer); return; }
    try {
      const r = await api('GET', `/api/agents/${encodeURIComponent(a.name)}/term`);
      a.term = r.term;
      status.textContent = termText(r.term);
    } catch { /* the next tick tries again */ }
  }, 5_000);
  frame.focus();
}

async function act(a, action, done) {
  try {
    const r = await api('POST', `/api/agents/${encodeURIComponent(a.name)}/${action}`);
    replaceAgent(r.agent);
    toast(done);
  } catch (e) { toast(e.message, true); }
  render();
}

// ── Page ────────────────────────────────────────────────────────────────────

function render(force = false) {
  const main = document.getElementById('main');
  // Leave a half-typed reply or config alone.
  if (main.contains(document.activeElement) && document.activeElement.matches('input[type=text], textarea')) return;
  const route = parseRoute(location.hash);
  // The terminal keeps its connection: a background refresh never redraws it.
  if (route.view === 'term' && main.querySelector('.term-frame') && !force) return;
  main.replaceChildren();
  document.body.classList.remove('fullscreen');
  if (route.view === 'term') renderTerm(main, route.name);
  else if (route.view === 'agent') { renderAgent(main, route.name); drawChat(); }
  else if (route.view === 'settings') renderSettings(main);
  else if (route.view === 'schedule') renderSchedule(main);
  else if (route.view === 'memory') renderMemory(main);
  else if (route.view === 'fact') renderFact(main, route.path);
  else if (route.view === 'ticket') renderTicket(main, route.name, route.path);
  else renderTiles(main);
  document.getElementById('checked').textContent = checkedAt ? `checked ${relTime(checkedAt)}` : '';
  const spend = document.getElementById('spend');
  if (spend) {
    const t = costTotals(agents);
    spend.textContent = `${gbp(t.today)} today`;
    spend.title = `${gbp(t.month)} this month: terminals and wl tasks`;
  }
  document.getElementById('back').hidden = route.view === 'tiles';
}

/** Folds `/api/live` into the tiles: terminal state and paused, fresh. */
export function applyLive(list, live) {
  return list.map((a) => {
    const l = live[a.name];
    if (!l) return a;
    const next = { ...a, term: l.term ?? a.term };
    if (l.machine_on !== null && l.machine_on !== undefined) next.machine_on = l.machine_on;
    if (l.queue) Object.assign(next, { tasks: l.queue.tasks, waiting: l.queue.waiting, line: l.queue.line, state: l.queue.state });
    if (l.paused !== null && l.paused !== undefined) {
      next.paused = l.paused;
      if (l.paused && a.state !== 'running') next.state = 'paused';
      else if (!l.paused && a.state === 'paused') next.state = 'ok';
    }
    return next;
  });
}

async function live() {
  if (parseRoute(location.hash).view !== 'tiles') return;
  try {
    agents = applyLive(agents, await api('GET', '/api/live'));
    render();
  } catch { /* the next tick tries again */ }
}

async function load(path = '/api/agents', method = 'GET') {
  try {
    const r = await api(method, path);
    agents = r.agents;
    checkedAt = agents.map((a) => a.checked_at).sort().at(-1) || null;
  } catch (e) { toast(e.message, true); }
  if (['tiles', 'agent', 'term'].includes(parseRoute(location.hash).view)) render();
}

if (typeof window !== 'undefined' && document.getElementById('main')) {
  window.addEventListener('hashchange', () => { window.scrollTo(0, 0); render(); });
  document.getElementById('refresh').addEventListener('click', () => load('/api/agents/refresh', 'POST'));
  load();
  // Only while someone is looking: nothing is fetched from a hidden tab.
  setInterval(() => { if (!document.hidden) load(); }, REFRESH_MS);
  setInterval(() => { if (!document.hidden) live(); }, LIVE_MS);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) { live(); load(); } });
  render();
}
