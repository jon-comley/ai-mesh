// ── Rooms panel ──────────────────────────────────────────────────────────────
// First-class spatial room objects with drag-and-drop device assignment
// and drag-to-reorder room cards.

import * as layout from '/static/layout.js';

let roomsData = [];
let devicesMap = new Map();
let scenesData = [];
let dragSrc = null;       // chip drag: { deviceId, fromRoomId }
let roomDragId = null;    // room reorder drag: room id being dragged
let effectDragSrc = null; // effect palette drag: effect name e.g. 'solar'

// ── Bulb-identify pulse (triggered on drag-grab) ─────────────────────────────
// Pulses the grabbed bulb full→dim so you know which physical unit you're holding.
// Restores original state on release.

let activePulse = null; // { deviceId, timerId, preGrabState }

function startPulse(deviceId) {
  if (activePulse) stopPulse(false); // cancel previous without restoring
  const preGrabState = devicesMap.get(deviceId) ?? null;
  if (!preGrabState) return; // device not yet known — can't pulse safely

  sendDeviceCommand(deviceId, { action: 'on' });
  // Candle temperature (500 mireds ≈ 2000 K) on any colour-capable bulb
  if (preGrabState.color_xy != null || preGrabState.color_temp != null) {
    sendDeviceCommand(deviceId, { action: 'color_temp', value: 500 });
  }
  sendDeviceCommand(deviceId, { action: 'brightness', value: 254, transition_secs: 0.6 });

  let step = 0;
  const timerId = setInterval(() => {
    step++;
    const phase = step * Math.PI / 2;
    const bri = Math.round(80 + 174 * (0.5 + 0.5 * Math.cos(phase)));
    sendDeviceCommand(deviceId, { action: 'brightness', value: bri, transition_secs: 0.6 });
  }, 700);

  activePulse = { deviceId, timerId, preGrabState };
}

function stopPulse(restore = true) {
  if (!activePulse) return;
  const { deviceId, timerId, preGrabState } = activePulse;
  clearInterval(timerId);
  activePulse = null;
  if (!restore || !preGrabState) return;
  if (!preGrabState.on) {
    sendDeviceCommand(deviceId, { action: 'off' });
  } else {
    // Restore original colour first, then brightness
    if (preGrabState.color_xy != null) {
      const [x, y] = preGrabState.color_xy;
      sendDeviceCommand(deviceId, { action: 'color_xy', x, y });
    } else if (preGrabState.color_temp != null) {
      sendDeviceCommand(deviceId, { action: 'color_temp', value: preGrabState.color_temp });
    }
    if (preGrabState.brightness != null) {
      sendDeviceCommand(deviceId, { action: 'brightness', value: preGrabState.brightness });
    } else {
      sendDeviceCommand(deviceId, { action: 'on' });
    }
  }
}

// Expose pulse helpers for layout.js (cross-module, avoids circular import)
window.__roomsStartPulse = startPulse;
window.__roomsStopPulse = stopPulse;

layout.init(devicesMap);

export function handleRoomsUpdate(evt) {
  roomsData = evt.rooms ?? [];
  render();
}

export function handleScenesUpdate(evt) {
  scenesData = evt.scenes ?? [];
  render();
}

export function notifyDevices(devices) {
  devicesMap.clear();
  for (const dev of devices) devicesMap.set(dev.device_id, dev);
  // Forward live state to canvas if layout is open
  for (const dev of devices) layout.notifyDeviceUpdate(dev.device_id, dev);
  render();
}

export function notifySolar(azimuth, elevation) {
  layout.notifySolarUpdate(azimuth, elevation);
}

// ── Main render ──────────────────────────────────────────────────────────────

