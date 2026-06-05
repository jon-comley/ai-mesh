let container = null;

const KIND_LABELS = {
  node_join: 'Node Join',
  node_leave: 'Node Leave',
  node_auth_failed: 'Auth Failed',
  dashboard_connect: 'Dashboard',
};

const KIND_CLASS = {
  node_join: 'sec-join',
  node_leave: 'sec-leave',
  node_auth_failed: 'sec-fail',
  dashboard_connect: 'sec-dash',
};

export function init(el) {
  container = el;
  container.innerHTML = `
    <table class="security-table">
      <thead>
        <tr><th>Time</th><th>Event</th><th>Source</th><th>Detail</th></tr>
      </thead>
      <tbody class="security-tbody"></tbody>
    </table>`;
}

export function handleSecurityUpdate(evt) {
  if (!container) return;
  const tbody = container.querySelector('.security-tbody');
  if (!tbody) return;
  tbody.innerHTML = '';
  for (const ev of evt.events) {
    const tr = document.createElement('tr');
    tr.className = KIND_CLASS[ev.kind] ?? '';
    const time = new Date(ev.ts_ms).toLocaleTimeString();
    tr.innerHTML =
      `<td>${time}</td>` +
      `<td>${KIND_LABELS[ev.kind] ?? ev.kind}</td>` +
      `<td>${escHtml(ev.source)}</td>` +
      `<td>${escHtml(ev.detail)}</td>`;
    tbody.appendChild(tr);
  }
}

function escHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}
