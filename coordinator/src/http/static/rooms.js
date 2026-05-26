// ── Rooms panel ──────────────────────────────────────────────────────────────
// First-class spatial room objects with drag-and-drop device assignment
// and drag-to-reorder room cards.

import * as layout from '/static/layout.js';

let roomsData = [];
let devicesMap = new Map();
let scenesData = [];
let deviceNamesMap = new Map();
let dragSrc = null;       // chip drag: { deviceId, fromRoomId }
let roomDragId = null;    // room reorder drag: room id being dragged
let effectDragSrc = null; // effect palette drag: effect name e.g. 'solar'
const openPickerIds = new Set();      // device IDs whose colour picker is currently open
const activeSceneByRoom = new Map();   // roomId → sceneId of last-recalled scene
const preSceneStateByRoom = new Map(); // roomId → Map<deviceId, snapshot> before last recall
let activeSceneEdit = null;           // { roomId, value } when a scene name input is open
let _sceneReorderTimer = null;

function updateSceneChipStates(roomId) {
  const card = document.querySelector(`[data-room-id="${CSS.escape(roomId)}"]`);
  if (!card) return;
  const activeId = activeSceneByRoom.get(roomId);
  card.querySelectorAll('.room-quick-scene-chip[data-scene-id]').forEach(chip => {
    chip.classList.toggle('active', chip.dataset.sceneId === activeId);
  });
}

function clearRoomActiveScene(roomId) {
  if (!activeSceneByRoom.has(roomId)) return;
  activeSceneByRoom.delete(roomId);
  updateSceneChipStates(roomId);
}

function cancelSceneEdit() {
  if (!activeSceneEdit) return;
  const card = document.querySelector(`[data-room-id="${CSS.escape(activeSceneEdit.roomId)}"]`);
  card?.querySelector('.room-scene-name-input')?.style.setProperty('display', 'none');
  const sb = card?.querySelector('.room-scene-save-btn');
  if (sb) sb.style.display = '';
  activeSceneEdit = null;
}

// Close pickers and scene input on Escape or click-outside
document.addEventListener('keydown', e => {
  if (e.key !== 'Escape') return;
  openPickerIds.clear();
  document.querySelectorAll('.light-colour-picker.open').forEach(el => el.classList.remove('open'));
  cancelSceneEdit();
});
document.addEventListener('click', e => {
  if (!e.target.closest('.light-colour-picker')) {
    openPickerIds.clear();
    document.querySelectorAll('.light-colour-picker.open').forEach(el => el.classList.remove('open'));
  }
  if (activeSceneEdit && !e.target.closest('.room-scene-save-row')) {
    cancelSceneEdit();
  }
});

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
fetchDeviceNames();

export function handleRoomsUpdate(evt) {
  roomsData = evt.rooms ?? [];
  if (evt.device_names) notifyDeviceNames(evt.device_names);
  // Forward orientation to layout canvas if a room is open (layout guards on dial presence)
  if (evt.rooms && layout.currentLayoutRoomId()) {
    const r = evt.rooms.find(r => r.id === layout.currentLayoutRoomId());
    if (r != null) layout.notifyOrientationUpdate(r.orientation_degrees);
  }
  render();
}

export function notifyDeviceNames(names) {
  deviceNamesMap = new Map(Object.entries(names));
}

async function fetchDeviceNames() {
  try {
    const res = await fetch(`/api/lights/names?token=${encodeURIComponent(tok())}`);
    if (res.ok) notifyDeviceNames(await res.json());
  } catch (_) {}
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
  if (roomsData.length > 0) container.appendChild(renderGlobalControls());
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

  // Prune stale picker IDs (device removed or moved between rooms since last render)
  for (const deviceId of openPickerIds) {
    if (!devicesMap.has(deviceId)) openPickerIds.delete(deviceId);
  }

  // Restore open colour pickers after re-render
  for (const deviceId of openPickerIds) {
    document.querySelector(`[data-device-id="${CSS.escape(deviceId)}"] [data-ctrl="colour-picker"]`)
      ?.classList.add('open');
  }

  // Restore active scene name input after re-render
  if (activeSceneEdit) {
    const card = document.querySelector(`[data-room-id="${CSS.escape(activeSceneEdit.roomId)}"]`);
    const ni = card?.querySelector('.room-scene-name-input');
    const sb = card?.querySelector('.room-scene-save-btn');
    if (ni && sb) {
      sb.style.display = 'none';
      ni.style.display = '';
      ni.value = activeSceneEdit.value;
      ni.focus();
    }
  }
}

