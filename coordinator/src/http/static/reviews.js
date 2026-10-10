import { api } from '/static/api.js';
import { esc, showToast } from '/static/util.js';

// ── Reviews tab (code reviews run by mac1) ──────────────────────────────────
// mac1 owns the repos, runs and findings; the coordinator relays its latest
// snapshot (GET /api/reviews, then ReviewUpdate events) and passes button
// presses back to it. See docs/code-review.md.

let view = { online: false, snapshot: null, roles: { control: [], work: [] }, models: [] };
let editing = null; // repo name being edited, or '' for a new one

const DAYS = ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'];

// "02:15, 6:00" → [135, 360]. Unreadable entries are dropped.
export function parseTimes(text) {
  return String(text ?? '')
    .split(',')
    .map(t => t.trim())
    .map(t => /^(\d{1,2}):(\d{2})$/.exec(t))
    .filter(m => m && Number(m[1]) < 24 && Number(m[2]) < 60)
    .map(m => Number(m[1]) * 60 + Number(m[2]));
}

export function formatTime(minutes) {
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
}

// One alias per line: "@app/ = guv:src".
export function parseAliases(text) {
  return String(text ?? '')
    .split('\n')
    .map(l => /^\s*(\S+)\s*=\s*([\w.-]+)\s*:\s*(\S*)\s*$/.exec(l))
    .filter(Boolean)
    .map(m => ({ prefix: m[1], repo: m[2], dir: m[3] }));
}

export function formatAliases(aliases) {
  return (aliases ?? []).map(a => `${a.prefix} = ${a.repo}:${a.dir}`).join('\n');
}

// The run-now body for the "Review now" picker.
export function onDemandBody(repo, what, arg) {
  const body = { repo, sweep: what === 'sweep' };
  const value = String(arg ?? '').trim();
  if (what === 'path') {
    if (!value) return null;
    body.path = value;
  } else if (what === 'branch') {
    if (!value) return null;
    body.branch = value;
  }
  return body;
}

const QUESTION_STATUS = {
  waiting: 'waiting for a free machine',
  thinking: 'reading the code…',
  answered: 'answered',
  failed: 'could not answer',
};

export function scheduleText(spec) {
  const parts = [];
  if (spec.timeslots?.length) parts.push(`nightly ${spec.timeslots.map(formatTime).join(', ')}`);
  if (spec.sweep_day != null && spec.sweep_slot != null) {
    parts.push(`sweep ${DAYS[spec.sweep_day]} ${formatTime(spec.sweep_slot)}`);
  }
  if (!spec.enabled) return 'paused';
  return parts.length ? parts.join(' · ') : 'Run now only';
}

