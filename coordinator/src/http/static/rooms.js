// ── Rooms panel ──────────────────────────────────────────────────────────────
// First-class spatial room objects with drag-and-drop device assignment
// and drag-to-reorder room cards.

import * as layout from '/static/layout.js';
import { xyToRgb, hslToXy } from '/static/colormath.js';
import { buildSlider, buildColourWheel } from '/static/controls.js';
import { buildLightControls, buildTempBar, dismissOpenLightControl } from '/static/lightcontrols.js';
import { buildSensorCard } from '/static/devicewidgets.js';
import {
  createRoom, deleteRoom, renameRoom, reorderRooms,
  addDeviceToRoom, removeDeviceFromRoom, reorderRoomDevices,
  deleteDevice, patchDeviceName,
} from '/static/actions.js';
import { esc, showToast } from '/static/util.js';
import { setPref } from '/static/prefs.js';
import { tok, api } from '/static/api.js';
import {
  model, devicesMap,
  lastEffectByRoom, openPickerIds, openRoomCtrlIds,
  activeSceneByRoom, pausedSceneDevices,
  roomDotDomain, roomEffectsMap,
  effectDrag, effectEditor, sceneEdit,
  pendingCommands, PENDING_TTL_MS,
  EFFECT_ICONS, DEFAULT_EFFECT_ICON, SCENE_ICON,
  HUE_DEFAULT_ON,
} from '/static/state.js';
import {
  clearDotForRoom, getRoomColourHsl, repaintModeDots,
  paintRoomDot, paintRoomButton, refreshRoomTriggers,
} from '/static/indicators.js';
import {
  initEffects, fetchEffectsCatalog,
  renderEffectsPalette, buildEffectBadge, buildEffectGhostBadge, buildEffectEditor,
  activateEffect, clearEffect, excludeFromEffect, includeInEffect,
} from '/static/effects.js';
import {
  initScenes, buildScenesSection, clearRoomActiveScene, cancelSceneEdit,
  toggleSceneDevice, recallScene, wireSceneChipTouchDrag, wireSceneBarDrag,
  reconcileSceneDivergence,
} from '/static/scenes.js';

// Shared state model + collections live in state.js (imported above). The vars
// below are drag gestures that only rooms.js touches. The effects and scenes
// domains live in effects.js / scenes.js; their cross-module UI state shares via
// the state.effectDrag / state.effectEditor / state.sceneEdit holders.
let dragSrc = null;             // chip drag: { deviceId, fromRoomId }
let roomDragId = null;          // room reorder drag: room id being dragged
let _roomCtrlDismiss = null;          // document outside-pointerdown listener for the open room colour/temp panel (only one at a time)

function markPending(deviceId, field, value) {
  let fields = pendingCommands.get(deviceId);
  if (!fields) { fields = {}; pendingCommands.set(deviceId, fields); }
  fields[field] = { value, ts: Date.now() };
}

// Optimistically merge fields into a device's cached state (no-op if unknown).
function patchDevice(deviceId, fields) {
  const cur = devicesMap.get(deviceId);
  if (cur) devicesMap.set(deviceId, { ...cur, ...fields });
}

function reconcilePending(dev) {
  const fields = pendingCommands.get(dev.device_id);
  if (!fields) return dev;
  const now = Date.now();
  let out = dev;
  for (const field of Object.keys(fields)) {
    const { value, ts } = fields[field];
    if (dev[field] === value || now - ts > PENDING_TTL_MS) {
      delete fields[field];
      continue;
    }
    if (out === dev) out = { ...dev };
    out[field] = value;
  }
  if (Object.keys(fields).length === 0) pendingCommands.delete(dev.device_id);
  return out;
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
  if (sceneEdit.active && !e.target.closest('.room-scene-save-row')) {
    cancelSceneEdit();
  }
});

// The effect-badge drag-to-remove document handlers live in effects.js (they own
// effectDrag/effectEditor). The generic drag-glow cleanup below stays here — it's
// shared by every drag gesture, not just effects.
// Safety net: a cancelled drag (Esc, or a release on an invalid target) fires no
// drop/dragleave on the highlighted card, so the drop glow would otherwise stick
// until the next render. Clear any lingering highlight when the drag ends.
document.addEventListener('dragend', () => {
  document.querySelectorAll('.room-drop-active').forEach(el => el.classList.remove('room-drop-active'));
}, true);

// ── Auto-scroll during drag ───────────────────────────────────────────────────
// Scrolls the lighting panel (or body on desktop) when the pointer is near the
// top or bottom edge of the viewport while any drag is active.
{
  const EDGE = 80;       // px from edge to start scrolling
  const MAX_SPEED = 16;  // px per frame at the very edge
  let scrollRaf = null;
  let scrollEl = null;   // cached per drag session

  const resolveScrollEl = () => {
    const panel = document.getElementById('panel-home');
    if (panel && getComputedStyle(panel).overflowY !== 'visible') return panel;
    return document.scrollingElement || document.documentElement;
  };

  const stopScroll = () => {
    if (scrollRaf) { cancelAnimationFrame(scrollRaf); scrollRaf = null; }
    scrollEl = null;
  };

  document.addEventListener('dragover', e => {
    const y = e.clientY;
    const h = window.innerHeight;
    let speed = 0;
    if (y < EDGE)          speed = -MAX_SPEED * (1 - y / EDGE);
    else if (y > h - EDGE) speed =  MAX_SPEED * (1 - (h - y) / EDGE);

    if (speed === 0) { stopScroll(); return; }

    // Cache scroll target once per drag; re-resolve only when not yet set
    if (!scrollEl) scrollEl = resolveScrollEl();

    if (!scrollRaf) {
      const tick = () => {
        // Re-check validity each frame — layout may change mid-drag
        if (!scrollEl || !document.contains(scrollEl)) { stopScroll(); return; }
        scrollEl.scrollBy(0, speed);
        scrollRaf = requestAnimationFrame(tick);
      };
      scrollRaf = requestAnimationFrame(tick);
    }
  }, { passive: true });

  document.addEventListener('dragend',  stopScroll);
  document.addEventListener('drop',     stopScroll);
  document.addEventListener('dragleave', e => {
    // Viewport exit: clientY outside window bounds is more reliable than relatedTarget === null
    if (e.clientY <= 0 || e.clientY >= window.innerHeight) stopScroll();
  });
}

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
// Wire the effects/scenes modules' core dependencies before the first catalogue
// fetch (and before any WS event) so their optimistic re-renders + commands
// work. render/sendDeviceCommand are hoisted function declarations, so the
// references are live at this point.
initEffects({ render });
initScenes({ render, sendDeviceCommand });
fetchEffectsCatalog();

