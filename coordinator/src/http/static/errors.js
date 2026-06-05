let container = null;

export function init(el) {
  container = el;
  container.innerHTML = '<ul class="error-feed-list"></ul>';
}

export function handleErrorUpdate(evt) {
  if (!container) return;
  const list = container.querySelector('.error-feed-list');
  if (!list) return;
  list.innerHTML = '';
  for (const entry of evt.errors) {
    const li = document.createElement('li');
    li.className = `error-entry level-${entry.level.toLowerCase()}`;
    const time = new Date(entry.ts_ms).toLocaleTimeString();
    li.innerHTML =
      `<span class="err-time">${time}</span>` +
      `<span class="err-level ${entry.level.toLowerCase()}">${entry.level}</span>` +
      `<span class="err-target">${escHtml(entry.target)}</span>` +
      `<span class="err-msg">${escHtml(entry.message)}</span>`;
    list.appendChild(li);
  }
}

function escHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}
