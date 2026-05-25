/** Models panel: per-node model list with load/unload controls. */

import { getLatestSample } from '/static/health.js';

const ORDER_KEY   = 'meshModelOrder';
const nodesMap    = new Map(); // nodeId -> NodeModelInfo
const modelListEl = document.getElementById('model-list');
let dragSrc = null;

// ── Public API ────────────────────────────────────────────────────────────────

export function handleModelUpdate(evt) {
  nodesMap.clear();
  for (const node of evt.nodes) nodesMap.set(node.node_id, node);
  render();
}

/** Re-render capacity bars with fresh health data — call on each HealthUpdate. */
export function repaintModels() {
  render();
}

// ── Event delegation ──────────────────────────────────────────────────────────

if (modelListEl) {
  enableDrag(modelListEl);
  modelListEl.addEventListener('click', e => {
    const unloadBtn = e.target.closest('[data-unload-node]');
    if (unloadBtn) {
      unloadModel(unloadBtn.dataset.unloadNode, unloadBtn.dataset.unloadModel);
      return;
    }
    const loadBtn = e.target.closest('[data-load-node]');
    if (loadBtn) promptLoad(loadBtn.dataset.loadNode);
  });
}

// ── Render ────────────────────────────────────────────────────────────────────

function render() {
  if (!modelListEl || dragSrc) return;
  if (nodesMap.size === 0) {
    modelListEl.innerHTML = '<p class="placeholder">No model data yet.</p>';
    return;
  }
  const nodes = applyOrder([...nodesMap.values()], n => n.node_id);
  modelListEl.innerHTML = nodes.map(nodeCard).join('');
}

function nodeCard(node) {
  const sample  = getLatestSample(node.node_id);
  const hasVram = sample && sample.gpu_vram_total_gb != null && sample.gpu_vram_total_gb > 0;
  const hasRam  = sample && sample.ram_total_gb > 0;

  const vramBar = hasVram
    ? capacityBar('VRAM', sample.gpu_vram_used_gb ?? 0, sample.gpu_vram_total_gb, 'var(--amber)')
    : '';
  const ramBar = hasRam
    ? capacityBar('RAM', sample.ram_used_gb, sample.ram_total_gb, 'var(--green)')
    : '';

  const modelRows = node.models.length > 0
    ? node.models.map(m => modelRow(node.node_id, m)).join('')
    : '<p class="model-empty">No models loaded.</p>';

  return `<div class="model-card" draggable="true" data-drag-id="${esc(node.node_id)}">
  <div class="model-card-header">
    <span class="model-node-name">${esc(node.hostname)}</span>
    <span class="badge badge-muted">${esc(node.role)}</span>
  </div>
  ${vramBar}${ramBar}
  <div class="model-rows">${modelRows}</div>
  <div class="model-card-footer">
    <button class="load-btn" data-load-node="${esc(node.node_id)}">+ Load model…</button>
  </div>
</div>`;
}

function capacityBar(label, used, total, color) {
  const pct      = Math.min((used / total) * 100, 100).toFixed(1);
  const usedStr  = used.toFixed(1);
  const totalStr = total.toFixed(1);
  return `<div class="capacity-row">
  <span class="capacity-label">${label}</span>
  <div class="capacity-track"><div class="capacity-fill" style="width:${pct}%;background:${color}"></div></div>
  <span class="capacity-value">${usedStr} / ${totalStr} GB</span>
</div>`;
}

function modelRow(nodeId, model) {
  const badgeClass = { ready: 'badge-green', loading: 'badge-amber', failed: 'badge-red' }[model.state.toLowerCase()] ?? 'badge-muted';
  const sizeStr    = model.size_mb >= 1000
    ? `${(model.size_mb / 1024).toFixed(1)} GB`
    : `${model.size_mb} MB`;
  const failNote = model.reason ? `: ${esc(model.reason)}` : '';
  return `<div class="model-row-item">
  <div class="model-row-main">
    <span class="model-name">${esc(model.name)}</span>
    <span class="badge ${badgeClass}">${esc(model.state)}${failNote}</span>
  </div>
  <div class="model-row-sub">
    <span class="model-size">${sizeStr}</span>
    <button class="unload-btn" data-unload-node="${esc(nodeId)}" data-unload-model="${esc(model.name)}">Unload</button>
  </div>
</div>`;
}

// ── API calls ─────────────────────────────────────────────────────────────────

function promptLoad(nodeId) {
  const name = window.prompt('Model name (e.g. qwen2.5:7b):');
  if (!name || !name.trim()) return;
  const rawMb = window.prompt('Model size in MB (e.g. 4000):');
  if (!rawMb) return;
  const sizeMb = parseInt(rawMb, 10);
  if (!Number.isFinite(sizeMb) || sizeMb < 1) {
    window.alert('Size must be a positive integer in MB.');
    return;
  }
  const token = localStorage.getItem('meshToken') ?? '';
  fetch(`/api/models/load?token=${encodeURIComponent(token)}`, {
    method:  'POST',
    headers: { 'content-type': 'application/json' },
    body:    JSON.stringify({ node_id: nodeId, model_name: name.trim(), size_mb: sizeMb }),
  })
    .then(r => { if (!r.ok) window.alert(`Load failed: HTTP ${r.status}`); })
    .catch(e => window.alert(`Error: ${e}`));
}

function unloadModel(nodeId, modelName) {
  if (!window.confirm(`Unload "${modelName}" from ${nodeId}?`)) return;
  const token = localStorage.getItem('meshToken') ?? '';
  fetch(`/api/models/unload?token=${encodeURIComponent(token)}`, {
    method:  'POST',
    headers: { 'content-type': 'application/json' },
    body:    JSON.stringify({ node_id: nodeId, model_name: modelName }),
  })
    .then(r => { if (!r.ok) window.alert(`Unload failed: HTTP ${r.status}`); })
    .catch(e => window.alert(`Error: ${e}`));
}

// ── Drag-to-reorder ───────────────────────────────────────────────────────────

function enableDrag(el) {
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
    localStorage.setItem(ORDER_KEY, JSON.stringify(ids));
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
