// ── Rooms panel ──────────────────────────────────────────────────────────────
// First-class spatial room objects with drag-and-drop device assignment.

let roomsData = [];
let devicesMap = new Map();
let dragSrc = null; // { deviceId, fromRoomId: string | 'unassigned' }

export function handleRoomsUpdate(evt) {
  roomsData = evt.rooms ?? [];
  render();
}

export function notifyDevices(devices) {
  devicesMap.clear();
  for (const dev of devices) devicesMap.set(dev.device_id, dev);
  render();
}

// ── Main render ──────────────────────────────────────────────────────────────

function render() {
  const container = document.getElementById('lighting-list');
  if (!container || dragSrc) return;

  const assigned = new Set(roomsData.flatMap(r => r.device_ids));
  const unassigned = [...devicesMap.keys()].filter(id => !assigned.has(id));

  container.innerHTML = '';
  container.appendChild(renderNewRoomBtn());
  container.appendChild(renderUnassigned(unassigned));
  for (const room of roomsData) {
    container.appendChild(renderRoomCard(room));
  }
}

// ── New Room button ──────────────────────────────────────────────────────────

function renderNewRoomBtn() {
  const wrap = document.createElement('div');
  wrap.className = 'room-new-wrap';
  const btn = document.createElement('button');
  btn.className = 'room-new-btn';
  btn.textContent = '+ New Room';
  btn.addEventListener('click', () => {
    wrap.innerHTML = '';
    const input = document.createElement('input');
    input.className = 'room-name-input';
    input.placeholder = 'Room name…';
    let confirmed = false;
    const confirm = () => {
      if (confirmed) return;
      confirmed = true;
      const name = input.value.trim();
      if (name) createRoom(name);
      else render();
    };
    input.addEventListener('keydown', e => {
      if (e.key === 'Enter') confirm();
      if (e.key === 'Escape') { confirmed = true; render(); }
    });
    input.addEventListener('blur', confirm);
    wrap.appendChild(input);
    input.focus();
  });
  wrap.appendChild(btn);
  return wrap;
}

// ── Unassigned strip ─────────────────────────────────────────────────────────

function renderUnassigned(deviceIds) {
  const strip = document.createElement('div');
  strip.className = 'room-unassigned';

  const label = document.createElement('div');
  label.className = 'room-unassigned-label';
  label.textContent = 'Unassigned';
  strip.appendChild(label);

  const chips = document.createElement('div');
  chips.className = 'room-chips';

  if (devicesMap.size === 0) {
    chips.innerHTML = '<span class="room-empty-hint">No lighting devices.</span>';
  } else if (deviceIds.length === 0) {
    chips.innerHTML = '<span class="room-empty-hint">All devices assigned</span>';
  } else {
    for (const id of deviceIds) {
      chips.appendChild(renderChip(id, 'unassigned', false));
    }
  }

  strip.appendChild(chips);
  wireDropZone(strip, 'unassigned');
  return strip;
}

// ── Room card ────────────────────────────────────────────────────────────────

function renderRoomCard(room) {
  const card = document.createElement('div');
  card.className = 'room-card';
  card.dataset.roomId = room.id;

  // Header: name + rename + delete
  const header = document.createElement('div');
  header.className = 'room-card-header';

  const nameEl = document.createElement('span');
  nameEl.className = 'room-name';
  nameEl.textContent = room.name;
  nameEl.addEventListener('click', () => startRename(nameEl, room));
  header.appendChild(nameEl);

  const actions = document.createElement('div');
  actions.className = 'room-actions';

  const renameBtn = document.createElement('button');
  renameBtn.className = 'room-action-btn';
  renameBtn.textContent = 'rename';
  renameBtn.addEventListener('click', () => startRename(nameEl, room));
  actions.appendChild(renameBtn);

  const deleteBtn = document.createElement('button');
  deleteBtn.className = 'room-action-btn room-action-delete';
  deleteBtn.textContent = 'delete';
  deleteBtn.addEventListener('click', () => deleteRoom(room.id));
  actions.appendChild(deleteBtn);

  header.appendChild(actions);
  card.appendChild(header);

  // Room-level controls: on/off only (devices have their own sliders)
  const controls = document.createElement('div');
  controls.className = 'room-controls';

  const empty = room.device_ids.length === 0;

  const onBtn = document.createElement('button');
  onBtn.className = 'light-toggle-btn';
  onBtn.innerHTML = '<span class="badge badge-green">On</span>';
  onBtn.disabled = empty;
  if (!empty) onBtn.addEventListener('click', () => sendRoomCommand(room.id, { action: 'on' }, room));

  const offBtn = document.createElement('button');
  offBtn.className = 'light-toggle-btn';
  offBtn.innerHTML = '<span class="badge badge-muted">Off</span>';
  offBtn.disabled = empty;
  if (!empty) offBtn.addEventListener('click', () => sendRoomCommand(room.id, { action: 'off' }, room));

  controls.appendChild(onBtn);
  controls.appendChild(offBtn);
  card.appendChild(controls);

  // Device cards
  const devicesEl = document.createElement('div');
  devicesEl.className = 'room-devices';

  if (room.device_ids.length === 0) {
    const hint = document.createElement('span');
    hint.className = 'room-drop-hint';
    hint.textContent = 'Drop bulbs here';
    devicesEl.appendChild(hint);
  } else {
    for (const deviceId of room.device_ids) {
      const dev = devicesMap.get(deviceId);
      if (dev) {
        const devCard = buildDeviceCard(dev, room.id);
        devicesEl.appendChild(devCard);
        wireDeviceControls(devCard, dev, room.id);
      } else {
        devicesEl.appendChild(buildDevicePlaceholder(deviceId, room.id));
      }
    }
  }

  card.appendChild(devicesEl);
  wireDropZone(card, room.id);
  return card;
}

