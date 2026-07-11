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
  appendEditLink,
} from '/static/devicewidgets.js';
import { buildBindingsPanel } from '/static/switchbindings.js';

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

// ── Speakers & displays (AV endpoints, non-Zigbee) ───────────────────────────
// Fed by GET /api/av-devices: each backend of each audio node is its own
// row (pi2 can be both the HDMI chain and a Bluetooth speaker host), plus
// the soundbar/TV appliances and the voice puck. Room assignment writes
// the `room-audio-sink:<room name>` preference the voice pipeline resolves
// — a different mechanism from the Zigbee devices table, same UI gesture.

let avDevices = [];
let avLastFetch = 0;
const AV_FETCH_MIN_INTERVAL_MS = 15_000;

// Other modules (rooms.js, for its room-card Bluetooth badge) can't just
// call getAvDevices() once — they need to know when it's worth re-reading
// it. Registering here avoids a circular import (rooms.js already imports
// from this module); dashboard.js wires the subscription once at startup.
const avDevicesChangeListeners = [];

export function onAvDevicesChanged(callback) {
  avDevicesChangeListeners.push(callback);
}

function fetchAvDevices(force = false) {
  const now = Date.now();
  if (!force && now - avLastFetch < AV_FETCH_MIN_INTERVAL_MS) return;
  avLastFetch = now;
  api('/av-devices').then(async res => {
    if (!res.ok) return;
    const json = await res.json();
    const changed = JSON.stringify(json.devices) !== JSON.stringify(avDevices);
    avDevices = json.devices ?? [];
    if (changed) {
      render();
      for (const listener of avDevicesChangeListeners) listener();
    }
  }).catch(() => {});
}

// A sink can belong to more than one room, and (separately) a room can
// hold more than one sink — GET /api/av-devices' dev.rooms is a list of
// { room, role } objects, "role" being which purpose that assignment
// serves there ("any" | "reply" | "media", see coordinator's SinkRole).
// Only the puck stays single-room (roomIdForAvDevice/onPuckRoomChange
// below), since a physical puck really is in exactly one place.

function roomIdForAvDevice(dev) {
  const name = dev.rooms?.[0]?.room;
  return name ? (model.rooms.find(r => r.name === name)?.id ?? null) : null;
}

function assignAvRoom(dev, roomName, role) {
  return api(`/av-devices/${encodeURIComponent(dev.id)}/rooms/${encodeURIComponent(roomName)}`,
    { method: 'PUT', body: { role } });
}

function unassignAvRoom(dev, roomName) {
  return api(`/av-devices/${encodeURIComponent(dev.id)}/rooms/${encodeURIComponent(roomName)}`,
    { method: 'DELETE' });
}

const AV_ROLE_LABELS = { any: 'any', reply: 'replies', media: 'media' };

// A removable chip per current assignment, plus a room+role picker to add
// another — re-picking a room the sink is already in just updates its
// role (the backend upserts by node+sink, doesn't duplicate).
function buildAvRoomAssignments(dev) {
  const wrap = document.createElement('div');
  wrap.className = 'av-room-assignments';

  const chips = document.createElement('div');
  chips.className = 'av-room-chips';
  for (const assignment of dev.rooms ?? []) {
    const chip = document.createElement('span');
    chip.className = 'av-room-chip';
    chip.textContent = assignment.role === 'any'
      ? assignment.room
      : `${assignment.room} · ${AV_ROLE_LABELS[assignment.role] ?? assignment.role}`;
    const remove = document.createElement('button');
    remove.className = 'av-room-chip-remove';
    remove.textContent = '✕';
    remove.title = `Remove from ${assignment.room}`;
    remove.addEventListener('click', () => {
      unassignAvRoom(dev, assignment.room).then(() => fetchAvDevices(true));
    });
    chip.appendChild(remove);
    chips.appendChild(chip);
  }
  wrap.appendChild(chips);

  const roomSelect = document.createElement('select');
  roomSelect.className = 'device-room-select';
  const placeholder = document.createElement('option');
  placeholder.value = '';
  placeholder.textContent = '+ add to room…';
  roomSelect.appendChild(placeholder);
  for (const room of model.rooms) {
    const o = document.createElement('option');
    o.value = room.id;
    o.textContent = room.name;
    roomSelect.appendChild(o);
  }

  const roleSelect = document.createElement('select');
  roleSelect.className = 'device-room-select av-role-select';
  roleSelect.hidden = true;
  for (const [value, label] of [['any', 'Any'], ['reply', 'Replies only'], ['media', 'Media only']]) {
    const o = document.createElement('option');
    o.value = value;
    o.textContent = label;
    roleSelect.appendChild(o);
  }

  const addBtn = document.createElement('button');
  addBtn.className = 'device-row-btn';
  addBtn.textContent = '+';
  addBtn.title = 'Assign to this room';
  addBtn.hidden = true;

  roomSelect.addEventListener('change', () => {
    const has = !!roomSelect.value;
    roleSelect.hidden = !has;
    addBtn.hidden = !has;
  });

  addBtn.addEventListener('click', () => {
    const room = model.rooms.find(r => r.id === roomSelect.value);
    if (!room) return;
    assignAvRoom(dev, room.name, roleSelect.value).then(() => fetchAvDevices(true));
  });

  wrap.append(roomSelect, roleSelect, addBtn);
  return wrap;
}