export function init(panel) {
  panel.innerHTML = `
    <div class="ebay-head">
      <h2>Reviews</h2>
      <span id="rv-status" class="gw-hint"></span>
      <button id="rv-settings-show" class="ebay-settings-show" type="button" hidden>Settings</button>
    </div>
    <p id="rv-notice" class="rv-notice" hidden></p>
    <div class="ebay-layout">
      <aside class="ebay-sidebar">
        <button id="rv-add" type="button">+ Add repo</button>
        <div id="rv-editor" class="ebay-editor" hidden></div>
        <div id="rv-repos"><p class="placeholder">No repos yet.</p></div>
        <details class="ebay-settings" id="rv-settings">
          <summary>Review settings</summary>
          <div class="gw">
            <div class="gw-field gw-inline">
              <label for="rv-ntfy">ntfy topic</label>
              <input id="rv-ntfy" type="text" autocomplete="off" placeholder="https://ntfy.sh/your-private-topic">
              <button id="rv-ntfy-save" type="button">Save</button>
            </div>
            <span id="rv-ntfy-status" class="gw-hint"></span>
            <div class="gw-field gw-inline">
              <label for="rv-max">Task size</label>
              <input id="rv-max" type="number" min="4000" step="1000">
              <span class="gw-hint">tokens overnight</span>
            </div>
            <div class="gw-field gw-inline">
              <label for="rv-evening">Evening</label>
              <input id="rv-evening" type="number" min="4000" step="1000">
              <span class="gw-hint">tokens, 17:00–23:00</span>
              <button id="rv-limits-save" type="button">Save</button>
            </div>
            <div class="gw-field">
              <span class="gw-label">Models: who answers home commands, who does reviews</span>
              <div id="rv-roles"></div>
              <button id="rv-roles-save" type="button">Save models</button>
            </div>
          </div>
        </details>
      </aside>
      <section class="ebay-main">
        <h3>Ask about the code</h3>
        <div class="rv-ask">
          <select id="rv-ask-repo" aria-label="Repo"></select>
          <textarea id="rv-ask-text" rows="2" maxlength="2000" placeholder="Where is the invoice total worked out?"></textarea>
          <button id="rv-ask-go" type="button">Ask</button>
        </div>
        <div id="rv-questions"></div>
        <h3>Review now</h3>
        <div class="rv-ondemand">
          <select id="rv-od-repo" aria-label="Repo"></select>
          <select id="rv-od-what" aria-label="What to review">
            <option value="new">New commits</option>
            <option value="sweep">The next folder</option>
            <option value="path">A folder or file…</option>
            <option value="branch">A branch…</option>
          </select>
          <input id="rv-od-arg" type="text" autocomplete="off" hidden>
          <button id="rv-od-go" type="button">Start</button>
        </div>
        <h3>Runs</h3>
        <div id="rv-runs"><p class="placeholder">No runs yet.</p></div>
        <div id="rv-report" class="rv-report" hidden></div>
        <h3>Open findings</h3>
        <div id="rv-findings"><p class="placeholder">Nothing open.</p></div>
      </section>
    </div>
  `;

  panel.querySelector('#rv-add').addEventListener('click', () => openEditor(''));
  panel.querySelector('#rv-settings-show').addEventListener('click', () => {
    const details = panel.querySelector('#rv-settings');
    details.hidden = false;
    details.open = true;
    panel.querySelector('#rv-settings-show').hidden = true;
  });
  panel.querySelector('#rv-ntfy-save').addEventListener('click', () => {
    const input = panel.querySelector('#rv-ntfy');
    post('/reviews/settings', { ntfy_topic_url: input.value.trim() }).then(ok => { if (ok) input.value = ''; });
  });
  panel.querySelector('#rv-limits-save').addEventListener('click', () => {
    const max = Number(panel.querySelector('#rv-max').value);
    const evening = Number(panel.querySelector('#rv-evening').value);
    post('/reviews/settings', {
      max_review_tokens: max > 0 ? max : undefined,
      evening_max_tokens: evening > 0 ? evening : undefined,
    });
  });
  panel.querySelector('#rv-roles-save').addEventListener('click', saveRoles);
  panel.querySelector('#rv-ask-go').addEventListener('click', askQuestion);
  panel.querySelector('#rv-ask-text').addEventListener('keydown', e => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) askQuestion();
  });
  panel.querySelector('#rv-od-what').addEventListener('change', e => {
    const arg = panel.querySelector('#rv-od-arg');
    arg.hidden = !['path', 'branch'].includes(e.target.value);
    arg.placeholder = e.target.value === 'path' ? 'src/services' : 'feature-branch';
  });
  panel.querySelector('#rv-od-go').addEventListener('click', async () => {
    const repo = panel.querySelector('#rv-od-repo').value;
    const what = panel.querySelector('#rv-od-what').value;
    const body = onDemandBody(repo, what, panel.querySelector('#rv-od-arg').value);
    if (!repo) { showToast('Add a repo first', true); return; }
    if (!body) { showToast(what === 'path' ? 'Which folder or file?' : 'Which branch?', true); return; }
    if (await post('/reviews/run-now', body)) showToast(`${repo}: review queued`);
  });
  panel.addEventListener('click', onClick);
  refresh();
}

export async function refresh() {
  try {
    const res = await api('/reviews');
    if (res.ok) { view = await res.json(); render(); }
  } catch { /* the dashboard shows the disconnected state elsewhere */ }
}

// WebSocket: mac1 sent a new snapshot.
export function handleUpdate(evt) {
  view.snapshot = evt.snapshot;
  view.online = true;
  render();
}

async function post(path, body, method = 'POST') {
  const res = await api(path, { method, body });
  if (!res.ok) {
    let msg = `Failed (${res.status})`;
    try { msg = (await res.json()).error ?? msg; } catch { /* not JSON */ }
    showToast(msg, true);
    return false;
  }
  return true;
}

function render() {
  const snap = view.snapshot;
  const status = document.getElementById('rv-status');
  if (status) {
    status.textContent = view.online
      ? `running on ${snap?.hostname ?? 'mac1'}`
      : 'mac1 is offline — showing the last state it sent';
    status.dataset.ok = view.online ? '1' : '0';
  }
  const notice = document.getElementById('rv-notice');
  if (notice) {
    notice.hidden = !snap?.notice;
    notice.textContent = snap?.notice ?? '';
  }
  renderRepos(snap?.repos ?? []);
  fillRepoSelects(snap?.repos ?? []);
  renderQuestions(snap?.questions ?? []);
  renderRuns(snap?.runs ?? []);
  renderFindings(snap?.findings ?? []);
  renderSettings(snap?.settings);
  renderRoles();
}

