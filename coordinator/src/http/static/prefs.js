/** Dashboard preferences — hybrid localStorage + server sync.
 *
 * On load: fetch all prefs from the server and merge into localStorage so the
 * dashboard looks the same from any device or browser. Server wins on conflict.
 *
 * On write: write to localStorage immediately (zero-latency), then fire an
 * async PUT to the server (best-effort; a failed write just leaves that device
 * as the source of truth until the next successful write).
 */

function token() {
  return localStorage.getItem('meshToken') ?? '';
}

export async function loadPrefs() {
  try {
    const res = await fetch('/api/preferences?token=' + encodeURIComponent(token()));
    if (!res.ok) return;
    const prefs = await res.json();
    for (const [k, v] of Object.entries(prefs)) {
      localStorage.setItem(k, v);
    }
  } catch {}
}

export function setPref(key, value) {
  localStorage.setItem(key, value);
  fetch('/api/preferences/' + encodeURIComponent(key) + '?token=' + encodeURIComponent(token()), {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ value }),
  }).catch(() => {});
}

const _debounceTimers = new Map();
export function setPrefDebounced(key, value, delay = 200) {
  localStorage.setItem(key, value);
  clearTimeout(_debounceTimers.get(key));
  _debounceTimers.set(key, setTimeout(() => {
    _debounceTimers.delete(key);
    fetch('/api/preferences/' + encodeURIComponent(key) + '?token=' + encodeURIComponent(token()), {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ value }),
    }).catch(() => {});
  }, delay));
}