// The puck's room binding is inverted from a sink's: `av-room:puck` names
// the room the puck sits in (the voice pipeline reads it to decide when a
// reply should divert to that room's speaker), rather than a per-room
// pref pointing at a device.
function onPuckRoomChange(newRoomId) {
  const newRoom = model.rooms.find(r => r.id === newRoomId);
  const write = newRoom
    ? api(`/preferences/${encodeURIComponent('av-room:puck')}`,
        { method: 'PUT', body: { value: newRoom.name } })
    : api(`/preferences/${encodeURIComponent('av-room:puck')}`, { method: 'DELETE' });
  write.then(() => fetchAvDevices(true)).catch(() => {});
}

function startAvRename(nameEl, dev) {
  const input = document.createElement('input');
  input.value = dev.name;
  input.className = 'room-rename-input';
  input.name = 'av-rename';
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
    if (name && name !== dev.name) {
      dev.name = name;
      nameEl.textContent = name;
      api(`/preferences/${encodeURIComponent(`av-name:${dev.id}`)}`,
        { method: 'PUT', body: { value: name } });
    }
  };
  input.addEventListener('keydown', e => {
    if (e.key === 'Enter') save();
    if (e.key === 'Escape') { saved = true; input.replaceWith(nameEl); }
  });
  input.addEventListener('blur', save);
}

function buildAvRow(dev) {
  const row = document.createElement('div');
  row.className = 'light-card device-row';
  row.dataset.avId = dev.id;

  const offline = dev.online === false;
  row.innerHTML = `
    <div class="light-name-group">
      <span class="light-name device-row-name">${esc(dev.name)}</span>
      <span class="av-badge">${esc(dev.transport)}</span>
      ${dev.hostname ? `<span class="av-host">via ${esc(dev.hostname)}</span>` : ''}
      ${offline ? '<span class="av-offline">offline</span>' : ''}
    </div>`;

  const nameEl = row.querySelector('.device-row-name');
  nameEl.style.cursor = 'pointer';
  nameEl.title = 'Click to rename';
  nameEl.addEventListener('click', () => startAvRename(nameEl, dev));
  appendEditLink(row.querySelector('.light-name-group'), () => startAvRename(nameEl, dev));

  const actions = document.createElement('div');
  actions.className = 'device-row-actions';
  if (dev.kind === 'sink') {
    actions.appendChild(buildAvRoomAssignments(dev));
  } else if (dev.id === 'puck') {
    actions.appendChild(buildRoomSelect(model.rooms, roomIdForAvDevice(dev),
      roomId => onPuckRoomChange(roomId)));
  }
  row.appendChild(actions);

  if (dev.transport === 'bluetooth' && dev.node_id) {
    if (dev.bluetooth_paired) {
      actions.appendChild(buildPairedBluetoothStatus(dev.node_id, dev.bluetooth_paired, dev.rooms));
    }
    const { button, panel } = buildBluetoothScanControls(dev.node_id);
    actions.appendChild(button);
    row.appendChild(panel);
  }

  return row;
}