function renderRepos(repos) {
  const el = document.getElementById('rv-repos');
  if (!el) return;
  if (!repos.length) { el.innerHTML = '<p class="placeholder">No repos yet.</p>'; return; }
  el.innerHTML = repos.map(r => `
    <div class="ebay-hunt-row${r.enabled ? '' : ' ebay-hunt-off'}" data-repo="${esc(r.name)}">
      <strong>${esc(r.name)}</strong> <span class="gw-hint">${esc(r.branch)}</span>
      <div class="gw-hint">${esc(scheduleText(r))}</div>
      <div class="gw-hint">${r.last_reviewed_commit ? `reviewed to ${esc(r.last_reviewed_commit.slice(0, 7))}` : 'not reviewed yet'}${r.last_sweep_folder ? ` · last swept ${esc(r.last_sweep_folder)}` : ''}</div>
      <div class="rv-actions">
        <button type="button" data-act="run" data-repo="${esc(r.name)}">Run now</button>
        <button type="button" data-act="sweep" data-repo="${esc(r.name)}">Sweep a folder</button>
        <button type="button" data-act="edit" data-repo="${esc(r.name)}">Edit</button>
      </div>
    </div>`).join('');
}

function fillRepoSelects(repos) {
  for (const id of ['rv-ask-repo', 'rv-od-repo']) {
    const sel = document.getElementById(id);
    if (!sel) continue;
    const keep = sel.value;
    sel.innerHTML = repos.map(r => `<option value="${esc(r.name)}">${esc(r.name)}</option>`).join('');
    if (repos.some(r => r.name === keep)) sel.value = keep;
  }
}

function renderQuestions(questions) {
  const el = document.getElementById('rv-questions');
  if (!el) return;
  if (!questions.length) { el.innerHTML = ''; return; }
  el.innerHTML = questions.map(q => `
    <div class="rv-question rv-q-${esc(q.status)}">
      <div><strong>${esc(q.repo)}:</strong> ${esc(q.question)}</div>
      <div class="gw-hint">${esc(QUESTION_STATUS[q.status] ?? q.status)}${q.worker ? ` · ${esc(q.worker)}` : ''}</div>
      ${q.answer ? `<div class="rv-answer">${esc(q.answer)}</div>` : ''}
      ${q.error ? `<div class="gw-hint" data-ok="0">${esc(q.error)}</div>` : ''}
      ${q.sources?.length ? `<details><summary class="gw-hint">${q.sources.length} file${q.sources.length === 1 ? '' : 's'} read</summary>
        <div class="gw-hint">${q.sources.map(esc).join('<br>')}</div></details>` : ''}
    </div>`).join('');
}

async function askQuestion() {
  const repo = document.getElementById('rv-ask-repo')?.value;
  const input = document.getElementById('rv-ask-text');
  const question = input?.value.trim();
  if (!repo) { showToast('Add a repo first', true); return; }
  if (!question) { showToast('Type a question first', true); return; }
  if (await post('/reviews/ask', { repo, question })) {
    input.value = '';
    showToast('Asked — the answer will appear here');
  }
}

function runStatusText(run) {
  if (run.status === 'running') {
    return run.tasks_total ? `running · ${run.tasks_done} of ${run.tasks_total} tasks` : 'running · getting ready';
  }
  if (run.status === 'done') {
    const c = run.counts ?? {};
    return `done · ${c.high ?? 0} high, ${c.medium ?? 0} medium, ${c.low ?? 0} low, ${c.unconfirmed ?? 0} not confirmed`;
  }
  if (run.status === 'nothing_new') return 'nothing to review';
  if (run.status === 'failed') return `failed: ${run.error ?? 'unknown error'}`;
  return run.status;
}

function renderRuns(runs) {
  const el = document.getElementById('rv-runs');
  if (!el) return;
  if (!runs.length) { el.innerHTML = '<p class="placeholder">No runs yet.</p>'; return; }
  el.innerHTML = runs.map(run => `
    <div class="rv-run rv-run-${esc(run.status)}">
      <div><strong>${esc(run.repo)}</strong> <span class="gw-hint">${esc(run.kind)}</span>
        ${run.started_at ? `<span class="gw-hint">${new Date(run.started_at * 1000).toLocaleString('en-GB')}</span>` : ''}</div>
      <div class="gw-hint">${esc(run.scope)}</div>
      <div>${esc(runStatusText(run))}</div>
      ${(run.running ?? []).map(t => `<div class="rv-task">${esc(t.kind)} on <strong>${esc(t.worker)}</strong>: ${esc(t.label)}</div>`).join('')}
      ${run.status === 'done' ? `<button type="button" data-act="report" data-run="${run.id}">Report</button>` : ''}
    </div>`).join('');
}