// ── Device card inside a room ────────────────────────────────────────────────

function buildDeviceCard(dev, roomId) {
  const card = document.createElement('div');
  card.className = 'light-card room-device-card';
  card.innerHTML = deviceCardHtml(dev);
  return card;
}

function deviceCardHtml(dev) {
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
        <button class="room-remove-btn" data-ctrl="room-remove" title="Remove from room" aria-label="Remove ${esc(displayName)} from room">✕</button>
      </div>
    </div>
    ${controls ? `<div class="light-card-details">${controls}</div>` : ''}
  `;
}

function buildDevicePlaceholder(deviceId, roomId) {
  const card = document.createElement('div');
  card.className = 'light-card room-device-card';
  card.innerHTML = `
    <div class="light-card-header">
      <div class="light-name-group">
        <span class="light-name">${esc(formatDeviceName(deviceId))}</span>
        <span class="light-node-badge">offline</span>
      </div>
      <div class="light-card-header-right">
        <button class="room-remove-btn" data-ctrl="room-remove" title="Remove from room" aria-label="Remove from room">✕</button>
      </div>
    </div>`;
  card.querySelector('[data-ctrl="room-remove"]').addEventListener('click', () => {
    removeDeviceFromRoom(roomId, deviceId);
  });
  return card;
}

function wireDeviceControls(card, dev, roomId) {
  const toggleBtn = card.querySelector('[data-ctrl="toggle"]');
  if (toggleBtn) {
    toggleBtn.addEventListener('click', e => {
      e.stopPropagation();
      devicesMap.set(dev.device_id, { ...dev, on: !dev.on });
      render();
      sendDeviceCommand(dev.device_id, { action: 'toggle' });
    });
  }

  const removeBtn = card.querySelector('[data-ctrl="room-remove"]');
  if (removeBtn) {
    removeBtn.addEventListener('click', e => {
      e.stopPropagation();
      removeDeviceFromRoom(roomId, dev.device_id);
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
      sendDeviceCommand(dev.device_id, { action: 'brightness', value: val });
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
      sendDeviceCommand(dev.device_id, { action: 'color_temp', value: val });
    });
  }

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
    sendDeviceCommand(dev.device_id, { action: 'color_xy', x, y });
  }

  if (hue) { hue.addEventListener('input', syncColourUI); hue.addEventListener('change', sendColour); }
  if (sat) { sat.addEventListener('input', syncColourUI); sat.addEventListener('change', sendColour); }
  if (hue || sat) syncColourUI();
}

// ── Chip (unassigned strip only) ─────────────────────────────────────────────

function renderChip(deviceId, fromRoomId, showRemove) {
  const dev = devicesMap.get(deviceId);
  const chip = document.createElement('div');
  chip.className = 'room-chip' + (dev?.on ? ' room-chip-on' : '');
  chip.setAttribute('draggable', 'true');
  chip.dataset.deviceId = deviceId;
  chip.title = deviceId;

  const label = document.createElement('span');
  label.className = 'room-chip-label';
  label.textContent = formatDeviceName(deviceId);
  chip.appendChild(label);

  if (showRemove) {
    const removeBtn = document.createElement('button');
    removeBtn.className = 'room-chip-remove';
    removeBtn.textContent = '✕';
    removeBtn.setAttribute('aria-label', `Remove ${formatDeviceName(deviceId)} from room`);
    removeBtn.addEventListener('click', e => {
      e.stopPropagation();
      removeDeviceFromRoom(fromRoomId, deviceId);
    });
    chip.appendChild(removeBtn);
  }

  chip.addEventListener('pointerdown', e => {
    if (e.target !== chip && e.target.closest('button')) {
      chip.setAttribute('draggable', 'false');
    }
  });
  chip.addEventListener('pointerup', () => chip.setAttribute('draggable', 'true'));
  chip.addEventListener('pointercancel', () => chip.setAttribute('draggable', 'true'));

  chip.addEventListener('dragstart', e => {
    dragSrc = { deviceId, fromRoomId };
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', deviceId);
    requestAnimationFrame(() => chip.classList.add('dragging'));
  });
  chip.addEventListener('dragend', () => {
    chip.classList.remove('dragging');
    dragSrc = null;
  });

  return chip;
}

// ── Drop zones ───────────────────────────────────────────────────────────────

function wireDropZone(el, roomId) {
  el.addEventListener('dragover', e => {
    if (!dragSrc) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    el.classList.add('room-drop-active');
  });
  el.addEventListener('dragleave', e => {
    if (!el.contains(e.relatedTarget)) {
      el.classList.remove('room-drop-active');
    }
  });
  el.addEventListener('drop', e => {
    e.preventDefault();
    el.classList.remove('room-drop-active');
    if (!dragSrc) return;
    const { deviceId, fromRoomId } = dragSrc;
    dragSrc = null;
    if (fromRoomId === roomId) return;

    if (roomId === 'unassigned') {
      if (fromRoomId !== 'unassigned') removeDeviceFromRoom(fromRoomId, deviceId);
    } else {
      addDeviceToRoom(roomId, deviceId);
    }
  });
}

// ── Inline rename ────────────────────────────────────────────────────────────

function startRename(nameEl, room) {
  const input = document.createElement('input');
  input.className = 'room-name-input room-name-input-inline';
  input.value = room.name;
  nameEl.replaceWith(input);
  input.focus();
  input.select();

  let confirmed = false;
  const confirm = () => {
    if (confirmed) return;
    confirmed = true;
    const name = input.value.trim();
    if (name && name !== room.name) renameRoom(room.id, name);
    else render();
  };
  input.addEventListener('keydown', e => {
    if (e.key === 'Enter') confirm();
    if (e.key === 'Escape') { confirmed = true; render(); }
  });
  input.addEventListener('blur', confirm);
}

// ── API calls ────────────────────────────────────────────────────────────────

function tok() { return localStorage.getItem('meshToken') ?? ''; }

async function createRoom(name) {
  try {
    const res = await fetch(`/api/rooms?token=${encodeURIComponent(tok())}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }),
    });
    if (!res.ok) showToast(`Create room failed (${res.status})`, true);
  } catch (e) { showToast(`Create room error: ${e.message}`, true); }
}