// Fed by GET /api/av-devices' `bluetooth_paired` field, so it's already
// current on page load — this replaces the old ephemeral-only scan-panel
// state (lost on refresh) with the coordinator's persisted view: paired,
// currently connected, or paired-but-unavailable. A live `BluetoothStatusUpdate`
// WS event (see handleBluetoothStatusUpdate below) just re-fetches this
// same list rather than tracking status separately.
export function getAvDevices() {
  return avDevices;
}

function buildPairedBluetoothStatus(nodeId, paired, rooms) {
  const wrap = document.createElement('div');
  wrap.className = 'bt-paired-status';

  const dot = document.createElement('span');
  dot.className = 'bt-paired-dot' + (paired.connected ? ' bt-paired-connected' : ' bt-paired-unavailable');
  dot.textContent = paired.connected ? '●' : '○';
  wrap.appendChild(dot);

  const label = document.createElement('span');
  label.className = 'bt-paired-name';
  // Which room this speaker serves is what actually matters day-to-day
  // (the device's own name is already in the row header above) — the
  // "unassigned" fallback flags that pairing and room assignment are two
  // separate steps (see buildAvRoomAssignments' dropdown) rather than
  // silently omitting the room.
  const roomPart = rooms?.length ? rooms.map(r => r.room).join(', ') : 'unassigned to a room';
  // BlueZ can't tell "powered off" apart from "out of range/disconnected" —
  // "off / out of range" names both possibilities honestly rather than
  // guessing which, and reads as normal/expected rather than an error
  // (unlike "unavailable", which sounded broken for a simply-switched-off
  // speaker).
  label.textContent = `${roomPart} — ${paired.connected ? 'in use' : 'off / out of range'}`;
  wrap.appendChild(label);

  const unpairBtn = document.createElement('button');
  unpairBtn.className = 'device-row-btn bt-unpair-btn';
  unpairBtn.textContent = 'Unpair';
  unpairBtn.title = `Disconnect and forget ${paired.name} on this node`;
  unpairBtn.addEventListener('click', () => {
    if (!confirm(`Unpair "${paired.name}"? This disconnects it and forgets it on this node.`)) return;
    unpairBluetoothDevice(nodeId, paired.mac);
  });
  wrap.appendChild(unpairBtn);

  return wrap;
}

function unpairBluetoothDevice(nodeId, mac) {
  api(`/bluetooth/unpair/${encodeURIComponent(nodeId)}`, { method: 'POST', body: { mac } })
    .then(res => {
      if (res.ok) return;
      showToast(`Unpair failed (${res.status})`, true);
    })
    .catch(e => showToast(`Unpair error: ${e.message}`, true));
}

export function handleBluetoothUnpairResult(evt) {
  if (evt.success) {
    showToast("Unpaired — no longer used for this node's Bluetooth audio.");
    fetchAvDevices(true);
  } else {
    showToast(`Unpair failed${evt.error ? ': ' + evt.error : ''}`, true);
  }
}

// The coordinator already folded this update into its paired-status map
// (see DashboardState::set_bluetooth_paired) before broadcasting it, so a
// plain re-fetch of /api/av-devices is enough to pick up the new
// connected/unavailable state — no separate tracking needed here.
export function handleBluetoothStatusUpdate(_evt) {
  fetchAvDevices(true);
}

// ── Live Bluetooth scan + pair (per bluetooth-backend node) ─────────────────
// Mirrors the Zigbee "Pair device" flow (permit-join → live join feed) but
// per-node rather than bridge-wide, and with a device *picker* instead of
// auto-join: BlueZ discovery surfaces every nearby device, not just the one
// the user means, so pairing needs an explicit "use this one" click. See
// capabilities/audio/src/bluetooth.rs for the agent-side scan/pair.

// One entry per node currently showing a scan panel: node_id -> { devices:
// Map<mac, {mac,name,rssi}>, listEl, countdown }. A node not present here
// has no open panel — WS events for it are just ignored.
const btScanPanels = new Map();

function rssiBarCount(rssi) {
  if (rssi == null) return 0;
  if (rssi >= -50) return 4;
  if (rssi >= -60) return 3;
  if (rssi >= -70) return 2;
  return 1;
}

