// ── Shared per-type device widgets ──────────────────────────────────────────
// The sensor widget is used identically by the Home tab's room cards
// (rooms.js, read-only — sensors have no controls to expose) and the
// Devices tab's inventory rows (devices.js, with management controls
// layered on via `opts`). Lights don't get a shared widget here: the Home
// tab needs interactive controls (buildLightControls in lightcontrols.js,
// already reusable as-is) while the Devices tab only needs a status line —
// those are different concerns, not the same logic built twice.

import { esc } from '/static/util.js';
import { model } from '/static/state.js';

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
  return model.names.get(id) ?? id.replace(/[_-]+/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
}

// ── Switch action flash ──────────────────────────────────────────────────────
// A Switch device (button remote, dial) has no persisted state to render —
// a button press / rotation is a one-off event, so instead of a status line
// it gets a brief flash + action label on whichever row(s) currently render
// it. A device can be visible in more than one place at once (Devices tab
// and, if room-assigned, the Home tab's read-only strip both stay mounted
// simultaneously, just hidden via CSS when their tab isn't active), so this
// flashes every matching `[data-device-id]` element, not just the first.
const SWITCH_FLASH_MS = 1500;
const switchFlashState = new Map(); // device_id -> { action, timer }
// Unlike switchFlashState (clears after SWITCH_FLASH_MS), this remembers the
// last action indefinitely — the switch-bindings form (switchbindings.js)
// pre-fills its action field from this so "press the button, then bind
// whatever just fired" doesn't need typing the exact z2m action string.
const lastSeenActionByDevice = new Map();

export function getLastSeenAction(deviceId) {
  return lastSeenActionByDevice.get(deviceId) ?? null;
}

// Every distinct action ever observed from a device, so the switch-bindings
// form (switchbindings.js) can offer a self-populating combo box instead of
// a blind text field — press each button/rotation direction once and it
// shows up as a suggestion from then on. Persisted to localStorage (keyed
// per device) so the list survives a page reload, not just the current
// session.
const SEEN_ACTIONS_PREFIX = 'mesh-switch-actions-';
const seenActionsByDevice = new Map(); // device_id -> Set<action>, lazily hydrated

function loadSeenActions(deviceId) {
  try {
    const raw = localStorage.getItem(SEEN_ACTIONS_PREFIX + deviceId);
    return raw ? new Set(JSON.parse(raw)) : new Set();
  } catch {
    return new Set();
  }
}

function saveSeenActions(deviceId, set) {
  try {
    localStorage.setItem(SEEN_ACTIONS_PREFIX + deviceId, JSON.stringify([...set]));
  } catch {
    // Storage full/unavailable — the combo box just stays a plain text
    // field for this device; not worth surfacing an error for.
  }
}

export function getSeenActions(deviceId) {
  if (!seenActionsByDevice.has(deviceId)) {
    seenActionsByDevice.set(deviceId, loadSeenActions(deviceId));
  }
  return [...seenActionsByDevice.get(deviceId)].sort();
}

export function formatSwitchAction(action) {
  return action.replace(/_/g, ' ').replace(/^./, c => c.toUpperCase());
}

function paintSwitchFlash(deviceId, action) {
  const rows = document.querySelectorAll(`[data-device-id="${CSS.escape(deviceId)}"]`);
  for (const row of rows) {
    row.classList.remove('switch-action-flash');
    // Force reflow so re-triggering the animation on a rapid second press restarts it.
    void row.offsetWidth;
    row.classList.add('switch-action-flash');
    let badge = row.querySelector('.switch-action-badge');
    if (!badge) {
      badge = document.createElement('span');
      badge.className = 'switch-action-badge';
      // The badge's text is the whole point of the flash (which button/
      // direction fired) — announce it for screen readers same as a toast.
      badge.setAttribute('aria-live', 'polite');
      (row.querySelector('.light-card-header-right') ?? row).appendChild(badge);
    }
    badge.textContent = formatSwitchAction(action);
  }
}

