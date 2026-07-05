/** Models panel: per-node model list with load/unload controls. */

import { getLatestSample } from '/static/health.js';
import { setPrefDebounced } from '/static/prefs.js';

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
// Nodes mid-unload: the loader stays disabled until the model is gone AND its VRAM
// has been released. There is no backend "Unloading" state (Ready → Unloaded, then
// filtered out), so this transition is tracked client-side.
// nodeId -> { model, startedAt, vramUsedAtStart }
const unloadingNodes = new Map();

// ── Public API ────────────────────────────────────────────────────────────────

export function handleModelUpdate(evt) {
  nodesMap.clear();
  // Skip the 'chaos' security-test harness — not a real node, not model-pickable.
  for (const node of evt.nodes) {
    if (node.hostname === 'chaos') continue;
    nodesMap.set(node.node_id, node);
  }
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
      return;
    }
    const customRow = e.target.closest('[data-custom-node]');
    if (customRow) {
      const btn = e.target.closest('.model-custom-load');
      if (!btn) return;
      const errEl = customRow.querySelector('.model-load-error');
      const name = customRow.querySelector('.model-custom-input').value.trim();
      const sizeMb = Number(customRow.querySelector('.model-custom-size').value);
      if (!name.startsWith('hf:')) {
        errEl.textContent = 'Must start with hf:org/repo:filename.gguf';
        return;
      }
      if (!sizeMb || sizeMb <= 0) {
        errEl.textContent = 'Enter the model size in MB';
        return;
      }
      loadModel(customRow.dataset.customNode, name, sizeMb, btn, errEl);
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
  for (const n of nodes) clearUnloadIfDone(n);
  modelListEl.innerHTML = nodes.map(nodeCard).join('');
}

/** Reason a node's loader is disabled, or null if it's free to load. */
function nodeBusy(node) {
  if (unloadingNodes.has(node.node_id)) return 'Unloading — freeing VRAM…';
  if (node.models.some(m => m.state.toLowerCase() === 'loading')) return 'Loading…';
  return null;
}

/** Clear a node's unloading lock once the model is gone and its VRAM is released. */
function clearUnloadIfDone(node) {
  const u = unloadingNodes.get(node.node_id);
  if (!u) return;
  if (node.models.some(m => m.name === u.model)) return; // still tearing down
  const vramNow = getLatestSample(node.node_id)?.gpu_vram_used_gb ?? null;
  // VRAM considered cleared when: the node reports no VRAM (CPU-only), usage dropped
  // ≥0.3 GB from the unload start, or a 30 s safety fallback elapsed (telemetry lag).
  const vramCleared =
    u.vramUsedAtStart == null ||
    vramNow == null ||
    vramNow <= u.vramUsedAtStart - 0.3 ||
    Date.now() - u.startedAt > 30000;
  if (vramCleared) unloadingNodes.delete(node.node_id);
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

  const busy = nodeBusy(node);

  const available = KNOWN_MODELS.filter(m =>
    !loaded.has(m.name) &&
    m.size_mb <= memLimitMb &&
    m.size_mb * 2 <= diskFreeMb
  );

  let options = '';
  if (available.length > 0) {
    const bestName = available.reduce((a, b) => b.size_mb > a.size_mb ? b : a).name;

    const byFamily = new Map();
    for (const m of available) {
      if (!byFamily.has(m.family)) byFamily.set(m.family, []);
      byFamily.get(m.family).push(m);
    }

    options = [...byFamily.entries()].map(([family, models]) => {
      const opts = models.map(m => {
        const sizeLabel = m.size_mb >= 1024
          ? `${(m.size_mb / 1024).toFixed(1)} GB`
          : `${m.size_mb} MB`;
        const sel = m.name === bestName ? ' selected' : '';
        return `<option value="${esc(m.name)}" data-size="${m.size_mb}"${sel}>${esc(m.name)}  ·  ${sizeLabel}</option>`;
      }).join('');
      return `<optgroup label="${esc(family)}">${opts}</optgroup>`;
    }).join('');
  }

  const disabled = busy ? ' disabled' : '';
  const note = busy
    ? `<span class="model-picker-busy">${esc(busy)}</span>`
    : '<span class="model-load-error"></span>';

  // Curated picker only makes sense when there's something in it (or the node
  // is busy, so the note still needs somewhere to show).
  const pickerRow = (available.length > 0 || busy)
    ? `<div class="model-picker-row" data-picker-node="${esc(node.node_id)}">
  <select class="model-picker-select"${disabled}>${options}</select>
  <button class="model-picker-load"${disabled}>Load</button>
  ${note}
</div>`
    : '';

  // Any single-file GGUF on Hugging Face, not just the curated list above —
  // there are thousands of models on the leaderboards this crate doesn't have
  // a name for. hf:<org>/<repo>:<filename.gguf> loads it directly (see
  // capabilities/llm's resolve_gguf); size is needed up front for the
  // disk-space check, same as the curated picker already requires. Always
  // offered, even when the curated picker above has nothing to show.
  const customRow = `<div class="model-custom-row" data-custom-node="${esc(node.node_id)}">
  <input class="model-custom-input" type="text" autocomplete="off"
         placeholder="hf:org/repo:filename.gguf"${disabled}>
  <input class="model-custom-size" type="number" min="1" step="1"
         placeholder="size MB"${disabled}>
  <button class="model-custom-load"${disabled}>Load custom</button>
  <span class="model-load-error"></span>
</div>`;

  return pickerRow + customRow;
}

async function loadModel(nodeId, modelName, sizeMb, btn, errEl) {
  const originalLabel = btn.textContent;
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
      btn.textContent = originalLabel;
    }
    // Success: WS ModelUpdate will re-render the card
  } catch (e) {
    errEl.textContent = e.message;
    btn.disabled = false;
    btn.textContent = originalLabel;
  }
}

function unloadModel(nodeId, modelName) {
  if (!window.confirm(`Unload "${modelName}" from ${nodeId}?`)) return;
  // Lock this node's loader until the model is gone and its VRAM is freed.
  const sample = getLatestSample(nodeId);
  unloadingNodes.set(nodeId, {
    model: modelName,
    startedAt: Date.now(),
    vramUsedAtStart: sample?.gpu_vram_used_gb ?? null,
  });
  render();
  const token = localStorage.getItem('meshToken') ?? '';
  const release = () => { unloadingNodes.delete(nodeId); render(); };
  fetch(`/api/models/unload?token=${encodeURIComponent(token)}`, {
    method:  'POST',
    headers: { 'content-type': 'application/json' },
    body:    JSON.stringify({ node_id: nodeId, model_name: modelName }),
  })
    .then(r => { if (!r.ok) { release(); window.alert(`Unload failed: HTTP ${r.status}`); } })
    .catch(e => { release(); window.alert(`Error: ${e}`); });
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
    setPrefDebounced(ORDER_KEY, JSON.stringify(ids));
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