function buildSignalBars(rssi) {
  const wrap = document.createElement('span');
  wrap.className = 'bt-signal';
  wrap.title = rssi == null ? 'signal strength unknown' : `${rssi} dBm`;
  const filled = rssiBarCount(rssi);
  for (let i = 1; i <= 4; i++) {
    const bar = document.createElement('span');
    bar.className = `bt-signal-bar bt-signal-bar-${i}` + (i <= filled ? ' bt-signal-bar-filled' : '');
    wrap.appendChild(bar);
  }
  return wrap;
}

// The device currently paired on `nodeId`, per the coordinator's persisted
// view (GET /api/av-devices' `bluetooth_paired`) — `scan()` seeds its
// results from BlueZ's own cache, which includes whatever's already
// connected, so the paired device reappears here every time. Without this
// check its row would always default to "Use this device" regardless of
// already being paired and working.
function pairedMacForNode(nodeId) {
  return getAvDevices().find(d => d.node_id === nodeId && d.transport === 'bluetooth')
    ?.bluetooth_paired?.mac;
}

function renderBluetoothScanList(nodeId) {
  const state = btScanPanels.get(nodeId);
  if (!state) return;
  state.listEl.innerHTML = '';
  const found = [...state.devices.values()].sort((a, b) => (b.rssi ?? -999) - (a.rssi ?? -999));
  if (found.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'bt-scan-empty';
    empty.textContent = 'No devices seen yet…';
    state.listEl.appendChild(empty);
    return;
  }
  const pairedMac = pairedMacForNode(nodeId);
  for (const dev of found) {
    const row = document.createElement('div');
    row.className = 'bt-scan-row';
    const label = document.createElement('span');
    label.className = 'bt-scan-name';
    label.textContent = `${dev.name} (${dev.mac})`;
    const useBtn = document.createElement('button');
    useBtn.className = 'device-row-btn';
    if (dev.mac === pairedMac) {
      useBtn.textContent = 'Unpair';
      useBtn.title = `Disconnect and forget ${dev.name} on this node`;
      useBtn.addEventListener('click', () => {
        if (!confirm(`Unpair "${dev.name}"? This disconnects it and forgets it on this node.`)) return;
        unpairBluetoothDevice(nodeId, dev.mac);
      });
    } else {
      useBtn.textContent = dev.pairing ? 'Pairing…' : (dev.paired === false ? 'Failed — retry' : 'Use this device');
      useBtn.disabled = !!dev.pairing;
      if (dev.paired === false && dev.error) useBtn.title = dev.error;
      useBtn.addEventListener('click', () => pairBluetoothDevice(nodeId, dev));
    }
    row.append(buildSignalBars(dev.rssi), label, useBtn);
    state.listEl.appendChild(row);
  }
}

function pairBluetoothDevice(nodeId, dev) {
  const state = btScanPanels.get(nodeId);
  if (state) {
    const entry = state.devices.get(dev.mac);
    if (entry) { entry.pairing = true; entry.paired = undefined; }
    renderBluetoothScanList(nodeId);
  }
  api(`/bluetooth/pair/${encodeURIComponent(nodeId)}`, { method: 'POST', body: { mac: dev.mac } })
    .then(res => {
      if (res.ok) return;
      const s = btScanPanels.get(nodeId);
      const entry = s?.devices.get(dev.mac);
      if (entry) { entry.pairing = false; entry.paired = false; }
      renderBluetoothScanList(nodeId);
      showToast(`Pair request failed (${res.status})`, true);
    })
    .catch(e => showToast(`Pair request error: ${e.message}`, true));
}

export function handleBluetoothDeviceFound(evt) {
  const state = btScanPanels.get(evt.node_id);
  if (!state) return; // no open panel for this node — ignore
  const existing = state.devices.get(evt.mac);
  state.devices.set(evt.mac, {
    mac: evt.mac,
    name: evt.name || existing?.name || evt.mac,
    rssi: evt.rssi ?? existing?.rssi ?? null,
    pairing: existing?.pairing,
    paired: existing?.paired,
  });
  renderBluetoothScanList(evt.node_id);
}

export function handleBluetoothPairResult(evt) {
  const state = btScanPanels.get(evt.node_id);
  if (state) {
    const entry = state.devices.get(evt.mac);
    if (entry) { entry.pairing = false; entry.paired = evt.success; entry.error = evt.error; }
    renderBluetoothScanList(evt.node_id);
  }
  if (evt.success) {
    showToast(`Paired ${evt.name} — now used for this node's Bluetooth audio.`);
    fetchAvDevices(true);
  } else {
    showToast(`Pairing ${evt.name} failed${evt.error ? ': ' + evt.error : ''}`, true);
  }
}