function renderFindings(findings) {
  const el = document.getElementById('rv-findings');
  if (!el) return;
  if (!findings.length) { el.innerHTML = '<p class="placeholder">Nothing open.</p>'; return; }
  const groups = [['high', 'High'], ['medium', 'Medium'], ['low', 'Low']];
  el.innerHTML = groups.map(([sev, label]) => {
    const list = findings.filter(f => f.severity === sev);
    if (!list.length) return '';
    return `<h4>${label}</h4>` + list.map(f => `
      <div class="rv-finding" data-id="${esc(f.id)}">
        <div><strong>${esc(f.title)}</strong>${f.verdict === 'confirmed' ? '' : ' <span class="gw-hint">(not confirmed)</span>'}</div>
        <div class="gw-hint"><code>${esc(`${f.repo}/${f.path}:${f.line}`)}</code></div>
        ${f.scenario ? `<div>${esc(f.scenario)}</div>` : ''}
        ${f.fix ? `<div class="gw-hint">Fix: ${esc(f.fix)}</div>` : ''}
        <pre class="rv-quote">${esc(f.quote)}</pre>
        <div class="rv-actions">
          <button type="button" data-act="fixed" data-id="${esc(f.id)}">Fixed</button>
          <button type="button" data-act="dismiss" data-id="${esc(f.id)}">Dismiss</button>
        </div>
      </div>`).join('');
  }).join('');
}

function renderSettings(settings) {
  if (!settings) return;
  const ntfy = document.getElementById('rv-ntfy-status');
  if (ntfy) ntfy.textContent = settings.ntfy_topic_set ? `topic set ${settings.ntfy_hint ?? ''}` : 'no notifications';
  const max = document.getElementById('rv-max');
  if (max && document.activeElement !== max) max.value = settings.max_review_tokens;
  const evening = document.getElementById('rv-evening');
  if (evening && document.activeElement !== evening) evening.value = settings.evening_max_tokens;
  // Out of the way once notifications are set, as on the Hunts tab; left alone while open.
  const details = document.getElementById('rv-settings');
  const show = document.getElementById('rv-settings-show');
  if (details && show && !details.open) {
    details.hidden = !!settings.ntfy_topic_set;
    show.hidden = !settings.ntfy_topic_set;
  }
}

function renderRoles() {
  const el = document.getElementById('rv-roles');
  if (!el) return;
  const models = [...new Set([...(view.models ?? []), ...view.roles.control, ...view.roles.work])].sort();
  if (!models.length) { el.innerHTML = '<p class="placeholder">No models loaded.</p>'; return; }
  el.innerHTML = `<p class="gw-hint">Nothing ticked in a column means every model.</p>` + models.map(m => `
    <div class="rv-role-row">
      <span>${esc(m)}</span>
      <label><input type="checkbox" data-role="control" value="${esc(m)}" ${view.roles.control.includes(m) ? 'checked' : ''}> home</label>
      <label><input type="checkbox" data-role="work" value="${esc(m)}" ${view.roles.work.includes(m) ? 'checked' : ''}> reviews</label>
    </div>`).join('');
}

async function saveRoles() {
  const pick = role => [...document.querySelectorAll(`#rv-roles input[data-role="${role}"]:checked`)].map(i => i.value);
  const res = await api('/work/roles', { method: 'POST', body: { control: pick('control'), work: pick('work') } });
  if (res.ok) { view.roles = await res.json(); renderRoles(); showToast('Models saved'); }
  else showToast('Could not save the models', true);
}