export function handleRoomsUpdate(evt) {
  model.rooms = evt.rooms ?? [];
  if (evt.device_names) notifyDeviceNames(evt.device_names);
  inferZigbeeStatus();
  
  // Forward orientation/room state to layout canvas if a room is open
  if (evt.rooms && layout.currentLayoutRoomId()) {
    const r = evt.rooms.find(r => r.id === layout.currentLayoutRoomId());
    if (r != null) {
      layout.notifyOrientationUpdate(r.orientation_degrees);
      layout.notifyRoomUpdate(r);
    }
  }
  render();
}

export function notifyDeviceNames(names) {
  model.names = new Map(Object.entries(names));
}

async function fetchDeviceNames() {
  try {
    const res = await api('/lights/names');
    if (res.ok) notifyDeviceNames(await res.json());
  } catch (_) {}
}

// devicesMap holds BOTH lights and sensors, tagged by `device_type` — each
// WS event (LightingUpdate/SensorUpdate) is always a full domain snapshot, so
// each notify* function only ever clears+repopulates ITS OWN type's entries,
// never touching the other domain's.
function clearByType(type) {
  for (const [id, dev] of devicesMap) {
    if (dev.device_type === type) devicesMap.delete(id);
  }
}

export function notifyDevices(devices) {
  clearByType('light');
  const reconciledDevices = [];
  for (const dev of devices) {
    const reconciled = reconcilePending(dev);
    devicesMap.set(dev.device_id, { ...reconciled, device_type: 'light' });
    layout.notifyDeviceUpdate(dev.device_id, reconciled);
    reconciledDevices.push(reconciled);
  }
  // Runs regardless of the drag guard below — detecting a scene divergence
  // (e.g. a chat command changed a light) isn't itself a rendering concern,
  // and reconcileSceneDivergence's own re-render is internally guarded
  // against clobbering an in-progress drag anyway.
  reconcileSceneDivergence(reconciledDevices);
  inferZigbeeStatus();
  // Skip full re-render while a slider/wheel/temp-bar is being dragged to prevent
  // mid-drag jumps (and so the live colour/temp dot isn't reset to its icon).
  if (document.querySelector('.slider-active, .colour-wheel.dragging, .temp-bar.dragging')) return;
  patchDeviceCards();
  refreshRoomTriggers();
}

// Sensors never participate in the 3D layout (fixture placement is a
// lights-only concept — see layout.js), so unlike notifyDevices there is no
// layout.notifyDeviceUpdate call here. Sensor updates are infrequent enough
// (push-on-change, no slider dragging involved) that a full render() is fine
// — no need for a sensor-specific patch path mirroring patchDeviceCards.
export function notifySensors(sensors) {
  clearByType('sensor');
  for (const dev of sensors) {
    devicesMap.set(dev.device_id, { ...dev, device_type: 'sensor' });
  }
  render();
}

// The colour/temperature indicator paint functions live in indicators.js.

// Lightweight patch: update on/off badge and colour swatch without touching
// sliders. For devices under an active effect, sliders stay frozen at their
// last rendered position — the effect owns them.
function patchDeviceCards() {
  for (const [deviceId, dev] of devicesMap) {
    const card = document.querySelector(`.room-device-card[data-device-id="${CSS.escape(deviceId)}"]`);
    if (!card) continue;

    // The card has 'device-under-effect' when last rendered under an active
    // effect. Use the DOM class as the source of truth — roomEffectsMap may
    // not yet be populated (e.g. right after coordinator restart).
    const underEffect = card.classList.contains('device-under-effect');

    // Sync offline / online state.
    const isOffline = dev.online === false;
    card.classList.toggle('device-offline', isOffline);

    // Always update on/off badge (clears any stale "Offline" text).
    const badge = card.querySelector('.light-toggle-btn .badge');
    if (badge) {
      if (isOffline) {
        badge.className = 'badge badge-offline';
        badge.textContent = 'Offline';
      } else {
        badge.className = `badge ${dev.on ? 'badge-green' : 'badge-muted'}`;
        badge.textContent = dev.on ? 'On' : 'Off';
      }
    }

    // Repaint the active-domain dot from current state (colour tint or CCT tint).
    repaintModeDots(card, dev);

    // Only update sliders when the device is NOT under an active effect.
    if (!underEffect) {
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
    }
  }
}

let zigbeeOnline = true;

export function handleZigbeeStatus(online) {
  zigbeeOnline = online;
  render();
}

function inferZigbeeStatus() {
  // Lights only: z2m marks a mains-powered light unreachable within ~10 min,
  // but battery sensors get a ~25h passive timeout (see docs/pi1-lighting-setup.md
  // §9) — a bridge that just went down would still show sensors "online" for
  // hours, making this heuristic less reliable if sensors were included.
  const lights = [...devicesMap.values()].filter(d => d.device_type === 'light');
  // If we have rooms but zero lights have ever arrived, zigbee2mqtt never
  // connected — treat as offline.
  if (model.rooms.length > 0 && lights.length === 0) {
    zigbeeOnline = false;
    return;
  }
  // If every known light is offline, the bridge is almost certainly down.
  if (lights.length > 0 && lights.every(d => !d.online)) {
    zigbeeOnline = false;
    return;
  }
  zigbeeOnline = true;
}

export function notifySolar(azimuth, elevation) {
  // Forward to the layout panel (compass + sun-arc display). The dashboard
  // does not run its own solar calculation any more — the runner pushes
  // SolarUpdate events on its tick.
  layout.notifySolarUpdate(azimuth, elevation);
}

// ── Main render ──────────────────────────────────────────────────────────────

function render() {
  const container = document.getElementById('home-list');
  if (!container || dragSrc || roomDragId) return;
  if (container.querySelector('.layout-view')) return; // layout open — don't wipe
  if (container.querySelector('.room-slider-input.slider-active')) return; // room slider thumb being dragged — don't wipe it out
  if (container.querySelector('.colour-wheel.dragging')) return; // colour wheel being dragged — don't wipe it out
  if (container.querySelector('.temp-bar.dragging')) return; // temp bar being dragged — don't wipe it out
  // NOTE: device-card sliders (.lc-slider) are not guarded here — the common
  // WS-update path (notifyDevices) already bails on any .slider-active before
  // patchDeviceCards. Only a rare full render() mid-device-drag is unguarded;
  // widen this selector to `.slider-active` if that edge case ever bites.
  inferZigbeeStatus();

  const assigned = new Set(model.rooms.flatMap(r => r.device_ids));
  // Lights only — this strip is the light-specific drag-and-drop assignment
  // UI (chips carry on/off state, pulse-identify, etc). Sensors are assigned
  // to rooms via the Devices tab's dropdown instead (devices.js), so an
  // unassigned sensor must never render here as a light-shaped chip.
  const unassigned = [...devicesMap.entries()]
    .filter(([id, d]) => d.device_type === 'light' && !assigned.has(id))
    .map(([id]) => id);

  // An open temp/colour section has a document-level pointerdown listener bound
  // to a card we're about to remove. Disarm it before wiping, or it leaks (and
  // keeps firing against a detached card) until another section happens to open.
  dismissOpenLightControl();

  container.innerHTML = '';
  if (!zigbeeOnline) {
    const banner = document.createElement('div');
    banner.id = 'zigbee-banner';
    banner.className = 'zigbee-offline-banner';
    banner.textContent = '⚠ Zigbee bridge offline — lights unavailable';
    container.appendChild(banner);
  }
  if (model.rooms.length > 0) container.appendChild(renderGlobalControls());
  container.appendChild(renderNewRoomBtn());
  container.appendChild(renderEffectsPalette());
  container.appendChild(renderUnassigned(unassigned));

  const roomList = document.createElement('div');
  roomList.className = 'room-list rooms-layout-root' + (zigbeeOnline ? '' : ' zigbee-offline');

  const sorted = [...model.rooms].sort((a, b) => a.position - b.position);
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
  if (sceneEdit.active) {
    const card = document.querySelector(`[data-room-id="${CSS.escape(sceneEdit.active.roomId)}"]`);
    const ni = card?.querySelector('.room-scene-name-input');
    const sb = card?.querySelector('.room-scene-save-btn');
    if (ni && sb) {
      sb.style.display = 'none';
      ni.style.display = '';
      ni.value = sceneEdit.active.value;
      ni.focus();
    }
  }
}

