/** Models panel: per-node model list with load/unload controls. */

import { getLatestSample } from '/static/health.js';
import { setPref } from '/static/prefs.js';

const ORDER_KEY   = 'meshModelOrder';
const nodesMap    = new Map(); // nodeId -> NodeModelInfo
const modelListEl = document.getElementById('model-list');

const KNOWN_MODELS = [
  { family: 'Qwen 3',       name: 'qwen3:4b',         size_mb: 2382  },
  { family: 'Qwen 3',       name: 'qwen3:8b',         size_mb: 4795  },
  { family: 'Qwen 3',       name: 'qwen3:14b',        size_mb: 8584  },
  { family: 'Qwen 3',       name: 'qwen3:32b',        size_mb: 18849 },
  { family: 'Qwen 2.5',     name: 'qwen2.5:0.5b',     size_mb: 500   },
  { family: 'Qwen 2.5',     name: 'qwen2.5:1.5b',     size_mb: 986   },
  { family: 'Qwen 2.5',     name: 'qwen2.5:7b',       size_mb: 4096  },
  { family: 'Qwen 2.5',     name: 'qwen2.5:14b',      size_mb: 8192  },
  { family: 'Qwen 2.5',     name: 'qwen2.5:32b',      size_mb: 19456 },
  { family: 'Llama 3.2',    name: 'llama3.2:1b',      size_mb: 770   },
  { family: 'Llama 3.2',    name: 'llama3.2:3b',      size_mb: 1926  },
  { family: 'Llama 3.1',    name: 'llama3.1:8b',      size_mb: 4692  },
  { family: 'Phi',          name: 'phi4:14b',          size_mb: 8635  },
  { family: 'Gemma 3',      name: 'gemma3:4b',        size_mb: 2374  },
  { family: 'Gemma 3',      name: 'gemma3:12b',       size_mb: 6964  },
  { family: 'Mistral',      name: 'mistral:7b',       size_mb: 4170  },
  { family: 'DeepSeek R1',  name: 'deepseek-r1:7b',   size_mb: 4466  },
  { family: 'DeepSeek R1',  name: 'deepseek-r1:8b',   size_mb: 4692  },
  { family: 'DeepSeek R1',  name: 'deepseek-r1:14b',  size_mb: 8572  },
  { family: 'DeepSeek R1',  name: 'deepseek-r1:32b',  size_mb: 18934 },
];
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

/** Returns the human-readable hostname for a node UUID, or the raw id if unknown. */
export function getHostname(nodeId) {
  return nodesMap.get(nodeId)?.hostname ?? nodeId;
}

/** Returns "nodeId/modelName" for the first Ready model, or null if none are ready. */
export function getReadyLlmModel() {
  for (const [nodeId, node] of nodesMap) {
    const ready = node.models.find(m => m.state.toLowerCase() === 'ready');
    if (ready) return `${nodeId}/${ready.name}`;
  }
  return null;
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
    const pickerRow = e.target.closest('[data-picker-node]');
    if (pickerRow) {
      const btn = e.target.closest('.model-picker-load');
      if (!btn) return;
      const select = pickerRow.querySelector('.model-picker-select');
      const opt    = select.selectedOptions[0];
      loadModel(pickerRow.dataset.pickerNode, opt.value, Number(opt.dataset.size), btn, pickerRow.querySelector('.model-load-error'));
    }
  });
}

// ── Render ────────────────────────────────────────────────────────────────────

function render() {
  if (!modelListEl || dragSrc) return;
  // Don't nuke the DOM while the user has a picker open.
  if (modelListEl.querySelector('.model-picker-select:focus')) return;
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
  ${loadFooter(node)}
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

function loadFooter(node) {
  const loaded  = new Set(node.models.map(m => m.name));
  const sample  = getLatestSample(node.node_id);

  const freeRamGb  = sample
    ? Math.max(0, (sample.ram_total_gb ?? 0) - (sample.ram_used_gb ?? 0))
    : (node.ram_gb ?? 0);
  const totalVramGb = sample?.gpu_vram_total_gb ?? 0;
  const freeVramGb  = sample
    ? Math.max(0, totalVramGb - (sample.gpu_vram_used_gb ?? 0))
    : 0;
  // GPU nodes: only show models that fit in free VRAM — loading into RAM on a
  // GPU node runs on CPU and defeats the purpose. CPU-only nodes: use free RAM.
  const memLimitMb = totalVramGb > 0
    ? freeVramGb * 1024
    : freeRamGb > 0 ? freeRamGb * 1024 : Infinity;

  // Also filter by available disk space — need 2× the model size for the
  // in-progress .tmp file plus the final .gguf.
  const diskFreeMb = sample?.disk_free_gb != null
    ? sample.disk_free_gb * 1024
    : Infinity;

  const available = KNOWN_MODELS.filter(m =>
    !loaded.has(m.name) &&
    m.size_mb <= memLimitMb &&
    m.size_mb * 2 <= diskFreeMb
  );
  if (available.length === 0) return '';

  const bestName = available.reduce((a, b) => b.size_mb > a.size_mb ? b : a).name;

  const byFamily = new Map();
  for (const m of available) {
    if (!byFamily.has(m.family)) byFamily.set(m.family, []);
    byFamily.get(m.family).push(m);
  }

  const options = [...byFamily.entries()].map(([family, models]) => {
    const opts = models.map(m => {
      const sizeLabel = m.size_mb >= 1024
        ? `${(m.size_mb / 1024).toFixed(1)} GB`
        : `${m.size_mb} MB`;
      const sel = m.name === bestName ? ' selected' : '';
      return `<option value="${esc(m.name)}" data-size="${m.size_mb}"${sel}>${esc(m.name)}  ·  ${sizeLabel}</option>`;
    }).join('');
    return `<optgroup label="${esc(family)}">${opts}</optgroup>`;
  }).join('');

  return `<div class="model-picker-row" data-picker-node="${esc(node.node_id)}">
  <select class="model-picker-select">${options}</select>
  <button class="model-picker-load">Load</button>
  <span class="model-load-error"></span>
</div>`;
}

async function loadModel(nodeId, modelName, sizeMb, btn, errEl) {
  btn.disabled = true;
  btn.textContent = 'Loading…';
  errEl.textContent = '';
  const token = localStorage.getItem('meshToken') ?? '';
  try {
    const r = await fetch(`/api/models/load?token=${encodeURIComponent(token)}`, {
      method:  'POST',
      headers: { 'content-type': 'application/json' },
      body:    JSON.stringify({ node_id: nodeId, model_name: modelName, size_mb: sizeMb }),
    });
    if (!r.ok) {
      errEl.textContent = `Failed (HTTP ${r.status})`;
      btn.disabled = false;
      btn.textContent = 'Load';
    }
    // Success: WS ModelUpdate will re-render the card
  } catch (e) {
    errEl.textContent = e.message;
    btn.disabled = false;
    btn.textContent = 'Load';
  }
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
    setPref(ORDER_KEY, JSON.stringify(ids));
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