function render() {
  const container = document.getElementById('lighting-list');
  if (!container || dragSrc || roomDragId) return;
  if (container.querySelector('.layout-view')) return; // layout open — don't wipe

  const assigned = new Set(roomsData.flatMap(r => r.device_ids));
  const unassigned = [...devicesMap.keys()].filter(id => !assigned.has(id));

  container.innerHTML = '';
  container.appendChild(renderEffectsPalette());
  container.appendChild(renderNewRoomBtn());
  container.appendChild(renderUnassigned(unassigned));

  const roomList = document.createElement('div');
  roomList.className = 'room-list rooms-layout-root';

  const sorted = [...roomsData].sort((a, b) => a.position - b.position);
  for (const room of sorted) {
    roomList.appendChild(renderRoomCard(room));
  }

  container.appendChild(roomList);
  wireRoomListDrag(roomList);
}

// ── Effects palette ──────────────────────────────────────────────────────────

function renderEffectsPalette() {
  const palette = document.createElement('div');
  palette.className = 'effects-palette';

  const label = document.createElement('span');
  label.className = 'effects-palette-label';
  label.textContent = 'Effects — drag onto a room:';
  palette.appendChild(label);

  const chip = document.createElement('div');
  chip.className = 'effect-chip';
  chip.setAttribute('draggable', 'true');
  chip.dataset.effect = 'solar';
  chip.innerHTML = '&#9728; Solar';
  chip.title = 'Drag onto a room to enable solar lighting mode for all its devices';

  chip.addEventListener('dragstart', e => {
    effectDragSrc = 'solar';
    e.dataTransfer.effectAllowed = 'copy';
    e.dataTransfer.setData('text/plain', 'effect:solar');
    requestAnimationFrame(() => chip.classList.add('dragging'));
  });
  chip.addEventListener('dragend', () => {
    effectDragSrc = null;
    chip.classList.remove('dragging');
  });

  palette.appendChild(chip);
  return palette;
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
    for (const id of deviceIds) chips.appendChild(renderChip(id, 'unassigned', false));
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

  // Make card draggable for reordering; disable when pointer is on controls
  card.setAttribute('draggable', 'true');
  card.addEventListener('pointerdown', e => {
    if (e.target.closest('button, input, .light-card, .room-chip')) {
      card.setAttribute('draggable', 'false');
    }
  });
  card.addEventListener('pointerup', () => card.setAttribute('draggable', 'true'));
  card.addEventListener('pointercancel', () => card.setAttribute('draggable', 'true'));
  card.addEventListener('dragstart', e => {
    if (effectDragSrc) { e.preventDefault(); return; } // let effect drag pass through
    if (card.getAttribute('draggable') !== 'true') { e.preventDefault(); return; }
    roomDragId = room.id;
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', `room:${room.id}`);
    requestAnimationFrame(() => card.classList.add('dragging'));
  });
  card.addEventListener('dragend', () => {
    card.classList.remove('dragging');
    const wasReordering = roomDragId !== null;
    roomDragId = null;
    card.setAttribute('draggable', 'true');
    if (wasReordering) saveRoomOrder();
  });

  // Header: name + rename + delete
  const header = document.createElement('div');
  header.className = 'room-card-header';

  const nameEl = document.createElement('span');
  nameEl.className = 'room-name';
  nameEl.textContent = room.name;
  nameEl.addEventListener('click', () => startRename(nameEl, room));
  header.appendChild(nameEl);

  const layoutBtn = document.createElement('button');
  layoutBtn.className = 'room-action-btn room-layout-btn';
  layoutBtn.title = 'Open floor plan';
  layoutBtn.textContent = '⊞';
  layoutBtn.addEventListener('click', e => { e.stopPropagation(); layout.openLayout(room); });
  header.appendChild(layoutBtn);

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

  // Solar active badge — shown when any device in room has solar_enabled
  const roomDevicesAll = room.device_ids.map(id => devicesMap.get(id)).filter(Boolean);
  const solarActive = roomDevicesAll.some(d => d.solar_enabled);
  if (solarActive) {
    const solarBadge = document.createElement('span');
    solarBadge.className = 'badge badge-solar';
    solarBadge.title = 'Solar mode active — click to disable';
    solarBadge.innerHTML = '&#9728; Solar';
    solarBadge.style.cursor = 'pointer';
    solarBadge.addEventListener('click', () => setSolarMode(room.id, false));
    actions.insertBefore(solarBadge, renameBtn);
  }

  header.appendChild(actions);
  card.appendChild(header);

  // Room-level controls: on/off + optional colour swatch
  const empty = room.device_ids.length === 0;
  const controls = document.createElement('div');
  controls.className = 'room-controls';

  const roomDevicesForState = room.device_ids.map(id => devicesMap.get(id)).filter(Boolean);
  const anyOn = roomDevicesForState.some(d => d.on);

  const onBtn  = document.createElement('button');
  const offBtn = document.createElement('button');

  const setRoomOnOff = (isOn) => {
    onBtn.innerHTML  = `<span class="badge ${isOn  ? 'badge-green' : 'badge-muted'}">On</span>`;
    offBtn.innerHTML = `<span class="badge ${!isOn ? 'badge-red'   : 'badge-muted'}">Off</span>`;
  };

  onBtn.className = 'light-toggle-btn';
  onBtn.disabled = empty;
  if (!empty) onBtn.addEventListener('click', () => {
    setRoomOnOff(true);
    sendRoomCommand(room.id, { action: 'on' }, room);
  });

  offBtn.className = 'light-toggle-btn';
  offBtn.disabled = empty;
  if (!empty) offBtn.addEventListener('click', () => {
    setRoomOnOff(false);
    sendRoomCommand(room.id, { action: 'off' }, room);
  });

  setRoomOnOff(anyOn);

  controls.appendChild(onBtn);
  controls.appendChild(offBtn);

  // Colour swatch — shown when at least one device in the room supports color_xy
  const roomDevices = room.device_ids.map(id => devicesMap.get(id)).filter(Boolean);
  const hasColour = roomDevices.some(d => d.color_xy != null);
  if (hasColour) {
    const { h, s } = getRoomColourHsl(roomDevices);
    const swatchBtn = document.createElement('button');
    swatchBtn.className = 'color-swatch-btn room-colour-swatch';
    swatchBtn.style.background = `hsl(${h},${s}%,50%)`;
    swatchBtn.title = 'Set room colour';
    swatchBtn.setAttribute('data-ctrl', 'room-colour-toggle');
    controls.appendChild(swatchBtn);
  }

  card.appendChild(controls);

  // Room colour picker (hidden until swatch clicked)
  if (hasColour) {
    const { h, s } = getRoomColourHsl(roomDevices);
    const pickerEl = buildRoomColourPicker(h, s);
    card.appendChild(pickerEl);

    const swatchBtn = controls.querySelector('[data-ctrl="room-colour-toggle"]');
    swatchBtn.addEventListener('click', () => pickerEl.classList.toggle('open'));
    wireRoomColourPicker(pickerEl, room.id, swatchBtn);
  }

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
        wireDeviceDrag(devCard, dev.device_id, room.id);
      } else {
        devicesEl.appendChild(buildDevicePlaceholder(deviceId, room.id));
      }
    }
  }

  card.appendChild(devicesEl);
  wireDropZone(card, room.id);

  // Scenes section
  card.appendChild(buildScenesSection(room.id));

  return card;
}