// ── Global controls ──────────────────────────────────────────────────────────

function renderGlobalControls() {
  const bar = document.createElement('div');
  bar.className = 'room-global-controls';

  const allOnBtn = document.createElement('button');
  allOnBtn.className = 'room-action-btn' + (model.globalLight === 'on' ? ' room-action-active-on' : '');
  allOnBtn.textContent = 'All On';
  allOnBtn.addEventListener('click', () => {
    model.globalLight = 'on';
    layout.freezeIconUpdates(3000);
    for (const r of model.rooms) sendRoomCommand(r.id, { action: 'on' }, r, true);
  });

  const allOffBtn = document.createElement('button');
  allOffBtn.className = 'room-action-btn' + (model.globalLight === 'off' ? ' room-action-active-off' : '');
  allOffBtn.textContent = 'All Off';
  allOffBtn.addEventListener('click', () => {
    model.globalLight = 'off';
    layout.freezeIconUpdates(3000);
    for (const r of model.rooms) sendRoomCommand(r.id, { action: 'off' }, r, true);
  });

  bar.appendChild(allOnBtn);
  bar.appendChild(allOffBtn);
  return bar;
}

// ── Effects palette ──────────────────────────────────────────────────────────

// ── Room controls panel (effect-editor style) ─────────────────────────────────
// Brightness always shown. Temperature OR Colour — based on current room state.
// Colour dot at top switches between the two modes.
function buildRoomControlsPanel(room, devices, hasColour, activeEffect, onClose) {
  const hasColourXY = devices.some(d => d.color_xy != null);
  const hasTempDevices = devices.some(d => d.color_temp != null);
  // Mode is persisted per room (Hue bulbs always report both, so it can't be
  // inferred from state). Default Temperature; adjusting a slider pins its mode.
  const modeKey = 'mesh-room-mode-' + room.id;
  let mode = localStorage.getItem(modeKey) || (hasTempDevices ? 'temp' : 'colour');
  if (mode === 'temp'   && !hasTempDevices) mode = 'colour';
  if (mode === 'colour' && !hasColourXY)    mode = 'temp';
  const setMode = (m) => { mode = m; localStorage.setItem(modeKey, m); };

  // Locate this room's colour/temp trigger buttons lazily — the card is always
  // attached by the time a drag fires (even if not when the panel is first built
  // during a render). Used to flash a live dot on the trigger while dragging.
  const trigger = (role) => document.querySelector(
    `.room-card[data-room-id="${CSS.escape(room.id)}"] .room-ctrl-trigger[data-role="${role}"]`);

  const panel = document.createElement('div');
  panel.className = 'room-ctrl-panel';

  // Header
  const hdr = document.createElement('div');
  hdr.className = 'room-ctrl-panel-header';

  const title = document.createElement('span');
  title.className = 'room-ctrl-panel-title';
  title.textContent = mode === 'colour' ? 'Colour' : 'Temperature';
  hdr.appendChild(title);

  // Mode is chosen by the 🎨 / 🌡 buttons on the room card (above), not in here.

  const closeBtn = document.createElement('button');
  closeBtn.className = 'effect-editor-close';
  closeBtn.textContent = '×';
  closeBtn.addEventListener('click', e => { e.stopPropagation(); onClose(); });
  hdr.appendChild(closeBtn);
  panel.appendChild(hdr);

  // Temperature slider
  const ctDevices = devices.filter(d => d.color_temp != null);
  const avgCT = ctDevices.length > 0
    ? Math.round(ctDevices.reduce((s, d) => s + (d.color_temp ?? 0), 0) / ctDevices.length)
    : 370;
  const tempSliderEl = buildTempBar({
    mireds: avgCT,
    onInput: v => paintRoomDot(trigger('room-temp'), 'temp', layout.ctToHex(v)),
    onChange: async v => {
      setMode('temp');
      roomDotDomain.set(room.id, 'temp');   // temp becomes the persistent dot
      paintRoomDot(trigger('room-temp'), 'temp', layout.ctToHex(v));
      if (activeEffect) await clearEffect(room.id);
      sendRoomCommand(room.id, { action: 'color_temp', value: v }, room);
    },
  });
  tempSliderEl.style.display = mode === 'temp' ? '' : 'none';
  panel.appendChild(tempSliderEl);

  // Colour wheel (Hue + Saturation)
  const colourSliderEl = document.createElement('div');
  colourSliderEl.className = 'lc-colour-wheel-wrap';
  colourSliderEl.style.display = mode === 'colour' ? '' : 'none';
  if (hasColourXY) {
    const { h, s } = getRoomColourHsl(devices);
    colourSliderEl.appendChild(buildColourWheel({
      hue: h, sat: s,
      onInput: (hh, ss) => paintRoomDot(trigger('room-colour'), 'colour', `hsl(${hh},${ss}%,50%)`),
      onChange: (hh, ss) => {
        setMode('colour');
        roomDotDomain.set(room.id, 'colour');   // colour becomes the persistent dot
        paintRoomDot(trigger('room-colour'), 'colour', `hsl(${hh},${ss}%,50%)`);
        const { x, y } = hslToXy(hh, ss);
        sendRoomCommand(room.id, { action: 'color_xy', x, y }, room);
      },
    }));
  }
  panel.appendChild(colourSliderEl);

  return panel;
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
    input.name = 'room-name';
    input.autocomplete = 'off';
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

  // Room drag: show warning drop zone
  strip.addEventListener('dragover', e => {
    if (!roomDragId) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    strip.classList.add('room-delete-drop-active');
  });
  strip.addEventListener('dragleave', e => {
    // relatedTarget is unreliable in HTML5 DnD; use bounding-box instead
    const rect = strip.getBoundingClientRect();
    const inside = e.clientX >= rect.left && e.clientX <= rect.right
                && e.clientY >= rect.top  && e.clientY <= rect.bottom;
    if (!inside) strip.classList.remove('room-delete-drop-active');
  });
  strip.addEventListener('drop', async e => {
    if (!roomDragId) return;
    e.preventDefault();
    e.stopPropagation();
    strip.classList.remove('room-delete-drop-active');
    const id = roomDragId;
    roomDragId = null;
    const room = model.rooms.find(r => r.id === id);
    const name = room?.name || 'this room';
    const count = room?.device_ids?.length ?? 0;
    const msg = count > 0
      ? `Delete "${name}"? This will unassign ${count} bulb${count !== 1 ? 's' : ''} and remove the floor plan.`
      : `Delete "${name}"? The floor plan will also be removed.`;
    if (!confirm(msg)) { requestAnimationFrame(saveRoomOrder); return; }
    await deleteRoom(id);
  });

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
    if (effectDrag.src) { e.preventDefault(); return; } // let effect drag pass through
    if (card.classList.contains('room-wheel-open')) { e.preventDefault(); return; } // colour wheel open — no reorder
    if (card.getAttribute('draggable') !== 'true') { e.preventDefault(); return; }
    roomDragId = room.id;
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', `room:${room.id}`);
    // Ghost: a compact card showing just the room header (name + bulb count)
    const ghost = document.createElement('div');
    ghost.className = 'room-drag-ghost';
    const ghostName = document.createElement('span');
    ghostName.className = 'room-drag-ghost-name';
    ghostName.textContent = room.name || 'Room';
    const bulbCount = room.device_ids.filter(id => devicesMap.get(id)?.device_type !== 'sensor').length;
    const ghostCount = document.createElement('span');
    ghostCount.className = 'room-drag-ghost-count';
    ghostCount.textContent = bulbCount
      ? `${bulbCount} bulb${bulbCount !== 1 ? 's' : ''}`
      : 'empty';
    ghost.appendChild(ghostName);
    ghost.appendChild(ghostCount);
    // Position off-screen but in normal flow so offsetWidth is computed correctly
    ghost.style.cssText = 'position:fixed;left:-9999px;top:0;';
    document.body.appendChild(ghost);
    e.dataTransfer.setDragImage(ghost, ghost.offsetWidth / 2, ghost.offsetHeight / 2);
    requestAnimationFrame(() => {
      card.classList.add('dragging');
      ghost.remove();
      const strip = document.getElementById('unassigned-strip');
      if (strip) {
        strip.style.display = '';
        strip.classList.add('room-drag-active');
        const lbl = strip.querySelector('.room-unassigned-label');
        if (lbl) lbl.textContent = '⚠ Drop to delete room + unassign bulbs';
      }
    });
  });
  card.addEventListener('dragend', () => {
    card.classList.remove('dragging');
    const wasReordering = roomDragId !== null;
    roomDragId = null;
    card.setAttribute('draggable', 'true');
    const strip = document.getElementById('unassigned-strip');
    if (strip) {
      strip.classList.remove('room-drag-active', 'room-delete-drop-active');
      const lbl = strip.querySelector('.room-unassigned-label');
      if (lbl) lbl.textContent = 'Unassigned';
      if (strip.querySelectorAll('.room-chip').length === 0) strip.style.display = 'none';
    }
    if (wasReordering) saveRoomOrder();
  });

  // Shared state for header — lights only: on/off, colour, brightness, and the
  // "empty" gate for the On/Off/brightness controls are all light-specific
  // concepts, so a room holding only a sensor must still count as empty here.
  const roomDevicesAll = room.device_ids
    .map(id => devicesMap.get(id))
    .filter(d => d && d.device_type !== 'sensor');
  const anyOn = roomDevicesAll.some(d => d.on);
  const hasColour = roomDevicesAll.some(d => d.color_xy != null);
  const activeEffect = roomEffectsMap.get(room.id) || null;
  const empty = roomDevicesAll.length === 0;

  // Header: two rows — name row on top, controls row below
  const header = document.createElement('div');
  header.className = 'room-card-header';

  // Row 1: collapse chevron + room name
  const nameRow = document.createElement('div');
  nameRow.className = 'room-header-name-row';

  const collapseBtn = document.createElement('button');
  collapseBtn.className = 'room-collapse-btn';
  collapseBtn.title = 'Collapse / expand';
  const isCollapsed = localStorage.getItem(`mesh-room-collapsed-${room.id}`) === '1';
  collapseBtn.textContent = isCollapsed ? '▸' : '▾';
  nameRow.appendChild(collapseBtn);

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
  nameRow.appendChild(nameWrap);
  header.appendChild(nameRow);

  // Row 2: quick controls + layout button + actions
  const ctrlRow = document.createElement('div');
  ctrlRow.className = 'room-header-controls-row';

  // ── On / Off — big segmented control (primary casual action) ─────────────
  const onBtn  = document.createElement('button');
  const offBtn = document.createElement('button');
  const setRoomOnOff = (isOn) => {
    onBtn.classList.toggle('active', isOn);
    offBtn.classList.toggle('active', !isOn);
  };
  onBtn.className = 'room-onoff-btn room-onoff-on';
  onBtn.textContent = 'On';
  onBtn.disabled = empty;
  if (!empty) onBtn.addEventListener('click', async e => {
    e.stopPropagation();
    setRoomOnOff(true);
    clearDotForRoom(room);   // on/off resets colour/temp dots to icons
    // Power on at the Hue default warm white so on/off is consistent.
    if (activeEffect) await clearEffect(room.id);
    for (const c of HUE_DEFAULT_ON) await sendRoomCommand(room.id, c, room);
  });
  offBtn.className = 'room-onoff-btn room-onoff-off';
  offBtn.textContent = 'Off';
  offBtn.disabled = empty;
  if (!empty) offBtn.addEventListener('click', async e => {
    e.stopPropagation();
    setRoomOnOff(false);
    clearDotForRoom(room);   // on/off resets colour/temp dots to icons
    if (activeEffect) await clearEffect(room.id);
    sendRoomCommand(room.id, { action: 'off' }, room);
  });
  setRoomOnOff(anyOn);

  const onOffWrap = document.createElement('div');
  onOffWrap.className = 'room-onoff';
  onOffWrap.appendChild(onBtn);
  onOffWrap.appendChild(offBtn);

  // Secondary controls: separate colour (🎨) and temperature (🌡) triggers, floor plan
  const hasTempDevices = roomDevicesAll.some(d => d.color_temp != null);
  const roomModeKey = 'mesh-room-mode-' + room.id;

  // Each domain gets a trigger showing its glyph icon, or a live dot if that
  // domain is the one last set for this room (persists until cleared).
  let colourBtn = null, tempBtn = null;
  if (!empty && hasColour) {
    colourBtn = document.createElement('button');
    colourBtn.className = 'room-action-btn room-ctrl-trigger';
    colourBtn.dataset.role = 'room-colour';
    paintRoomButton(colourBtn, 'colour', roomDevicesAll, room.id);
  }
  if (!empty && hasTempDevices) {
    tempBtn = document.createElement('button');
    tempBtn.className = 'room-action-btn room-ctrl-trigger';
    tempBtn.dataset.role = 'room-temp';
    paintRoomButton(tempBtn, 'temp', roomDevicesAll, room.id);
  }

  const layoutBtn = document.createElement('button');
  layoutBtn.className = 'room-action-btn room-layout-btn';
  layoutBtn.title = 'Floor plan';
  layoutBtn.textContent = '⊞';
  layoutBtn.addEventListener('click', e => {
    e.stopPropagation();
    const eff = roomEffectsMap.get(room.id);
    if (eff) layout.notifyEffectActive(room.id, eff.effect_id, eff.params ?? {});
    layout.openLayout(room);
  });

  // Top row: [On|Off]  ───spacer───  [effect]  🎨  ⊞
  const topRow = document.createElement('div');
  topRow.className = 'room-controls-top';
  topRow.appendChild(onOffWrap);

  const topSpacer = document.createElement('span');
  topSpacer.className = 'room-controls-spacer';
  topRow.appendChild(topSpacer);

  if (activeEffect) {
    topRow.appendChild(buildEffectBadge(room, activeEffect));
  } else if (lastEffectByRoom.has(room.id)) {
    topRow.appendChild(buildEffectGhostBadge(room, lastEffectByRoom.get(room.id)));
  }
  if (colourBtn) topRow.appendChild(colourBtn);
  if (tempBtn) topRow.appendChild(tempBtn);
  topRow.appendChild(layoutBtn);
  ctrlRow.appendChild(topRow);

  // ── Brightness — always visible (top casual control) ─────────────────────
  if (!empty) {
    const briDevices = roomDevicesAll.filter(d => d.brightness != null);
    const avgBri = briDevices.length > 0
      ? Math.round(briDevices.reduce((s, d) => s + (d.brightness ?? 0), 0) / briDevices.length)
      : 200;
    ctrlRow.appendChild(buildSlider({
      label: 'Brightness', min: 1, max: 254, value: avgBri,
      format: v => Math.round((v / 254) * 100) + '%',
      onCommit: async v => {
        if (activeEffect) await clearEffect(room.id);
        sendRoomCommand(room.id, { action: 'brightness', value: v }, room);
      },
    }));
  }

  // Colour / temperature popup. Open-state lives in a module Set (openRoomCtrlIds)
  // so a commit-triggered render() re-opens it. The 🎨 and 🌡 buttons each open
  // the panel in their mode; clicking the active one closes it.
  if (colourBtn || tempBtn) {
    let ctPanel = null;
    const curMode = () => localStorage.getItem(roomModeKey) || (hasTempDevices ? 'temp' : 'colour');

    const syncButtons = () => {
      const m = curMode();
      // Glyph icon, or a live dot for the domain last set for this room.
      paintRoomButton(colourBtn, 'colour', roomDevicesAll, room.id);
      paintRoomButton(tempBtn, 'temp', roomDevicesAll, room.id);
      // Ring the trigger whose panel is open.
      colourBtn?.classList.toggle('active', !!ctPanel && m === 'colour');
      tempBtn?.classList.toggle('active', !!ctPanel && m === 'temp');
      // Disable room drag-to-reorder while the colour wheel is showing.
      card.classList.toggle('room-wheel-open', !!ctPanel && m === 'colour');
    };
    const openPanel = () => {
      if (!ctPanel) {
        ctPanel = buildRoomControlsPanel(room, roomDevicesAll, hasColour, activeEffect, closePanel);
        ctrlRow.appendChild(ctPanel);
      }
      syncButtons();
    };
    // Remove whatever outside-dismiss listener is currently armed (there is only
    // ever one, module-wide — a fresh render of this or any room reuses it, so we
    // never leak a pile of stale document listeners that fire on every pointerdown).
    const disarmOutside = () => {
      if (_roomCtrlDismiss) { document.removeEventListener('pointerdown', _roomCtrlDismiss, true); _roomCtrlDismiss = null; }
    };
    const closePanel = () => {
      openRoomCtrlIds.delete(room.id);
      ctPanel?.remove();
      ctPanel = null;
      card.classList.remove('room-wheel-open');
      disarmOutside();
      syncButtons();
    };
    const armOutside = () => {
      disarmOutside();
      const handler = (e) => {
        if (!ctPanel) return;
        // Allow taps on the panel (incl. the colour wheel / temp bar inside it)
        // and on the colour/temp trigger buttons.
        if (ctPanel.contains(e.target)) return;
        if (colourBtn?.contains(e.target) || tempBtn?.contains(e.target)) return;
        closePanel();
      };
      _roomCtrlDismiss = handler;
      // setTimeout so the tap that opened the panel doesn't immediately close it.
      setTimeout(() => {
        if (ctPanel && _roomCtrlDismiss === handler) document.addEventListener('pointerdown', handler, true);
      }, 0);
    };
    const selectMode = (mode) => {
      if (ctPanel && curMode() === mode) { closePanel(); return; }   // toggle off
      localStorage.setItem(roomModeKey, mode);
      openRoomCtrlIds.add(room.id);
      ctPanel?.remove(); ctPanel = null;   // rebuild in the chosen mode
      openPanel();
      armOutside();
    };

    colourBtn?.addEventListener('click', e => { e.stopPropagation(); selectMode('colour'); });
    tempBtn?.addEventListener('click', e => { e.stopPropagation(); selectMode('temp'); });

    // Restore on render if it was open before, and re-arm the (single) dismiss.
    if (openRoomCtrlIds.has(room.id)) {
      openPanel();
      armOutside();
    }
  }

  // Hide room controls when the effect editor is open
  if (activeEffect && effectEditor.openRoomId === room.id) {
    topRow.style.display = 'none';
  }

  header.appendChild(ctrlRow);
  card.appendChild(header);

  // Effect param editor popover — visible when the badge for this room is clicked
  if (activeEffect && effectEditor.openRoomId === room.id) {
    card.appendChild(buildEffectEditor(room, activeEffect));
  }

  // Quick scenes bar — always visible, horizontal scroll (primary casual control)
  const roomScenesList = model.scenes.filter(s => s.room_id === room.id).sort((a, b) => (a.position - b.position) || (b.created_at - a.created_at));
  if (roomScenesList.length > 0) {
    const sceneWrap = document.createElement('div');
    sceneWrap.className = 'room-scenes-wrap';
    const sceneLabel = document.createElement('span');
    sceneLabel.className = 'room-scenes-label';
    sceneLabel.textContent = 'Scenes';
    sceneWrap.appendChild(sceneLabel);

    const sceneBar = document.createElement('div');
    sceneBar.className = 'room-quick-scenes';
    for (const scene of roomScenesList) {
      const chip = document.createElement('button');
      chip.className = 'room-quick-scene-chip';
      chip.dataset.sceneId = scene.id;
      chip.setAttribute('draggable', 'true');   // mouse only; touch uses wireSceneChipTouchDrag
      chip.textContent = scene.name;
      chip.title = `Recall "${scene.name}"`;
      if (scene.preview_color) {
        const { r, g, b } = xyToRgb(scene.preview_color[0], scene.preview_color[1], 180);
        chip.style.setProperty('--scene-chip-color', `rgb(${r},${g},${b})`);
      }
      if (activeSceneByRoom.get(room.id) === scene.id) {
        chip.classList.add('active');
        // Scenes are a lights-only concept — a sensor sharing the room isn't
        // "in" or "paused from" the scene, so it must not count as a member.
        const memberCount = (room.device_ids ?? [])
          .filter(id => devicesMap.get(id)?.device_type !== 'sensor').length;
        const pausedCount = pausedSceneDevices.get(room.id)?.size ?? 0;
        if (pausedCount > 0 && pausedCount < memberCount) chip.classList.add('partly-paused');
        if (memberCount > 0 && pausedCount >= memberCount) chip.classList.add('all-paused');
      }
      chip.addEventListener('click', e => { e.stopPropagation(); recallScene(scene.id); });
      wireSceneChipTouchDrag(chip, sceneBar);
      sceneBar.appendChild(chip);
    }
    wireSceneBarDrag(sceneBar, room.id);
    sceneWrap.appendChild(sceneBar);
    card.appendChild(sceneWrap);
  }

  // Collapsible body
  const body = document.createElement('div');
  body.className = 'room-body' + (isCollapsed ? ' collapsed' : '');
  collapseBtn.addEventListener('click', e => {
    e.stopPropagation();
    const nowCollapsed = !body.classList.contains('collapsed');
    body.classList.toggle('collapsed', nowCollapsed);
    collapseBtn.textContent = nowCollapsed ? '▸' : '▾';
    setPref(`mesh-room-collapsed-${room.id}`, nowCollapsed ? '1' : '0');
  });

  // Device cards
  // room.device_ids is untyped (any device, light or sensor); the light-only
  // ids drive this list, a placeholder covers a light not yet reported.
  // Sensor members render in their own read-only strip below instead — they
  // have no brightness/on-off shape for buildDeviceCard to work with, and no
  // controls to expose (see buildSensorCard's own comment for why the widget
  // is shared as-is between here and the Devices tab).
  const lightIds = room.device_ids.filter(id => devicesMap.get(id)?.device_type !== 'sensor');
  const devicesEl = document.createElement('div');
  devicesEl.className = 'room-devices';
  if (lightIds.length === 0) {
    const hint = document.createElement('span');
    hint.className = 'room-drop-hint';
    hint.textContent = 'Drop bulbs here';
    devicesEl.appendChild(hint);
  } else {
    for (const deviceId of lightIds) {
      const dev = devicesMap.get(deviceId);
      if (dev) {
        // Device cards are intentionally NOT draggable — accidental drags were
        // yanking bulbs out of rooms. Removal is via the card's ✕ button only.
        // (Assigning a bulb INTO a room still works by dragging it from the
        // Unassigned strip onto the room.)
        const devCard = buildDeviceCard(dev, room.id);
        devicesEl.appendChild(devCard);
      } else {
        devicesEl.appendChild(buildDevicePlaceholder(deviceId, room.id));
      }
    }
  }
  body.appendChild(devicesEl);
  wireDropZone(body, room.id);

  // Sensor members — read-only, room-assignment happens on the Devices tab.
  const roomSensors = room.device_ids
    .map(id => devicesMap.get(id))
    .filter(d => d?.device_type === 'sensor');
  if (roomSensors.length > 0) {
    const sensorsEl = document.createElement('div');
    sensorsEl.className = 'room-sensors';
    for (const dev of roomSensors) {
      sensorsEl.appendChild(buildSensorCard(dev));
    }
    body.appendChild(sensorsEl);
  }

  // Scenes section (full list with save button)
  body.appendChild(buildScenesSection(room.id));

  // Delete room — tucked at the bottom of the body, subtle (rare, destructive)
  const deleteRow = document.createElement('div');
  deleteRow.className = 'room-delete-row';
  const deleteBtn = document.createElement('button');
  deleteBtn.className = 'room-action-btn room-action-delete';
  deleteBtn.textContent = 'Delete room';
  deleteBtn.title = 'Delete room';
  deleteBtn.addEventListener('click', () => deleteRoom(room.id));
  deleteRow.appendChild(deleteBtn);
  body.appendChild(deleteRow);

  card.appendChild(body);

  // Card-level drop handlers: effect drops always land here (body may be collapsed);
  // device-chip drops also land here when the body is collapsed (body has zero height
  // so dragover never fires on it). When expanded, device drops land on body instead
  // and bubble up — by then dragSrc is already cleared, so no double-fire.
  card.addEventListener('dragover', e => {
    const collapsed = body.classList.contains('collapsed');
    if (!effectDrag.src && !(dragSrc && collapsed)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = effectDrag.src ? 'copy' : 'move';
    card.classList.add('room-drop-active');
  });
  card.addEventListener('dragleave', e => {
    if (!card.contains(e.relatedTarget)) card.classList.remove('room-drop-active');
  });
  card.addEventListener('drop', e => {
    card.classList.remove('room-drop-active');
    if (effectDrag.src) {
      e.preventDefault();
      const effect = effectDrag.src;
      effectDrag.src = null;
      activateEffect(room.id, effect);
      return;
    }
    if (dragSrc && body.classList.contains('collapsed')) {
      e.preventDefault();
      const { deviceId, fromRoomId } = dragSrc;
      dragSrc = null;
      if (fromRoomId !== room.id) {
        addDeviceToRoom(room.id, deviceId);
        // Auto-expand so the user can see the newly assigned device
        body.classList.remove('collapsed');
        collapseBtn.textContent = '▾';
        setPref(`mesh-room-collapsed-${room.id}`, '0');
      }
    }
  });

  return card;
}

