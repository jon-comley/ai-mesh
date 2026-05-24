/** Health panel: CPU/RAM sparklines + heartbeat interval control. */

const samplesMap   = new Map(); // nodeId -> HealthSample[]
const hostnamesMap = new Map(); // nodeId -> hostname

const healthChartsEl = document.getElementById('health-charts');

// ── Public API ────────────────────────────────────────────────────────────────

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
    const btn = e.target.closest('[data-interval-node]');
    if (btn) promptHeartbeatInterval(btn.dataset.intervalNode);
  });
}

function renderHealthPanel() {
  if (!healthChartsEl) return;
  if (samplesMap.size === 0) {
    healthChartsEl.innerHTML = '<p class="placeholder">No health data yet.</p>';
    return;
  }
  healthChartsEl.innerHTML = [...samplesMap.entries()]
    .map(([id, samp]) => healthCard(id, samp))
    .join('');
}

function healthCard(nodeId, samp) {
  const name  = hostnamesMap.get(nodeId) ?? nodeId;
  const last  = samp.at(-1);
  const cpu   = last ? last.cpu_pct.toFixed(1) : '—';
  const ramPct = last && last.ram_total_gb > 0
    ? ((last.ram_used_gb / last.ram_total_gb) * 100).toFixed(1) : '—';
  const ramStr = last
    ? `${last.ram_used_gb.toFixed(1)} / ${last.ram_total_gb.toFixed(1)} GB`
    : '—';
  const cpuData = samp.map(s => s.cpu_pct);
  const ramData = samp.map(s => s.ram_total_gb > 0
    ? (s.ram_used_gb / s.ram_total_gb) * 100 : 0);
  const hasGpu  = samp.some(s => s.gpu_pct != null);
  const gpuData  = hasGpu ? samp.map(s => s.gpu_pct ?? 0) : [];
  const vramData = hasGpu ? samp.map(s =>
    (s.gpu_vram_total_gb ?? 0) > 0
      ? ((s.gpu_vram_used_gb ?? 0) / s.gpu_vram_total_gb) * 100 : 0
  ) : [];
  const lastGpu = hasGpu ? samp.findLast(s => s.gpu_pct != null) : null;
  const gpuPct  = lastGpu ? lastGpu.gpu_pct.toFixed(1) : '—';
  const vramStr = lastGpu && lastGpu.gpu_vram_total_gb > 0
    ? `${(lastGpu.gpu_vram_used_gb ?? 0).toFixed(1)} / ${lastGpu.gpu_vram_total_gb.toFixed(1)} GB`
    : '—';

  return `<div class="health-card">
  <div class="health-card-header">
    <span class="health-node-name">${esc(name)}</span>
    <button class="interval-btn" data-interval-node="${esc(nodeId)}">Set interval</button>
  </div>
  <div class="health-metric">
    <div class="metric-row">
      <span class="metric-label">CPU</span>
      <span class="metric-value">${cpu}%</span>
    </div>
    ${sparklineSvg(cpuData, 52, 'var(--accent)')}
  </div>
  <div class="health-metric">
    <div class="metric-row">
      <span class="metric-label">RAM</span>
      <span class="metric-value">${ramStr} · ${ramPct}%</span>
    </div>
    ${sparklineSvg(ramData, 52, 'var(--green)')}
  </div>
  ${hasGpu ? `<div class="health-metric">
    <div class="metric-row">
      <span class="metric-label">GPU</span>
      <span class="metric-value">${gpuPct}%</span>
    </div>
    ${sparklineSvg(gpuData, 52, 'var(--amber)')}
    <div class="metric-row" style="margin-top:4px">
      <span class="metric-label">VRAM</span>
      <span class="metric-value">${vramStr}</span>
    </div>
    ${sparklineSvg(vramData, 28, 'var(--amber)')}
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

function sparklineSvg(data, h, color) {
  if (!data || data.length < 2) return '';
  const W   = 240;
  const max = Math.max(...data, 1);
  const pts = data
    .map((v, i) => `${((i / (data.length - 1)) * W).toFixed(1)},${(h - (v / max) * h).toFixed(1)}`)
    .join(' ');
  return `<svg width="100%" height="${h}" viewBox="0 0 ${W} ${h}" class="sparkline" aria-hidden="true" preserveAspectRatio="none"><polyline points="${pts}" fill="none" stroke="${color}" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round"/></svg>`;
}

// ── Interval prompt ───────────────────────────────────────────────────────────

function promptHeartbeatInterval(nodeId) {
  const raw = window.prompt('New heartbeat interval (1–3600 seconds):', '30');
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
    .then(r => { if (!r.ok) window.alert(`Failed: HTTP ${r.status}`); })
    .catch(e => window.alert(`Error: ${e}`));
}

// ── Utilities ─────────────────────────────────────────────────────────────────

function esc(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}