// A full devices-tab re-render (`render()`, triggered by any LightingUpdate/
// RoomsUpdate/SensorUpdate WS event — all common while a scan is sitting
// open) throws away and rebuilds every AV row's DOM, including this panel.
// Without reattaching, an in-flight scan's `listEl`/countdown would keep
// writing into now-detached, invisible nodes — the visible replacement
// panel would silently stop updating. So `btScanPanels` entries own the
// live element references and this function re-syncs them into any state
// already open for `nodeId` instead of assuming a fresh start.
function tickBluetoothCountdown(nodeId) {
  const state = btScanPanels.get(nodeId);
  if (!state) return;
  if (state.remaining <= 0) {
    clearInterval(state.countdown);
    state.countdown = null;
    state.buttonEl.disabled = false;
    state.buttonEl.textContent = 'Scan for Bluetooth';
    return;
  }
  state.buttonEl.textContent = `Scanning… ${state.remaining}s`;
  state.remaining -= 1;
}

// Scan results are seeded (agent-side) from BlueZ's own device cache so a
// currently-connected device still shows up — but that means anything
// bluetoothd remembers (out of range, or from long ago) reappears looking
// identical to something live right now. This button forgets everything
// non-connected so the next scan only shows what's actually there.
function clearBluetoothCache(nodeId) {
  api(`/bluetooth/clear-cache/${encodeURIComponent(nodeId)}`, { method: 'POST' })
    .then(res => {
      if (res.ok) return;
      showToast(`Clear cache failed (${res.status})`, true);
    })
    .catch(e => showToast(`Clear cache error: ${e.message}`, true));
}

export function handleBluetoothClearCacheResult(evt) {
  if (evt.error) {
    showToast(`Clear cache failed: ${evt.error}`, true);
    return;
  }
  const state = btScanPanels.get(evt.node_id);
  if (state) {
    state.devices.clear();
    renderBluetoothScanList(evt.node_id);
  }
  showToast(`Cleared ${evt.cleared ?? 0} cached Bluetooth device${evt.cleared === 1 ? '' : 's'}.`);
}