// ── Scenes section ──────────────────────────────────────────────────────────

// ── Device card inside a room ────────────────────────────────────────────────

function buildDeviceCard(dev, roomId) {
  const card = document.createElement('div');
  const offline = dev.online === false;
  card.className = 'light-card room-device-card' + (offline ? ' device-offline' : '');
  card.dataset.deviceId = dev.device_id;

  const eff = roomEffectsMap.get(roomId);
  const underEffect = eff && !eff.overrides.has(dev.device_id);
  if (underEffect) card.classList.add('device-under-effect');

  // Header: name + remove button
  const displayName = formatDeviceName(dev.device_id);
  const header = document.createElement('div');
  header.className = 'light-card-header';
  header.innerHTML = `
    <div class="light-name-group">
      <span class="light-name" title="Click to rename" style="cursor:pointer">${esc(displayName)}</span>
      <span class="light-node-badge">${esc(dev.node_id)}</span>
    </div>
    <div class="light-card-header-right">
      <button class="room-remove-btn" data-ctrl="room-remove"
              title="Remove from room" aria-label="Remove from room">✕</button>
    </div>`;
  if (offline) {
    const badge = document.createElement('span');
    badge.className = 'badge badge-offline';
    badge.textContent = 'Offline';
    header.querySelector('.light-card-header-right').prepend(badge);
  }
  card.appendChild(header);

  // Rename + remove wiring
  const nameEl2 = card.querySelector('.light-name');
  if (nameEl2) nameEl2.addEventListener('click', e => { e.stopPropagation(); startDeviceRename(nameEl2, dev.device_id); });
  card.querySelector('[data-ctrl="room-remove"]')?.addEventListener('click', e => {
    e.stopPropagation(); removeDeviceFromRoom(roomId, dev.device_id);
  });

  // Per-bulb effect indicator — lit when participating, greyed when paused.
  if (eff) {
    const overridden = eff.overrides.has(dev.device_id);
    const icon = EFFECT_ICONS[eff.effect_id] || DEFAULT_EFFECT_ICON;
    const btn = document.createElement('button');
    btn.className = 'device-effect-btn' + (overridden ? ' device-effect-overridden' : '');
    btn.title = overridden ? 'Paused from effect — click to resume' : 'In effect — click to pause';
    btn.textContent = icon;
    btn.addEventListener('click', e => {
      e.stopPropagation();
      if (overridden) includeInEffect(roomId, dev.device_id);
      else excludeFromEffect(roomId, dev.device_id);
    });
    card.querySelector('.light-card-header-right')?.prepend(btn);
  }

  // Per-bulb scene indicator — present whenever the room has an active scene;
  // lit when this light follows it, greyed when paused. Same model as effects.
  const activeSceneId = activeSceneByRoom.get(roomId);
  if (activeSceneId) {
    const paused = pausedSceneDevices.get(roomId)?.has(dev.device_id) ?? false;
    const sbtn = document.createElement('button');
    sbtn.className = 'device-scene-btn' + (paused ? ' device-scene-paused' : '');
    sbtn.title = paused ? 'Paused from scene — click to resume' : 'In scene — click to pause';
    sbtn.textContent = SCENE_ICON;
    sbtn.addEventListener('click', e => {
      e.stopPropagation();
      toggleSceneDevice(roomId, dev.device_id);
    });
    card.querySelector('.light-card-header-right')?.prepend(sbtn);
  }

  // Controls — only if online and has brightness
  if (!offline && dev.brightness != null) {
    const maybeExclude = () => {
      const e2 = roomEffectsMap.get(roomId);
      if (!e2 || e2.overrides.has(dev.device_id)) return;
      e2.overrides.add(dev.device_id);
      const btn2 = card.querySelector('.device-effect-btn');
      if (btn2) { btn2.classList.add('device-effect-overridden'); btn2.title = 'Paused from effect — click to resume'; }
      api(`/rooms/${encodeURIComponent(roomId)}/effect/override`,
        { method: 'PATCH', body: { device_id: dev.device_id, excluded: true } })
        .catch(err => {
          e2.overrides.delete(dev.device_id);
          if (btn2) { btn2.classList.remove('device-effect-overridden'); btn2.title = 'In effect — click to pause'; }
          showToast(`Override error: ${err.message}`, true);
        });
    };

    const controls = buildLightControls(dev, {
      onOn:  () => { maybeExclude(); for (const c of HUE_DEFAULT_ON) sendDeviceCommand(dev.device_id, c); },
      onOff: () => { maybeExclude(); sendDeviceCommand(dev.device_id, { action: 'off' }); },
      onBrightness: v => {
        maybeExclude();
        patchDevice(dev.device_id, { brightness: v });
        markPending(dev.device_id, 'brightness', v);
        sendDeviceCommand(dev.device_id, { action: 'brightness', value: v, transition_secs: 0.4 });
      },
      onTemp: v => {
        maybeExclude();
        patchDevice(dev.device_id, { color_temp: v });
        markPending(dev.device_id, 'color_temp', v);
        sendDeviceCommand(dev.device_id, { action: 'color_temp', value: v, transition_secs: 0.4 });
      },
      onColorXY: (x, y) => {
        maybeExclude();
        patchDevice(dev.device_id, { color_xy: [x, y] });
        sendDeviceCommand(dev.device_id, { action: 'color_xy', x, y });
      },
    });
    controls.className += ' light-card-details';
    card.appendChild(controls);
  }

  return card;
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
    // Defer DOM mutations that could reflow the source card. Chrome aborts the
    // drag immediately (fires dragend with dropEffect=none before any dragover)
    // if the source element shifts during dragstart — the unassigned strip
    // sits above the room list, so revealing it pushes the dragged card down.
    requestAnimationFrame(() => {
      card.classList.add('dragging');
      document.querySelector(`[data-room-id="${CSS.escape(roomId)}"]`)?.classList.add('drag-leaving');
      const strip = document.getElementById('unassigned-strip');
      if (strip) strip.style.display = '';
    });
    startPulse(deviceId);
  });
  card.addEventListener('dragend', () => {
    card.classList.remove('dragging');
    dragSrc = null;
    card.setAttribute('draggable', 'true');
    card.closest('.room-card')?.setAttribute('draggable', 'true');
    stopPulse();
    document.querySelectorAll('.drag-leaving').forEach(el => el.classList.remove('drag-leaving'));
    // Re-hide the unassigned strip if nothing was dropped into it
    const strip = document.getElementById('unassigned-strip');
    if (strip && strip.querySelectorAll('.room-chip').length === 0) strip.style.display = 'none';
  });
  // Allow effect chips (e.g. solar) to be dropped onto device cards; the drop
  // event bubbles up to the room body's wireDropZone handler which applies the effect.
  card.addEventListener('dragover', e => {
    if (!effectDrag.src) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
  });
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

  // Add delete button for unassigned devices
  if (fromRoomId === 'unassigned') {
    const deleteBtn = document.createElement('button');
    deleteBtn.className = 'room-chip-delete';
    deleteBtn.textContent = '✕';
    deleteBtn.title = 'Delete device';
    deleteBtn.addEventListener('click', e => {
      e.stopPropagation();
      deleteDevice(deviceId);
    });
    chip.appendChild(deleteBtn);
  }

  chip.addEventListener('pointerdown', e => {
    if (e.target !== chip && e.target.closest('button')) chip.setAttribute('draggable', 'false');
  });
  chip.addEventListener('pointerup', () => chip.setAttribute('draggable', 'true'));
  chip.addEventListener('pointercancel', () => chip.setAttribute('draggable', 'true'));

  chip.addEventListener('dragstart', e => {
    dragSrc = { deviceId, fromRoomId };
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', deviceId);
    // Defer DOM mutations that could reflow the source chip — see the matching
    // comment in wireDeviceDrag(). Today the strip is always already visible
    // when this code runs, but the same shape would break if call sites change.
    requestAnimationFrame(() => {
      chip.classList.add('dragging');
      document.querySelector(`[data-room-id="${CSS.escape(fromRoomId)}"]`)?.classList.add('drag-leaving');
      const strip = document.getElementById('unassigned-strip');
      if (strip) strip.style.display = '';
    });
    startPulse(deviceId);
  });
  chip.addEventListener('dragend', () => {
    chip.classList.remove('dragging');
    dragSrc = null;
    stopPulse();
    document.querySelectorAll('.drag-leaving').forEach(el => el.classList.remove('drag-leaving'));
    const strip = document.getElementById('unassigned-strip');
    if (strip && strip.querySelectorAll('.room-chip').length === 0) strip.style.display = 'none';
  });

  return chip;
}