// ── Scenes section ──────────────────────────────────────────────────────────

function buildScenesSection(roomId) {
  const section = document.createElement('div');
  section.className = 'room-scenes';
  section.dataset.roomId = roomId;

  // Save scene row
  const saveRow = document.createElement('div');
  saveRow.className = 'room-scene-save-row';

  const saveBtn = document.createElement('button');
  saveBtn.className = 'room-scene-save-btn';
  saveBtn.textContent = '+ Save scene';
  saveRow.appendChild(saveBtn);

  const nameInput = document.createElement('input');
  nameInput.type = 'text';
  nameInput.className = 'room-scene-name-input';
  nameInput.placeholder = 'Scene name…';
  nameInput.style.display = 'none';
  saveRow.appendChild(nameInput);

  section.appendChild(saveRow);

  saveBtn.addEventListener('click', () => {
    saveBtn.style.display = 'none';
    nameInput.style.display = '';
    nameInput.value = '';
    nameInput.focus();
  });

  let savingScene = false;
  const doSave = () => {
    if (savingScene) return;
    const name = nameInput.value.trim();
    nameInput.style.display = 'none';
    saveBtn.style.display = '';
    if (!name) return;
    savingScene = true;
    saveScene(name, roomId).finally(() => { savingScene = false; });
  };
  nameInput.addEventListener('keydown', e => {
    if (e.key === 'Enter') doSave();
    if (e.key === 'Escape') { nameInput.style.display = 'none'; saveBtn.style.display = ''; }
  });
  nameInput.addEventListener('blur', doSave);

  // Scene list
  const roomScenes = scenesData
    .filter(s => s.room_id === roomId)
    .sort((a, b) => b.created_at - a.created_at);

  if (roomScenes.length > 0) {
    const list = document.createElement('ul');
    list.className = 'room-scene-list';

    for (const scene of roomScenes) {
      const li = document.createElement('li');
      li.className = 'room-scene-item';

      const nameSpan = document.createElement('span');
      nameSpan.className = 'room-scene-name';
      nameSpan.textContent = scene.name;
      li.appendChild(nameSpan);

      const recallBtn = document.createElement('button');
      recallBtn.className = 'room-scene-recall-btn';
      recallBtn.textContent = 'Recall';
      recallBtn.addEventListener('click', () => recallScene(scene.id));
      li.appendChild(recallBtn);

      const delBtn = document.createElement('button');
      delBtn.className = 'room-scene-delete-btn';
      delBtn.textContent = '✕';
      delBtn.title = `Delete scene "${scene.name}"`;
      delBtn.addEventListener('click', () => deleteSceneApi(scene.id));
      li.appendChild(delBtn);

      list.appendChild(li);
    }

    section.appendChild(list);
  }

  return section;
}