function buildBluetoothScanControls(nodeId) {
  const button = document.createElement('button');
  button.className = 'device-row-btn';

  const panel = document.createElement('div');
  panel.className = 'bt-scan-panel';
  const list = document.createElement('div');
  list.className = 'bt-scan-list';
  panel.appendChild(list);

  const clearBtn = document.createElement('button');
  clearBtn.className = 'device-row-btn bt-scan-clear-cache';
  clearBtn.textContent = 'Clear cache';
  clearBtn.title = 'Forget cached devices that are out of range or no longer relevant';
  clearBtn.addEventListener('click', () => clearBluetoothCache(nodeId));
  panel.appendChild(clearBtn);

  const existing = btScanPanels.get(nodeId);
  if (existing) {
    // Re-render happened mid-scan: adopt the existing device list/countdown
    // into these fresh elements instead of losing them.
    existing.listEl = list;
    existing.buttonEl = button;
    existing.panelEl = panel;
    panel.hidden = false;
    button.disabled = existing.countdown != null;
    button.textContent = existing.countdown != null
      ? `Scanning… ${existing.remaining}s`
      : 'Scan for Bluetooth';
    renderBluetoothScanList(nodeId);
  } else {
    button.textContent = 'Scan for Bluetooth';
    panel.hidden = true;
  }

  button.addEventListener('click', async () => {
    if (btScanPanels.get(nodeId)?.countdown) return; // window already open
    btScanPanels.set(nodeId, {
      devices: new Map(),
      listEl: list,
      buttonEl: button,
      panelEl: panel,
      countdown: null,
      remaining: 0,
    });
    panel.hidden = false;
    renderBluetoothScanList(nodeId);
    try {
      const res = await api(`/bluetooth/scan/${encodeURIComponent(nodeId)}`, { method: 'POST' });
      if (!res.ok) {
        const text = await res.text().catch(() => '');
        showToast(`Scan failed (${res.status})${text ? ': ' + text : ''}`, true);
        btScanPanels.delete(nodeId);
        panel.hidden = true;
        return;
      }
      const { seconds } = await res.json();
      const state = btScanPanels.get(nodeId);
      if (!state) return; // scan was abandoned before the response landed
      state.remaining = seconds;
      state.buttonEl.disabled = true;
      tickBluetoothCountdown(nodeId);
      state.countdown = setInterval(() => tickBluetoothCountdown(nodeId), 1000);
    } catch (e) {
      showToast(`Scan error: ${e.message}`, true);
      btScanPanels.delete(nodeId);
      panel.hidden = true;
    }
  });

  return { button, panel };
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
  appendEditLink(row.querySelector('.light-name-group'), () => startRename(nameEl, dev.device_id));

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

  // Only a real Switch (button/dial) ever fires SwitchAction events —
  // Blinds/HVAC share this same presence-only row shape but have nothing
  // to bind.
  if (dev.device_type === 'switch') {
    const { toggle, panel } = buildBindingsPanel(dev.device_id, dev.actions ?? []);
    actions.appendChild(toggle);
    row.appendChild(panel);
  }

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
  appendEditLink(row.querySelector('.light-name-group'), () => startRename(nameEl, dev.device_id));

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
      && hvac.length === 0 && switches.length === 0 && avDevices.length === 0) {
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

  if (avDevices.length > 0) {
    const { heading, body } = buildCategorySection('Speakers & displays', avDevices.length);
    container.appendChild(heading);
    for (const dev of [...avDevices].sort((a, b) => a.name.localeCompare(b.name))) {
      body.appendChild(buildAvRow(dev));
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

// Collapse chevron per sensor subcategory — same pattern as
// buildCategoryHeading/buildCategorySection above, just one level deeper
// (h4 instead of h3, its own localStorage key so each subcategory remembers
// its own state independently of its siblings and of the parent "Sensors"
// category).
function buildSubcategorySection(key, label, count) {
  const heading = document.createElement('h4');
  heading.className = 'device-subcategory-heading';

  const storageKey = `mesh-devsubcat-collapsed-${key}`;
  const isCollapsed = localStorage.getItem(storageKey) === '1';

  const chevron = document.createElement('button');
  chevron.className = 'device-category-collapse-btn';
  chevron.title = 'Collapse / expand';
  chevron.textContent = isCollapsed ? '▸' : '▾';
  heading.appendChild(chevron);

  const labelEl = document.createElement('span');
  labelEl.textContent = `${label} (${count})`;
  heading.appendChild(labelEl);

  const body = document.createElement('div');
  body.className = 'device-subcategory-body' + (isCollapsed ? ' collapsed' : '');
  const toggle = () => {
    const nowCollapsed = !body.classList.contains('collapsed');
    body.classList.toggle('collapsed', nowCollapsed);
    chevron.textContent = nowCollapsed ? '▸' : '▾';
    localStorage.setItem(storageKey, nowCollapsed ? '1' : '0');
  };
  chevron.addEventListener('click', toggle);
  heading.addEventListener('click', e => { if (e.target !== chevron) toggle(); });

  return { heading, body };
}

function renderSensorSubcategories(body, sensors, container) {
  const remaining = new Set(sensors.map(d => d.device_id));
  const byId = new Map(sensors.map(d => [d.device_id, d]));

  for (const sub of SENSOR_SUBCATEGORIES) {
    const members = sensors.filter(d => sub.test(d));
    if (members.length === 0) continue;
    const { heading, body: subBody } = buildSubcategorySection(sub.key, sub.label, members.length);
    body.appendChild(heading);
    for (const dev of members.sort((a, b) => a.device_id.localeCompare(b.device_id))) {
      subBody.appendChild(buildSensorRow(dev, container));
      remaining.delete(dev.device_id);
    }
    body.appendChild(subBody);
  }

  if (remaining.size > 0) {
    const { heading, body: subBody } = buildSubcategorySection('other', 'Other', remaining.size);
    body.appendChild(heading);
    for (const id of [...remaining].sort()) {
      subBody.appendChild(buildSensorRow(byId.get(id), container));
    }
    body.appendChild(subBody);
  }
}

// Called by dashboard.js after rooms.js processes LightingUpdate/SensorUpdate/
// RoomsUpdate — devicesMap/model.rooms are already up to date by then.
export function refresh() {
  render();
  fetchAvDevices();
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
