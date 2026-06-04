// ── Shared UI utilities ───────────────────────────────────────────────────────
// Pure, state-free helpers used across the dashboard panels. No imports.

// HTML-escape a string for safe interpolation into innerHTML.
export function esc(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

// Transient toast notification. Reuses a single #light-toast element; the last
// call wins and auto-fades after 4s. Pass isError=true for the error styling.
export function showToast(msg, isError = false) {
  let el = document.getElementById('light-toast');
  if (!el) {
    el = document.createElement('div');
    el.id = 'light-toast';
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.className = 'light-toast' + (isError ? ' light-toast-error' : '');
  el.style.opacity = '1';
  clearTimeout(el._timer);
  el._timer = setTimeout(() => { el.style.opacity = '0'; }, 4000);
}
