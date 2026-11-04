// ── Workers tab ───────────────────────────────────────────────────────────────
// The agent watch page (plans/agent-watch.md) is served by lenovo1, not by the
// coordinator, so it keeps working when pi1 is off its wifi. This tab only
// shows that page in a frame; nothing about the workers passes through pi1.

// Over https (https://lenovo1.tail3c4ee3.ts.net/) a plain-http frame is blocked, so the
// Workers page comes through Tailscale serve on 8443 there.
export const WORKERS_URL = typeof location !== 'undefined' && location.protocol === 'https:'
  ? 'https://lenovo1.tail3c4ee3.ts.net:8443/'
  : 'http://192.168.1.47/';

export function init(panel) {
  panel.innerHTML = `<iframe class="workers-frame" title="Workers" src="${WORKERS_URL}"></iframe>`;
}
