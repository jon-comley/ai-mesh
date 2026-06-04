// ── Authenticated API client ──────────────────────────────────────────────────
// One place for talking to the coordinator's /api endpoints. Pure leaf — no
// imports, no module state (the token lives in localStorage).

// The dashboard auth token, persisted in localStorage by the login flow.
export function tok() { return localStorage.getItem('meshToken') ?? ''; }

// Authenticated JSON API call: builds `/api<path>` with the token, JSON-encodes
// an optional body, and returns the Response (callers check res.ok / handle
// errors as they need). `path` is everything after `/api`, e.g. '/rooms' or
// `/rooms/${encodeURIComponent(id)}/name`.
export function api(path, { method = 'GET', body } = {}) {
  const sep = path.includes('?') ? '&' : '?';
  const url = `/api${path}${sep}token=${encodeURIComponent(tok())}`;
  const opts = { method };
  if (body !== undefined) {
    opts.headers = { 'Content-Type': 'application/json' };
    opts.body = JSON.stringify(body);
  }
  return fetch(url, opts);
}