// ── Room colour picker ───────────────────────────────────────────────────────

function getRoomColourHsl(roomDevices) {
  const dev = roomDevices.find(d => d.color_xy != null);
  if (dev) {
    const [x, y] = dev.color_xy;
    const { r, g, b } = xyToRgb(x, y, dev.brightness ?? 254);
    const { h, s } = rgbToHsl(r, g, b);
    return { h, s };
  }
  return { h: 30, s: 80 };
}

function buildRoomColourPicker(h, s) {
  const picker = document.createElement('div');
  picker.className = 'light-colour-picker room-colour-picker';
  picker.innerHTML = `
    <div class="light-detail-row">
      <span class="light-detail-label">Hue</span>
      <input class="hue-slider" type="range" min="0" max="359" value="${h}"
             data-ctrl="room-hue" aria-label="Room hue">
      <span class="colour-swatch-preview" style="background:hsl(${h},${s}%,50%)"></span>
    </div>
    <div class="light-detail-row">
      <span class="light-detail-label">Saturation</span>
      <input class="light-slider" type="range" min="0" max="100" value="${s}"
             data-ctrl="room-sat" aria-label="Room saturation"
             style="background:linear-gradient(to right,#fff,hsl(${h},100%,50%))">
      <span class="light-detail-value">${s}%</span>
    </div>`;
  return picker;
}