// ── Global controls ──────────────────────────────────────────────────────────

function renderGlobalControls() {
  const bar = document.createElement('div');
  bar.className = 'room-global-controls';

  const allOnBtn = document.createElement('button');
  allOnBtn.className = 'room-action-btn';
  allOnBtn.textContent = 'All On';
  allOnBtn.addEventListener('click', () => {
    for (const r of roomsData) sendRoomCommand(r.id, { action: 'on' }, r);
  });

  const allOffBtn = document.createElement('button');
  allOffBtn.className = 'room-action-btn room-action-delete';
  allOffBtn.textContent = 'All Off';
  allOffBtn.addEventListener('click', () => {
    for (const r of roomsData) sendRoomCommand(r.id, { action: 'off' }, r);
  });

  bar.appendChild(allOnBtn);
  bar.appendChild(allOffBtn);
  return bar;
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
  strip.id = 'unassigned-strip';

  const label = document.createElement('div');
  label.className = 'room-unassigned-label';
  label.textContent = 'Unassigned';
  strip.appendChild(label);

  const chips = document.createElement('div');
  chips.className = 'room-chips';

  if (devicesMap.size === 0) {
    chips.innerHTML = '<span class="room-empty-hint">No lighting devices discovered yet.</span>';
  } else if (deviceIds.length === 0) {
    chips.innerHTML = '<span class="room-unassigned-drop-hint">Drop here to unassign.</span>';
  } else {
    for (const id of deviceIds) chips.appendChild(renderChip(id, 'unassigned', false));
  }

  strip.appendChild(chips);
  wireDropZone(strip, 'unassigned');

  // Hide when all devices are assigned; shown as a drop target during device drags
  if (devicesMap.size > 0 && deviceIds.length === 0) strip.style.display = 'none';

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

  // Shared state for header
  const roomDevicesAll = room.device_ids.map(id => devicesMap.get(id)).filter(Boolean);
  const anyOn = roomDevicesAll.some(d => d.on);
  const hasColour = roomDevicesAll.some(d => d.color_xy != null);
  const solarActive = room.solar_enabled;
  const empty = room.device_ids.length === 0;

  // Header: collapse chevron + name + quick controls + layout button + actions
  const header = document.createElement('div');
  header.className = 'room-card-header';

  const collapseBtn = document.createElement('button');
  collapseBtn.className = 'room-collapse-btn';
  collapseBtn.title = 'Collapse / expand';
  const isCollapsed = localStorage.getItem(`mesh-room-collapsed-${room.id}`) === '1';
  collapseBtn.textContent = isCollapsed ? '▸' : '▾';
  header.appendChild(collapseBtn);

  const nameWrap = document.createElement('span');
  nameWrap.className = 'room-name-wrap';
  const nameEl = document.createElement('span');
  nameEl.className = 'room-name';
  nameEl.textContent = room.name;
  nameWrap.appendChild(nameEl);
  const pencilBtn = document.createElement('button');
  pencilBtn.className = 'room-rename-pencil';
  pencilBtn.title = 'Rename room';
  pencilBtn.textContent = '✎';
  pencilBtn.addEventListener('click', e => { e.stopPropagation(); startRename(nameEl, room); });
  nameWrap.appendChild(pencilBtn);
  nameWrap.addEventListener('click', () => startRename(nameEl, room));
  header.appendChild(nameWrap);

  // Quick on/off + colour swatch in header (always visible when collapsed)
  const quickCtrl = document.createElement('div');
  quickCtrl.className = 'room-header-quick';

  const onBtn  = document.createElement('button');
  const offBtn = document.createElement('button');
  const setRoomOnOff = (isOn) => {
    onBtn.innerHTML  = `<span class="badge ${isOn  ? 'badge-green' : 'badge-muted'}">On</span>`;
    offBtn.innerHTML = `<span class="badge ${!isOn ? 'badge-red'   : 'badge-muted'}">Off</span>`;
  };
  onBtn.className = 'light-toggle-btn';
  onBtn.disabled = empty;
  if (!empty) onBtn.addEventListener('click', e => { e.stopPropagation(); setRoomOnOff(true);  sendRoomCommand(room.id, { action: 'on' }, room); });
  offBtn.className = 'light-toggle-btn';
  offBtn.disabled = empty;
  if (!empty) offBtn.addEventListener('click', e => { e.stopPropagation(); setRoomOnOff(false); sendRoomCommand(room.id, { action: 'off' }, room); });
  setRoomOnOff(anyOn);
  quickCtrl.appendChild(onBtn);
  quickCtrl.appendChild(offBtn);

  let roomSwatchBtn = null;
  if (hasColour) {
    const { h, s } = getRoomColourHsl(roomDevicesAll);
    roomSwatchBtn = document.createElement('button');
    roomSwatchBtn.className = 'color-swatch-btn room-colour-swatch';
    roomSwatchBtn.style.background = `hsl(${h},${s}%,50%)`;
    roomSwatchBtn.title = 'Set room colour';
    roomSwatchBtn.setAttribute('data-ctrl', 'room-colour-toggle');
    quickCtrl.appendChild(roomSwatchBtn);
  }
  header.appendChild(quickCtrl);

  const layoutBtn = document.createElement('button');
  layoutBtn.className = 'room-action-btn room-layout-btn';
  layoutBtn.title = 'Open floor plan';
  layoutBtn.textContent = '⊞';
  layoutBtn.addEventListener('click', e => { e.stopPropagation(); layout.openLayout(room); });
  header.appendChild(layoutBtn);

  const actions = document.createElement('div');
  actions.className = 'room-actions';
  if (solarActive) {
    const solarBadge = document.createElement('span');
    solarBadge.className = 'badge badge-solar';
    solarBadge.title = 'Solar mode active — click to disable';
    solarBadge.innerHTML = '&#9728; Solar';
    solarBadge.style.cursor = 'pointer';
    solarBadge.addEventListener('click', () => setSolarMode(room.id, false));
    actions.appendChild(solarBadge);
  }
  const deleteBtn = document.createElement('button');
  deleteBtn.className = 'room-action-btn room-action-delete';
  deleteBtn.textContent = 'delete';
  deleteBtn.addEventListener('click', () => deleteRoom(room.id));
  actions.appendChild(deleteBtn);
  header.appendChild(actions);
  card.appendChild(header);

  // Room colour picker — outside body so it stays accessible when collapsed
  if (hasColour && roomSwatchBtn) {
    const { h, s } = getRoomColourHsl(roomDevicesAll);
    const pickerEl = buildRoomColourPicker(h, s);
    card.appendChild(pickerEl);
    roomSwatchBtn.addEventListener('click', e => { e.stopPropagation(); pickerEl.classList.toggle('open'); });
    wireRoomColourPicker(pickerEl, room.id, roomSwatchBtn);
  }

  // Quick scenes bar — always visible, horizontal scroll
  const roomScenesList = scenesData.filter(s => s.room_id === room.id).sort((a, b) => (a.position - b.position) || (b.created_at - a.created_at));
  if (roomScenesList.length > 0) {
    const sceneBar = document.createElement('div');
    sceneBar.className = 'room-quick-scenes';
    for (const scene of roomScenesList) {
      const chip = document.createElement('button');
      chip.className = 'room-quick-scene-chip';
      chip.dataset.sceneId = scene.id;
      chip.setAttribute('draggable', 'true');
      chip.textContent = scene.name;
      chip.title = `Recall "${scene.name}"`;
      if (scene.preview_color) {
        const { r, g, b } = xyToRgb(scene.preview_color[0], scene.preview_color[1], 180);
        chip.style.setProperty('--scene-chip-color', `rgb(${r},${g},${b})`);
      }
      if (activeSceneByRoom.get(room.id) === scene.id) chip.classList.add('active');
      chip.addEventListener('click', e => { e.stopPropagation(); recallScene(scene.id); });
      sceneBar.appendChild(chip);
    }
    wireSceneBarDrag(sceneBar, room.id);
    card.appendChild(sceneBar);
  }

  // Collapsible body
  const body = document.createElement('div');
  body.className = 'room-body' + (isCollapsed ? ' collapsed' : '');
  collapseBtn.addEventListener('click', e => {
    e.stopPropagation();
    const nowCollapsed = !body.classList.contains('collapsed');
    body.classList.toggle('collapsed', nowCollapsed);
    collapseBtn.textContent = nowCollapsed ? '▸' : '▾';
    localStorage.setItem(`mesh-room-collapsed-${room.id}`, nowCollapsed ? '1' : '0');
  });

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
        const devCard = buildDeviceCard(dev, room.id, room.solar_enabled);
        devicesEl.appendChild(devCard);
        wireDeviceControls(devCard, dev, room.id);
        wireDeviceDrag(devCard, dev.device_id, room.id);
      } else {
        devicesEl.appendChild(buildDevicePlaceholder(deviceId, room.id));
      }
    }
  }
  body.appendChild(devicesEl);
  wireDropZone(body, room.id);

  // Scenes section (full list with save button)
  body.appendChild(buildScenesSection(room.id));

  card.appendChild(body);

  // Effect drops must land on the whole card so they work when the body is collapsed.
  // Drop bubbles up from body's wireDropZone (which clears effectDragSrc first), so
  // by the time it reaches here effectDragSrc is already null — no double-fire.
  card.addEventListener('dragover', e => {
    if (!effectDragSrc) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
    card.classList.add('room-drop-active');
  });
  card.addEventListener('dragleave', e => {
    if (!card.contains(e.relatedTarget)) card.classList.remove('room-drop-active');
  });
  card.addEventListener('drop', e => {
    if (!effectDragSrc) return;
    e.preventDefault();
    card.classList.remove('room-drop-active');
    const effect = effectDragSrc;
    effectDragSrc = null;
    if (effect === 'solar') setSolarMode(room.id, true);
  });

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

  saveBtn.addEventListener('click', e => {
    e.stopPropagation();
    activeSceneEdit = { roomId, value: '' };
    saveBtn.style.display = 'none';
    nameInput.style.display = '';
    nameInput.value = '';
    nameInput.focus();
  });

  nameInput.addEventListener('input', () => {
    if (activeSceneEdit) activeSceneEdit.value = nameInput.value;
  });

  let savingScene = false;
  const doSave = () => {
    if (savingScene) return;
    const name = nameInput.value.trim();
    nameInput.style.display = 'none';
    saveBtn.style.display = '';
    activeSceneEdit = null;
    if (!name) return;
    savingScene = true;
    saveScene(name, roomId).finally(() => { savingScene = false; });
  };
  nameInput.addEventListener('keydown', e => {
    if (e.key === 'Enter') { e.stopPropagation(); doSave(); }
    if (e.key === 'Escape') { e.stopPropagation(); cancelSceneEdit(); }
  });

  // Scene list
  const roomScenes = scenesData
    .filter(s => s.room_id === roomId)
    .sort((a, b) => (a.position - b.position) || (b.created_at - a.created_at));

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

