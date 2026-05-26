// ── Room Layout Canvas ────────────────────────────────────────────────────────
// SVG top-down floor plan for placing bulbs and (Phase B) windows/doors.
// Coordinates are always 0–1 normalised; the SVG scales to any screen size.

// ── State ─────────────────────────────────────────────────────────────────────

let layoutRoom = null;          // RoomRecord currently in view
let devicesRef = new Map();     // reference to rooms.js devicesMap — set via init()
let placedBulbs = {};           // device_id → { x, y, z, fixture_type, el, labelEl }
let placedOpenings = {};        // opening_id → { opening_type, wall_edge, x_norm, width_norm, transmission, el }
let lastSolar = { azimuth: 180, elevation: -90 };
let undoStack = [];             // position snapshots for Ctrl+Z
let redoStack = [];
let snapDivisions = 20;         // invisible grid: 1/N of canvas width
let showLabels = true;
let activePopover = null;       // currently open popover element
let dragType = null;            // 'bulb' | 'opening' — set on dragstart since getData is unavailable in dragover
// Global safety net: clear dragType if a drag is cancelled or ends outside the canvas
window.addEventListener('dragend', () => { dragType = null; });
window.addEventListener('drop', () => { dragType = null; });

const FIXTURE_TYPES = [
  { id: 'ceiling_spot', label: 'Ceiling spot', defaultZ: 1.0 },
  { id: 'pendant',      label: 'Pendant',      defaultZ: 0.6 },
  { id: 'table_lamp',   label: 'Table lamp',   defaultZ: 0.3 },
  { id: 'floor_lamp',   label: 'Floor lamp',   defaultZ: 0.1 },
  { id: 'led_strip',    label: 'LED strip',    defaultZ: 0.5 },
];

// ── Public API ────────────────────────────────────────────────────────────────

export function init(devicesMap) {
  devicesRef = devicesMap;
}

export function openLayout(room) {
  layoutRoom = room;
  placedBulbs = {};
  placedOpenings = {};
  undoStack = [];
  redoStack = [];

  const container = document.getElementById('lighting-list');
  for (const child of container.children) child.style.display = 'none';

  // Make the panel fill-height so the canvas has room
  document.getElementById('panel-lighting')?.classList.add('layout-open');

  const view = buildLayoutView(room);
  container.appendChild(view);

  loadPlacedBulbs(room.id);
  loadPlacedOpenings(room.id);

  document.addEventListener('keydown', onKeyDown);
}

export function closeLayout() {
  document.removeEventListener('keydown', onKeyDown);
  dismissPopover();

  document.getElementById('panel-lighting')?.classList.remove('layout-open');

  const container = document.getElementById('lighting-list');
  const view = container.querySelector('.layout-view');
  if (view) view.remove();

  for (const child of container.children) child.style.display = '';

  layoutRoom = null;
  placedBulbs = {};
  placedOpenings = {};
}

// Called by rooms.js when a LightingUpdate WS event arrives so canvas icons stay live.
export function notifyDeviceUpdate(deviceId, state) {
  const entry = placedBulbs[deviceId];
  if (!entry) return;
  updateBulbIcon(entry, state);
}

// Called by rooms.js when a SolarUpdate WS event arrives — redraws all light cones.
export function notifySolarUpdate(azimuth, elevation) {
  lastSolar = { azimuth, elevation };
  const shadowLayer = document.getElementById('lc-shadow');
  if (!shadowLayer) return;
  shadowLayer.innerHTML = '';
  for (const [id, o] of Object.entries(placedOpenings)) {
    const cone = buildCone(o, azimuth, elevation);
    if (cone) { cone.id = `cone-${id}`; shadowLayer.appendChild(cone); }
  }
}

// ── View construction ─────────────────────────────────────────────────────────

function buildLayoutView(room) {
  const view = document.createElement('div');
  view.className = 'layout-view';

  // Header
  const header = document.createElement('div');
  header.className = 'layout-header';

  const backBtn = document.createElement('button');
  backBtn.className = 'layout-back-btn';
  backBtn.textContent = '← Rooms';
  backBtn.addEventListener('click', closeLayout);
  header.appendChild(backBtn);

  const title = document.createElement('span');
  title.className = 'layout-title';
  title.textContent = room.name;
  header.appendChild(title);

  const controls = document.createElement('div');
  controls.className = 'layout-header-controls';

  const labelToggle = document.createElement('label');
  labelToggle.className = 'layout-toggle';
  const labelCb = document.createElement('input');
  labelCb.type = 'checkbox';
  labelCb.checked = showLabels;
  labelCb.addEventListener('change', () => {
    showLabels = labelCb.checked;
    Object.values(placedBulbs).forEach(e => {
      if (!e.el) return;
      e.el.querySelectorAll('text, rect[fill="rgba(0,0,0,0.55)"]').forEach(el => {
        el.style.display = showLabels ? '' : 'none';
      });
    });
  });
  labelToggle.appendChild(labelCb);
  labelToggle.appendChild(document.createTextNode(' Labels'));
  controls.appendChild(labelToggle);

  const autoBtn = document.createElement('button');
  autoBtn.className = 'layout-auto-btn';
  autoBtn.textContent = 'Auto-arrange remaining';
  autoBtn.style.display = 'none';
  autoBtn.id = 'layout-auto-btn';
  autoBtn.addEventListener('click', autoArrange);
  controls.appendChild(autoBtn);

  const undoBtn = document.createElement('button');
  undoBtn.className = 'layout-undo-btn';
  undoBtn.textContent = '↩ Undo';
  undoBtn.addEventListener('click', undo);
  controls.appendChild(undoBtn);

  const redoBtn = document.createElement('button');
  redoBtn.className = 'layout-redo-btn';
  redoBtn.textContent = '↪ Redo';
  redoBtn.addEventListener('click', redo);
  controls.appendChild(redoBtn);

  header.appendChild(controls);
  view.appendChild(header);

  // Body: sidebar + canvas
  const body = document.createElement('div');
  body.className = 'layout-body';

  body.appendChild(buildSidebar(room));
  body.appendChild(buildCanvas());

  view.appendChild(body);
  return view;
}