function wireRoomColourPicker(pickerEl, roomId, swatchBtn) {
  const hue = pickerEl.querySelector('[data-ctrl="room-hue"]');
  const sat = pickerEl.querySelector('[data-ctrl="room-sat"]');
  const preview = pickerEl.querySelector('.colour-swatch-preview');

  function syncUI() {
    const h = hue.value, s = sat.value;
    if (preview) preview.style.background = `hsl(${h},${s}%,50%)`;
    sat.style.background = `linear-gradient(to right,#fff,hsl(${h},100%,50%))`;
    const satLabel = sat.parentElement?.querySelector('.light-detail-value');
    if (satLabel) satLabel.textContent = `${s}%`;
    if (swatchBtn) swatchBtn.style.background = `hsl(${h},${s}%,50%)`;
  }

  function sendColour() {
    const { x, y } = hslToXy(parseInt(hue.value), parseInt(sat.value));
    sendRoomCommand(roomId, { action: 'color_xy', x, y });
  }

  hue.addEventListener('input', syncUI);
  hue.addEventListener('change', sendColour);
  sat.addEventListener('input', syncUI);
  sat.addEventListener('change', sendColour);
  syncUI();
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
      style="background:${swatchRgb}" title="Pick colour"
      aria-label="Pick colour for ${esc(displayName)}"></button>`;
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
        <button class="room-remove-btn" data-ctrl="room-remove"
                title="Remove from room" aria-label="Remove ${esc(displayName)} from room">✕</button>
      </div>
    </div>
    ${controls ? `<div class="light-card-details">${controls}</div>` : ''}`;
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
        <button class="room-remove-btn" data-ctrl="room-remove"
                title="Remove from room" aria-label="Remove from room">✕</button>
      </div>
    </div>`;
  card.querySelector('[data-ctrl="room-remove"]').addEventListener('click', () => {
    removeDeviceFromRoom(roomId, deviceId);
  });
  return card;
}

function wireDeviceDrag(card, deviceId, roomId) {
  card.setAttribute('draggable', 'true');
  card.addEventListener('pointerdown', e => {
    if (e.target.closest('input, button')) card.setAttribute('draggable', 'false');
  });
  card.addEventListener('pointerup', () => card.setAttribute('draggable', 'true'));
  card.addEventListener('pointercancel', () => card.setAttribute('draggable', 'true'));

  card.addEventListener('dragstart', e => {
    e.stopPropagation(); // prevent room card from also starting a drag
    dragSrc = { deviceId, fromRoomId: roomId };
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', deviceId);
    requestAnimationFrame(() => card.classList.add('dragging'));
    startPulse(deviceId);
  });
  card.addEventListener('dragend', () => {
    card.classList.remove('dragging');
    dragSrc = null;
    card.setAttribute('draggable', 'true');
    card.closest('.room-card')?.setAttribute('draggable', 'true');
    stopPulse();
  });
}

function wireDeviceControls(card, dev, roomId) {
  card.querySelector('[data-ctrl="toggle"]')?.addEventListener('click', e => {
    e.stopPropagation();
    devicesMap.set(dev.device_id, { ...dev, on: !dev.on });
    render();
    sendDeviceCommand(dev.device_id, { action: 'toggle' });
  });

  card.querySelector('[data-ctrl="room-remove"]')?.addEventListener('click', e => {
    e.stopPropagation();
    removeDeviceFromRoom(roomId, dev.device_id);
  });

  const bri = card.querySelector('[data-ctrl="brightness"]');
  if (bri) {
    bri.addEventListener('input', () => {
      const pct = Math.round((bri.value / 255) * 100);
      bri.title = `${pct}%`;
      bri.parentElement.querySelector('.light-detail-value').textContent = `${pct}%`;
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
      ct.parentElement.querySelector('.light-detail-value').textContent = `${kelvin} K`;
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

  colourToggle?.addEventListener('click', e => {
    e.stopPropagation();
    colourPicker.classList.toggle('open');
  });

  function syncColourUI() {
    if (!hue || !sat) return;
    const h = hue.value, s = sat.value;
    if (preview) preview.style.background = `hsl(${h},${s}%,50%)`;
    sat.style.background = `linear-gradient(to right,#fff,hsl(${h},100%,50%))`;
    sat.parentElement?.querySelector('.light-detail-value')?.textContent === `${s}%`;
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

  const dot = document.createElement('span');
  dot.className = 'room-chip-dot' + (dev?.on ? ' room-chip-dot-on' : '');
  dot.setAttribute('aria-hidden', 'true');
  chip.appendChild(dot);

  const label = document.createElement('span');
  label.className = 'room-chip-label';
  label.textContent = formatDeviceName(deviceId);
  chip.appendChild(label);

  chip.addEventListener('pointerdown', e => {
    if (e.target !== chip && e.target.closest('button')) chip.setAttribute('draggable', 'false');
  });
  chip.addEventListener('pointerup', () => chip.setAttribute('draggable', 'true'));
  chip.addEventListener('pointercancel', () => chip.setAttribute('draggable', 'true'));

  chip.addEventListener('dragstart', e => {
    dragSrc = { deviceId, fromRoomId };
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', deviceId);
    requestAnimationFrame(() => chip.classList.add('dragging'));
    startPulse(deviceId);
  });
  chip.addEventListener('dragend', () => {
    chip.classList.remove('dragging');
    dragSrc = null;
    stopPulse();
  });

  return chip;
}

// ── Drop zones (chip → room assignment) ──────────────────────────────────────

function wireDropZone(el, roomId) {
  el.addEventListener('dragover', e => {
    if (!dragSrc && !effectDragSrc) return; // ignore room-reorder drags
    e.preventDefault();
    e.dataTransfer.dropEffect = effectDragSrc ? 'copy' : 'move';
    el.classList.add('room-drop-active');
  });
  el.addEventListener('dragleave', e => {
    if (!el.contains(e.relatedTarget)) el.classList.remove('room-drop-active');
  });
  el.addEventListener('drop', e => {
    e.preventDefault();
    el.classList.remove('room-drop-active');

    // Effect palette drop (only valid on real rooms, not unassigned)
    if (effectDragSrc && roomId !== 'unassigned') {
      const effect = effectDragSrc;
      effectDragSrc = null;
      if (effect === 'solar') setSolarMode(roomId, true);
      return;
    }

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

// ── Room list drag-to-reorder ─────────────────────────────────────────────────

function wireRoomListDrag(roomList) {
  roomList.addEventListener('dragover', e => {
    if (!roomDragId) return;
    e.preventDefault();
    const dragging = roomList.querySelector('.room-card.dragging');
    if (!dragging) return;
    const others = [...roomList.querySelectorAll('.room-card:not(.dragging)')];
    const after = others.reduce((closest, child) => {
      const box = child.getBoundingClientRect();
      const offset = e.clientY - box.top - box.height / 2;
      if (offset < 0 && offset > closest.offset) return { offset, element: child };
      return closest;
    }, { offset: Number.NEGATIVE_INFINITY }).element;
    if (after == null) roomList.appendChild(dragging);
    else roomList.insertBefore(dragging, after);
  });
}

function saveRoomOrder() {
  const roomList = document.querySelector('.room-list');
  if (!roomList) return;
  const ids = [...roomList.querySelectorAll('.room-card')].map(c => c.dataset.roomId);
  reorderRooms(ids);
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
      method: 'POST', headers: { 'Content-Type': 'application/json' },
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
      method: 'PATCH', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }),
    });
    if (!res.ok) showToast(`Rename failed (${res.status})`, true);
  } catch (e) { showToast(`Rename error: ${e.message}`, true); }
}