function buildDeviceCard(dev, roomId, roomSolarEnabled = false) {
  const card = document.createElement('div');
  card.className = 'light-card room-device-card';
  card.dataset.deviceId = dev.device_id;
  card.innerHTML = deviceCardHtml(dev, roomSolarEnabled);
  return card;
}

function deviceCardHtml(dev, roomSolarEnabled = false) {
  const badgeClass = dev.on ? 'badge-green' : 'badge-muted';
  const badgeLabel = dev.on ? 'On' : 'Off';
  const displayName = formatDeviceName(dev.device_id);

  let swatch = '';
  let colourPicker = '';
  if (dev.color_xy != null || dev.color_temp != null) {
    let h = 30, s = 80;
    let swatchRgb = `hsl(${h},${s}%,50%)`;
    if (dev.solar_enabled) {
      // Circadian: warm amber ~2700 K
      swatchRgb = 'rgb(255, 195, 120)';
      h = 30; s = 100;
    } else if (dev.color_xy != null) {
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
        <span class="light-name" title="Click to rename" style="cursor:pointer">${esc(displayName)}</span>
        <span class="light-node-badge">${esc(dev.node_id)}</span>
      </div>
      <div class="light-card-header-right">
        ${swatch}
        ${roomSolarEnabled ? `<button class="solar-dot ${dev.solar_enabled ? 'solar-dot-active' : 'solar-dot-dim'}"
          data-ctrl="restore-solar"
          title="${dev.solar_enabled ? 'Solar active' : 'Solar overridden — click to restore'}"
          aria-label="${dev.solar_enabled ? 'Solar active' : 'Restore solar for this device'}">&#9728;</button>` : ''}
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
    // Reveal the unassigned strip so there's a visible drop target
    const strip = document.getElementById('unassigned-strip');
    if (strip) strip.style.display = '';
  });
  card.addEventListener('dragend', () => {
    card.classList.remove('dragging');
    dragSrc = null;
    card.setAttribute('draggable', 'true');
    card.closest('.room-card')?.setAttribute('draggable', 'true');
    stopPulse();
    // Re-hide the unassigned strip if nothing was dropped into it
    const strip = document.getElementById('unassigned-strip');
    if (strip && !strip.querySelector('.room-chip')) strip.style.display = 'none';
  });
  // Allow effect chips (e.g. solar) to be dropped onto device cards; the drop
  // event bubbles up to the room body's wireDropZone handler which applies the effect.
  card.addEventListener('dragover', e => {
    if (!effectDragSrc) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
  });
}

