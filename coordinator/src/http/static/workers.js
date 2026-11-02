// ── Workers tab ───────────────────────────────────────────────────────────────
// The agent watch page (plans/agent-watch.md) is served by lenovo1, not by the
// coordinator, so it keeps working when pi1 is off its wifi. This tab only
// shows that page in a frame; nothing about the workers passes through pi1.

export const WORKERS_URL = 'http://192.168.1.47/';

export function init(panel) {
  panel.innerHTML = `
    <div class="workers-bar">
      <h2>Workers</h2>
      <a href="${WORKERS_URL}" target="_blank" rel="noopener">Open in its own tab</a>
    </div>
    <iframe class="workers-frame" title="Workers" src="${WORKERS_URL}"></iframe>`;
}
