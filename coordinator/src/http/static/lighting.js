// ── Lighting panel ──────────────────────────────────────────────────────────
// Renders per-device light state cards with interactive controls.

import { buildLightControls, repaintModeDots, HUE_DEFAULT_ON } from '/static/rooms.js';
import { esc, showToast } from '/static/util.js';

const ORDER_KEY = 'meshLightOrder';
let devicesMap = new Map();
let groupsSet = new Set();
let dragSrc = null;
let roomsActive = false;
let renderedIds = new Set();

export function setRoomsActive() { roomsActive = true; }

export function handleLightingUpdate(evt) {
  devicesMap.clear();
  for (const dev of evt.devices) devicesMap.set(dev.device_id, dev);
  groupsSet = new Set(evt.groups ?? []);

  const newIds = new Set(evt.devices.map(d => d.device_id));
  const idsChanged = newIds.size !== renderedIds.size || [...newIds].some(id => !renderedIds.has(id));
  if (idsChanged) { render(); } else { patchCards(); }
}

function patchCards() {
  const container = document.getElementById('lighting-list');
  if (!container || dragSrc || roomsActive) return;
  for (const dev of devicesMap.values()) {
    const card = container.querySelector(`[data-drag-id="${CSS.escape(dev.device_id)}"]`);
    if (!card) continue;

    const badge = card.querySelector('.badge');
    if (badge) {
      badge.className = `badge ${dev.on ? 'badge-green' : 'badge-muted'}`;
      badge.textContent = dev.on ? 'On' : 'Off';
    }

    const bri = card.querySelector('[data-ctrl="brightness"]');
    if (bri && !bri.classList.contains('slider-active') && document.activeElement !== bri) {
      bri.value = dev.brightness ?? 200;
      const pct = Math.round(((dev.brightness ?? 200) / 255) * 100);
      bri.title = `${pct}%`;
      const label = bri.parentElement?.querySelector('.light-detail-value');
      if (label) label.textContent = `${pct}%`;
    }

    const ct = card.querySelector('[data-ctrl="color_temp"]');
    if (ct && !ct.classList.contains('slider-active') && document.activeElement !== ct) {
      ct.value = dev.color_temp ?? 300;
      const kelvin = Math.round(1_000_000 / (dev.color_temp ?? 300));
      ct.title = `${kelvin} K`;
      const label = ct.parentElement?.querySelector('.light-detail-value');
      if (label) label.textContent = `${kelvin} K`;
    }

    // Repaint the active-domain dot from current state (colour tint or CCT tint).
    repaintModeDots(card, dev);
  }
}

function render() {
  const container = document.getElementById('lighting-list');
  if (!container || dragSrc || roomsActive) return;

  if (devicesMap.size === 0 && groupsSet.size === 0) {
    container.innerHTML = '<p class="placeholder">No lighting devices.</p>';
    return;
  }

  container.innerHTML = '';

  // Group cards first — skip the zigbee catch-all 'all' group: it duplicates
  // whole-house control and showed up as an undeletable, bulb-like card.
  for (const name of [...groupsSet].sort()) {
    if (name.toLowerCase() === 'all') continue;
    const card = document.createElement('div');
    card.className = 'light-card light-group-card';
    card.innerHTML = groupCard(name);
    container.appendChild(card);
    wireGroupControls(card, name);
  }

  // Individual device cards
  const items = [...devicesMap.values()];
  const sorted = applyOrder(items, d => d.device_id);
  for (const dev of sorted) {
    const card = document.createElement('div');
    card.className = 'light-card';
    card.setAttribute('draggable', 'true');
    card.setAttribute('data-drag-id', dev.device_id);
    card.setAttribute('data-device-id', dev.device_id);

    // Header
    const displayName = formatDeviceName(dev.device_id);
    const header = document.createElement('div');
    header.className = 'light-card-header';
    header.innerHTML = `
      <div class="light-name-group">
        <span class="light-name">${esc(displayName)}</span>
        <span class="light-node-badge">${esc(dev.node_id)}</span>
      </div>`;
    card.appendChild(header);

    // Controls
    if (dev.brightness != null) {
      const patch = fields => { const c = devicesMap.get(dev.device_id); if (c) devicesMap.set(dev.device_id, { ...c, ...fields }); };
      const controls = buildLightControls(dev, {
        onOn:  () => { patch({ on: true, brightness: 200, color_temp: 370 }); render(); for (const c of HUE_DEFAULT_ON) sendCommand(dev.device_id, c); },
        onOff: () => { patch({ on: false }); render(); sendCommand(dev.device_id, { action: 'off' }); },
        onBrightness: v => { patch({ brightness: v }); sendCommand(dev.device_id, { action: 'brightness', value: v }); },
        onTemp:       v => { patch({ color_temp: v }); sendCommand(dev.device_id, { action: 'color_temp', value: v }); },
        onColorXY: (x, y) => { patch({ color_xy: [x, y] }); sendCommand(dev.device_id, { action: 'color_xy', x, y }); },
      });
      controls.className += ' light-card-details';
      card.appendChild(controls);
    }

    container.appendChild(card);
    enableDrag(card);
  }
  renderedIds = new Set(devicesMap.keys());
}


function groupCard(name) {
  const displayName = formatDeviceName(name);
  return `
    <div class="light-card-header">
      <div class="light-name-group">
        <span class="light-name">${esc(displayName)}</span>
        <span class="light-node-badge">group</span>
      </div>
      <div class="light-card-header-right">
        <button class="light-toggle-btn" data-ctrl="group-on"  aria-label="Turn ${esc(displayName)} on">
          <span class="badge badge-green">On</span>
        </button>
        <button class="light-toggle-btn" data-ctrl="group-off" aria-label="Turn ${esc(displayName)} off">
          <span class="badge badge-muted">Off</span>
        </button>
      </div>
    </div>
    <div class="light-card-details">
      <div class="light-detail-row">
        <span class="light-detail-label">Brightness</span>
        <input class="light-slider" type="range" min="0" max="255" value="254"
               data-ctrl="group-brightness" aria-label="Group brightness">
        <span class="light-detail-value">100%</span>
      </div>
    </div>
  `;
}

function wireGroupControls(card, name) {
  const onBtn = card.querySelector('[data-ctrl="group-on"]');
  const offBtn = card.querySelector('[data-ctrl="group-off"]');
  if (onBtn) onBtn.addEventListener('click', () => sendGroupCommand(name, { action: 'on' }));
  if (offBtn) offBtn.addEventListener('click', () => sendGroupCommand(name, { action: 'off' }));

  const bri = card.querySelector('[data-ctrl="group-brightness"]');
  if (bri) {
    bri.addEventListener('input', () => {
      const pct = Math.round((bri.value / 255) * 100);
      const label = bri.parentElement.querySelector('.light-detail-value');
      if (label) label.textContent = `${pct}%`;
    });
    bri.addEventListener('change', () => {
      sendGroupCommand(name, { action: 'brightness', value: parseInt(bri.value, 10) });
    });
  }
}

async function sendGroupCommand(groupName, body) {
  const token = localStorage.getItem('meshToken') ?? '';
  try {
    const res = await fetch(
      `/api/lights/group/${encodeURIComponent(groupName)}/command?token=${encodeURIComponent(token)}`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      }
    );
    if (!res.ok) {
      const text = await res.text().catch(() => '');
      showToast(`Group command failed (${res.status})${text ? ': ' + text : ''}`, true);
    }
  } catch (e) {
    showToast(`Group command error: ${e.message}`, true);
  }
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

function formatDeviceName(id) {
  return id.replace(/[_-]+/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
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