async function deleteRoom(id) {
  try {
    const res = await fetch(`/api/rooms/${id}?token=${encodeURIComponent(tok())}`, { method: 'DELETE' });
    if (!res.ok && res.status !== 404) showToast(`Delete room failed (${res.status})`, true);
  } catch (e) { showToast(`Delete room error: ${e.message}`, true); }
}

async function renameRoom(id, name) {
  try {
    const res = await fetch(`/api/rooms/${id}/name?token=${encodeURIComponent(tok())}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }),
    });
    if (!res.ok) showToast(`Rename failed (${res.status})`, true);
  } catch (e) { showToast(`Rename error: ${e.message}`, true); }
}

async function addDeviceToRoom(roomId, deviceId) {
  try {
    const res = await fetch(`/api/rooms/${roomId}/devices?token=${encodeURIComponent(tok())}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ add: [deviceId], remove: [] }),
    });
    if (!res.ok) showToast(`Add device failed (${res.status})`, true);
  } catch (e) { showToast(`Add device error: ${e.message}`, true); }
}

async function removeDeviceFromRoom(roomId, deviceId) {
  try {
    const res = await fetch(`/api/rooms/${roomId}/devices?token=${encodeURIComponent(tok())}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ add: [], remove: [deviceId] }),
    });
    if (!res.ok) showToast(`Remove device failed (${res.status})`, true);
  } catch (e) { showToast(`Remove device error: ${e.message}`, true); }
}

async function sendRoomCommand(roomId, body, room) {
  // Optimistic update so the UI reacts immediately
  if (room) {
    for (const deviceId of room.device_ids) {
      const dev = devicesMap.get(deviceId);
      if (dev) {
        if (body.action === 'on') devicesMap.set(deviceId, { ...dev, on: true });
        else if (body.action === 'off') devicesMap.set(deviceId, { ...dev, on: false });
      }
    }
    render();
  }
  try {
    const res = await fetch(`/api/rooms/${roomId}/command?token=${encodeURIComponent(tok())}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      if (res.status === 503) {
        showToast('Some devices offline — others updated', false);
      } else {
        showToast(`Room command failed (${res.status})`, true);
      }
    }
  } catch (e) { showToast(`Room command error: ${e.message}`, true); }
}

async function sendDeviceCommand(deviceId, body) {
  try {
    const res = await fetch(
      `/api/lights/${encodeURIComponent(deviceId)}/command?token=${encodeURIComponent(tok())}`,
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
  } catch (e) { showToast(`Command error: ${e.message}`, true); }
}

// ── Colour math (same as lighting.js) ───────────────────────────────────────

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

// ── Utilities ────────────────────────────────────────────────────────────────

function formatDeviceName(id) {
  return id.replace(/[_-]+/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
}

function esc(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
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