// ── Device reorder within a room ─────────────────────────────────────────────
// Drag any device card vertically to change its display order.  Same-room drops
// are ignored by wireDropZone (fromRoomId === roomId guard), so this handler
// owns them instead.

function wireDeviceReorder(devicesEl, roomId) {
  let reordering = false;

  // Capture phase fires before wireDeviceDrag's stopPropagation on dragstart.
  devicesEl.addEventListener('dragstart', e => {
    if (e.target.closest('.room-device-card[data-device-id]')) reordering = true;
  }, { capture: true });

  devicesEl.addEventListener('dragover', e => {
    if (!reordering) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    const dragging = devicesEl.querySelector('.room-device-card.dragging');
    if (!dragging) return;
    const others = [...devicesEl.querySelectorAll('.room-device-card:not(.dragging)')];
    const after = others.reduce((closest, child) => {
      const box = child.getBoundingClientRect();
      const offset = e.clientY - box.top - box.height / 2;
      if (offset < 0 && offset > closest.offset) return { offset, element: child };
      return closest;
    }, { offset: Number.NEGATIVE_INFINITY }).element;
    if (after == null) devicesEl.appendChild(dragging);
    else devicesEl.insertBefore(dragging, after);
  });

  // Bubble phase: fires after wireDeviceDrag's dragend clears dragSrc,
  // so we use our own `reordering` flag.
  devicesEl.addEventListener('dragend', () => {
    if (!reordering) return;
    reordering = false;
    const ids = [...devicesEl.querySelectorAll('.room-device-card[data-device-id]')]
      .map(c => c.dataset.deviceId).filter(Boolean);
    if (ids.length > 0) reorderRoomDevices(roomId, ids);
  });
}