function buildSidebar(room) {
  const sidebar = document.createElement('div');
  sidebar.className = 'layout-sidebar';

  const label = document.createElement('div');
  label.className = 'layout-sidebar-label';
  label.textContent = 'Bulbs:';
  sidebar.appendChild(label);

  const chips = document.createElement('div');
  chips.className = 'layout-sidebar-chips';
  chips.id = 'layout-sidebar-chips';
  sidebar.appendChild(chips);

  // Openings section — always shown
  const openingsLabel = document.createElement('div');
  openingsLabel.className = 'layout-sidebar-label';
  openingsLabel.style.marginTop = '12px';
  openingsLabel.textContent = 'Openings (drop to wall):';
  sidebar.appendChild(openingsLabel);

  const openingChips = document.createElement('div');
  openingChips.className = 'layout-sidebar-chips';
  for (const { type, label: lbl } of [
    { type: 'window', label: '⬜ Window' },
    { type: 'door',   label: '▯ Door'   },
  ]) {
    const chip = document.createElement('div');
    chip.className = 'layout-chip layout-opening-chip';
    chip.draggable = true;
    chip.textContent = lbl;
    chip.addEventListener('dragstart', e => {
      dragType = 'opening';
      e.dataTransfer.effectAllowed = 'copy';
      e.dataTransfer.setData('text/plain', `opening:${type}`);
    });
    chip.addEventListener('dragend', () => { dragType = null; });
    openingChips.appendChild(chip);
  }
  sidebar.appendChild(openingChips);

  sidebar._room = room;
  return sidebar;
}

function rebuildSidebar() {
  const chips = document.getElementById('layout-sidebar-chips');
  if (!chips) return;
  chips.innerHTML = '';

  const room = layoutRoom;
  const unplaced = (room.device_ids || []).filter(id => !placedBulbs[id]);

  if (unplaced.length === 0) {
    const hint = document.createElement('span');
    hint.className = 'layout-sidebar-empty';
    hint.textContent = 'All bulbs placed';
    chips.appendChild(hint);
  } else {
    for (const id of unplaced) {
      chips.appendChild(makeSidebarChip(id));
    }
  }

  // Show/hide auto-arrange button
  const autoBtn = document.getElementById('layout-auto-btn');
  if (autoBtn) {
    autoBtn.style.display =
      Object.keys(placedBulbs).length >= 2 && unplaced.length > 0 ? '' : 'none';
  }
}

function makeSidebarChip(deviceId) {
  const dev = devicesRef.get(deviceId);
  const chip = document.createElement('div');
  chip.className = 'layout-chip';
  chip.draggable = true;
  chip.dataset.deviceId = deviceId;
  chip.textContent = dev ? dev.friendly_name ?? deviceId : deviceId;

  if (dev) {
    chip.style.setProperty('--chip-color', devStateColor(dev));
  }

  chip.addEventListener('dragstart', e => {
    dragType = 'bulb';
    e.dataTransfer.effectAllowed = 'copy';
    e.dataTransfer.setData('text/plain', `bulb:${deviceId}`);
    // Trigger pulse-on-grab via rooms.js exported function
    if (typeof window.__roomsStartPulse === 'function') {
      window.__roomsStartPulse(deviceId);
    }
  });
  chip.addEventListener('dragend', () => {
    dragType = null;
    if (typeof window.__roomsStopPulse === 'function') {
      window.__roomsStopPulse(true);
    }
  });
  return chip;
}

function buildCanvas() {
  const wrap = document.createElement('div');
  wrap.className = 'layout-canvas-wrap';

  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.id = 'layout-canvas';
  svg.setAttribute('viewBox', '0 0 1000 1000');
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');

  // Background floor
  const floor = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
  floor.setAttribute('x', '0'); floor.setAttribute('y', '0');
  floor.setAttribute('width', '1000'); floor.setAttribute('height', '1000');
  floor.setAttribute('fill', 'var(--layout-floor, #1a1a2e)');
  floor.setAttribute('rx', '8');
  svg.appendChild(floor);

  // Layer groups (matching the plan)
  for (const id of ['lc-openings', 'lc-shadow', 'lc-bulbs', 'lc-preview', 'lc-sun-arc']) {
    const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
    g.id = id;
    svg.appendChild(g);
  }

  svg.addEventListener('dragover', onCanvasDragOver);
  svg.addEventListener('dragleave', onCanvasDragLeave);
  svg.addEventListener('drop', onCanvasDrop);
  svg.addEventListener('click', onCanvasClick);

  wrap.appendChild(svg);
  return wrap;
}

// ── Drag / drop ───────────────────────────────────────────────────────────────

function svgPoint(svg, clientX, clientY) {
  const pt = svg.createSVGPoint();
  pt.x = clientX;
  pt.y = clientY;
  const svgP = pt.matrixTransform(svg.getScreenCTM().inverse());
  return {
    nx: svgP.x / 1000,
    ny: svgP.y / 1000,
  };
}

function snap(v) {
  return Math.round(v * snapDivisions) / snapDivisions;
}

function onCanvasDragOver(e) {
  e.preventDefault();
  if (!e.dataTransfer.types.includes('text/plain')) return;

  const svg = document.getElementById('layout-canvas');
  const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
  const preview = document.getElementById('lc-preview');
  if (!preview) return;
  preview.innerHTML = '';

  if (dragType === 'opening') {
    const wall = detectWall(nx, ny);
    if (wall) {
      // Snap x_norm to midpoint (0.5) when close
      const rawPos = isHorizontalWall(wall) ? nx : ny;
      const xNorm = Math.abs(rawPos - 0.5) < 0.06 ? 0.5 : snap(rawPos);
      const r = openingToSvgRect({ wall_edge: wall, x_norm: xNorm, width_norm: 0.3 });
      const ghost = svgEl('rect', {
        x: r.x, y: r.y, width: r.w, height: r.h, rx: 3,
        fill: 'rgba(100,200,255,0.45)', stroke: 'rgba(100,200,255,0.9)',
        'stroke-width': 2, 'pointer-events': 'none',
      });
      preview.appendChild(ghost);
    } else {
      const reject = svgEl('text', {
        x: snap(nx) * 1000, y: snap(ny) * 1000,
        'text-anchor': 'middle', 'dominant-baseline': 'central',
        fill: 'rgba(255,80,80,0.85)', 'font-size': 48, 'pointer-events': 'none',
      });
      reject.textContent = '✕';
      preview.appendChild(reject);
    }
  } else {
    const sx = snap(nx), sy = snap(ny);
    const glow = svgEl('circle', {
      cx: sx * 1000, cy: sy * 1000, r: 28,
      fill: 'rgba(255,255,200,0.25)', stroke: 'rgba(255,255,200,0.7)', 'stroke-width': 2,
      'pointer-events': 'none',
    });
    preview.appendChild(glow);
  }
}

