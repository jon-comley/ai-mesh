/** Health panel: CPU/RAM sparklines + heartbeat interval control. */

const ORDER_KEY    = 'meshHealthOrder';
const COLLAPSE_PFX = 'mesh-health-collapsed-';

const samplesMap   = new Map(); // nodeId -> HealthSample[]
const hostnamesMap = new Map(); // nodeId -> hostname
const intervalsMap = new Map(); // nodeId -> secs (last successfully set)

const healthChartsEl = document.getElementById('health-charts');
let dragSrc = null;

// ── Public API ────────────────────────────────────────────────────────────────

/** Return the most recent health sample for a node, or null if none. */
export function getLatestSample(nodeId) {
  const samp = samplesMap.get(nodeId);
  return samp ? samp.at(-1) : null;
}

/** Called on each TopologyUpdate to sync hostname labels then repaint. */
export function handleTopology(nodes) {
  for (const n of nodes) hostnamesMap.set(n.id, n.name);
  repaintAll();
}

/** Called on each HealthUpdate. */
export function handleHealth(evt) {
  samplesMap.set(evt.node_id, evt.samples);
  renderHealthPanel();
  paintMiniSparkline(evt.node_id);
}

/** Refill all mini sparklines — call after topology re-renders node cards. */
export function repaintAll() {
  renderHealthPanel();
  for (const id of samplesMap.keys()) paintMiniSparkline(id);
}

// ── Health panel ──────────────────────────────────────────────────────────────

if (healthChartsEl) {
  healthChartsEl.addEventListener('click', e => {
    const collapseBtn = e.target.closest('[data-collapse-node]');
    if (collapseBtn) {
      const id = collapseBtn.dataset.collapseNode;
      const nowCollapsed = localStorage.getItem(COLLAPSE_PFX + id) !== '1';
      localStorage.setItem(COLLAPSE_PFX + id, nowCollapsed ? '1' : '0');
      renderHealthPanel();
      return;
    }
    const intervalBtn = e.target.closest('[data-interval-node]');
    if (intervalBtn) promptHeartbeatInterval(intervalBtn.dataset.intervalNode);
  });
  enableDrag(healthChartsEl);
}

function renderHealthPanel() {
  if (!healthChartsEl || dragSrc) return;
  if (samplesMap.size === 0) {
    healthChartsEl.innerHTML = '<p class="placeholder">No health data yet.</p>';
    return;
  }
  const entries = applyOrder([...samplesMap.entries()], ([id]) => id);
  healthChartsEl.innerHTML = entries.map(([id, samp]) => healthCard(id, samp)).join('');
}

function healthCard(nodeId, samp) {
  const name      = hostnamesMap.get(nodeId) ?? nodeId;
  const collapsed = localStorage.getItem(COLLAPSE_PFX + nodeId) === '1';
  const last  = samp.at(-1);
  const cpu   = last ? last.cpu_pct.toFixed(1) : '—';
  const ramPct = last && last.ram_total_gb > 0
    ? ((last.ram_used_gb / last.ram_total_gb) * 100).toFixed(1) : '—';
  const ramStr = last
    ? `${last.ram_used_gb.toFixed(1)} / ${last.ram_total_gb.toFixed(1)} GB`
    : '—';
  const cpuData  = samp.map(s => s.cpu_pct);
  const ramData  = samp.map(s => s.ram_total_gb > 0
    ? (s.ram_used_gb / s.ram_total_gb) * 100 : 0);
  const ts       = samp.map(s => s.ts_ms);
  const hasGpu   = samp.some(s => s.gpu_pct != null);
  const gpuData  = hasGpu ? samp.map(s => s.gpu_pct ?? 0) : [];
  const vramData = hasGpu ? samp.map(s =>
    (s.gpu_vram_total_gb ?? 0) > 0
      ? ((s.gpu_vram_used_gb ?? 0) / s.gpu_vram_total_gb) * 100 : 0
  ) : [];
  const lastGpu  = hasGpu ? samp.findLast(s => s.gpu_pct != null) : null;
  const gpuPct   = lastGpu ? lastGpu.gpu_pct.toFixed(1) : '—';
  const vramPct  = lastGpu && lastGpu.gpu_vram_total_gb > 0
    ? ((lastGpu.gpu_vram_used_gb ?? 0) / lastGpu.gpu_vram_total_gb * 100).toFixed(1) : '—';
  const vramStr  = lastGpu && lastGpu.gpu_vram_total_gb > 0
    ? `${(lastGpu.gpu_vram_used_gb ?? 0).toFixed(1)} / ${lastGpu.gpu_vram_total_gb.toFixed(1)} GB`
    : '—';

  return `<div class="health-card${collapsed ? ' collapsed' : ''}" draggable="true" data-drag-id="${esc(nodeId)}">
  <div class="health-card-header">
    <button class="health-collapse-btn" data-collapse-node="${esc(nodeId)}">${collapsed ? '▸' : '▾'}</button>
    <span class="health-node-name">${esc(name)}</span>
    <button class="interval-btn" data-interval-node="${esc(nodeId)}">Set interval · ${intervalsMap.get(nodeId) ?? 30}s</button>
  </div>
  <div class="health-metric">
    <div class="metric-row">
      <span class="metric-label">CPU</span>
      <span class="metric-value ${metricClass(cpu, 'CPU')}">${cpu}%</span>
    </div>
    ${sparklineSvg(cpuData, 52, 'var(--accent)', ts, 'CPU')}
  </div>
  <div class="health-metric">
    <div class="metric-row">
      <span class="metric-label">RAM</span>
      <span class="metric-value ${metricClass(ramPct, 'RAM')}">${ramStr} · ${ramPct}%</span>
    </div>
    ${sparklineSvg(ramData, 52, 'var(--green)', ts, 'RAM')}
  </div>
  ${hasGpu ? `<div class="health-metric">
    <div class="metric-row">
      <span class="metric-label">GPU</span>
      <span class="metric-value ${metricClass(gpuPct, 'GPU')}">${gpuPct}%</span>
    </div>
    ${sparklineSvg(gpuData, 52, 'var(--amber)', ts, 'GPU')}
    <div class="metric-row" style="margin-top:4px">
      <span class="metric-label">VRAM</span>
      <span class="metric-value ${metricClass(vramPct, 'VRAM')}">${vramStr}</span>
    </div>
    ${sparklineSvg(vramData, 28, 'var(--amber)', ts, 'VRAM')}
  </div>` : ''}
</div>`;
}