// ── Drop zones (chip → room assignment) ──────────────────────────────────────

function wireDropZone(el, roomId) {
  el.addEventListener('dragover', e => {
    if (!dragSrc && !effectDrag.src) return; // ignore room-reorder drags
    e.preventDefault();
    e.dataTransfer.dropEffect = effectDrag.src ? 'copy' : 'move';
    el.classList.add('room-drop-active');
  });
  el.addEventListener('dragleave', e => {
    if (!el.contains(e.relatedTarget)) el.classList.remove('room-drop-active');
  });
  el.addEventListener('drop', e => {
    e.preventDefault();
    el.classList.remove('room-drop-active');

    // Effect palette drop (only valid on real rooms, not unassigned)
    if (effectDrag.src && roomId !== 'unassigned') {
      const effect = effectDrag.src;
      effectDrag.src = null;
      activateEffect(roomId, effect);
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
  input.name = 'room-rename';
  input.autocomplete = 'off';
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
// The tok()/api() primitives live in api.js; the wrappers below add per-action
// toasts on failure.

async function sendRoomCommand(roomId, body, room, isGlobal = false) {
  if (!isGlobal) model.globalLight = null;
  clearRoomActiveScene(roomId);
  // Optimistic update — lights only; a room command is a light command, and
  // stamping fake on/brightness fields onto a sensor's cached reading would
  // corrupt it for no reason.
  if (room) {
    for (const deviceId of room.device_ids) {
      const dev = devicesMap.get(deviceId);
      if (dev && dev.device_type !== 'sensor') {
        let updated = dev;
        if (body.action === 'on') updated = { ...updated, on: true };
        else if (body.action === 'off') updated = { ...updated, on: false };
        else if (body.action === 'brightness') updated = { ...updated, brightness: body.value, on: true };
        else if (body.action === 'color_temp') updated = { ...updated, color_temp: body.value };
        else if (body.action === 'color_xy') updated = { ...updated, color_xy: [body.x, body.y] };
        devicesMap.set(deviceId, updated);
      }
    }
    // Skip the rebuild for colour picks: the wheel already sits where the user
    // released it, and re-deriving its position from the stored CIE xy
    // (xy → rgb → hsl) isn't lossless — saturated hues snap to the gamut edge.
    // The next WS state report settles the rest of the card.
    if (body.action !== 'color_xy') render();
  }
  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/command`, { method: 'POST', body });
    if (!res.ok) {
      if (res.status === 503) showToast('Some devices offline — others updated', false);
      else showToast(`Room command failed (${res.status})`, true);
    }
  } catch (e) { showToast(`Room command error: ${e.message}`, true); }
}

async function sendDeviceCommand(deviceId, body, opts = {}) {
  model.globalLight = null;
  // Per-device scene pause/resume keeps the room's scene active for other lights,
  // so it must NOT clear the active scene the way a manual command does.
  if (!opts.keepScene) {
    const owningRoom = model.rooms.find(r => r.device_ids.includes(deviceId));
    if (owningRoom) clearRoomActiveScene(owningRoom.id);
  }
  try {
    const res = await api(`/lights/${encodeURIComponent(deviceId)}/command`, { method: 'POST', body });
    if (!res.ok) {
      const text = await res.text().catch(() => '');
      showToast(`Command failed (${res.status})${text ? ': ' + text : ''}`, true);
    }
  } catch (e) { showToast(`Command error: ${e.message}`, true); }
}

// ── Utilities ────────────────────────────────────────────────────────────────

function formatDeviceName(id) {
  return model.names.get(id) ?? id.replace(/[_-]+/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
}

function startDeviceRename(nameEl, deviceId) {
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