function onCanvasDragLeave() {
  const preview = document.getElementById('lc-preview');
  if (preview) preview.innerHTML = '';
}

function onCanvasDrop(e) {
  e.preventDefault();
  dragType = null;
  const preview = document.getElementById('lc-preview');
  if (preview) preview.innerHTML = '';

  const raw = e.dataTransfer.getData('text/plain');
  const svg = document.getElementById('layout-canvas');
  const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);

  if (raw.startsWith('opening:')) {
    const openingType = raw.slice(8);
    const wall = detectWall(nx, ny);
    if (!wall || !layoutRoom) return;
    const rawPos = isHorizontalWall(wall) ? nx : ny;
    const xNorm = Math.abs(rawPos - 0.5) < 0.06 ? 0.5 : snap(rawPos);
    const transmission = openingType === 'window' ? 1.0 : 0.1;
    postCreateOpening(layoutRoom.id, openingType, wall, xNorm, 0.3, transmission);
    return;
  }

  if (!raw.startsWith('bulb:')) return;
  const deviceId = raw.slice(5);

  const x = snap(nx);
  const y = snap(ny);

  const existing = placedBulbs[deviceId];
  const fixtureType = existing?.fixture_type ?? 'ceiling_spot';
  const z = existing?.z ?? defaultZ(fixtureType);

  pushUndo();
  placeBulb(deviceId, x, y, z, fixtureType, true);
}

function onCanvasClick(e) {
  // Close popover if clicking outside it
  if (activePopover && !activePopover.contains(e.target)) {
    dismissPopover();
  }
}

// ── Bulb placement ────────────────────────────────────────────────────────────

function placeBulb(deviceId, x, y, z, fixtureType, postToServer) {
  const svg = document.getElementById('layout-canvas');
  const layer = document.getElementById('lc-bulbs');
  if (!svg || !layer) return;

  // Remove existing element if re-placing
  if (placedBulbs[deviceId]?.el) {
    placedBulbs[deviceId].el.remove();
    placedBulbs[deviceId].labelEl?.remove();
  }

  const cx = x * 1000;
  const cy = y * 1000;

  const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  g.dataset.deviceId = deviceId;
  g.style.cursor = 'grab';
  g.addEventListener('click', e => e.stopPropagation());
  makeBulbDraggable(g, deviceId);

  drawFixtureIcon(g, cx, cy, z, fixtureType, devicesRef.get(deviceId));

  // Label: background pill + text, clear of the icon
  const dev = devicesRef.get(deviceId);
  const name = dev?.friendly_name ?? deviceId;
  const labelText = name.length > 14 ? name.slice(0, 13) + '…' : name;
  const labelY = cy + 58;

  const labelBg = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
  labelBg.setAttribute('x', cx - 52); labelBg.setAttribute('y', labelY - 18);
  labelBg.setAttribute('width', '104'); labelBg.setAttribute('height', '22');
  labelBg.setAttribute('rx', '6');
  labelBg.setAttribute('fill', 'rgba(0,0,0,0.55)');
  labelBg.setAttribute('pointer-events', 'none');
  labelBg.style.display = showLabels ? '' : 'none';
  g.appendChild(labelBg);

  const labelEl = document.createElementNS('http://www.w3.org/2000/svg', 'text');
  labelEl.setAttribute('x', cx);
  labelEl.setAttribute('y', labelY - 2);
  labelEl.setAttribute('text-anchor', 'middle');
  labelEl.setAttribute('font-size', '18');
  labelEl.setAttribute('fill', 'rgba(255,255,255,0.85)');
  labelEl.setAttribute('pointer-events', 'none');
  labelEl.textContent = labelText;
  labelEl.style.display = showLabels ? '' : 'none';
  g.appendChild(labelEl);

  layer.appendChild(g);

  placedBulbs[deviceId] = { x, y, z, fixture_type: fixtureType, el: g, labelEl };

  if (postToServer) {
    postPosition(deviceId, x, y, z, fixtureType);
  }

  rebuildSidebar();
}