function openEditor(name) {
  editing = name;
  const spec = (view.snapshot?.repos ?? []).find(r => r.name === name) ?? {
    name: '', url: '', branch: 'main', timeslots: [135], sweep_day: 6, sweep_slot: 195, enabled: true, aliases: [],
  };
  const el = document.getElementById('rv-editor');
  el.hidden = false;
  el.innerHTML = `
    <div class="gw-field"><label>Name</label><input id="rv-e-name" type="text" value="${esc(spec.name)}" ${name ? 'disabled' : ''}></div>
    <div class="gw-field"><label>GitHub URL</label><input id="rv-e-url" type="text" value="${esc(spec.url)}" placeholder="git@github-dashboard:jon-comley/dashboard.git"></div>
    <div class="gw-field"><label>Branch</label><input id="rv-e-branch" type="text" value="${esc(spec.branch)}"></div>
    <div class="gw-field"><label>Nightly times</label><input id="rv-e-times" type="text" value="${esc((spec.timeslots ?? []).map(formatTime).join(', '))}" placeholder="02:15"></div>
    <div class="gw-field"><label>Weekly sweep</label>
      <select id="rv-e-day"><option value="">none</option>${DAYS.map((d, i) => `<option value="${i}" ${spec.sweep_day === i ? 'selected' : ''}>${d}</option>`).join('')}</select>
      <input id="rv-e-sweep" type="text" value="${spec.sweep_slot != null ? formatTime(spec.sweep_slot) : ''}" placeholder="03:15"></div>
    <div class="gw-field"><label>Imports from other repos</label>
      <textarea id="rv-e-aliases" rows="2" placeholder="@app/ = guv:src">${esc(formatAliases(spec.aliases))}</textarea></div>
    <label><input id="rv-e-enabled" type="checkbox" ${spec.enabled ? 'checked' : ''}> scheduled runs on</label>
    <div class="rv-actions">
      <button type="button" data-act="save-repo">Save</button>
      <button type="button" data-act="cancel-edit">Cancel</button>
      ${name ? '<button type="button" data-act="remove-repo">Remove</button>' : ''}
    </div>`;
}

function editorSpec() {
  const v = id => document.getElementById(id)?.value ?? '';
  const day = v('rv-e-day');
  const sweep = parseTimes(v('rv-e-sweep'));
  return {
    name: editing || v('rv-e-name').trim(),
    url: v('rv-e-url').trim(),
    branch: v('rv-e-branch').trim() || 'main',
    timeslots: parseTimes(v('rv-e-times')),
    sweep_day: day === '' ? null : Number(day),
    sweep_slot: day === '' || !sweep.length ? null : sweep[0],
    enabled: document.getElementById('rv-e-enabled')?.checked ?? true,
    aliases: parseAliases(v('rv-e-aliases')),
  };
}

function closeEditor() {
  editing = null;
  const el = document.getElementById('rv-editor');
  if (el) { el.hidden = true; el.innerHTML = ''; }
}

async function showReport(runId) {
  const el = document.getElementById('rv-report');
  el.hidden = false;
  el.innerHTML = '<p class="placeholder">Fetching the report from mac1…</p>';
  const res = await api(`/reviews/runs/${encodeURIComponent(runId)}/report`);
  if (!res.ok) { el.innerHTML = '<p class="placeholder">The report could not be fetched.</p>'; return; }
  const text = await res.text();
  el.innerHTML = `
    <div class="rv-actions"><button type="button" data-act="copy-report">Copy report</button>
    <button type="button" data-act="close-report">Close</button></div>
    <pre class="rv-report-text"></pre>`;
  el.querySelector('.rv-report-text').textContent = text;
}

async function onClick(e) {
  const btn = e.target.closest('button[data-act]');
  if (!btn) return;
  const act = btn.dataset.act;
  if (act === 'run' || act === 'sweep') {
    if (await post('/reviews/run-now', { repo: btn.dataset.repo, sweep: act === 'sweep' })) {
      showToast(`${btn.dataset.repo}: review queued`);
    }
  } else if (act === 'edit') {
    openEditor(btn.dataset.repo);
  } else if (act === 'save-repo') {
    const spec = editorSpec();
    if (!spec.name || !spec.url) { showToast('A name and a GitHub URL are needed', true); return; }
    if (await post('/reviews/repos', spec)) closeEditor();
  } else if (act === 'cancel-edit') {
    closeEditor();
  } else if (act === 'remove-repo') {
    if (confirm(`Stop reviewing ${editing}? Its findings stay.`)) {
      if (await post(`/reviews/repos/${encodeURIComponent(editing)}`, undefined, 'DELETE')) closeEditor();
    }
  } else if (act === 'fixed' || act === 'dismiss') {
    const status = act === 'fixed' ? 'fixed' : 'dismissed';
    if (await post(`/reviews/findings/${encodeURIComponent(btn.dataset.id)}`, { status })) {
      btn.closest('.rv-finding')?.remove();
    }
  } else if (act === 'report') {
    showReport(btn.dataset.run);
  } else if (act === 'copy-report') {
    const text = document.querySelector('.rv-report-text')?.textContent ?? '';
    try { await navigator.clipboard.writeText(text); showToast('Report copied'); }
    catch { showToast('Copy failed — select the text instead', true); }
  } else if (act === 'close-report') {
    const el = document.getElementById('rv-report');
    el.hidden = true;
    el.innerHTML = '';
  }
}
