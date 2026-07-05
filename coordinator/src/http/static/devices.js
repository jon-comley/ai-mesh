// ── Devices tab ──────────────────────────────────────────────────────────────
// Single inventory for every paired device, grouped by type (pairing is
// bridge-wide — permit-join accepts whatever announces, so "add device" can't
// live on a per-type tab). Reads the same shared `devicesMap`/`model.rooms`
// rooms.js already maintains from LightingUpdate/SensorUpdate/RoomsUpdate —
// this module owns no state of its own, just re-renders on `refresh()`.

import { esc, showToast } from '/static/util.js';
import { api } from '/static/api.js';
import { model, devicesMap } from '/static/state.js';
import { addDeviceToRoom, removeDeviceFromRoom, deleteDevice, patchDeviceName } from '/static/actions.js';
import {
  buildSensorCard, buildRoomSelect, formatLightStatus, applySwitchFlashIfActive,
} from '/static/devicewidgets.js';

function formatDeviceName(id) {
  return id.replace(/[_-]+/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
}

function roomIdForDevice(deviceId) {
  return model.rooms.find(r => r.device_ids.includes(deviceId))?.id ?? null;
}

function onRoomChange(deviceId, newRoomId) {
  const current = roomIdForDevice(deviceId);
  if (current === newRoomId) return;
  if (current) removeDeviceFromRoom(current, deviceId);
  if (newRoomId) addDeviceToRoom(newRoomId, deviceId);
}

function startRename(nameEl, deviceId) {
  const current = model.names.get(deviceId) ?? formatDeviceName(deviceId);
  const input = document.createElement('input');
  input.value = current;
  input.className = 'room-rename-input';
  input.name = 'device-rename';
  input.autocomplete = 'off';
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
      model.names.set(deviceId, name);
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

function confirmDelete(deviceId) {
  if (confirm(`Delete "${formatDeviceName(deviceId)}"? This unpairs it from the Zigbee network.`)) {
    deleteDevice(deviceId);
  }
}

// Cover/Climate/Switch rows: name + room-assignment + delete, no status
// readout — none of these classes has a live-state pipeline yet (see
// rooms.js's notifyOtherDevices), just presence.
function buildPresenceRow(dev) {
  const row = document.createElement('div');
  row.className = 'light-card device-row';
  row.dataset.deviceId = dev.device_id;

  const displayName = formatDeviceName(dev.device_id);
  row.innerHTML = `
    <div class="light-name-group">
      <span class="light-name device-row-name">${esc(displayName)}</span>
    </div>`;

  const nameEl = row.querySelector('.device-row-name');
  nameEl.style.cursor = 'pointer';
  nameEl.title = 'Click to rename';
  nameEl.addEventListener('click', () => startRename(nameEl, dev.device_id));

  const actions = document.createElement('div');
  actions.className = 'device-row-actions';

  const currentRoomId = roomIdForDevice(dev.device_id);
  actions.appendChild(buildRoomSelect(model.rooms, currentRoomId,
    roomId => onRoomChange(dev.device_id, roomId)));

  const deleteBtn = document.createElement('button');
  deleteBtn.className = 'device-row-btn device-row-btn-delete';
  deleteBtn.textContent = '✕';
  deleteBtn.title = 'Delete';
  deleteBtn.addEventListener('click', () => confirmDelete(dev.device_id));
  actions.appendChild(deleteBtn);

  row.appendChild(actions);
  applySwitchFlashIfActive(dev.device_id);
  return row;
}

function buildLightRow(dev) {
  const row = document.createElement('div');
  row.className = 'light-card device-row';
  row.dataset.deviceId = dev.device_id;

  const displayName = formatDeviceName(dev.device_id);
  row.innerHTML = `
    <div class="light-name-group">
      <span class="light-name device-row-name">${esc(displayName)}</span>
      <span class="light-node-badge">${esc(dev.node_id)}</span>
    </div>
    <div class="light-card-header-right">
      <span class="sensor-readout">${esc(formatLightStatus(dev))}</span>
    </div>`;

  const nameEl = row.querySelector('.device-row-name');
  nameEl.style.cursor = 'pointer';
  nameEl.title = 'Click to rename';
  nameEl.addEventListener('click', () => startRename(nameEl, dev.device_id));

  const actions = document.createElement('div');
  actions.className = 'device-row-actions';

  const currentRoomId = roomIdForDevice(dev.device_id);
  actions.appendChild(buildRoomSelect(model.rooms, currentRoomId,
    roomId => onRoomChange(dev.device_id, roomId)));

  const deleteBtn = document.createElement('button');
  deleteBtn.className = 'device-row-btn device-row-btn-delete';
  deleteBtn.textContent = '✕';
  deleteBtn.title = 'Delete';
  deleteBtn.addEventListener('click', () => confirmDelete(dev.device_id));
  actions.appendChild(deleteBtn);

  row.appendChild(actions);
  return row;
}

// Collapse chevron per category — mirrors rooms.js's room-collapse-btn
// pattern (default expanded, remembered per-browser via localStorage).
function buildCategoryHeading(category, count) {
  const heading = document.createElement('h3');
  heading.className = 'device-category-heading';

  const key = `mesh-devcat-collapsed-${category}`;
  const isCollapsed = localStorage.getItem(key) === '1';

  const chevron = document.createElement('button');
  chevron.className = 'device-category-collapse-btn';
  chevron.title = 'Collapse / expand';
  chevron.textContent = isCollapsed ? '▸' : '▾';
  heading.appendChild(chevron);

  const label = document.createElement('span');
  label.textContent = `${category} (${count})`;
  heading.appendChild(label);

  return { heading, chevron, isCollapsed, key };
}

function buildCategorySection(category, count) {
  const { heading, chevron, isCollapsed, key } = buildCategoryHeading(category, count);
  const body = document.createElement('div');
  body.className = 'device-category-body' + (isCollapsed ? ' collapsed' : '');
  const toggle = () => {
    const nowCollapsed = !body.classList.contains('collapsed');
    body.classList.toggle('collapsed', nowCollapsed);
    chevron.textContent = nowCollapsed ? '▸' : '▾';
    localStorage.setItem(key, nowCollapsed ? '1' : '0');
  };
  chevron.addEventListener('click', toggle);
  heading.addEventListener('click', e => { if (e.target !== chevron) toggle(); });
  return { heading, body };
}

function render() {
  const container = document.getElementById('device-list');
  if (!container) return;

  const lights = [...devicesMap.values()].filter(d => d.device_type === 'light');
  const sensors = [...devicesMap.values()].filter(d => d.device_type === 'sensor');
  const blinds = [...devicesMap.values()].filter(d => d.device_type === 'cover');
  const hvac = [...devicesMap.values()].filter(d => d.device_type === 'climate');
  const switches = [...devicesMap.values()].filter(d => d.device_type === 'switch');

  if (lights.length === 0 && sensors.length === 0 && blinds.length === 0
      && hvac.length === 0 && switches.length === 0) {
    container.innerHTML = '<p class="placeholder">No devices paired yet.</p>';
    return;
  }

  container.innerHTML = '';

  if (lights.length > 0) {
    const { heading, body } = buildCategorySection('Lights', lights.length);
    container.appendChild(heading);
    for (const dev of lights.sort((a, b) => a.device_id.localeCompare(b.device_id))) {
      body.appendChild(buildLightRow(dev));
    }
    container.appendChild(body);
  }

  if (sensors.length > 0) {
    const { heading, body } = buildCategorySection('Sensors', sensors.length);
    container.appendChild(heading);
    renderSensorSubcategories(body, sensors, container);
    container.appendChild(body);
  }

  // Blinds/HVAC/Switches: presence-only rows (name + room assignment), no
  // status readout — none of these classes has a live-state pipeline yet.
  for (const [label, list] of [
    ['Blinds', blinds],
    ['HVAC', hvac],
    ['Switches', switches],
  ]) {
    if (list.length === 0) continue;
    const { heading, body } = buildCategorySection(label, list.length);
    container.appendChild(heading);
    for (const dev of list.sort((a, b) => a.device_id.localeCompare(b.device_id))) {
      body.appendChild(buildPresenceRow(dev));
    }
    container.appendChild(body);
  }
}

// Sensors get sub-categories rather than one flat list — grouped by which
// readings a device reports (a device can appear in more than one group,
// e.g. a combined temp/humidity + motion sensor).
const SENSOR_SUBCATEGORIES = [
  { key: 'climate', label: 'Temperature & Humidity', test: d => d.temperature != null || d.humidity != null },
  { key: 'motion', label: 'Motion & Occupancy', test: d => d.occupancy != null },
  { key: 'contact', label: 'Contact', test: d => d.contact != null },
];

function buildSensorRow(dev, container) {
  const currentRoomId = roomIdForDevice(dev.device_id);
  return buildSensorCard(dev, {
    rooms: model.rooms,
    currentRoomId,
    onRoomChange: newRoomId => onRoomChange(dev.device_id, newRoomId),
    onRename: () => {
      const nameEl = container.querySelector(
        `[data-device-id="${CSS.escape(dev.device_id)}"] .light-name`);
      if (nameEl) startRename(nameEl, dev.device_id);
    },
    onDelete: () => confirmDelete(dev.device_id),
  });
}

function renderSensorSubcategories(body, sensors, container) {
  const remaining = new Set(sensors.map(d => d.device_id));
  const byId = new Map(sensors.map(d => [d.device_id, d]));

  for (const sub of SENSOR_SUBCATEGORIES) {
    const members = sensors.filter(d => sub.test(d));
    if (members.length === 0) continue;
    const subHeading = document.createElement('h4');
    subHeading.className = 'device-subcategory-heading';
    subHeading.textContent = `${sub.label} (${members.length})`;
    body.appendChild(subHeading);
    for (const dev of members.sort((a, b) => a.device_id.localeCompare(b.device_id))) {
      body.appendChild(buildSensorRow(dev, container));
      remaining.delete(dev.device_id);
    }
  }

  if (remaining.size > 0) {
    const subHeading = document.createElement('h4');
    subHeading.className = 'device-subcategory-heading';
    subHeading.textContent = `Other (${remaining.size})`;
    body.appendChild(subHeading);
    for (const id of [...remaining].sort()) {
      body.appendChild(buildSensorRow(byId.get(id), container));
    }
  }
}

// Called by dashboard.js after rooms.js processes LightingUpdate/SensorUpdate/
// RoomsUpdate — devicesMap/model.rooms are already up to date by then.
export function refresh() {
  render();
}

// ── Pairing (bridge-wide permit-join + live join feed) ──────────────────────
// Moved here wholesale from lighting.js — pairing belongs on the Devices tab,
// not a lights-only panel, since permit-join accepts any device type.

let pairCountdown = null;

function wirePairButton() {
  const btn = document.getElementById('pair-device-btn');
  if (!btn) return;
  btn.addEventListener('click', async () => {
    if (pairCountdown) return; // window already open
    try {
      const res = await api('/zigbee/permit-join', { method: 'POST' });
      if (!res.ok) {
        const text = await res.text().catch(() => '');
        showToast(`Pairing failed (${res.status})${text ? ': ' + text : ''}`, true);
        return;
      }
      const { seconds } = await res.json();
      startPairCountdown(btn, seconds);
      pairFeedLine('Pairing window open — power-cycle the device or hold its pair button.');
    } catch (e) {
      showToast(`Pairing error: ${e.message}`, true);
    }
  });
}

function startPairCountdown(btn, seconds) {
  let remaining = seconds;
  btn.disabled = true;
  const tick = () => {
    if (remaining <= 0) {
      clearInterval(pairCountdown);
      pairCountdown = null;
      btn.disabled = false;
      btn.textContent = 'Pair device';
      pairFeedLine('Pairing window closed.');
      return;
    }
    btn.textContent = `Pairing… ${remaining}s`;
    remaining -= 1;
  };
  tick();
  pairCountdown = setInterval(tick, 1000);
}

function pairFeedLine(text, tsMs) {
  const feed = document.getElementById('pair-feed');
  if (!feed) return;
  feed.hidden = false;
  const line = document.createElement('div');
  line.className = 'pair-feed-line';
  const ts = new Date(tsMs ?? Date.now()).toLocaleTimeString();
  line.innerHTML = `<span class="pair-feed-ts">${esc(ts)}</span> ${esc(text)}`;
  feed.prepend(line);
  while (feed.children.length > 20) feed.removeChild(feed.lastChild);
}

// Interview success gets an inline "assign to room" picker instead of a plain
// line — the follow-up prompt the plan asked for, right where you're already
// looking (no separate dialog to dismiss). Replaced with a confirmation once
// chosen; still editable later via the Devices tab's own row picker.
function pairFeedRoomPrompt(deviceId, text, tsMs) {
  const feed = document.getElementById('pair-feed');
  if (!feed) return;
  feed.hidden = false;
  const line = document.createElement('div');
  line.className = 'pair-feed-line';
  const ts = new Date(tsMs ?? Date.now()).toLocaleTimeString();

  const lead = document.createElement('span');
  lead.innerHTML = `<span class="pair-feed-ts">${esc(ts)}</span> ${esc(text)} — assign to room: `;
  line.appendChild(lead);

  const select = buildRoomSelect(model.rooms, roomIdForDevice(deviceId), roomId => {
    onRoomChange(deviceId, roomId);
    select.remove();
    const room = model.rooms.find(r => r.id === roomId);
    const confirmSpan = document.createElement('span');
    confirmSpan.textContent = room ? `assigned to ${room.name}` : 'left unassigned';
    line.appendChild(confirmSpan);
  });
  line.appendChild(select);

  feed.prepend(line);
  while (feed.children.length > 20) feed.removeChild(feed.lastChild);
}

// Recent events are replayed on every WS (re)connect (phone screen locking
// mid-pairing drops the socket) — skip anything already rendered.
let lastJoinTs = 0;

export function handleJoinEvent(evt) {
  if (evt.ts_ms <= lastJoinTs) return;
  lastJoinTs = evt.ts_ms;
  const name = formatDeviceName(evt.device_id);
  if (evt.event === 'device_interview_successful') {
    pairFeedRoomPrompt(evt.device_id, `Paired: ${evt.model ?? name} ✓`, evt.ts_ms);
    return;
  }
  const lines = {
    device_joined: `${name} joined — interviewing…`,
    device_interview_started: `Interviewing ${name}…`,
    device_interview_failed: `Interview failed for ${name} — move it closer and retry.`,
    device_announce: `${name} announced itself.`,
    device_leave: `${name} left the network.`,
  };
  pairFeedLine(lines[evt.event] ?? `${name}: ${evt.event}`, evt.ts_ms);
}

wirePairButton();
