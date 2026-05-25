// ── Lighting panel ──────────────────────────────────────────────────────────
// Renders per-device light state cards with interactive controls.

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
    wireControls(card, dev);
  }
}

function deviceCard(dev) {
  const badgeClass = dev.on ? 'badge-green' : 'badge-muted';
  const badgeLabel = dev.on ? 'On' : 'Off';
  const displayName = formatDeviceName(dev.device_id);

  let swatch = '';
  let colourPicker = '';
  if (dev.color_xy != null || dev.color_temp != null) {
    let h = 30, s = 80;
    let swatchRgb = `hsl(${h},${s}%,50%)`;
    if (dev.color_xy != null) {
      const [x, y] = dev.color_xy;
      const { r, g, b } = xyToRgb(x, y, dev.brightness ?? 254);
      ({ h, s } = rgbToHsl(r, g, b));
      swatchRgb = `rgb(${r},${g},${b})`;
    }
    swatch = `<button class="color-swatch-btn" data-ctrl="colour-toggle"
      style="background:${swatchRgb}"
      title="Pick colour" aria-label="Pick colour for ${esc(displayName)}"></button>`;
    colourPicker = `
      <div class="light-colour-picker" data-ctrl="colour-picker" role="group" aria-label="Colour controls">
        <div class="light-detail-row">
          <span class="light-detail-label">Hue</span>
          <input class="hue-slider" type="range" min="0" max="359" value="${h}"
                 data-ctrl="hue" aria-label="Hue">
          <span class="colour-swatch-preview" style="background:hsl(${h},${s}%,50%)"></span>
        </div>
        <div class="light-detail-row">
          <span class="light-detail-label">Saturation</span>
          <input class="light-slider" type="range" min="0" max="100" value="${s}"
                 data-ctrl="saturation" aria-label="Saturation"
                 style="background:linear-gradient(to right,#fff,hsl(${h},100%,50%))">
          <span class="light-detail-value">${s}%</span>
        </div>
      </div>`;
  }

  let controls = '';
  if (dev.brightness != null) {
    const pct = Math.round((dev.brightness / 255) * 100);
    controls += `
      <div class="light-detail-row">
        <span class="light-detail-label">Brightness</span>
        <input class="light-slider" type="range" min="0" max="255" value="${dev.brightness}"
               data-ctrl="brightness" title="${pct}%" aria-label="Brightness">
        <span class="light-detail-value">${pct}%</span>
      </div>`;
  }
  if (dev.color_temp != null) {
    const kelvin = Math.round(1_000_000 / dev.color_temp);
    controls += `
      <div class="light-detail-row">
        <span class="light-detail-label">Color temp</span>
        <input class="light-slider" type="range" min="154" max="500" value="${dev.color_temp}"
               data-ctrl="color_temp" title="${kelvin} K" aria-label="Color temperature">
        <span class="light-detail-value">${kelvin} K</span>
      </div>`;
  }
  controls += colourPicker;

  return `
    <div class="light-card-header">
      <div class="light-name-group">
        <span class="light-name">${esc(displayName)}</span>
        <span class="light-node-badge">${esc(dev.node_id)}</span>
      </div>
      <div class="light-card-header-right">
        ${swatch}
        <button class="light-toggle-btn" data-ctrl="toggle" aria-label="Toggle ${esc(displayName)}">
          <span class="badge ${badgeClass}">${badgeLabel}</span>
        </button>
      </div>
    </div>
    ${controls ? `<div class="light-card-details">${controls}</div>` : ''}
  `;
}