function svgEl(tag, attrs) {
  const el = document.createElementNS('http://www.w3.org/2000/svg', tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  return el;
}

function drawFixtureIcon(g, cx, cy, z, fixtureType, dev) {
  const color = dev ? devStateColor(dev) : '#666';
  const on = dev?.on ?? false;
  const alpha = on ? 1 : 0.35;

  // Clear previous icon children (keep text/rect labels)
  [...g.children].forEach(c => {
    if (c.tagName !== 'text' && c.getAttribute('fill') !== 'rgba(0,0,0,0.55)') c.remove();
  });

  const els = [];

  if (fixtureType === 'led_strip') {
    // Wide pill — represents a strip mounted on a wall or ceiling
    els.push(svgEl('rect', { x: cx - 44, y: cy - 8, width: 88, height: 16, rx: 8,
      fill: color, opacity: on ? 0.2 : 0.1 }));
    const strip = svgEl('rect', { x: cx - 40, y: cy - 5, width: 80, height: 10, rx: 5,
      fill: color, opacity: alpha });
    strip.classList.add('lc-bulb-shape');
    els.push(strip);

  } else if (fixtureType === 'pendant') {
    // Hanging cord from ceiling, then bulb circle
    const cordLen = 20 + (1 - z) * 40;
    els.push(svgEl('line', { x1: cx, y1: cy - 16, x2: cx, y2: cy - 16 - cordLen,
      stroke: 'rgba(255,255,255,0.35)', 'stroke-width': 2 }));
    // Glow halo
    els.push(svgEl('circle', { cx, cy, r: 22, fill: color, opacity: on ? 0.15 : 0.05 }));
    const bulb = svgEl('circle', { cx, cy, r: 14, fill: color, opacity: alpha });
    bulb.classList.add('lc-bulb-shape');
    els.push(bulb);
    // Ring outline
    els.push(svgEl('circle', { cx, cy, r: 14, fill: 'none',
      stroke: color, 'stroke-width': 2, opacity: Math.min(alpha + 0.3, 1) }));

  } else if (fixtureType === 'table_lamp') {
    // Shade (downward triangle) + short stem + base
    const shade = svgEl('polygon', {
      points: `${cx},${cy - 8} ${cx - 16},${cy + 10} ${cx + 16},${cy + 10}`,
      fill: color, opacity: alpha });
    shade.classList.add('lc-bulb-shape');
    els.push(shade);
    els.push(svgEl('line', { x1: cx, y1: cy + 10, x2: cx, y2: cy + 22,
      stroke: 'rgba(255,255,255,0.4)', 'stroke-width': 3 }));
    els.push(svgEl('rect', { x: cx - 10, y: cy + 22, width: 20, height: 4, rx: 2,
      fill: 'rgba(255,255,255,0.35)' }));

  } else if (fixtureType === 'floor_lamp') {
    // Tall arc-head lamp
    els.push(svgEl('line', { x1: cx, y1: cy + 28, x2: cx, y2: cy - 4,
      stroke: 'rgba(255,255,255,0.4)', 'stroke-width': 3 }));
    // Shade arc (semi-circle open downwards)
    els.push(svgEl('path', {
      d: `M ${cx - 16} ${cy - 4} A 16 16 0 0 1 ${cx + 16} ${cy - 4}`,
      fill: color, opacity: alpha }));
    const head = svgEl('circle', { cx, cy: cy - 4, r: 7, fill: color, opacity: alpha });
    head.classList.add('lc-bulb-shape');
    els.push(head);
    els.push(svgEl('rect', { x: cx - 8, y: cy + 28, width: 16, height: 4, rx: 2,
      fill: 'rgba(255,255,255,0.35)' }));

  } else {
    // ceiling_spot (default) — downlight: halo ring + filled dot
    els.push(svgEl('circle', { cx, cy, r: 26, fill: color, opacity: on ? 0.12 : 0.04 }));
    els.push(svgEl('circle', { cx, cy, r: 18, fill: 'none',
      stroke: color, 'stroke-width': 2, opacity: on ? 0.5 : 0.2 }));
    const dot = svgEl('circle', { cx, cy, r: 10, fill: color, opacity: alpha });
    dot.classList.add('lc-bulb-shape');
    els.push(dot);
    // Cross-hatch tick marks like a recessed light symbol
    els.push(svgEl('line', { x1: cx - 18, y1: cy, x2: cx - 10, y2: cy,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 }));
    els.push(svgEl('line', { x1: cx + 10, y1: cy, x2: cx + 18, y2: cy,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 }));
    els.push(svgEl('line', { x1: cx, y1: cy - 18, x2: cx, y2: cy - 10,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 }));
    els.push(svgEl('line', { x1: cx, y1: cy + 10, x2: cx, y2: cy + 18,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 }));
  }

  // Insert before any text/label children
  const firstLabel = [...g.children].find(c => c.tagName === 'text' || c.getAttribute('fill') === 'rgba(0,0,0,0.55)');
  for (const el of els) {
    if (firstLabel) g.insertBefore(el, firstLabel);
    else g.appendChild(el);
  }
}

function makeBulbDraggable(g, deviceId) {
  let dragging = false;
  let moved = false;
  let startNx, startNy, startBulbX, startBulbY;

  g.addEventListener('pointerdown', e => {
    if (e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    dismissPopover();

    const svg = document.getElementById('layout-canvas');
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const entry = placedBulbs[deviceId];
    if (!entry) return;

    dragging = true;
    moved = false;
    startNx = nx; startNy = ny;
    startBulbX = entry.x; startBulbY = entry.y;

    g.setPointerCapture(e.pointerId);
    g.style.cursor = 'grabbing';

    if (typeof window.__roomsStartPulse === 'function') window.__roomsStartPulse(deviceId);
  });

  g.addEventListener('pointermove', e => {
    if (!dragging) return;
    e.stopPropagation();

    const svg = document.getElementById('layout-canvas');
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const dx = nx - startNx;
    const dy = ny - startNy;

    if (Math.abs(dx) > 0.005 || Math.abs(dy) > 0.005) moved = true;
    if (!moved) return;

    const newX = Math.max(0, Math.min(1, snap(startBulbX + dx)));
    const newY = Math.max(0, Math.min(1, snap(startBulbY + dy)));
    // Translate the group visually without re-creating it
    const entry = placedBulbs[deviceId];
    const tx = (newX - entry.x) * 1000;
    const ty = (newY - entry.y) * 1000;
    g.setAttribute('transform', `translate(${tx},${ty})`);
  });

  g.addEventListener('pointerup', e => {
    if (!dragging) return;
    dragging = false;
    g.style.cursor = 'grab';
    g.releasePointerCapture(e.pointerId);

    if (!moved) {
      // Tap — open popover; keep pulse running until popover is dismissed
      openPopover(deviceId, g);
      return;
    }

    if (typeof window.__roomsStopPulse === 'function') window.__roomsStopPulse(true);

    g.removeAttribute('transform');

    const svg = document.getElementById('layout-canvas');
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const newX = Math.max(0, Math.min(1, snap(startBulbX + (nx - startNx))));
    const newY = Math.max(0, Math.min(1, snap(startBulbY + (ny - startNy))));

    const entry = placedBulbs[deviceId];
    if (newX !== entry.x || newY !== entry.y) {
      pushUndo();
      placeBulb(deviceId, newX, newY, entry.z, entry.fixture_type, true);
    }
  });

  g.addEventListener('pointercancel', () => {
    if (!dragging) return;
    dragging = false;
    g.removeAttribute('transform');
    g.style.cursor = 'grab';
    if (typeof window.__roomsStopPulse === 'function') window.__roomsStopPulse(false);
  });
}

function updateBulbIcon(entry, state) {
  const dev = devicesRef.get(
    Object.entries(placedBulbs).find(([, v]) => v === entry)?.[0] ?? ''
  );
  if (!entry.el) return;
  const shape = entry.el.querySelector('.lc-bulb-shape');
  if (!shape) return;
  const color = devStateColor(state ?? dev);
  shape.setAttribute('fill', color);
  shape.setAttribute('opacity', (state ?? dev)?.on ? 1 : 0.45);
}

// ── Popover ───────────────────────────────────────────────────────────────────

function openPopover(deviceId, anchorEl) {
  dismissPopover();

  const entry = placedBulbs[deviceId];
  if (!entry) return;

  const pop = document.createElement('div');
  pop.className = 'layout-popover';
  activePopover = pop;

  // Fixture type picker
  const typeLabel = document.createElement('div');
  typeLabel.className = 'layout-popover-label';
  typeLabel.textContent = 'Fixture type';
  pop.appendChild(typeLabel);

  const typeSelect = document.createElement('select');
  typeSelect.className = 'layout-popover-select';
  for (const ft of FIXTURE_TYPES) {
    const opt = document.createElement('option');
    opt.value = ft.id;
    opt.textContent = ft.label;
    opt.selected = ft.id === (entry.fixture_type ?? 'ceiling_spot');
    typeSelect.appendChild(opt);
  }
  typeSelect.addEventListener('change', () => {
    pushUndo();
    const newType = typeSelect.value;
    entry.fixture_type = newType;
    drawFixtureIcon(entry.el, entry.x * 1000, entry.y * 1000, entry.z, newType, devicesRef.get(deviceId));
    postPosition(deviceId, entry.x, entry.y, entry.z, newType);
  });
  pop.appendChild(typeSelect);

  // Height slider
  const heightLabel = document.createElement('div');
  heightLabel.className = 'layout-popover-label';
  heightLabel.textContent = `Height: ${Math.round(entry.z * 100)}%`;
  pop.appendChild(heightLabel);

  const slider = document.createElement('input');
  slider.type = 'range';
  slider.min = '0'; slider.max = '100';
  slider.value = Math.round(entry.z * 100);
  slider.className = 'layout-popover-slider';
  slider.addEventListener('input', () => {
    const z = parseInt(slider.value) / 100;
    heightLabel.textContent = `Height: ${slider.value}%`;
    entry.z = z;
    drawFixtureIcon(entry.el, entry.x * 1000, entry.y * 1000, z, entry.fixture_type, devicesRef.get(deviceId));
  });
  slider.addEventListener('change', () => {
    pushUndo();
    postPosition(deviceId, entry.x, entry.y, entry.z, entry.fixture_type);
  });
  pop.appendChild(slider);

  // Remove button
  const removeBtn = document.createElement('button');
  removeBtn.className = 'layout-popover-remove';
  removeBtn.textContent = 'Remove from canvas';
  removeBtn.addEventListener('click', () => {
    pushUndo();
    removeBulb(deviceId);
    dismissPopover();
  });
  pop.appendChild(removeBtn);

  // Position popover near the bulb using proper SVG coordinates
  const svg = document.getElementById('layout-canvas');
  const pt = svg.createSVGPoint();
  pt.x = entry.x * 1000;
  pt.y = entry.y * 1000;
  const screenPt = pt.matrixTransform(svg.getScreenCTM());
  const cx = screenPt.x;
  const cy = screenPt.y;
  pop.style.left = `${Math.min(cx + 30, window.innerWidth - 220)}px`;
  pop.style.top = `${Math.min(cy - 20, window.innerHeight - 260)}px`;

  document.body.appendChild(pop);
}

function dismissPopover() {
  if (activePopover) { activePopover.remove(); activePopover = null; }
  if (typeof window.__roomsStopPulse === 'function') window.__roomsStopPulse(true);
}

// ── Remove bulb ───────────────────────────────────────────────────────────────

function removeBulb(deviceId) {
  const entry = placedBulbs[deviceId];
  if (!entry) return;
  entry.el?.remove();
  entry.labelEl?.remove();
  delete placedBulbs[deviceId];
  rebuildSidebar();
  // Server: post zero coords so the position record is cleared
  postPosition(deviceId, 0, 0, 0, null);
}

// ── Auto-arrange ──────────────────────────────────────────────────────────────

function autoArrange() {
  const room = layoutRoom;
  if (!room) return;

  const unplaced = (room.device_ids || []).filter(id => !placedBulbs[id]);
  if (unplaced.length === 0) return;

  pushUndo();

  const wallTypes = new Set(['floor_lamp', 'table_lamp', 'led_strip']);
  const n = unplaced.length;
  const cols = Math.ceil(Math.sqrt(n));

  unplaced.forEach((id, i) => {
    const fixtureType = guessFixtureType(id);
    const z = defaultZ(fixtureType);
    let x, y;

    if (wallTypes.has(fixtureType)) {
      // Distribute along the walls (perimeter)
      const perimeterFrac = i / Math.max(n - 1, 1);
      ({ x, y } = perimeterPoint(perimeterFrac));
    } else {
      // Uniform grid, centred, avoiding edges
      const col = i % cols;
      const row = Math.floor(i / cols);
      const rows = Math.ceil(n / cols);
      x = 0.15 + (col / Math.max(cols - 1, 1)) * 0.7;
      y = 0.15 + (row / Math.max(rows - 1, 1)) * 0.7;
    }

    x = snap(x); y = snap(y);
    placeBulb(id, x, y, z, fixtureType, true);
  });
}

function perimeterPoint(frac) {
  // Walk perimeter: top → right → bottom → left, inset by 0.1
  const p = frac * 4;
  const inset = 0.12;
  if (p < 1) return { x: inset + (1 - 2 * inset) * p, y: inset };
  if (p < 2) return { x: 1 - inset, y: inset + (1 - 2 * inset) * (p - 1) };
  if (p < 3) return { x: 1 - inset - (1 - 2 * inset) * (p - 2), y: 1 - inset };
  return { x: inset, y: 1 - inset - (1 - 2 * inset) * (p - 3) };
}

function guessFixtureType(deviceId) {
  // If user already placed it before and it had a type, reuse it.
  // Otherwise default to ceiling_spot.
  return placedBulbs[deviceId]?.fixture_type ?? 'ceiling_spot';
}

function defaultZ(fixtureType) {
  return FIXTURE_TYPES.find(f => f.id === fixtureType)?.defaultZ ?? 1.0;
}

// ── Undo / redo ───────────────────────────────────────────────────────────────

function snapshotPositions() {
  const snap = {};
  for (const [id, e] of Object.entries(placedBulbs)) {
    snap[id] = { x: e.x, y: e.y, z: e.z, fixture_type: e.fixture_type };
  }
  return snap;
}

function pushUndo() {
  undoStack.push(snapshotPositions());
  redoStack = [];
}

function restoreSnapshot(snapshot) {
  // Clear current canvas bulbs
  const layer = document.getElementById('lc-bulbs');
  if (layer) layer.innerHTML = '';
  placedBulbs = {};

  for (const [id, pos] of Object.entries(snapshot)) {
    placeBulb(id, pos.x, pos.y, pos.z, pos.fixture_type, true);
  }
}

function undo() {
  if (undoStack.length === 0) return;
  redoStack.push(snapshotPositions());
  restoreSnapshot(undoStack.pop());
}

function redo() {
  if (redoStack.length === 0) return;
  undoStack.push(snapshotPositions());
  restoreSnapshot(redoStack.pop());
}

function onKeyDown(e) {
  if (e.ctrlKey && !e.shiftKey && e.key === 'z') { e.preventDefault(); undo(); }
  if (e.ctrlKey && e.shiftKey  && e.key === 'z') { e.preventDefault(); redo(); }
  if (e.key === 'Escape') dismissPopover();
}

// ── Server I/O ────────────────────────────────────────────────────────────────

function tok() { return localStorage.getItem('meshToken') ?? ''; }

async function loadPlacedBulbs(roomId) {
  try {
    const res = await fetch(`/api/rooms/${roomId}/positions?token=${encodeURIComponent(tok())}`);
    if (!res.ok) return;
    const items = await res.json();
    for (const item of items) {
      if (item.x === 0 && item.y === 0 && item.z === 0) continue; // unset
      placeBulb(item.device_id, item.x, item.y, item.z, item.fixture_type ?? 'ceiling_spot', false);
    }
  } catch (err) {
    console.warn('layout: failed to load positions', err);
  }
  rebuildSidebar();
}

async function postPosition(deviceId, x, y, z, fixtureType) {
  try {
    await fetch(`/api/lights/${encodeURIComponent(deviceId)}/position?token=${encodeURIComponent(tok())}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ x, y, z, fixture_type: fixtureType }),
    });
  } catch (err) {
    console.warn('layout: postPosition failed', err);
  }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function devStateColor(dev) {
  if (!dev || !dev.on) return '#444';
  if (dev.color_xy) {
    const [x, y] = dev.color_xy;
    return xyToHex(x, y);
  }
  if (dev.color_temp) return ctToHex(dev.color_temp);
  const b = dev.brightness ?? 200;
  const l = Math.round(30 + (b / 255) * 60);
  return `hsl(45,80%,${l}%)`;
}

function xyToHex(x, y) {
  // CIE xy → approximate sRGB (simplified wide-gamut path)
  const z = 1 - x - y;
  const Y = 1;
  const X = (Y / y) * x;
  const Z = (Y / y) * z;
  let r =  X * 1.656492 - Y * 0.354851 - Z * 0.255038;
  let g = -X * 0.707196 + Y * 1.655397 + Z * 0.036152;
  let b =  X * 0.051713 - Y * 0.121364 + Z * 1.011530;
  const m = Math.max(r, g, b, 1);
  r = Math.round(Math.min(Math.max(r / m, 0), 1) * 255);
  g = Math.round(Math.min(Math.max(g / m, 0), 1) * 255);
  b = Math.round(Math.min(Math.max(b / m, 0), 1) * 255);
  return `rgb(${r},${g},${b})`;
}

function ctToHex(mireds) {
  // Approximate colour temperature (mireds) → warm/cool white
  const t = ((mireds - 153) / (500 - 153));
  const r = Math.round(255);
  const g = Math.round(200 + (1 - t) * 55);
  const b = Math.round(100 + (1 - t) * 155);
  return `rgb(${r},${g},${b})`;
}

// ── Openings — geometry helpers ───────────────────────────────────────────────

const WALL_THICKNESS = 18;

function isHorizontalWall(wallEdge) {
  return wallEdge === 'N' || wallEdge === 'S';
}

function detectWall(nx, ny) {
  const Z = 0.08;
  if (ny < Z) return 'N';
  if (ny > 1 - Z) return 'S';
  if (nx < Z) return 'W';
  if (nx > 1 - Z) return 'E';
  return null;
}

function openingToSvgRect(o) {
  const hw = (o.width_norm * 1000) / 2;
  switch (o.wall_edge) {
    case 'N': return { x: o.x_norm * 1000 - hw, y: 0,                         w: o.width_norm * 1000, h: WALL_THICKNESS };
    case 'S': return { x: o.x_norm * 1000 - hw, y: 1000 - WALL_THICKNESS,     w: o.width_norm * 1000, h: WALL_THICKNESS };
    case 'E': return { x: 1000 - WALL_THICKNESS, y: o.x_norm * 1000 - hw,     w: WALL_THICKNESS, h: o.width_norm * 1000 };
    case 'W': return { x: 0,                     y: o.x_norm * 1000 - hw,     w: WALL_THICKNESS, h: o.width_norm * 1000 };
    default:  return { x: 0, y: 0, w: 0, h: 0 };
  }
}

function openingHandlePositions(o, rect) {
  if (isHorizontalWall(o.wall_edge)) {
    const my = rect.y + rect.h / 2;
    return [{ x: rect.x + 5, y: my }, { x: rect.x + rect.w - 5, y: my }];
  } else {
    const mx = rect.x + rect.w / 2;
    return [{ x: mx, y: rect.y + 5 }, { x: mx, y: rect.y + rect.h - 5 }];
  }
}

function edgeCursor(wallEdge) {
  return isHorizontalWall(wallEdge) ? 'ew-resize' : 'ns-resize';
}

// ── Openings — rendering ──────────────────────────────────────────────────────

function renderOpening(o) {
  const layer = document.getElementById('lc-openings');
  if (!layer) return;

  // Remove previous element for this opening
  placedOpenings[o.id]?.el?.remove();

  const rect = openingToSvgRect(o);
  const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  g.dataset.openingId = o.id;

  const body = svgEl('rect', {
    x: rect.x, y: rect.y, width: rect.w, height: rect.h, rx: 3,
    class: o.opening_type === 'window' ? 'lc-opening-window' : 'lc-opening-door',
    cursor: 'grab',
  });
  makeMoveDraggable(body, o.id);
  g.addEventListener('click', e => e.stopPropagation());
  g.appendChild(body);

  // Resize handles at each end
  const [h1pos, h2pos] = openingHandlePositions(o, rect);
  for (const [pos, side] of [[h1pos, 'start'], [h2pos, 'end']]) {
    const h = svgEl('rect', {
      x: pos.x - 5, y: pos.y - 5, width: 10, height: 10,
      rx: 2, class: 'lc-resize-handle', cursor: edgeCursor(o.wall_edge),
    });
    makeResizeDraggable(h, o.id, side);
    g.appendChild(h);
  }

  layer.appendChild(g);
  placedOpenings[o.id] = { ...o, el: g };
  updateOpeningCone(o.id);
}

function updateOpeningRectAttrs(openingId) {
  const o = placedOpenings[openingId];
  if (!o?.el) return;
  const rect = openingToSvgRect(o);
  const body = o.el.querySelector('rect');
  if (body) {
    body.setAttribute('x', rect.x);
    body.setAttribute('y', rect.y);
    body.setAttribute('width', rect.w);
    body.setAttribute('height', rect.h);
  }
  const handles = [...o.el.querySelectorAll('.lc-resize-handle')];
  const [h1pos, h2pos] = openingHandlePositions(o, rect);
  const hCursor = edgeCursor(o.wall_edge);
  for (const [i, pos] of [[0, h1pos], [1, h2pos]]) {
    if (handles[i]) {
      handles[i].setAttribute('x', pos.x - 5);
      handles[i].setAttribute('y', pos.y - 5);
      handles[i].setAttribute('cursor', hCursor);
    }
  }
  updateOpeningCone(openingId);
}

function updateOpeningCone(openingId) {
  const o = placedOpenings[openingId];
  if (!o) return;
  const layer = document.getElementById('lc-shadow');
  if (!layer) return;
  const old = document.getElementById(`cone-${CSS.escape(openingId)}`);
  if (old) old.remove();
  const cone = buildCone(o, lastSolar.azimuth, lastSolar.elevation);
  if (cone) { cone.id = `cone-${openingId}`; layer.appendChild(cone); }
}

function buildCone(o, azimuth, elevation) {
  if (elevation <= -18) return null;
  const wallFacing = { N: 0, E: 90, S: 180, W: 270 }[o.wall_edge] ?? 0;
  const diff = Math.abs(((azimuth - wallFacing) + 360) % 360);
  const norm = diff > 180 ? 360 - diff : diff;
  if (norm > 90) return null;

  const T = WALL_THICKNESS / 2;
  let ox, oy;
  switch (o.wall_edge) {
    case 'N': ox = o.x_norm * 1000; oy = T;       break;
    case 'S': ox = o.x_norm * 1000; oy = 1000 - T; break;
    case 'E': ox = 1000 - T;        oy = o.x_norm * 1000; break;
    case 'W': ox = T;               oy = o.x_norm * 1000; break;
    default:  return null;
  }

  const inwardAngle = ((azimuth + 180) % 360) * Math.PI / 180;
  const coneLen = 150 + Math.max(elevation, 0) * 2;
  const halfAngle = (15 + o.width_norm * 30) * Math.PI / 180;
  const elevFactor = Math.min(Math.max(elevation / 45, 0), 1);
  const opacity = o.transmission * elevFactor * (1 - norm / 90) * 0.22;
  if (opacity < 0.01) return null;

  const lx = ox + Math.sin(inwardAngle - halfAngle) * coneLen;
  const ly = oy - Math.cos(inwardAngle - halfAngle) * coneLen;
  const rx = ox + Math.sin(inwardAngle + halfAngle) * coneLen;
  const ry = oy - Math.cos(inwardAngle + halfAngle) * coneLen;

  return svgEl('polygon', {
    points: `${ox.toFixed(1)},${oy.toFixed(1)} ${lx.toFixed(1)},${ly.toFixed(1)} ${rx.toFixed(1)},${ry.toFixed(1)}`,
    fill: `rgba(255,220,100,${opacity.toFixed(3)})`,
    'pointer-events': 'none',
  });
}

// ── Openings — move drag ──────────────────────────────────────────────────────

function makeMoveDraggable(body, openingId) {
  let pressing = false;  // pointerdown received, drag not yet confirmed
  let moving = false;    // drag confirmed (past distance threshold)
  let startX = 0, startY = 0;

  body.addEventListener('pointerdown', e => {
    if (e.button !== 0) return;
    e.stopPropagation();
    pressing = true;
    moving = false;
    startX = e.clientX;
    startY = e.clientY;
    // Pointer capture deferred — held only while actually dragging so that
    // a short tap falls through to the click/popover path.
  });

  body.addEventListener('pointermove', e => {
    if (!pressing) return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;

    if (!moving) {
      if (Math.sqrt(dx * dx + dy * dy) < 6) return; // below drag threshold
      moving = true;
      body.setPointerCapture(e.pointerId);
      body.style.cursor = 'grabbing';
    }

    const o = placedOpenings[openingId];
    if (!o) return;
    const svg = document.getElementById('layout-canvas');
    const pt = svgPoint(svg, e.clientX, e.clientY);
    const wall = detectWall(pt.nx, pt.ny) ?? o.wall_edge;
    const coord = isHorizontalWall(wall) ? pt.nx : pt.ny;
    o.wall_edge = wall;
    o.x_norm = Math.min(1 - o.width_norm / 2 - 0.02, Math.max(o.width_norm / 2 + 0.02, snap(coord)));
    updateOpeningRectAttrs(openingId);
  });

  body.addEventListener('pointerup', e => {
    if (!pressing) return;
    const wasDrag = moving;
    pressing = false;
    moving = false;
    body.style.cursor = 'grab';
    if (body.hasPointerCapture(e.pointerId)) body.releasePointerCapture(e.pointerId);

    if (!wasDrag) {
      openOpeningPopover(openingId, body);
      return;
    }
    const o = placedOpenings[openingId];
    if (o) patchOpening(openingId, { wall_edge: o.wall_edge, x_norm: o.x_norm });
  });

  body.addEventListener('pointercancel', () => {
    pressing = false; moving = false; body.style.cursor = 'grab';
  });
}

// ── Openings — resize drag ────────────────────────────────────────────────────

function makeResizeDraggable(handle, openingId, side) {
  let dragging = false;
  let startCoord, startXNorm, startWidthNorm;

  handle.addEventListener('pointerdown', e => {
    e.stopPropagation(); e.preventDefault();
    const o = placedOpenings[openingId];
    if (!o) return;
    dragging = true;
    const svg = document.getElementById('layout-canvas');
    const pt = svgPoint(svg, e.clientX, e.clientY);
    startCoord = isHorizontalWall(o.wall_edge) ? pt.nx : pt.ny;
    startXNorm = o.x_norm;
    startWidthNorm = o.width_norm;
    handle.setPointerCapture(e.pointerId);
  });

  handle.addEventListener('pointermove', e => {
    if (!dragging) return;
    const o = placedOpenings[openingId];
    if (!o) return;
    const svg = document.getElementById('layout-canvas');
    const pt = svgPoint(svg, e.clientX, e.clientY);
    const coord = isHorizontalWall(o.wall_edge) ? pt.nx : pt.ny;
    const delta = coord - startCoord;

    if (side === 'start') {
      const oldEnd = startXNorm + startWidthNorm / 2;
      const newStart = Math.max(0.02, Math.min(oldEnd - 0.05, startXNorm - startWidthNorm / 2 + delta));
      const newWidth = Math.max(0.05, oldEnd - newStart);
      o.width_norm = Math.min(0.96, Math.max(0.05, snap(newWidth)));
      o.x_norm    = Math.min(0.97, Math.max(0.03, snap(newStart + o.width_norm / 2)));
    } else {
      const oldStart = startXNorm - startWidthNorm / 2;
      const newEnd = Math.min(0.98, Math.max(oldStart + 0.05, startXNorm + startWidthNorm / 2 + delta));
      const newWidth = Math.max(0.05, newEnd - oldStart);
      o.width_norm = Math.min(0.96, Math.max(0.05, snap(newWidth)));
      o.x_norm    = Math.min(0.97, Math.max(0.03, snap(oldStart + o.width_norm / 2)));
    }
    updateOpeningRectAttrs(openingId);
  });

  handle.addEventListener('pointerup', e => {
    if (!dragging) return;
    dragging = false;
    handle.releasePointerCapture(e.pointerId);
    const o = placedOpenings[openingId];
    if (o) patchOpening(openingId, { x_norm: o.x_norm, width_norm: o.width_norm });
  });

  handle.addEventListener('pointercancel', () => { dragging = false; });
}

// ── Openings — popover ────────────────────────────────────────────────────────

function openOpeningPopover(openingId, anchorEl) {
  dismissPopover();
  const o = placedOpenings[openingId];
  if (!o) return;

  const pop = document.createElement('div');
  pop.className = 'layout-popover';
  activePopover = pop;

  const typeRow = document.createElement('div');
  typeRow.className = 'layout-popover-label';
  typeRow.textContent = o.opening_type === 'window' ? '⬜ Window' : '▯ Door';
  typeRow.style.fontWeight = 'bold';
  pop.appendChild(typeRow);

  const transLabel = document.createElement('div');
  transLabel.className = 'layout-popover-label';
  transLabel.textContent = `Transmission: ${Math.round(o.transmission * 100)}%`;
  pop.appendChild(transLabel);

  const slider = document.createElement('input');
  slider.type = 'range'; slider.min = '0'; slider.max = '100';
  slider.value = Math.round(o.transmission * 100);
  slider.className = 'layout-popover-slider';
  slider.addEventListener('input', () => {
    o.transmission = parseInt(slider.value) / 100;
    transLabel.textContent = `Transmission: ${slider.value}%`;
    updateOpeningCone(openingId);
  });
  slider.addEventListener('change', () => {
    patchOpening(openingId, { transmission: o.transmission });
  });
  pop.appendChild(slider);

  const presets = o.opening_type === 'window'
    ? [{ label: 'Clear',       v: 1.0 }, { label: 'Frosted', v: 0.5 }, { label: 'Blind',       v: 0.05 }]
    : [{ label: 'Solid door',  v: 0.1 }, { label: '½ glazed', v: 0.3 }, { label: 'Full glazed', v: 0.7 }];
  const presetRow = document.createElement('div');
  presetRow.className = 'layout-popover-presets';
  for (const p of presets) {
    const btn = document.createElement('button');
    btn.textContent = p.label;
    btn.className = 'layout-popover-preset-btn';
    btn.addEventListener('click', () => {
      o.transmission = p.v;
      slider.value = Math.round(p.v * 100);
      transLabel.textContent = `Transmission: ${Math.round(p.v * 100)}%`;
      patchOpening(openingId, { transmission: p.v });
      updateOpeningCone(openingId);
    });
    presetRow.appendChild(btn);
  }
  pop.appendChild(presetRow);

  const removeBtn = document.createElement('button');
  removeBtn.className = 'layout-popover-remove';
  removeBtn.textContent = 'Remove opening';
  removeBtn.addEventListener('click', () => {
    removeOpening(openingId);
    dismissPopover();
  });
  pop.appendChild(removeBtn);

  // Position near opening
  const svg = document.getElementById('layout-canvas');
  const r = openingToSvgRect(o);
  const pt = svg.createSVGPoint();
  pt.x = r.x + r.w / 2;
  pt.y = r.y + r.h / 2;
  const sp = pt.matrixTransform(svg.getScreenCTM());
  pop.style.left = `${Math.min(sp.x + window.scrollX + 12, window.innerWidth - 220)}px`;
  pop.style.top  = `${Math.min(sp.y + window.scrollY + 12, window.innerHeight - 300)}px`;
  document.body.appendChild(pop);
}

// ── Openings — remove ─────────────────────────────────────────────────────────

async function removeOpening(id) {
  const entry = placedOpenings[id];
  if (!entry) return;
  await apiDeleteOpening(id);
  entry.el?.remove();
  const cone = document.getElementById(`cone-${CSS.escape(id)}`);
  if (cone) cone.remove();
  delete placedOpenings[id];
}

// ── Openings — server I/O ─────────────────────────────────────────────────────

async function loadPlacedOpenings(roomId) {
  try {
    const res = await fetch(`/api/rooms/${encodeURIComponent(roomId)}/openings?token=${encodeURIComponent(tok())}`);
    if (!res.ok) return;
    const items = await res.json();
    for (const item of items) renderOpening(item);
  } catch (err) {
    console.warn('layout: failed to load openings', err);
  }
}

async function postCreateOpening(roomId, openingType, wallEdge, xNorm, widthNorm, transmission) {
  try {
    const res = await fetch(
      `/api/rooms/${encodeURIComponent(roomId)}/openings?token=${encodeURIComponent(tok())}`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          opening_type: openingType,
          wall_edge: wallEdge,
          x_norm: xNorm,
          width_norm: widthNorm,
          transmission,
        }),
      }
    );
    if (!res.ok) return;
    const { id } = await res.json();
    renderOpening({ id, room_id: roomId, opening_type: openingType,
      wall_edge: wallEdge, x_norm: xNorm, width_norm: widthNorm, transmission });
  } catch (err) {
    console.warn('layout: postCreateOpening failed', err);
  }
}

async function patchOpening(id, data) {
  const o = placedOpenings[id];
  if (!o) return;
  try {
    await fetch(
      `/api/rooms/${encodeURIComponent(o.room_id)}/openings/${encodeURIComponent(id)}?token=${encodeURIComponent(tok())}`,
      { method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(data) }
    );
  } catch (err) {
    console.warn('layout: patchOpening failed', err);
  }
}

async function apiDeleteOpening(id) {
  const o = placedOpenings[id] ?? { room_id: layoutRoom?.id };
  if (!o.room_id) return;
  try {
    await fetch(
      `/api/rooms/${encodeURIComponent(o.room_id)}/openings/${encodeURIComponent(id)}?token=${encodeURIComponent(tok())}`,
      { method: 'DELETE' }
    );
  } catch (err) {
    console.warn('layout: apiDeleteOpening failed', err);
  }
}
