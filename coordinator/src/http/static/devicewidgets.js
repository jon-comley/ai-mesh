// ── Shared per-type device widgets ──────────────────────────────────────────
// The sensor widget is used identically by the Home tab's room cards
// (rooms.js, read-only — sensors have no controls to expose) and the
// Devices tab's inventory rows (devices.js, with management controls
// layered on via `opts`). Lights don't get a shared widget here: the Home
// tab needs interactive controls (buildLightControls in lightcontrols.js,
// already reusable as-is) while the Devices tab only needs a status line —
// those are different concerns, not the same logic built twice.

import { esc } from '/static/util.js';

export function formatSensorReadout(s) {
  const parts = [];
  if (s.temperature != null) parts.push(`${s.temperature.toFixed(1)}°C`);
  if (s.humidity != null) parts.push(`${Math.round(s.humidity)}% RH`);
  if (s.battery != null) parts.push(`🔋${s.battery}%`);
  if (s.occupancy != null) parts.push(s.occupancy ? 'Motion' : 'Clear');
  // z2m convention: contact=true means the reed switch is made — i.e. closed.
  if (s.contact != null) parts.push(s.contact ? 'Closed' : 'Open');
  if (s.illuminance != null) parts.push(`💡${Math.round(s.illuminance)} lx`);
  return parts.join(' · ');
}

export function formatLightStatus(dev) {
  if (dev.online === false) return 'Offline';
  if (!dev.on) return 'Off';
  const parts = ['On'];
  if (dev.brightness != null) parts.push(`${Math.round((dev.brightness / 254) * 100)}%`);
  if (dev.color_temp) parts.push(`${Math.round(1_000_000 / dev.color_temp)} K`);
  return parts.join(', ');
}

function formatDeviceName(id) {
  return id.replace(/[_-]+/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
}

/// Room-assignment dropdown, shared by the Devices tab's rows (both types)
/// and the post-pair "assign to room" prompt in the join feed.
export function buildRoomSelect(rooms, currentRoomId, onChange) {
  const select = document.createElement('select');
  select.className = 'device-room-select';
  const noneOpt = document.createElement('option');
  noneOpt.value = '';
  noneOpt.textContent = 'Unassigned';
  select.appendChild(noneOpt);
  for (const room of rooms) {
    const o = document.createElement('option');
    o.value = room.id;
    o.textContent = room.name;
    if (room.id === currentRoomId) o.selected = true;
    select.appendChild(o);
  }
  select.addEventListener('change', () => onChange(select.value || null));
  return select;
}

const PINNED_SENSORS_KEY = 'mesh-pinned-sensors';

/// Pin is a global per-device preference (not per-room) — a pinned sensor
/// stays visible in a room's collapsed summary regardless of which room it's
/// in. Deliberately sensor-only; lights don't get this (explicit product
/// call — effects already keep light cards busy, and the room card itself
/// is the "collapsed summary" for lights).
export function isSensorPinned(deviceId) {
  try {
    const raw = localStorage.getItem(PINNED_SENSORS_KEY);
    return raw ? JSON.parse(raw).includes(deviceId) : false;
  } catch { return false; }
}

export function toggleSensorPin(deviceId) {
  let pinned = [];
  try { pinned = JSON.parse(localStorage.getItem(PINNED_SENSORS_KEY) ?? '[]'); } catch { pinned = []; }
  const idx = pinned.indexOf(deviceId);
  if (idx === -1) pinned.push(deviceId); else pinned.splice(idx, 1);
  localStorage.setItem(PINNED_SENSORS_KEY, JSON.stringify(pinned));
  return idx === -1; // now pinned?
}

/// One sensor's card: name + node badge + read-only readout, optionally with
/// management controls (rename/delete/room-assignment/remove-from-room/pin)
/// when `opts` is given — the Devices tab passes rename/delete/rooms; the
/// Home tab's per-room strip passes onRemoveFromRoom + pinnable.
export function buildSensorCard(dev, opts = {}) {
  const card = document.createElement('div');
  card.className = `light-card sensor-card${dev.online === false ? ' is-offline' : ''}`;
  card.dataset.deviceId = dev.device_id;
  if (opts.draggable) card.setAttribute('draggable', 'true');

  const displayName = formatDeviceName(dev.device_id);
  const readout = formatSensorReadout(dev);
  const statusBadge = dev.online === false ? '<span class="badge badge-muted">Offline</span>' : '';

  card.innerHTML = `
    <div class="light-name-group">
      <span class="light-name">${esc(displayName)}</span>
      <span class="light-node-badge">${esc(dev.node_id)}</span>
    </div>
    <div class="light-card-header-right">
      ${readout ? `<span class="sensor-readout">${esc(readout)}</span>` : ''}
      ${statusBadge}
    </div>`;

  if (opts.pinnable) {
    const pinBtn = document.createElement('button');
    pinBtn.className = 'device-row-btn sensor-pin-btn';
    const paintPin = () => {
      const pinned = isSensorPinned(dev.device_id);
      pinBtn.textContent = pinned ? '📌' : '📍';
      pinBtn.title = pinned ? 'Pinned — shows when room is collapsed' : 'Pin — keep visible when room is collapsed';
      pinBtn.classList.toggle('sensor-pin-btn-active', pinned);
    };
    paintPin();
    pinBtn.addEventListener('click', e => {
      e.stopPropagation();
      toggleSensorPin(dev.device_id);
      paintPin();
      opts.onPinChange?.();
    });
    card.querySelector('.light-card-header-right')?.appendChild(pinBtn);
  }

  if (opts.onRename || opts.onDelete || opts.onRemoveFromRoom || opts.rooms) {
    const actions = document.createElement('div');
    actions.className = 'device-row-actions';

    if (opts.rooms) {
      actions.appendChild(buildRoomSelect(opts.rooms, opts.currentRoomId,
        roomId => opts.onRoomChange?.(roomId)));
    }
    if (opts.onRename) {
      const btn = document.createElement('button');
      btn.className = 'device-row-btn';
      btn.textContent = '✎';
      btn.title = 'Rename';
      btn.addEventListener('click', () => opts.onRename());
      actions.appendChild(btn);
    }
    if (opts.onRemoveFromRoom) {
      const btn = document.createElement('button');
      btn.className = 'device-row-btn';
      btn.textContent = '✕';
      btn.title = 'Remove from room';
      btn.addEventListener('click', () => opts.onRemoveFromRoom());
      actions.appendChild(btn);
    }
    if (opts.onDelete) {
      const btn = document.createElement('button');
      btn.className = 'device-row-btn device-row-btn-delete';
      btn.textContent = '✕';
      btn.title = 'Delete';
      btn.addEventListener('click', () => opts.onDelete());
      actions.appendChild(btn);
    }
    card.appendChild(actions);
  }

  return card;
}
