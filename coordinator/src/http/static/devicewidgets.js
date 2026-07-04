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

/// One sensor's card: name + node badge + read-only readout, optionally with
/// management controls (rename/delete/room-assignment) when `opts` is given
/// — the Devices tab passes these; the Home tab's per-room strip doesn't.
export function buildSensorCard(dev, opts = {}) {
  const card = document.createElement('div');
  card.className = `light-card sensor-card${dev.online === false ? ' is-offline' : ''}`;
  card.dataset.deviceId = dev.device_id;

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

  if (opts.onRename || opts.onDelete || opts.rooms) {
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