function wireDeviceControls(card, dev, roomId) {
  card.querySelector('[data-ctrl="restore-solar"]')?.addEventListener('click', async e => {
    e.stopPropagation();
    const cur = devicesMap.get(dev.device_id);
    if (!cur || cur.solar_enabled) return; // already active, dot is just an indicator
    devicesMap.set(dev.device_id, { ...cur, solar_enabled: true });
    render();
    try {
      const res = await fetch(
        `/api/lights/${encodeURIComponent(dev.device_id)}/restore-solar?token=${encodeURIComponent(tok())}`,
        { method: 'POST' }
      );
      if (!res.ok) throw new Error(`${res.status}`);
    } catch (err) {
      // Revert optimistic update on failure
      devicesMap.set(dev.device_id, { ...cur, solar_enabled: false });
      render();
      showToast(`Restore solar error: ${err.message}`, true);
    }
  });

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

  // Device rename on name click
  const nameEl = card.querySelector('.light-name');
  if (nameEl) nameEl.addEventListener('click', e => { e.stopPropagation(); startDeviceRename(nameEl, dev.device_id); });

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
      sendDeviceCommand(dev.device_id, { action: 'brightness', value: val, transition_secs: 0.4 });
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
      sendDeviceCommand(dev.device_id, { action: 'color_temp', value: val, transition_secs: 0.4 });
    });
  }

  const colourToggle = card.querySelector('[data-ctrl="colour-toggle"]');
  const colourPicker = card.querySelector('[data-ctrl="colour-picker"]');
  const hue = card.querySelector('[data-ctrl="hue"]');
  const sat = card.querySelector('[data-ctrl="saturation"]');
  const preview = colourPicker?.querySelector('.colour-swatch-preview');

  colourToggle?.addEventListener('click', e => {
    e.stopPropagation();
    const isOpen = colourPicker.classList.toggle('open');
    if (isOpen) openPickerIds.add(dev.device_id);
    else openPickerIds.delete(dev.device_id);
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

// ── Scene bar drag-to-reorder ─────────────────────────────────────────────────

let sceneDragId = null; // sceneId being dragged within the bar

function wireSceneBarDrag(bar, roomId) {
  bar.addEventListener('dragstart', e => {
    const chip = e.target.closest('.room-quick-scene-chip[data-scene-id]');
    if (!chip) return;
    sceneDragId = chip.dataset.sceneId;
    chip.classList.add('dragging');
    e.dataTransfer.effectAllowed = 'move';
    e.stopPropagation();
  });

  bar.addEventListener('dragend', e => {
    const chip = e.target.closest('.room-quick-scene-chip');
    chip?.classList.remove('dragging');
    if (sceneDragId) {
      const ids = [...bar.querySelectorAll('.room-quick-scene-chip[data-scene-id]')]
        .map(c => c.dataset.sceneId);
      clearTimeout(_sceneReorderTimer);
      _sceneReorderTimer = setTimeout(() => reorderScenes(ids), 80);
    }
    sceneDragId = null;
  });

  bar.addEventListener('dragover', e => {
    if (!sceneDragId) return;
    e.preventDefault();
    e.stopPropagation();
    const dragging = bar.querySelector('.room-quick-scene-chip.dragging');
    if (!dragging) return;
    const others = [...bar.querySelectorAll('.room-quick-scene-chip:not(.dragging)')];
    const after = others.reduce((closest, child) => {
      const box = child.getBoundingClientRect();
      const offset = e.clientX - box.left - box.width / 2;
      if (offset < 0 && offset > closest.offset) return { offset, element: child };
      return closest;
    }, { offset: Number.NEGATIVE_INFINITY }).element;
    if (after == null) bar.appendChild(dragging);
    else bar.insertBefore(dragging, after);
  });
}

async function reorderScenes(ids) {
  try {
    const res = await fetch(`/api/scenes/reorder?token=${encodeURIComponent(tok())}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ids }),
    });
    if (!res.ok) showToast(`Scene reorder failed (${res.status})`, true);
  } catch (e) { showToast(`Scene reorder error: ${e.message}`, true); }
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
  // Optimistic update
  const idx = roomsData.indexOf(room);
  if (idx !== -1) roomsData[idx] = { ...room, solar_enabled: enable };
  for (const deviceId of room.device_ids) {
    const dev = devicesMap.get(deviceId);
    if (dev) devicesMap.set(deviceId, { ...dev, solar_enabled: enable });
  }
  render();
  try {
    const res = await fetch(`/api/rooms/${roomId}/solar?token=${encodeURIComponent(tok())}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled: enable }),
    });
    if (!res.ok) showToast(`Solar mode failed (${res.status})`, true);
  } catch (e) { showToast(`Solar mode error: ${e.message}`, true); }
}

