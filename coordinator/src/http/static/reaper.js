const PLAY_STATE_LABELS = ['Stopped', 'Playing', 'Paused', '', '', 'Recording'];

let logEl = null;
let cmdLog = [];

export function init(panel) {
  panel.innerHTML = `
    <div class="reaper-status">
      <span class="reaper-badge" id="reaper-badge">Offline</span>
      <span class="reaper-state" id="reaper-state">—</span>
    </div>
    <div class="reaper-info">
      <div class="reaper-field"><label>Position</label><span id="reaper-position">—</span></div>
      <div class="reaper-field"><label>Tempo</label><span id="reaper-tempo">—</span></div>
      <div class="reaper-field"><label>Time Sig</label><span id="reaper-ts">—</span></div>
    </div>
    <h3 class="reaper-log-title">Command Log</h3>
    <ul class="reaper-log" id="reaper-log"></ul>
  `;
  logEl = panel.querySelector('#reaper-log');
}

export function handleReaperUpdate(evt) {
  const badge = document.getElementById('reaper-badge');
  const stateEl = document.getElementById('reaper-state');
  const posEl = document.getElementById('reaper-position');
  const tempoEl = document.getElementById('reaper-tempo');
  const tsEl = document.getElementById('reaper-ts');

  if (badge) {
    badge.textContent = evt.online ? 'Online' : 'Offline';
    badge.dataset.online = evt.online ? '1' : '0';
  }
  if (stateEl) stateEl.textContent = evt.online ? (PLAY_STATE_LABELS[evt.play_state] ?? '?') : '—';
  if (posEl) posEl.textContent = evt.online ? formatPosition(evt.position) : '—';
  if (tempoEl) tempoEl.textContent = evt.online ? `${evt.tempo.toFixed(1)} BPM` : '—';
  if (tsEl) tsEl.textContent = evt.online ? `${evt.ts_num}/${evt.ts_denom}` : '—';

  if (evt.last_command) {
    const [action, ok, message] = evt.last_command;
    appendLog(action, ok, message);
  }
}

function formatPosition(secs) {
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return `${m}:${String(s).padStart(2, '0')}`;
}

function appendLog(action, ok, message) {
  const now = new Date().toLocaleTimeString();
  cmdLog.unshift({ ts: now, action, ok, message });
  if (cmdLog.length > 10) cmdLog.length = 10;
  renderLog();
}

function renderLog() {
  if (!logEl) return;
  logEl.innerHTML = cmdLog.map(entry => `
    <li class="reaper-log-entry ${entry.ok ? 'ok' : 'err'}">
      <span class="reaper-log-ts">${entry.ts}</span>
      <span class="reaper-log-icon">${entry.ok ? '✓' : '✗'}</span>
      <span class="reaper-log-action">${entry.action}</span>
      ${entry.message && entry.message !== 'ok' ? `<span class="reaper-log-msg">${entry.message}</span>` : ''}
    </li>
  `).join('');
}