function clearSwitchFlash(deviceId) {
  const rows = document.querySelectorAll(`[data-device-id="${CSS.escape(deviceId)}"]`);
  for (const row of rows) {
    row.classList.remove('switch-action-flash');
    row.querySelector('.switch-action-badge')?.remove();
  }
}

/// Called on a fresh SwitchAction WS event.
export function registerSwitchAction(deviceId, action) {
  lastSeenActionByDevice.set(deviceId, action);
  // Hydrate the in-memory Set unconditionally, not just when the action
  // turns out to be new — otherwise a repeat of an already-known action
  // (the common case) never populates seenActionsByDevice, and every
  // subsequent press re-hits localStorage/JSON.parse instead of the cache.
  let seen = seenActionsByDevice.get(deviceId);
  if (!seen) {
    seen = loadSeenActions(deviceId);
    seenActionsByDevice.set(deviceId, seen);
  }
  if (!seen.has(action)) {
    seen.add(action);
    saveSeenActions(deviceId, seen);
  }
  const existing = switchFlashState.get(deviceId);
  if (existing) clearTimeout(existing.timer);
  const timer = setTimeout(() => {
    switchFlashState.delete(deviceId);
    clearSwitchFlash(deviceId);
  }, SWITCH_FLASH_MS);
  switchFlashState.set(deviceId, { action, timer });
  paintSwitchFlash(deviceId, action);
}

/// Called when (re)building a switch's row/card, so an in-progress flash
/// survives a full re-render triggered by an unrelated WS event (e.g. a
/// SensorUpdate arriving mid-flash rebuilds every row from scratch).
export function applySwitchFlashIfActive(deviceId) {
  const state = switchFlashState.get(deviceId);
  if (state) paintSwitchFlash(deviceId, state.action);
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

/// In-room-group assignment dropdown for a light — mirrors buildRoomSelect
/// but scoped to one room's groups (a device can only belong to a group
/// inside its own room). Only rendered when the room has at least one
/// group defined (rooms.js's caller checks this).
export function buildGroupSelect(groups, currentGroupId, onChange) {
  const select = document.createElement('select');
  select.className = 'device-group-select';
  const noneOpt = document.createElement('option');
  noneOpt.value = '';
  noneOpt.textContent = 'Ungrouped';
  select.appendChild(noneOpt);
  for (const group of groups) {
    const o = document.createElement('option');
    o.value = group.id;
    o.textContent = group.name;
    if (group.id === currentGroupId) o.selected = true;
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

/// Small "✎ Edit" affordance placed directly under a device's name (a child
/// of `.light-name-group`, which is already `flex-direction: column`) —
/// shared by every row type in the Devices tab so renaming is equally
/// discoverable whether the row is a light, sensor, or presence-only device.
export function appendEditLink(nameGroupEl, onRename) {
  if (!nameGroupEl) return;
  const btn = document.createElement('button');
  btn.className = 'device-edit-link';
  btn.textContent = '✎ Edit';
  btn.addEventListener('click', e => { e.stopPropagation(); onRename(); });
  nameGroupEl.appendChild(btn);
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
      ${dev.node_id ? `<span class="light-node-badge">${esc(dev.node_id)}</span>` : ''}
    </div>
    <div class="light-card-header-right">
      ${readout ? `<span class="sensor-readout">${esc(readout)}</span>` : ''}
      ${statusBadge}
    </div>`;

  if (opts.onRename) appendEditLink(card.querySelector('.light-name-group'), opts.onRename);

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

  if (opts.onDelete || opts.onRemoveFromRoom || opts.rooms) {
    const actions = document.createElement('div');
    actions.className = 'device-row-actions';

    if (opts.rooms) {
      actions.appendChild(buildRoomSelect(opts.rooms, opts.currentRoomId,
        roomId => opts.onRoomChange?.(roomId)));
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

  applySwitchFlashIfActive(dev.device_id);
  return card;
}