async function sendRoomCommand(roomId, body, room) {
  clearRoomActiveScene(roomId);
  // Optimistic update
  if (room) {
    for (const deviceId of room.device_ids) {
      const dev = devicesMap.get(deviceId);
      if (dev) {
        let updated = dev;
        if (body.action === 'on') updated = { ...updated, on: true };
        else if (body.action === 'off') updated = { ...updated, on: false };
        // Manual room command suspends solar for each device if room has solar
        if (room.solar_enabled && updated.solar_enabled) updated = { ...updated, solar_enabled: false };
        devicesMap.set(deviceId, updated);
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
  const owningRoom = roomsData.find(r => r.device_ids.includes(deviceId));
  if (owningRoom) clearRoomActiveScene(owningRoom.id);
  // Manual command suspends solar for this device if the room has solar active
  if (owningRoom?.solar_enabled) {
    const dev = devicesMap.get(deviceId);
    if (dev?.solar_enabled) { devicesMap.set(deviceId, { ...dev, solar_enabled: false }); render(); }
  }
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
  const scene = scenesData.find(s => s.id === id);
  const roomId = scene?.room_id;

  // Toggle: clicking the active scene reverts to pre-scene state
  if (roomId && activeSceneByRoom.get(roomId) === id) {
    const preState = preSceneStateByRoom.get(roomId);
    activeSceneByRoom.delete(roomId);
    preSceneStateByRoom.delete(roomId);
    updateSceneChipStates(roomId);
    if (preState) {
      const room = roomsData.find(r => r.id === roomId);
      for (const deviceId of (room?.device_ids ?? [])) {
        const snap = preState.get(deviceId);
        if (!snap) continue;
        if (!snap.on) { sendDeviceCommand(deviceId, { action: 'off' }); continue; }
        // brightness transition implicitly turns the bulb on — no separate 'on' needed
        if (snap.brightness != null)
          sendDeviceCommand(deviceId, { action: 'brightness', value: snap.brightness, transition_secs: 0.8 });
        else
          sendDeviceCommand(deviceId, { action: 'on' });
        if (snap.color_xy != null)
          sendDeviceCommand(deviceId, { action: 'color_xy', x: snap.color_xy[0], y: snap.color_xy[1], transition_secs: 0.8 });
        else if (snap.color_temp != null)
          sendDeviceCommand(deviceId, { action: 'color_temp', value: snap.color_temp, transition_secs: 0.8 });
      }
    }
    return;
  }

  // Snapshot current device state before recalling
  if (roomId) {
    const room = roomsData.find(r => r.id === roomId);
    const snap = new Map();
    for (const deviceId of (room?.device_ids ?? [])) {
      const dev = devicesMap.get(deviceId);
      if (dev) snap.set(deviceId, { on: dev.on, brightness: dev.brightness ?? null, color_xy: dev.color_xy ?? null, color_temp: dev.color_temp ?? null });
    }
    preSceneStateByRoom.set(roomId, snap);
  }

  try {
    const res = await fetch(`/api/scenes/${id}/recall?token=${encodeURIComponent(tok())}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ transition_secs: 1.0 }),
    });
    if (res.ok || res.status === 503) {
      if (roomId) {
        activeSceneByRoom.set(roomId, id);
        updateSceneChipStates(roomId);
      }
      if (res.status === 503) showToast('Some devices offline — others recalled', false);
    } else {
      preSceneStateByRoom.delete(roomId);
      showToast(`Recall failed (${res.status})`, true);
    }
  } catch (e) {
    preSceneStateByRoom.delete(roomId);
    showToast(`Recall error: ${e.message}`, true);
  }
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
  return deviceNamesMap.get(id) ?? id.replace(/[_-]+/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
}

function startDeviceRename(nameEl, deviceId) {
  const current = deviceNamesMap.get(deviceId) ?? formatDeviceName(deviceId);
  const input = document.createElement('input');
  input.value = current;
  input.className = 'room-rename-input';
  nameEl.replaceWith(input);
  input.focus();
  input.select();

  let saved = false;
  const save = () => {
    if (saved) return;
    saved = true;
    const name = input.value.trim();
    input.replaceWith(nameEl);
    if (name && name !== current) {
      deviceNamesMap.set(deviceId, name);
      nameEl.textContent = name;
      patchDeviceName(deviceId, name);
    }
  };

  input.addEventListener('keydown', e => {
    if (e.key === 'Enter') save();
    if (e.key === 'Escape') { saved = true; input.replaceWith(nameEl); }
  });
  input.addEventListener('blur', save);
}

async function patchDeviceName(deviceId, name) {
  try {
    await fetch(`/api/lights/${encodeURIComponent(deviceId)}/name?token=${encodeURIComponent(tok())}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }),
    });
  } catch (e) { showToast(`Rename error: ${e.message}`, true); }
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