// ── Mini sparklines (in node cards on Nodes tab) ──────────────────────────────

function paintMiniSparkline(nodeId) {
  const el = document.getElementById(`ms-${nodeId}`);
  if (!el) return;
  const samp = samplesMap.get(nodeId);
  if (!samp || samp.length < 2) return;
  el.innerHTML = sparklineSvg(samp.map(s => s.cpu_pct), 28, 'var(--accent)');
}

// ── SVG sparkline ─────────────────────────────────────────────────────────────

function sparklineSvg(data, h, color, timestamps, label) {
  if (!data || data.length < 2) return '';
  const W   = 240;
  const n   = data.length;
  const max = Math.max(...data, 1);
  const xOf = i => (i / (n - 1)) * W;
  const yOf = v => h - (v / max) * h;
  const pts = data.map((v, i) => `${xOf(i).toFixed(1)},${yOf(v).toFixed(1)}`).join(' ');

  // Fill polygon: line points + bottom-right + bottom-left closes the shape
  const fillPts = `${pts} ${W.toFixed(1)},${h} 0,${h}`;

  // Per-point tooltip rects — each covers the Voronoi region around that sample
  const hasTs = timestamps && timestamps.length === n;
  const tooltipRects = data.map((v, i) => {
    const left  = i === 0     ? 0 : (xOf(i - 1) + xOf(i)) / 2;
    const right = i === n - 1 ? W : (xOf(i) + xOf(i + 1)) / 2;
    const when  = hasTs ? `  at ${new Date(timestamps[i]).toLocaleTimeString()}` : `  (sample ${i + 1})`;
    const title = `${label ? label + ': ' : ''}${v.toFixed(1)}%${when}`;
    return `<rect x="${left.toFixed(1)}" y="0" width="${(right - left).toFixed(1)}" height="${h}" fill="transparent"><title>${title}</title></rect>`;
  }).join('');

  return `<svg width="100%" height="${h}" viewBox="0 0 ${W} ${h}" class="sparkline" aria-hidden="true" preserveAspectRatio="none">`
    + `<polygon points="${fillPts}" fill="${color}" fill-opacity="var(--sparkline-fill, 0.15)"/>`
    + `<polyline points="${pts}" fill="none" stroke="${color}" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round"/>`
    + tooltipRects
    + `</svg>`;
}

// ── Metric threshold colouring ────────────────────────────────────────────────

const METRIC_THRESHOLDS = {
  CPU:  { warn: 75, crit: 90 },
  RAM:  { warn: 75, crit: 90 },
  GPU:  { warn: 90, crit: 98 },
  VRAM: { warn: 95, crit: 99 },
};

function metricClass(pct, metric) {
  const v = parseFloat(pct);
  if (!isFinite(v)) return '';
  const { warn, crit } = METRIC_THRESHOLDS[metric] ?? { warn: 75, crit: 90 };
  if (v >= crit) return 'metric-crit';
  if (v >= warn) return 'metric-warn';
  return '';
}

// ── Interval prompt ───────────────────────────────────────────────────────────

function promptHeartbeatInterval(nodeId) {
  const current = intervalsMap.get(nodeId) ?? 30;
  const raw = window.prompt('New heartbeat interval (1–3600 seconds):', String(current));
  if (raw === null) return;
  const secs = parseInt(raw, 10);
  if (!Number.isFinite(secs) || secs < 1 || secs > 3600) {
    window.alert('Must be an integer between 1 and 3600.');
    return;
  }
  const token = localStorage.getItem('meshToken') ?? '';
  fetch(
    `/api/nodes/${encodeURIComponent(nodeId)}/heartbeat-interval?token=${encodeURIComponent(token)}`,
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ secs }),
    }
  )
    .then(r => {
      if (r.ok) {
        intervalsMap.set(nodeId, secs);
        renderHealthPanel();
      } else {
        window.alert(`Failed: HTTP ${r.status}`);
      }
    })
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
