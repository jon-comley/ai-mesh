// ── Lighting panel ──────────────────────────────────────────────────────────
// Renders per-device light state cards from LightingUpdate events.

const ORDER_KEY = 'meshLightOrder';
let devicesMap = new Map();
let dragSrc = null;

export function handleLightingUpdate(evt) {
  devicesMap.clear();
  for (const dev of evt.devices) {
    devicesMap.set(dev.device_id, dev);
  }
  render();
}

function render() {
  const container = document.getElementById('lighting-list');
  if (!container || dragSrc) return;

  if (devicesMap.size === 0) {
    container.innerHTML = '<p class="placeholder">No lighting devices.</p>';
    return;
  }

  const items = [...devicesMap.values()];
  const sorted = applyOrder(items, d => d.device_id);

  container.innerHTML = '';
  for (const dev of sorted) {
    const card = document.createElement('div');
    card.className = 'light-card';
    card.setAttribute('draggable', 'true');
    card.setAttribute('data-drag-id', dev.device_id);
    card.innerHTML = deviceCard(dev);
    container.appendChild(card);
    enableDrag(card);
  }
}

function deviceCard(dev) {
  const badgeClass = dev.on ? 'badge-green' : 'badge-muted';
  const badgeLabel = dev.on ? 'On' : 'Off';

  let details = '';
  if (dev.brightness != null) {
    const pct = Math.round((dev.brightness / 255) * 100);
    details += `
      <div class="light-detail-row">
        <span class="light-detail-label">Brightness</span>
        <div class="light-bar-track"><div class="light-bar-fill" style="width:${pct}%"></div></div>
        <span class="light-detail-value">${pct}%</span>
      </div>`;
  }
  if (dev.color_temp != null) {
    const kelvin = Math.round(1_000_000 / dev.color_temp);
    details += `
      <div class="light-detail-row">
        <span class="light-detail-label">Color temp</span>
        <span class="light-detail-value">${kelvin} K</span>
      </div>`;
  }

  let swatch = '';
  if (dev.color_xy != null) {
    const [x, y] = dev.color_xy;
    const { r, g, b } = xyToRgb(x, y, dev.brightness ?? 254);
    swatch = `<span class="color-swatch" style="background:rgb(${r},${g},${b})" title="XY (${x.toFixed(3)}, ${y.toFixed(3)})"></span>`;
  }

  return `
    <div class="light-card-header">
      <span class="light-name">${esc(dev.device_id)}</span>
      <div class="light-card-header-right">
        ${swatch}
        <span class="badge ${badgeClass}">${badgeLabel}</span>
      </div>
    </div>
    ${details ? `<div class="light-card-details">${details}</div>` : ''}
  `;
}

// CIE XY + brightness → approximate sRGB (Philips Hue algorithm)
function xyToRgb(x, y, bri = 254) {
  if (y === 0) return { r: 0, g: 0, b: 0 };
  const z = 1.0 - x - y;
  const Y = bri / 254;
  const X = (Y / y) * x;
  const Z = (Y / y) * z;
  let r =  X * 1.656492 - Y * 0.354851 - Z * 0.255038;
  let g = -X * 0.707196 + Y * 1.655397 + Z * 0.036152;
  let b =  X * 0.051713 - Y * 0.121364 + Z * 1.011530;
  if (r < 0) r = 0; if (g < 0) g = 0; if (b < 0) b = 0;
  const max = Math.max(r, g, b);
  if (max > 1) { r /= max; g /= max; b /= max; }
  const gc = v => v <= 0.0031308 ? 12.92 * v : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
  return { r: Math.round(gc(r) * 255), g: Math.round(gc(g) * 255), b: Math.round(gc(b) * 255) };
}

function esc(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

// ── Drag-to-reorder (same pattern as models.js / topology.js) ───────────────

function enableDrag(el) {
  el.addEventListener('dragstart', e => {
    dragSrc = el;
    e.dataTransfer.effectAllowed = 'move';
    requestAnimationFrame(() => el.classList.add('dragging'));
  });
  el.addEventListener('dragover', e => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    const container = el.parentElement;
    const afterEl = getDragAfterEl(container, e.clientY);
    if (afterEl == null) {
      container.appendChild(dragSrc);
    } else {
      container.insertBefore(dragSrc, afterEl);
    }
  });
  el.addEventListener('dragend', () => {
    el.classList.remove('dragging');
    dragSrc = null;
    saveOrder(el.parentElement);
  });
}

function getDragAfterEl(container, y) {
  const draggables = [...container.querySelectorAll('[draggable="true"]:not(.dragging)')];
  return draggables.reduce((closest, child) => {
    const box = child.getBoundingClientRect();
    const offset = y - box.top - box.height / 2;
    if (offset < 0 && offset > closest.offset) {
      return { offset, element: child };
    }
    return closest;
  }, { offset: Number.NEGATIVE_INFINITY }).element;
}

function saveOrder(container) {
  const ids = [...container.querySelectorAll('[data-drag-id]')].map(el => el.dataset.dragId);
  localStorage.setItem(ORDER_KEY, JSON.stringify(ids));
}

function applyOrder(items, idFn) {
  const saved = JSON.parse(localStorage.getItem(ORDER_KEY) || '[]');
  if (!saved.length) return items;
  return [...items].sort((a, b) => {
    const ia = saved.indexOf(idFn(a));
    const ib = saved.indexOf(idFn(b));
    if (ia === -1 && ib === -1) return 0;
    if (ia === -1) return 1;
    if (ib === -1) return -1;
    return ia - ib;
  });
}
