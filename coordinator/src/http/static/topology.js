/** Nodes panel — renders a live list of mesh nodes. */

import { setPref } from '/static/prefs.js';

const ORDER_KEY = 'meshNodeOrder';
let container = null;
let dragSrc   = null;

export function init(el) {
  container = el;
  enableDrag(container, ORDER_KEY);
}

export function handleEvent(evt) {
  if (evt.type === 'TopologyUpdate') renderNodes(evt.nodes);
}

function renderNodes(nodes) {
  if (!container || dragSrc) return;
  if (!nodes || nodes.length === 0) {
    container.innerHTML = '<p class="placeholder">No nodes registered.</p>';
    return;
  }
  container.innerHTML = applyOrder(nodes, n => n.id).map(nodeCard).join('');
}

function nodeCard(n) {
  const roleClass   = n.role === 'Controller' ? 'badge-muted' : 'badge-green';
  const healthClass = { green: 'badge-green', amber: 'badge-amber', red: 'badge-red' }[n.health] ?? 'badge-muted';
  const age = formatAge(n.last_seen_secs);
  return `<div class="node-card" draggable="true" data-drag-id="${esc(n.id)}">
      <div class="node-header">
        <span class="node-health ${n.health}"></span>
        <span class="node-name">${esc(n.name)}</span>
        <span class="badge ${roleClass}">${esc(n.role)}</span>
      </div>
      <div class="node-meta">
        <span class="node-ip">${esc(n.ip)}</span>
        <span class="node-age ${healthClass}">${age}</span>
      </div>
      <div class="node-sparkline" id="ms-${esc(n.id)}"></div>
    </div>`;
}

function formatAge(secs) {
  if (secs < 5) return 'just now';
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  return `${Math.floor(secs / 3600)}h ago`;
}

// ── Drag-to-reorder ───────────────────────────────────────────────────────────

function enableDrag(el, key) {
  if (!el) return;

  el.addEventListener('dragstart', e => {
    const card = e.target.closest('[data-drag-id]');
    if (!card) return;
    dragSrc = card;
    e.dataTransfer.effectAllowed = 'move';
    requestAnimationFrame(() => card.classList.add('dragging'));
  });

  el.addEventListener('dragover', e => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    const card = e.target.closest('[data-drag-id]');
    if (!card || card === dragSrc) return;
    const after = e.clientY > card.getBoundingClientRect().top + card.offsetHeight / 2;
    el.insertBefore(dragSrc, after ? card.nextSibling : card);
  });

  el.addEventListener('dragend', () => {
    if (dragSrc) dragSrc.classList.remove('dragging');
    const ids = [...el.querySelectorAll('[data-drag-id]')].map(c => c.dataset.dragId);
    setPref(key, JSON.stringify(ids));
    dragSrc = null;
  });
}

function applyOrder(items, idFn) {
  try {
    const order = JSON.parse(localStorage.getItem(ORDER_KEY) ?? '[]');
    if (!order.length) return items;
    return [...items].sort((a, b) => {
      const ia = order.indexOf(idFn(a));
      const ib = order.indexOf(idFn(b));
      return (ia === -1 ? Infinity : ia) - (ib === -1 ? Infinity : ib);
    });
  } catch { return items; }
}

// ── Utilities ─────────────────────────────────────────────────────────────────

function esc(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}
