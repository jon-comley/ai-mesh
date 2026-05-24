/** Nodes panel — renders a live list of mesh nodes. */

let container = null;

export function init(el) {
  container = el;
}

export function handleEvent(evt) {
  if (evt.type === 'TopologyUpdate') renderNodes(evt.nodes);
}

function renderNodes(nodes) {
  if (!container) return;

  if (!nodes || nodes.length === 0) {
    container.innerHTML = '<p class="placeholder">No nodes registered.</p>';
    return;
  }

  container.innerHTML = nodes.map(nodeCard).join('');
}

function nodeCard(n) {
  const roleClass = n.role === 'Controller' ? 'badge-muted' : 'badge-green';
  const healthClass = { green: 'badge-green', amber: 'badge-amber', red: 'badge-red' }[n.health] ?? 'badge-muted';
  const age = formatAge(n.last_seen_secs);

  return `
    <div class="node-card">
      <div class="node-header">
        <span class="node-health ${n.health}"></span>
        <span class="node-name">${esc(n.name)}</span>
        <span class="badge ${roleClass}">${esc(n.role)}</span>
      </div>
      <div class="node-meta">
        <span class="node-ip">${esc(n.ip)}</span>
        <span class="node-age ${healthClass}">${age}</span>
      </div>
    </div>`;
}

function formatAge(secs) {
  if (secs < 5) return 'just now';
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  return `${Math.floor(secs / 3600)}h ago`;
}

function esc(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}
