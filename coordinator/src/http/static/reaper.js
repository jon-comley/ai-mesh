import { createChatWidget } from '/static/chat.js';

const PLAY_STATE_LABELS = ['Stopped', 'Playing', 'Paused', '', '', 'Recording'];

export function init(panel) {
  panel.innerHTML = `
    <div class="reaper-header">
      <span class="reaper-badge" id="reaper-badge">Offline</span>
      <span class="reaper-state" id="reaper-state">—</span>
      <span class="reaper-hdr-field" id="reaper-position">—</span>
      <span class="reaper-hdr-field" id="reaper-tempo">—</span>
      <span class="reaper-hdr-field" id="reaper-ts">—</span>
    </div>
    <div class="reaper-chat"></div>
  `;
  createChatWidget(panel.querySelector('.reaper-chat'), {
    placeholder: 'Control REAPER — e.g. set tempo to 120, add ReaComp to drums…',
  });
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
}

function formatPosition(secs) {
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return `${m}:${String(s).padStart(2, '0')}`;
}