async function addDeviceToRoom(roomId, deviceId) {
  try {
    const res = await fetch(`/api/rooms/${roomId}/devices?token=${encodeURIComponent(tok())}`, {
      method: 'PATCH', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ add: [deviceId], remove: [] }),
    });
    if (!res.ok) showToast(`Add device failed (${res.status})`, true);
  } catch (e) { showToast(`Add device error: ${e.message}`, true); }
}

async function removeDeviceFromRoom(roomId, deviceId) {
  try {
    const res = await fetch(`/api/rooms/${roomId}/devices?token=${encodeURIComponent(tok())}`, {
      method: 'PATCH', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ add: [], remove: [deviceId] }),
    });
    if (!res.ok) showToast(`Remove device failed (${res.status})`, true);
  } catch (e) { showToast(`Remove device error: ${e.message}`, true); }
}

async function reorderRooms(ids) {
  try {
    const res = await fetch(`/api/rooms/reorder?token=${encodeURIComponent(tok())}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ids }),
    });
    if (!res.ok) showToast(`Reorder failed (${res.status})`, true);
  } catch (e) { showToast(`Reorder error: ${e.message}`, true); }
}

async function setSolarMode(roomId, enable) {
  const room = roomsData.find(r => r.id === roomId);
  if (!room) return;
  // Optimistic update of solar_enabled on each device
  for (const deviceId of room.device_ids) {
    const dev = devicesMap.get(deviceId);
    if (dev) devicesMap.set(deviceId, { ...dev, solar_enabled: enable });
  }
  render();
  try {
    const res = await fetch(`/api/rooms/${roomId}/command?token=${encodeURIComponent(tok())}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ action: 'solar_mode', value: enable ? 1 : 0 }),
    });
    if (!res.ok) showToast(`Solar mode failed (${res.status})`, true);
  } catch (e) { showToast(`Solar mode error: ${e.message}`, true); }
}

