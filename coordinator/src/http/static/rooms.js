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

  // Header: name + actions
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

  // Controls: on/off + sliders
  const controls = document.createElement('div');
  controls.className = 'room-controls';

  const onBtn = document.createElement('button');
  onBtn.className = 'light-toggle-btn';
  onBtn.innerHTML = '<span class="badge badge-green">On</span>';
  onBtn.addEventListener('click', () => sendRoomCommand(room.id, { action: 'on' }));

  const offBtn = document.createElement('button');
  offBtn.className = 'light-toggle-btn';
  offBtn.innerHTML = '<span class="badge badge-muted">Off</span>';
  offBtn.addEventListener('click', () => sendRoomCommand(room.id, { action: 'off' }));

  controls.appendChild(onBtn);
  controls.appendChild(offBtn);

  const roomDevices = room.device_ids.map(id => devicesMap.get(id)).filter(Boolean);
  if (roomDevices.some(d => d.brightness != null)) {
    controls.appendChild(buildSliderRow('Brightness', 0, 255, 127,
      val => sendRoomCommand(room.id, { action: 'brightness', value: val }),
      val => `${Math.round((val / 255) * 100)}%`
    ));
  }
  if (roomDevices.some(d => d.color_temp != null)) {
    controls.appendChild(buildSliderRow('Color temp', 154, 500, 300,
      val => sendRoomCommand(room.id, { action: 'color_temp', value: val }),
      val => `${Math.round(1_000_000 / val)} K`
    ));
  }

  card.appendChild(controls);

  // Device chips
  const chips = document.createElement('div');
  chips.className = 'room-chips';
  if (room.device_ids.length === 0) {
    const hint = document.createElement('span');
    hint.className = 'room-drop-hint';
    hint.textContent = 'Drop bulbs here';
    chips.appendChild(hint);
  } else {
    for (const deviceId of room.device_ids) {
      chips.appendChild(renderChip(deviceId, room.id, true));
    }
  }
  card.appendChild(chips);

  wireDropZone(card, room.id);
  return card;
}

function buildSliderRow(label, min, max, defaultVal, onChange, formatFn) {
  const row = document.createElement('div');
  row.className = 'light-detail-row room-slider-row';

  const labelEl = document.createElement('span');
  labelEl.className = 'light-detail-label';
  labelEl.textContent = label;

  const slider = document.createElement('input');
  slider.type = 'range';
  slider.className = 'light-slider';
  slider.min = min;
  slider.max = max;
  slider.value = defaultVal;

  const valueEl = document.createElement('span');
  valueEl.className = 'light-detail-value';
  valueEl.textContent = formatFn(defaultVal);

  slider.addEventListener('input', () => { valueEl.textContent = formatFn(parseInt(slider.value, 10)); });
  slider.addEventListener('change', () => { onChange(parseInt(slider.value, 10)); });

  row.appendChild(labelEl);
  row.appendChild(slider);
  row.appendChild(valueEl);
  return row;
}

// ── Device chip ──────────────────────────────────────────────────────────────

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
      // server's add_device_to_room evicts from previous room atomically
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

async function sendRoomCommand(roomId, body) {
  try {
    const res = await fetch(`/api/rooms/${roomId}/command?token=${encodeURIComponent(tok())}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (!res.ok && res.status !== 503) showToast(`Room command failed (${res.status})`, true);
  } catch (e) { showToast(`Room command error: ${e.message}`, true); }
}

// ── Utilities ────────────────────────────────────────────────────────────────

function formatDeviceName(id) {
  return id.replace(/[_-]+/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
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