function wireControls(card, dev) {
  const toggleBtn = card.querySelector('[data-ctrl="toggle"]');
  if (toggleBtn) {
    toggleBtn.addEventListener('click', e => {
      e.stopPropagation();
      devicesMap.set(dev.device_id, { ...dev, on: !dev.on });
      render();
      sendCommand(dev.device_id, { action: 'toggle' });
    });
  }

  const bri = card.querySelector('[data-ctrl="brightness"]');
  if (bri) {
    bri.addEventListener('input', () => {
      const pct = Math.round((bri.value / 255) * 100);
      bri.title = `${pct}%`;
      const label = bri.parentElement.querySelector('.light-detail-value');
      if (label) label.textContent = `${pct}%`;
    });
    bri.addEventListener('change', () => {
      const val = parseInt(bri.value, 10);
      devicesMap.set(dev.device_id, { ...dev, brightness: val });
      sendCommand(dev.device_id, { action: 'brightness', value: val });
    });
  }

  const ct = card.querySelector('[data-ctrl="color_temp"]');
  if (ct) {
    ct.addEventListener('input', () => {
      const kelvin = Math.round(1_000_000 / ct.value);
      ct.title = `${kelvin} K`;
      const label = ct.parentElement.querySelector('.light-detail-value');
      if (label) label.textContent = `${kelvin} K`;
    });
    ct.addEventListener('change', () => {
      const val = parseInt(ct.value, 10);
      devicesMap.set(dev.device_id, { ...dev, color_temp: val });
      sendCommand(dev.device_id, { action: 'color_temp', value: val });
    });
  }

  // Colour picker
  const colourToggle = card.querySelector('[data-ctrl="colour-toggle"]');
  const colourPicker = card.querySelector('[data-ctrl="colour-picker"]');
  const hue = card.querySelector('[data-ctrl="hue"]');
  const sat = card.querySelector('[data-ctrl="saturation"]');
  const preview = colourPicker?.querySelector('.colour-swatch-preview');

  if (colourToggle && colourPicker) {
    colourToggle.addEventListener('click', e => {
      e.stopPropagation();
      colourPicker.classList.toggle('open');
    });
  }

  function syncColourUI() {
    if (!hue || !sat) return;
    const h = hue.value, s = sat.value;
    if (preview) preview.style.background = `hsl(${h},${s}%,50%)`;
    sat.style.background = `linear-gradient(to right,#fff,hsl(${h},100%,50%))`;
    const satLabel = sat.parentElement?.querySelector('.light-detail-value');
    if (satLabel) satLabel.textContent = `${s}%`;
    if (colourToggle) colourToggle.style.background = `hsl(${h},${s}%,50%)`;
  }

  function sendColour() {
    if (!hue || !sat) return;
    const { x, y } = hslToXy(parseInt(hue.value), parseInt(sat.value));
    devicesMap.set(dev.device_id, { ...dev, color_xy: [x, y] });
    sendCommand(dev.device_id, { action: 'color_xy', x, y });
  }

  if (hue) { hue.addEventListener('input', syncColourUI); hue.addEventListener('change', sendColour); }
  if (sat) { sat.addEventListener('input', syncColourUI); sat.addEventListener('change', sendColour); }
  if (hue || sat) syncColourUI();
}

async function sendCommand(deviceId, body) {
  const token = localStorage.getItem('meshToken') ?? '';
  try {
    const res = await fetch(
      `/api/lights/${encodeURIComponent(deviceId)}/command?token=${encodeURIComponent(token)}`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      }
    );
    if (!res.ok) {
      const text = await res.text().catch(() => '');
      showToast(`Command failed (${res.status})${text ? ': ' + text : ''}`, true);
    }
  } catch (e) {
    showToast(`Command error: ${e.message}`, true);
  }
}

function showToast(msg, isError = false) {
  let el = document.getElementById('light-toast');
  if (!el) {
    el = document.createElement('div');
    el.id = 'light-toast';
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.className = 'light-toast' + (isError ? ' light-toast-error' : '');
  el.style.opacity = '1';
  clearTimeout(el._timer);
  el._timer = setTimeout(() => { el.style.opacity = '0'; }, 4000);
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

// RGB (0-255) → HSL (h: 0-359, s: 0-100, l: 0-100)
function rgbToHsl(r, g, b) {
  r /= 255; g /= 255; b /= 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) return { h: 0, s: 0, l: Math.round(l * 100) };
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h;
  if (max === r) h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
  else if (max === g) h = ((b - r) / d + 2) / 6;
  else h = ((r - g) / d + 4) / 6;
  return { h: Math.round(h * 360) % 360, s: Math.round(s * 100), l: Math.round(l * 100) };
}

// HSL hue (0-359) + saturation (0-100) → CIE XY, L fixed at 50%
function hslToXy(h, s) {
  s /= 100;
  const l = 0.5;
  const a = s * Math.min(l, 1 - l);
  const f = n => { const k = (n + h / 30) % 12; return l - a * Math.max(-1, Math.min(k - 3, Math.min(9 - k, 1))); };
  const gc = v => v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  const r = gc(f(0)), g = gc(f(8)), b = gc(f(4));
  const X = r * 0.664511 + g * 0.154324 + b * 0.162028;
  const Y = r * 0.283881 + g * 0.668433 + b * 0.047685;
  const Z = r * 0.000088 + g * 0.072310 + b * 0.986039;
  const sum = X + Y + Z;
  if (sum === 0) return { x: 0.3227, y: 0.3290 };
  return { x: parseFloat((X / sum).toFixed(4)), y: parseFloat((Y / sum).toFixed(4)) };
}

function formatDeviceName(id) {
  return id.replace(/[_-]+/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
}

function esc(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

// ── Drag-to-reorder (same pattern as models.js / topology.js) ───────────────

function enableDrag(el) {
  // Disable card drag while the pointer is held on a form control (slider, button).
  el.addEventListener('pointerdown', e => {
    if (e.target !== el && e.target.closest('input, button')) {
      el.setAttribute('draggable', 'false');
    }
  });
  el.addEventListener('pointerup', () => el.setAttribute('draggable', 'true'));
  el.addEventListener('pointercancel', () => el.setAttribute('draggable', 'true'));

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
    el.setAttribute('draggable', 'true');
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