async function sendRoomCommand(roomId, body, room) {
  // Optimistic update
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
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      if (res.status === 503) showToast('Some devices offline — others updated', false);
      else showToast(`Room command failed (${res.status})`, true);
    }
  } catch (e) { showToast(`Room command error: ${e.message}`, true); }
}

async function sendDeviceCommand(deviceId, body) {
  try {
    const res = await fetch(
      `/api/lights/${encodeURIComponent(deviceId)}/command?token=${encodeURIComponent(tok())}`,
      { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }
    );
    if (!res.ok) {
      const text = await res.text().catch(() => '');
      showToast(`Command failed (${res.status})${text ? ': ' + text : ''}`, true);
    }
  } catch (e) { showToast(`Command error: ${e.message}`, true); }
}

async function saveScene(name, roomId) {
  try {
    const body = roomId ? { name, room_id: roomId } : { name };
    const res = await fetch(`/api/scenes?token=${encodeURIComponent(tok())}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (!res.ok) showToast(`Save scene failed (${res.status})`, true);
  } catch (e) { showToast(`Save scene error: ${e.message}`, true); }
}

async function recallScene(id) {
  try {
    const res = await fetch(`/api/scenes/${id}/recall?token=${encodeURIComponent(tok())}`, {
      method: 'POST',
    });
    if (!res.ok) {
      if (res.status === 503) showToast('Some devices offline — others recalled', false);
      else showToast(`Recall failed (${res.status})`, true);
    }
  } catch (e) { showToast(`Recall error: ${e.message}`, true); }
}

async function deleteSceneApi(id) {
  try {
    const res = await fetch(`/api/scenes/${id}?token=${encodeURIComponent(tok())}`, {
      method: 'DELETE',
    });
    if (!res.ok && res.status !== 404) showToast(`Delete scene failed (${res.status})`, true);
  } catch (e) { showToast(`Delete scene error: ${e.message}`, true); }
}

// ── Colour math ──────────────────────────────────────────────────────────────

function xyToRgb(x, y, bri = 254) {
  if (y === 0) return { r: 0, g: 0, b: 0 };
  const z = 1.0 - x - y, Y = bri / 254, X = (Y / y) * x, Z = (Y / y) * z;
  let r = X * 1.656492 - Y * 0.354851 - Z * 0.255038;
  let g = -X * 0.707196 + Y * 1.655397 + Z * 0.036152;
  let b = X * 0.051713 - Y * 0.121364 + Z * 1.011530;
  if (r < 0) r = 0; if (g < 0) g = 0; if (b < 0) b = 0;
  const max = Math.max(r, g, b);
  if (max > 1) { r /= max; g /= max; b /= max; }
  const gc = v => v <= 0.0031308 ? 12.92 * v : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
  return { r: Math.round(gc(r) * 255), g: Math.round(gc(g) * 255), b: Math.round(gc(b) * 255) };
}

function rgbToHsl(r, g, b) {
  r /= 255; g /= 255; b /= 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2;
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
  const l = 0.5, a = s * Math.min(l, 1 - l);
  const f = n => { const k = (n + h / 30) % 12; return l - a * Math.max(-1, Math.min(k - 3, Math.min(9 - k, 1))); };
  const gc = v => v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  const r = gc(f(0)), g = gc(f(8)), bv = gc(f(4));
  const X = r * 0.664511 + g * 0.154324 + bv * 0.162028;
  const Y = r * 0.283881 + g * 0.668433 + bv * 0.047685;
  const Z = r * 0.000088 + g * 0.072310 + bv * 0.986039;
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
