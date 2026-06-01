// ── Rooms panel ──────────────────────────────────────────────────────────────
// First-class spatial room objects with drag-and-drop device assignment
// and drag-to-reorder room cards.

// Prevent click-to-jump on any range slider — user must grab the thumb.
// Standalone helper for sliders not built via attachThumbSlider (e.g. the
// layout time-scrubber). Exported for layout.js.
export function lockSliderToThumb(slider) {
  slider.addEventListener('pointerdown', e => {
    const rect = slider.getBoundingClientRect();
    const ratio = (slider.value - slider.min) / (slider.max - slider.min);
    const thumbX = rect.left + ratio * rect.width;
    if (Math.abs(e.clientX - thumbX) > (e.pointerType === 'touch' ? 30 : 16)) {
      e.preventDefault();
      e.stopPropagation();
    }
  }, { capture: true });
}

import * as layout from '/static/layout.js';

let roomsData = [];
let devicesMap = new Map();
let globalLightState = null;  // 'on' | 'off' | null — tracks last All On/Off press, cleared on any individual change
let scenesData = [];
let deviceNamesMap = new Map();
let dragSrc = null;             // chip drag: { deviceId, fromRoomId }
let roomDragId = null;          // room reorder drag: room id being dragged
let effectDragSrc = null;       // effect palette drag: effect_id e.g. 'solar'
let effectRemoveRoomId = null;  // effect badge drag: room id whose effect is being dragged off
let effectDragIsPermanent = false; // true when dragging a ghost (paused) badge → permanent delete
const lastEffectByRoom = new Map(); // roomId → { effect_id, params } — paused/remembered state
const openPickerIds = new Set();      // device IDs whose colour picker is currently open
const openRoomCtrlIds = new Set();    // room IDs whose 🎨 colour/temp panel is open (survives render)
const activeSceneByRoom = new Map();      // roomId → sceneId of last-recalled scene
const preSceneStateByRoom = new Map();    // roomId → Map<deviceId, snapshot> before last recall

// ── F-Effects-2: effects catalogue + active-effect map ──────────────────────
// Catalogue is fetched once on dashboard load from GET /api/effects and never
// changes at runtime; the active-effect map is driven by EffectUpdate events.
let effectsCatalog = [];                       // [{ id, display_name, description, category, params_schema, default_params }, ...]
const effectsById = new Map();                 // id → metadata
const roomEffectsMap = new Map();              // room_id → { effect_id, params }
let openEffectEditorRoomId = null;             // id of room whose param editor popover is open
const EFFECT_ICONS = {                         // static icon per effect_id; falls back to ✦ for unknown
  solar: '☀',           // ☀
  sunset: '\u{1F305}',       // 🌅
  sunrise: '\u{1F304}',      // 🌄
  candlelight: '\u{1F56F}',  // 🕯
  aurora: '\u{1F30C}',       // 🌌
  breathing: '\u{1FAC1}',    // 🫁
  snake: '\u{1F40D}',        // 🐍
};
const DEFAULT_EFFECT_ICON = '✨';          // ✨
let activeSceneEdit = null;           // { roomId, value } when a scene name input is open
let _lcOpenDismiss = null;            // collapse fn of the currently-open temp/colour section (only one at a time)
let _sceneReorderTimer = null;

// Pending optimistic command values per (deviceId, field). Each entry { value, ts }.
// Overlaid onto incoming WS snapshots so the slider doesn't snap back to the
// pre-command server value while the round-trip is still in flight.
const pendingCommands = new Map();
const PENDING_TTL_MS = 2000;

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

// ── Effect badge drag-to-remove ───────────────────────────────────────────────
// Dragging an active-effect badge off the room card removes the effect.
// The document-level handlers accept the drag everywhere so the user can drop
// on any empty area. Pressing Escape fires dragend without drop, cancelling.
document.addEventListener('dragover', e => {
  if (!effectRemoveRoomId) return;
  e.preventDefault();
  e.dataTransfer.dropEffect = 'move';
});
document.addEventListener('drop', e => {
  if (!effectRemoveRoomId) return;
  e.preventDefault();
  const roomId = effectRemoveRoomId;
  const permanent = effectDragIsPermanent;
  effectRemoveRoomId = null;
  effectDragIsPermanent = false;
  openEffectEditorRoomId = null;
  if (permanent) removeEffect(roomId);
  else clearEffect(roomId); // pause — remembers config
});
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
    const panel = document.getElementById('panel-lighting');
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
fetchEffectsCatalog();

async function fetchEffectsCatalog() {
  try {
    const res = await fetch(`/api/effects?token=${encodeURIComponent(tok())}`);
    if (!res.ok) return;
    const list = await res.json();
    effectsCatalog = Array.isArray(list) ? list : [];
    effectsById.clear();
    for (const eff of effectsCatalog) effectsById.set(eff.id, eff);
    render();
  } catch (_) {}
}

export function handleEffectUpdate(evt) {
  const { room_id, effect_id, params, overrides } = evt;
  if (!room_id) return;
  if (effect_id == null) {
    const existing = roomEffectsMap.get(room_id);
    roomEffectsMap.delete(room_id);
    // Server-initiated clear (e.g. device offline): store paused state if not
    // already stored by a user-triggered clearEffect call.
    if (existing && !lastEffectByRoom.has(room_id)) {
      lastEffectByRoom.set(room_id, {
        effect_id: existing.effect_id,
        params: { ...existing.params },
      });
    }
  } else {
    roomEffectsMap.set(room_id, {
      effect_id,
      params: params ?? {},
      overrides: new Set(Array.isArray(overrides) ? overrides : []),
    });
    lastEffectByRoom.delete(room_id); // effect is live — no longer paused
  }
  layout.notifyEffectActive(room_id, effect_id, params ?? {});
  render();
}

export function handleRoomsUpdate(evt) {
  roomsData = evt.rooms ?? [];
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
  for (const dev of devices) {
    const reconciled = reconcilePending(dev);
    devicesMap.set(dev.device_id, reconciled);
    layout.notifyDeviceUpdate(dev.device_id, reconciled);
  }
  inferZigbeeStatus();
  // Skip full re-render while a slider is being dragged to prevent mid-drag jumps
  if (document.querySelector('.slider-active')) return;
  patchDeviceCards();
  refreshRoomColourDots();
}

// Paint a room's colour trigger: a coloured dot when every bulb shares one
// colour, otherwise the 🎨 palette icon. Shared by the initial render and the
// live refresh so the two never drift.
function paintRoomColourDot(btn, devices) {
  const uniform = roomUniformColour(devices);
  if (uniform) {
    btn.classList.add('room-colour-dot');
    btn.style.background = `hsl(${uniform.h},${uniform.s}%,50%)`;
    btn.textContent = '';
  } else {
    btn.classList.remove('room-colour-dot');
    btn.style.background = '';
    btn.textContent = '🎨';
  }
}

// Keep each room's colour trigger in sync with its bulbs without a full render:
// when every bulb in the room shares one colour (e.g. after a scene is recalled)
// the icon becomes a dot of that colour; otherwise it falls back to the palette.
function refreshRoomColourDots() {
  for (const room of roomsData) {
    const card = document.querySelector(`.room-card[data-room-id="${CSS.escape(room.id)}"]`);
    const btn = card?.querySelector('.room-ctrl-trigger[data-role="room-colour"]');
    if (!btn) continue;
    const devs = room.device_ids.map(id => devicesMap.get(id)).filter(Boolean);
    paintRoomColourDot(btn, devs);
  }
}

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

    // Always update colour swatch.
    if (dev.color_xy) {
      const [x, y] = dev.color_xy;
      const { r, g, b } = xyToRgb(x, y, dev.brightness ?? 254);
      const swatchBtn = card.querySelector('[data-ctrl="colour-toggle"]');
      if (swatchBtn) swatchBtn.style.background = `rgb(${r},${g},${b})`;
    }

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
  // If we have rooms but zero devices have ever arrived, zigbee2mqtt never
  // connected — treat as offline.
  if (roomsData.length > 0 && devicesMap.size === 0) {
    zigbeeOnline = false;
    return;
  }
  // If every known device is offline, the bridge is almost certainly down.
  if (devicesMap.size > 0 && [...devicesMap.values()].every(d => !d.online)) {
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
  const container = document.getElementById('lighting-list');
  if (!container || dragSrc || roomDragId) return;
  if (container.querySelector('.layout-view')) return; // layout open — don't wipe
  if (container.querySelector('.room-slider-input.slider-active')) return; // room slider thumb being dragged — don't wipe it out
  if (container.querySelector('.colour-wheel.dragging')) return; // colour wheel being dragged — don't wipe it out
  // NOTE: device-card sliders (.lc-slider) are not guarded here — the common
  // WS-update path (notifyDevices) already bails on any .slider-active before
  // patchDeviceCards. Only a rare full render() mid-device-drag is unguarded;
  // widen this selector to `.slider-active` if that edge case ever bites.
  inferZigbeeStatus();

  const assigned = new Set(roomsData.flatMap(r => r.device_ids));
  const unassigned = [...devicesMap.keys()].filter(id => !assigned.has(id));

  container.innerHTML = '';
  if (!zigbeeOnline) {
    const banner = document.createElement('div');
    banner.id = 'zigbee-banner';
    banner.className = 'zigbee-offline-banner';
    banner.textContent = '⚠ Zigbee bridge offline — lights unavailable';
    container.appendChild(banner);
  }
  if (roomsData.length > 0) container.appendChild(renderGlobalControls());
  container.appendChild(renderEffectsPalette());
  container.appendChild(renderNewRoomBtn());
  container.appendChild(renderUnassigned(unassigned));

  const roomList = document.createElement('div');
  roomList.className = 'room-list rooms-layout-root' + (zigbeeOnline ? '' : ' zigbee-offline');

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
  allOnBtn.className = 'room-action-btn' + (globalLightState === 'on' ? ' room-action-active-on' : '');
  allOnBtn.textContent = 'All On';
  allOnBtn.addEventListener('click', () => {
    globalLightState = 'on';
    layout.freezeIconUpdates(3000);
    for (const r of roomsData) sendRoomCommand(r.id, { action: 'on' }, r, true);
  });

  const allOffBtn = document.createElement('button');
  allOffBtn.className = 'room-action-btn' + (globalLightState === 'off' ? ' room-action-active-off' : '');
  allOffBtn.textContent = 'All Off';
  allOffBtn.addEventListener('click', () => {
    globalLightState = 'off';
    layout.freezeIconUpdates(3000);
    for (const r of roomsData) sendRoomCommand(r.id, { action: 'off' }, r, true);
  });

  bar.appendChild(allOnBtn);
  bar.appendChild(allOffBtn);
  return bar;
}

// ── Shared slider core ────────────────────────────────────────────────────────
const SLIDER_THUMB_W = 18;

// The single thumb-only slider interaction used by every slider in the UI:
//  • value changes ONLY by grabbing the thumb (track clicks pass through so
//    card-level gestures like drag-to-reorder still fire)
//  • a value bubble follows the thumb while dragging
//  • the slider carries `.slider-active` (and the container `.dragging`) so
//    render() won't wipe the element out from under the user mid-drag
// slider: <input type=range>. opts: { format, bubble?, container?, onInput?(v), onChange(v) }
function attachThumbSlider(slider, { format, bubble, container, onInput, onChange }) {
  const positionBubble = () => {
    if (!bubble) return;
    const min = parseFloat(slider.min), max = parseFloat(slider.max);
    const ratio = (slider.value - min) / (max - min);
    const w = slider.getBoundingClientRect().width;
    const centre = SLIDER_THUMB_W / 2 + ratio * (w - SLIDER_THUMB_W);
    // offsetLeft is correct here because the bubble's offsetParent is the same
    // positioned ancestor offsetLeft is measured against, and no slider sits
    // inside a transformed parent. If that changes, switch to getBoundingClientRect
    // deltas (bubble vs offsetParent) for transform-safe positioning.
    bubble.style.left = `${slider.offsetLeft + centre}px`;
    bubble.textContent = format(parseInt(slider.value, 10));
  };

  slider.addEventListener('pointerdown', e => {
    const rect = slider.getBoundingClientRect();
    const min = parseFloat(slider.min), max = parseFloat(slider.max);
    const ratio = (slider.value - min) / (max - min);
    const thumbCentre = rect.left + SLIDER_THUMB_W / 2 + ratio * (rect.width - SLIDER_THUMB_W);
    const hitRadius = e.pointerType === 'touch' ? 26 : 16;
    if (Math.abs(e.clientX - thumbCentre) > hitRadius) { e.preventDefault(); return; }
    slider.classList.add('slider-active');
    container?.classList.add('dragging');
    bubble?.classList.add('visible');
    positionBubble();
  }, { capture: true });

  slider.addEventListener('input', () => {
    positionBubble();
    onInput?.(parseInt(slider.value, 10));
  });

  const finish = () => {
    slider.classList.remove('slider-active');
    container?.classList.remove('dragging');
    bubble?.classList.remove('visible');
  };
  slider.addEventListener('change', () => { finish(); onChange(parseInt(slider.value, 10)); });
  slider.addEventListener('pointercancel', finish);
}

// Full-width slider with its own label/value header and floating bubble.
// opts: { label, min, max, value, format(v)->string, onCommit(v)->void, onInput?(v)->void }
export function buildSlider(opts) {
  const { label, min, max, value, format, onCommit, onInput } = opts;

  const container = document.createElement('div');
  container.className = 'room-slider';

  const headerRow = document.createElement('div');
  headerRow.className = 'room-slider-header';
  const labelEl = document.createElement('span');
  labelEl.className = 'room-slider-label';
  labelEl.textContent = label;
  const valueEl = document.createElement('span');
  valueEl.className = 'room-slider-current-value';
  valueEl.textContent = format(value);
  headerRow.append(labelEl, valueEl);
  container.appendChild(headerRow);

  const track = document.createElement('div');
  track.className = 'room-slider-track';
  const bubble = document.createElement('div');
  bubble.className = 'room-slider-bubble';
  bubble.textContent = format(value);

  const slider = document.createElement('input');
  slider.type = 'range';
  slider.min = String(min);
  slider.max = String(max);
  slider.value = String(value);
  slider.className = 'light-slider room-slider-input';
  slider.title = label;

  attachThumbSlider(slider, {
    format, bubble, container,
    onInput: v => { valueEl.textContent = format(v); onInput?.(v); },
    onChange: v => onCommit(v),
  });

  track.append(bubble, slider);
  container.appendChild(track);
  return container;
}

// Wire the shared interaction onto an existing inline <input type=range> (device
// cards, where the row HTML is fixed). Exported for lighting.js.
// opts: { format(v)->string, onInput(v, valEl)->void, onChange(v)->void }
export function wireDeviceSlider(slider, opts) {
  const { format, onInput, onChange } = opts;
  const valEl = slider.parentElement?.querySelector('.light-detail-value');

  const bubble = document.createElement('div');
  bubble.className = 'room-slider-bubble device-slider-bubble';
  slider.parentElement?.insertBefore(bubble, slider);

  attachThumbSlider(slider, {
    format, bubble,
    onInput: v => { if (valEl) onInput(v, valEl); },
    onChange: v => onChange(v),
  });
}

// ── Common light control card ─────────────────────────────────────────────────
// Renders a standardised control block usable in both the lighting panel and
// room device cards:
//   Row 1 (always): [On] [Off]  ───────────────────────────────  [● mode dot]
//   Row 2 (always): Brightness   ──────────●──────  78%
//   Row 3 (one of): Temperature OR Hue+Saturation — chosen by the mode dot
//
// The dot toggles between Temperature and Colour. Mode is persisted per device
// in localStorage (`mesh-mode-<id>`) — it can't be inferred from state because
// Hue bulbs always report both color_xy and color_temp. Adjusting either slider
// pins its mode so the card stays put across the next render.
//
// dev: LightStateReport-shaped object
// cb:  { onOn, onOff, onBrightness(v), onTemp(v), onColorXY(x,y) }
//      each callback fires on committed value (change event)
export function buildLightControls(dev, cb) {
  const hasTemp = dev.color_temp != null;

  const wrap = document.createElement('div');
  wrap.className = 'lc-wrap';

  // ── Row 1: on/off + toggle buttons ───────────────────────────────────────
  const row1 = document.createElement('div');
  row1.className = 'lc-row lc-row-controls';

  const onBtn = document.createElement('button');
  onBtn.className = 'light-toggle-btn';
  onBtn.innerHTML = `<span class="badge ${dev.on ? 'badge-green' : 'badge-muted'}">On</span>`;
  onBtn.addEventListener('click', e => { e.stopPropagation(); cb.onOn?.(); });

  const offBtn = document.createElement('button');
  offBtn.className = 'light-toggle-btn';
  offBtn.innerHTML = `<span class="badge ${!dev.on ? 'badge-red' : 'badge-muted'}">Off</span>`;
  offBtn.addEventListener('click', e => { e.stopPropagation(); cb.onOff?.(); });

  row1.appendChild(onBtn);
  row1.appendChild(offBtn);

  // Spacer
  const spacer = document.createElement('span');
  spacer.style.flex = '1';
  row1.appendChild(spacer);

  // ── Single colour dot — switches between temperature and colour mode ─────
  // Hue bulbs always report BOTH color_xy and color_temp, so the active mode
  // can't be inferred from state — it's persisted per device in localStorage
  // (default Temperature). Adjusting a slider also pins that mode so the card
  // stays put after the next render.
  const supportsTemp   = hasTemp;
  const supportsColour = dev.color_xy != null;
  const supportsBoth   = supportsTemp && supportsColour;

  const modeKey = 'mesh-mode-' + dev.device_id;
  let lcMode = (!supportsTemp && !supportsColour) ? null
    : (localStorage.getItem(modeKey) || (supportsTemp ? 'temp' : 'colour'));
  if (lcMode === 'temp'   && !supportsTemp)   lcMode = 'colour';
  if (lcMode === 'colour' && !supportsColour) lcMode = 'temp';
  const setMode = (m) => { lcMode = m; localStorage.setItem(modeKey, m); };

  if (lcMode) {
    let h = 30, s = 80;
    if (dev.color_xy) {
      const [x, y] = dev.color_xy;
      const { r, g, b } = xyToRgb(x, y, dev.brightness ?? 254);
      ({ h, s } = rgbToHsl(r, g, b));
    }
    // Two mode buttons side by side (only when the bulb supports both): a
    // warm→cool "temperature" swatch and a colour swatch. Tap one to switch.
    let tempBtn = null, colourBtn = null;
    if (supportsBoth) {
      tempBtn = document.createElement('button');
      tempBtn.className = 'lc-mode-btn';
      tempBtn.title = 'Temperature';
      tempBtn.style.background = TEMP_GRADIENT;

      colourBtn = document.createElement('button');
      colourBtn.className = 'lc-mode-btn';
      colourBtn.title = 'Colour';
      colourBtn.style.background = `hsl(${h},${s}%,50%)`;

      const modeGroup = document.createElement('div');
      modeGroup.className = 'lc-mode-group';
      modeGroup.append(tempBtn, colourBtn);
      row1.appendChild(modeGroup);
    }

    // Secondary rows — only one visible at a time
    let tempRow = null, colourRows = null;

    if (supportsTemp) {
      tempRow = document.createElement('div');
      tempRow.className = 'lc-row lc-slider-row';
      const tempLabel = document.createElement('span');
      tempLabel.className = 'lc-label';
      tempLabel.textContent = 'Temperature';
      const tempVal = document.createElement('span');
      tempVal.className = 'lc-value';
      const fmtK = v => Math.round(1e6 / v) + 'K';
      tempVal.textContent = fmtK(dev.color_temp ?? 370);
      const tempBar = buildTempBar({
        mireds: dev.color_temp ?? 370,
        onInput: v => { tempVal.textContent = fmtK(v); },
        onChange: v => { tempVal.textContent = fmtK(v); setMode('temp'); applyMode(); cb.onTemp?.(v); },
      });
      tempRow.append(tempLabel, tempBar, tempVal);
    }

    if (supportsColour) {
      colourRows = document.createElement('div');
      colourRows.className = 'lc-colour-wheel-wrap';
      colourRows.appendChild(buildColourWheel({
        hue: h, sat: s,
        onInput: (hh, ss) => { if (colourBtn) colourBtn.style.background = `hsl(${hh},${ss}%,50%)`; },
        onChange: (hh, ss) => {
          setMode('colour'); applyMode();
          if (colourBtn) colourBtn.style.background = `hsl(${hh},${ss}%,50%)`;
          const { x, y } = hslToXy(hh, ss);
          cb.onColorXY?.(x, y);
        },
      }));
    }

    // The secondary control can be collapsed: tapping the active swatch again
    // hides it, and so does a tap anywhere outside the open section (popover
    // dismiss). Single-capability bulbs (no swatches) always show their control.
    let lcExpanded = !supportsBoth;
    let lcOutside = null;
    const disarmOutside = () => {
      if (lcOutside) { document.removeEventListener('pointerdown', lcOutside, true); lcOutside = null; }
    };
    const applyMode = () => {
      const showTemp   = supportsTemp   && lcMode === 'temp'   && lcExpanded;
      const showColour = supportsColour && lcMode === 'colour' && lcExpanded;
      if (tempRow)    tempRow.style.display    = showTemp   ? '' : 'none';
      if (colourRows) colourRows.style.display = showColour ? '' : 'none';
      tempBtn?.classList.toggle('active', showTemp);
      colourBtn?.classList.toggle('active', showColour);
    };
    const collapse = () => {
      if (!lcExpanded) return;
      lcExpanded = false;
      applyMode();
      disarmOutside();
      if (_lcOpenDismiss === collapse) _lcOpenDismiss = null;
    };
    const expand = (mode) => {
      // Close any other card's open section first (one popover at a time).
      if (_lcOpenDismiss && _lcOpenDismiss !== collapse) _lcOpenDismiss();
      setMode(mode);
      lcExpanded = true;
      applyMode();
      _lcOpenDismiss = collapse;
      // Arm dismissal: a tap outside the open section (and off the swatches)
      // closes it. Capture phase so we see the tap before the wheel/bar's own
      // pointerdown calls stopPropagation.
      disarmOutside();
      lcOutside = (e) => {
        const sect = lcMode === 'temp' ? tempRow : colourRows;
        if (sect?.contains(e.target)) return;
        if (tempBtn?.contains(e.target) || colourBtn?.contains(e.target)) return;
        collapse();
      };
      document.addEventListener('pointerdown', lcOutside, true);
    };
    const pickMode = (mode) => {
      if (lcMode === mode && lcExpanded) collapse();   // tap active again → hide
      else expand(mode);
    };
    tempBtn?.addEventListener('click', e => { e.stopPropagation(); pickMode('temp'); });
    colourBtn?.addEventListener('click', e => { e.stopPropagation(); pickMode('colour'); });

    // Assemble once: controls row, brightness, then the active secondary section
    const briRow = makeLcSliderRow('Brightness', 1, 254, dev.brightness ?? 200,
      v => Math.round((v / 254) * 100) + '%',
      v => cb.onBrightness?.(v));
    wrap.append(row1, briRow);
    if (tempRow)    wrap.appendChild(tempRow);
    if (colourRows) wrap.appendChild(colourRows);
    applyMode();
  } else {
    // No secondary controls at all — just brightness
    const briRow = makeLcSliderRow('Brightness', 1, 254, dev.brightness ?? 200,
      v => Math.round((v / 254) * 100) + '%',
      v => cb.onBrightness?.(v));
    wrap.append(row1, briRow);
  }

  return wrap;
}

// Build one label + slider + value row for buildLightControls
function makeLcSliderRow(label, min, max, value, format, onCommit) {
  const row = document.createElement('div');
  row.className = 'lc-row lc-slider-row';

  const labelEl = document.createElement('span');
  labelEl.className = 'lc-label';
  labelEl.textContent = label;

  const valEl = document.createElement('span');
  valEl.className = 'lc-value';
  valEl.textContent = format(value);

  const slider = document.createElement('input');
  slider.type = 'range';
  slider.min = String(min);
  slider.max = String(max);
  slider.value = String(value);
  slider.className = 'light-slider lc-slider';

  wireDeviceSlider(slider, {
    format,
    onInput: (v, _) => { valEl.textContent = format(v); },
    onChange: onCommit,
  });

  row.appendChild(labelEl);
  row.appendChild(slider);
  row.appendChild(valEl);
  return row;
}

// ── Colour wheel ──────────────────────────────────────────────────────────────
// A Hue/Saturation wheel (the "ball thing" from Hue): angle around the circle is
// hue (0° at top, clockwise), distance from the centre is saturation (centre =
// white, edge = full). Grab the knob (or tap anywhere) and drag.
//   opts: { hue, sat, onInput?(h,s), onChange(h,s) }
// onInput fires live during the drag (for preview); onChange fires on release.
// While dragging, the wheel carries `.dragging` so render() won't wipe it mid-drag.
export function buildColourWheel({ hue, sat, onInput, onChange }) {
  const wheel = document.createElement('div');
  wheel.className = 'colour-wheel';

  const knob = document.createElement('div');
  knob.className = 'colour-wheel-knob';
  wheel.appendChild(knob);

  let curH = hue, curS = sat;

  const placeKnob = () => {
    const r = curS / 100;                 // 0..1 from centre
    const rad = (curH * Math.PI) / 180;   // 0° at top, clockwise
    const x = 50 + r * Math.sin(rad) * 50;
    const y = 50 - r * Math.cos(rad) * 50;
    knob.style.left = `${x}%`;
    knob.style.top = `${y}%`;
    knob.style.background = `hsl(${curH},${curS}%,50%)`;
  };

  const fromPointer = (e) => {
    const rect = wheel.getBoundingClientRect();
    const dx = e.clientX - (rect.left + rect.width / 2);
    const dy = e.clientY - (rect.top + rect.height / 2);
    const R = rect.width / 2;
    curS = Math.round(Math.min(Math.hypot(dx, dy) / R, 1) * 100);
    let deg = (Math.atan2(dx, -dy) * 180) / Math.PI; // 0 at top, clockwise
    if (deg < 0) deg += 360;
    curH = Math.round(deg);
    placeKnob();
  };

  wireDragSurface(wheel, {
    fromPointer,
    onInput: () => onInput?.(curH, curS),
    onChange: () => onChange(curH, curS),
  });

  placeKnob();
  return wheel;
}

// Shared pointer machinery for the direct-manipulation colour controls (the
// colour wheel and the temperature bar). Grabbing the surface captures the
// pointer, disables native drag on EVERY draggable ancestor (device card AND
// room card) — otherwise the gesture is hijacked into a card reorder, no
// pointerup fires, and the value never commits (symptom: handle snaps back,
// light doesn't change) — and marks `.dragging` so render() won't wipe the
// control out from under the user mid-drag.
//   el: the draggable surface. fromPointer(e) reads the pointer and updates
//   internal state + visuals. onInput fires live during the drag (preview);
//   onChange fires once on release (commit).
function wireDragSurface(el, { fromPointer, onInput, onChange }) {
  let dragging = false;
  let suppressedDrags = [];
  const suppressAncestorDrags = () => {
    suppressedDrags = [];
    for (let p = el.parentElement; p; p = p.parentElement) {
      if (p.getAttribute && p.getAttribute('draggable') === 'true') {
        p.setAttribute('draggable', 'false');
        suppressedDrags.push(p);
      }
    }
  };
  const restoreAncestorDrags = () => {
    suppressedDrags.forEach(p => p.setAttribute('draggable', 'true'));
    suppressedDrags = [];
  };

  el.addEventListener('pointerdown', e => {
    e.preventDefault();
    e.stopPropagation();
    dragging = true;
    el.classList.add('dragging');
    suppressAncestorDrags();
    try { el.setPointerCapture(e.pointerId); } catch { /* older browsers */ }
    fromPointer(e);
    onInput?.();
  });
  el.addEventListener('pointermove', e => {
    if (!dragging) return;
    fromPointer(e);
    onInput?.();
  });
  // Belt-and-braces: cancel any native drag that still tries to start.
  el.addEventListener('dragstart', e => e.preventDefault());
  const end = (e) => {
    if (!dragging) return;
    dragging = false;
    el.classList.remove('dragging');
    restoreAncestorDrags();
    try { el.releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    onChange();
  };
  el.addEventListener('pointerup', end);
  el.addEventListener('pointercancel', end);
}

// ── Temperature bar ────────────────────────────────────────────────────────────
// A warm→cool track you can tap or drag anywhere along (no thumb-hit gate — the
// generic sliders are thumb-only, which is what made the old temperature control
// impossible to grab). The handle shows the live colour at the current
// temperature. Range is 154–500 mireds (cool ≈ 6500K → warm ≈ 2000K).
const TEMP_MIN_MIRED = 154, TEMP_MAX_MIRED = 500;
// Gradient sampled from the real mireds→colour mapping so the bar shows the
// actual perceived colour: cool/white on the left (154), warm/amber on the
// right (500). Single source of truth for the bar and the mode swatch.
const TEMP_GRADIENT = (() => {
  const stops = [];
  for (let i = 0; i <= 6; i++) {
    const m = TEMP_MIN_MIRED + (i / 6) * (TEMP_MAX_MIRED - TEMP_MIN_MIRED);
    stops.push(layout.ctToHex(m));
  }
  return `linear-gradient(to right, ${stops.join(', ')})`;
})();

// opts: { mireds, onInput?(m), onChange(m) }
// onInput fires live during the drag; onChange fires on release.
export function buildTempBar({ mireds, onInput, onChange }) {
  const bar = document.createElement('div');
  bar.className = 'temp-bar';
  bar.style.background = TEMP_GRADIENT;

  const handle = document.createElement('div');
  handle.className = 'temp-bar-handle';
  bar.appendChild(handle);

  let cur = mireds;
  const place = () => {
    const ratio = (cur - TEMP_MIN_MIRED) / (TEMP_MAX_MIRED - TEMP_MIN_MIRED);
    handle.style.left = `${ratio * 100}%`;
    handle.style.background = layout.ctToHex(cur);
  };
  const fromPointer = (e) => {
    const rect = bar.getBoundingClientRect();
    const ratio = Math.min(Math.max((e.clientX - rect.left) / rect.width, 0), 1);
    cur = Math.round(TEMP_MIN_MIRED + ratio * (TEMP_MAX_MIRED - TEMP_MIN_MIRED));
    place();
  };
  wireDragSurface(bar, {
    fromPointer,
    onInput: () => onInput?.(cur),
    onChange: () => onChange(cur),
  });
  place();
  return bar;
}

// ── Effects palette ──────────────────────────────────────────────────────────

function renderEffectsPalette() {
  const palette = document.createElement('div');
  palette.className = 'effects-palette';
  palette.title = 'Drag into room';

  const label = document.createElement('span');
  label.className = 'effects-palette-label';
  label.textContent = 'Effects';
  palette.appendChild(label);

  if (effectsCatalog.length === 0) {
    const hint = document.createElement('span');
    hint.className = 'effects-palette-hint';
    hint.textContent = 'Loading…';
    palette.appendChild(hint);
    return palette;
  }

  for (const meta of effectsCatalog) {
    palette.appendChild(buildEffectChip(meta));
  }
  return palette;
}

function buildEffectBadge(room, activeEffect) {
  const meta = effectsById.get(activeEffect.effect_id);
  const badge = document.createElement('span');
  badge.className = 'badge badge-effect';
  badge.dataset.effect = activeEffect.effect_id;
  badge.setAttribute('draggable', 'true');
  const icon = EFFECT_ICONS[activeEffect.effect_id] || DEFAULT_EFFECT_ICON;
  const name = meta?.display_name || activeEffect.effect_id;
  badge.textContent = `${icon} ${name}`;
  badge.style.cursor = 'pointer';
  badge.title = `${name} active — click for options`;
  badge.addEventListener('click', e => {
    e.stopPropagation();
    openEffectEditorRoomId = openEffectEditorRoomId === room.id ? null : room.id;
    render();
  });
  badge.addEventListener('dragstart', e => {
    e.stopPropagation();
    effectRemoveRoomId = room.id;
    effectDragIsPermanent = false; // drag active badge = pause
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', `effect-remove:${room.id}`);
    requestAnimationFrame(() => badge.classList.add('dragging'));
  });
  badge.addEventListener('dragend', () => {
    effectRemoveRoomId = null;
    effectDragIsPermanent = false;
    badge.classList.remove('dragging');
  });
  return badge;
}

function buildEffectGhostBadge(room, last) {
  const meta = effectsById.get(last.effect_id);
  const badge = document.createElement('span');
  badge.className = 'badge badge-effect badge-effect-paused';
  badge.setAttribute('draggable', 'true');
  const icon = EFFECT_ICONS[last.effect_id] || DEFAULT_EFFECT_ICON;
  const name = meta?.display_name || last.effect_id;
  badge.textContent = `${icon} ${name}`;
  badge.style.cursor = 'pointer';
  badge.title = `${name} paused — click to resume, drag off to remove`;
  badge.addEventListener('click', e => {
    e.stopPropagation();
    activateEffect(room.id, last.effect_id, last.params);
    openEffectEditorRoomId = room.id;
  });
  badge.addEventListener('dragstart', e => {
    e.stopPropagation();
    effectRemoveRoomId = room.id;
    effectDragIsPermanent = true; // drag ghost badge = permanent remove
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', `effect-remove:${room.id}`);
    requestAnimationFrame(() => badge.classList.add('dragging'));
  });
  badge.addEventListener('dragend', () => {
    effectRemoveRoomId = null;
    effectDragIsPermanent = false;
    badge.classList.remove('dragging');
  });
  return badge;
}

function buildEffectEditor(room, activeEffect) {
  const meta = effectsById.get(activeEffect.effect_id);
  const wrap = document.createElement('div');
  wrap.className = 'effect-editor';

  const header = document.createElement('div');
  header.className = 'effect-editor-header';
  const title = document.createElement('span');
  title.className = 'effect-editor-title';
  const icon = EFFECT_ICONS[activeEffect.effect_id] || DEFAULT_EFFECT_ICON;
  title.textContent = `${icon} ${meta?.display_name || activeEffect.effect_id}`;
  header.appendChild(title);

  const closeBtn = document.createElement('button');
  closeBtn.className = 'effect-editor-close';
  closeBtn.textContent = '×';
  closeBtn.title = 'Close';
  closeBtn.addEventListener('click', e => {
    e.stopPropagation();
    openEffectEditorRoomId = null;
    render();
  });
  header.appendChild(closeBtn);
  wrap.appendChild(header);

  if (meta?.description) {
    const desc = document.createElement('p');
    desc.className = 'effect-editor-desc';
    desc.textContent = meta.description;
    wrap.appendChild(desc);
  }

  // Params form — empty schema renders nothing here. Full JSON-Schema → form
  // arrives with F-Effects-2.4 (Sunset) when there's a first non-trivial schema
  // to drive it. For now we handle the schema subset the plan declared and
  // skip everything else gracefully so unsupported effects don't 500.
  const schema = meta?.params_schema;
  const params = { ...(activeEffect.params || {}) };
  const propEntries = schemaProperties(schema);
  if (propEntries.length > 0) {
    const form = document.createElement('div');
    form.className = 'effect-editor-form';
    let dirty = false;
    for (const [key, spec] of propEntries) {
      const field = buildSchemaField(key, spec, params, () => { dirty = true; });
      if (field) form.appendChild(field);
    }
    wrap.appendChild(form);

    const btnRow = document.createElement('div');
    btnRow.className = 'effect-editor-btn-row';

    const defaultsBtn = document.createElement('button');
    defaultsBtn.className = 'effect-editor-defaults';
    defaultsBtn.textContent = 'Defaults';
    defaultsBtn.title = 'Reset all params to their default values';
    defaultsBtn.addEventListener('click', e => {
      e.stopPropagation();
      const defaults = meta?.default_params ?? {};
      const entry = roomEffectsMap.get(room.id);
      if (entry) entry.params = { ...defaults };
      render();
    });
    btnRow.appendChild(defaultsBtn);

    const apply = document.createElement('button');
    apply.className = 'effect-editor-apply';
    apply.textContent = 'Apply';
    apply.addEventListener('click', e => {
      e.stopPropagation();
      if (dirty) activateEffect(room.id, activeEffect.effect_id, params);
      openEffectEditorRoomId = null;
      render();
    });
    btnRow.appendChild(apply);
    wrap.appendChild(btnRow);
  }

  const removeBtn = document.createElement('button');
  removeBtn.className = 'effect-editor-disable';
  removeBtn.textContent = 'Remove effect';
  removeBtn.title = 'Permanently remove this effect from the room';
  removeBtn.addEventListener('click', e => {
    e.stopPropagation();
    removeEffect(room.id);
  });
  wrap.appendChild(removeBtn);
  return wrap;
}

function schemaProperties(schema) {
  if (!schema || typeof schema !== 'object') return [];
  const props = schema.properties;
  if (!props || typeof props !== 'object') return [];
  return Object.entries(props);
}

function formatSliderValue(v, type) {
  if (type === 'integer') return String(Math.round(v));
  const n = parseFloat(v);
  return Number.isInteger(n) ? String(n) : n.toFixed(2).replace(/\.?0+$/, '');
}

function buildSchemaField(key, spec, paramsObj, onChange) {
  if (!spec || typeof spec !== 'object') return null;
  const row = document.createElement('label');
  row.className = 'effect-editor-row';
  const labelEl = document.createElement('span');
  labelEl.className = 'effect-editor-label';
  labelEl.textContent = key;
  row.appendChild(labelEl);

  const current = paramsObj[key] ?? spec.default;

  if (spec.type === 'integer' || spec.type === 'number') {
    const input = document.createElement('input');
    input.type = 'range';
    // JSON Schema uses `minimum`/`maximum`; tolerate legacy `min`/`max` too.
    const lo = spec.minimum ?? spec.min;
    const hi = spec.maximum ?? spec.max;
    if (lo != null) input.min = lo;
    if (hi != null) input.max = hi;
    input.step = spec.type === 'integer' ? 1 : 0.01;
    input.value = current ?? spec.default ?? lo ?? 0;
    const valueEl = document.createElement('span');
    valueEl.className = 'effect-editor-value';
    valueEl.textContent = formatSliderValue(input.value, spec.type);
    input.addEventListener('input', () => {
      const v = spec.type === 'integer' ? parseInt(input.value, 10) : parseFloat(input.value);
      valueEl.textContent = formatSliderValue(v, spec.type);
      paramsObj[key] = v;
      onChange();
    });
    // No lockSliderToThumb here — the effect editor is a focused popup where
    // click-anywhere-on-track is expected, unlike device card sliders on a
    // draggable card.
    row.appendChild(input);
    row.appendChild(valueEl);
    paramsObj[key] = paramsObj[key] ?? (spec.type === 'integer' ? parseInt(input.value, 10) : parseFloat(input.value));
    return row;
  }
  if (spec.type === 'string' && Array.isArray(spec.enum)) {
    const group = document.createElement('span');
    group.className = 'effect-editor-segmented';
    for (const opt of spec.enum) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.textContent = opt;
      if (opt === current) btn.classList.add('selected');
      btn.addEventListener('click', () => {
        paramsObj[key] = opt;
        group.querySelectorAll('button').forEach(b => b.classList.toggle('selected', b === btn));
        onChange();
      });
      group.appendChild(btn);
    }
    row.appendChild(group);
    paramsObj[key] = current ?? spec.enum[0];
    return row;
  }
  if (spec.type === 'boolean') {
    const cb = document.createElement('input');
    cb.type = 'checkbox';
    cb.checked = !!current;
    cb.addEventListener('change', () => { paramsObj[key] = cb.checked; onChange(); });
    row.appendChild(cb);
    paramsObj[key] = !!current;
    return row;
  }
  // Unsupported field — render nothing rather than 500 on apply.
  return null;
}

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
  const tempSliderEl = buildSlider({
    label: 'Temperature',
    min: 154, max: 500, value: avgCT,
    format: v => Math.round(1e6 / v) + 'K',
    onCommit: async v => {
      setMode('temp');
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
      onChange: (hh, ss) => {
        setMode('colour');
        const { x, y } = hslToXy(hh, ss);
        sendRoomCommand(room.id, { action: 'color_xy', x, y }, room);
      },
    }));
  }
  panel.appendChild(colourSliderEl);

  return panel;
}

function buildEffectChip(meta) {
  const chip = document.createElement('div');
  chip.className = 'effect-chip';
  chip.setAttribute('draggable', 'true');
  chip.dataset.effect = meta.id;
  const icon = EFFECT_ICONS[meta.id] || DEFAULT_EFFECT_ICON;
  chip.textContent = `${icon} ${meta.display_name}`;
  chip.title = meta.description || `Drag onto a room to activate ${meta.display_name}`;

  chip.addEventListener('dragstart', e => {
    effectDragSrc = meta.id;
    e.dataTransfer.effectAllowed = 'copy';
    e.dataTransfer.setData('text/plain', `effect:${meta.id}`);
    requestAnimationFrame(() => chip.classList.add('dragging'));
  });
  chip.addEventListener('dragend', () => {
    effectDragSrc = null;
    chip.classList.remove('dragging');
  });
  wireEffectChipTouchDrag(chip, meta.id);
  return chip;
}

// Touch / pen drag for effect chips. Native HTML5 drag never fires from a
// finger, so on phones the chips would be inert. This mirrors wireChipTouchDrag
// (used for bulbs): past an 8px threshold a floating ghost follows the finger,
// the room card under it highlights, and releasing over a room applies the
// effect. Mouse falls through to the native DnD path already wired above.
function wireEffectChipTouchDrag(chip, effectId) {
  const EDGE = 80, MAX_SPEED = 16;                  // edge auto-scroll, mirrors the native-drag one
  let startX = 0, startY = 0;
  let dragging = false;
  let ghost = null;
  let pointerId = null;
  let lastCard = null;
  let scrollRaf = null, scrollSpeed = 0, scrollTarget = null;

  const cardUnder = (x, y) => {
    // The ghost has pointer-events:none, so elementFromPoint sees through it.
    const card = document.elementFromPoint(x, y)?.closest('.room-card');
    return (card?.dataset.roomId && card.dataset.roomId !== 'unassigned') ? card : null;
  };
  const highlight = (card) => {
    if (lastCard === card) return;
    lastCard?.classList.remove('room-drop-active');
    card?.classList.add('room-drop-active');
    lastCard = card;
  };
  // Auto-scroll the room list when the finger nears the top/bottom edge, so a
  // target card below the fold can be reached (the native-drag autoscroller
  // keys off dragover events, which pointer drags never fire).
  const stopScroll = () => { if (scrollRaf) cancelAnimationFrame(scrollRaf); scrollRaf = null; scrollSpeed = 0; scrollTarget = null; };
  const edgeScroll = (y) => {
    const h = window.innerHeight;
    scrollSpeed = y < EDGE ? -MAX_SPEED * (1 - y / EDGE)
                : y > h - EDGE ?  MAX_SPEED * (1 - (h - y) / EDGE) : 0;
    if (!scrollSpeed) { stopScroll(); return; }
    if (!scrollTarget) scrollTarget = document.getElementById('panel-lighting') || document.scrollingElement || document.documentElement;
    if (!scrollRaf) {
      const tick = () => {
        if (!scrollSpeed) { scrollRaf = null; return; }
        scrollTarget?.scrollBy(0, scrollSpeed);
        scrollRaf = requestAnimationFrame(tick);
      };
      scrollRaf = requestAnimationFrame(tick);
    }
  };
  const cleanup = () => {
    if (ghost) { ghost.remove(); ghost = null; }
    highlight(null);
    stopScroll();
    effectDragSrc = null;
    dragging = false;
    pointerId = null;
  };

  chip.addEventListener('pointerdown', e => {
    if (e.pointerType === 'mouse') return;            // mouse uses native HTML5 DnD
    if (e.button !== 0 && e.button !== -1) return;
    startX = e.clientX; startY = e.clientY;
    pointerId = e.pointerId;
    dragging = false;
    chip.setPointerCapture(e.pointerId);
  });

  chip.addEventListener('pointermove', e => {
    if (e.pointerType === 'mouse') return;
    if (e.pointerId !== pointerId) return;
    if (!dragging) {
      if (Math.hypot(e.clientX - startX, e.clientY - startY) < 8) return;
      dragging = true;
      effectDragSrc = effectId;
      ghost = chip.cloneNode(true);
      ghost.style.position = 'fixed';
      ghost.style.transform = 'translate(-50%, -50%)';
      ghost.style.pointerEvents = 'none';
      ghost.style.opacity = '0.85';
      ghost.style.zIndex = '9999';
      ghost.style.boxShadow = '0 4px 16px rgba(0,0,0,0.4)';
      document.body.appendChild(ghost);
      e.preventDefault();
    }
    ghost.style.left = `${e.clientX}px`;
    ghost.style.top = `${e.clientY}px`;
    highlight(cardUnder(e.clientX, e.clientY));
    edgeScroll(e.clientY);
  });

  const finish = e => {
    if (e.pointerType === 'mouse') return;
    if (e.pointerId !== pointerId) return;
    if (chip.hasPointerCapture(e.pointerId)) chip.releasePointerCapture(e.pointerId);
    if (dragging) {
      const card = cardUnder(e.clientX, e.clientY);
      if (card) activateEffect(card.dataset.roomId, effectId);
    }
    cleanup();
  };
  chip.addEventListener('pointerup', finish);
  chip.addEventListener('pointercancel', finish);
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
    const room = roomsData.find(r => r.id === id);
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
    if (effectDragSrc) { e.preventDefault(); return; } // let effect drag pass through
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
    const ghostCount = document.createElement('span');
    ghostCount.className = 'room-drag-ghost-count';
    ghostCount.textContent = room.device_ids.length
      ? `${room.device_ids.length} bulb${room.device_ids.length !== 1 ? 's' : ''}`
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

  // Shared state for header
  const roomDevicesAll = room.device_ids.map(id => devicesMap.get(id)).filter(Boolean);
  const anyOn = roomDevicesAll.some(d => d.on);
  const hasColour = roomDevicesAll.some(d => d.color_xy != null);
  const activeEffect = roomEffectsMap.get(room.id) || null;
  const empty = room.device_ids.length === 0;

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
    // Power on with sensible defaults so the light is visibly on
    if (activeEffect) await clearEffect(room.id);
    await sendRoomCommand(room.id, { action: 'brightness', value: 200 }, room);
    await sendRoomCommand(room.id, { action: 'color_temp', value: 370 }, room);
    await sendRoomCommand(room.id, { action: 'on' }, room);
  });
  offBtn.className = 'room-onoff-btn room-onoff-off';
  offBtn.textContent = 'Off';
  offBtn.disabled = empty;
  if (!empty) offBtn.addEventListener('click', async e => {
    e.stopPropagation();
    setRoomOnOff(false);
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

  let colourBtn = null, tempBtn = null;
  if (!empty && hasColour) {
    colourBtn = document.createElement('button');
    colourBtn.className = 'room-action-btn room-ctrl-trigger';
    colourBtn.dataset.role = 'room-colour';
    colourBtn.title = 'Colour';
    // Coloured dot when all bulbs match, else the palette icon (shared helper).
    paintRoomColourDot(colourBtn, roomDevicesAll);
  }
  if (!empty && hasTempDevices) {
    tempBtn = document.createElement('button');
    tempBtn.className = 'room-action-btn room-ctrl-trigger';
    tempBtn.title = 'Temperature';
    tempBtn.textContent = '🌡';
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
    const closePanel = () => {
      openRoomCtrlIds.delete(room.id);
      ctPanel?.remove();
      ctPanel = null;
      card.classList.remove('room-wheel-open');
      syncButtons();
    };
    const selectMode = (mode) => {
      if (ctPanel && curMode() === mode) { closePanel(); return; }   // toggle off
      localStorage.setItem(roomModeKey, mode);
      openRoomCtrlIds.add(room.id);
      ctPanel?.remove(); ctPanel = null;   // rebuild in the chosen mode
      openPanel();
    };

    colourBtn?.addEventListener('click', e => { e.stopPropagation(); selectMode('colour'); });
    tempBtn?.addEventListener('click', e => { e.stopPropagation(); selectMode('temp'); });

    // Restore on render if it was open before
    if (openRoomCtrlIds.has(room.id)) openPanel();
  }

  // Hide room controls when the effect editor is open
  if (activeEffect && openEffectEditorRoomId === room.id) {
    topRow.style.display = 'none';
  }

  header.appendChild(ctrlRow);
  card.appendChild(header);

  // Effect param editor popover — visible when the badge for this room is clicked
  if (activeEffect && openEffectEditorRoomId === room.id) {
    card.appendChild(buildEffectEditor(room, activeEffect));
  }

  // Quick scenes bar — always visible, horizontal scroll (primary casual control)
  const roomScenesList = scenesData.filter(s => s.room_id === room.id).sort((a, b) => (a.position - b.position) || (b.created_at - a.created_at));
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
    if (!effectDragSrc && !(dragSrc && collapsed)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = effectDragSrc ? 'copy' : 'move';
    card.classList.add('room-drop-active');
  });
  card.addEventListener('dragleave', e => {
    if (!card.contains(e.relatedTarget)) card.classList.remove('room-drop-active');
  });
  card.addEventListener('drop', e => {
    card.classList.remove('room-drop-active');
    if (effectDragSrc) {
      e.preventDefault();
      const effect = effectDragSrc;
      effectDragSrc = null;
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
        localStorage.setItem(`mesh-room-collapsed-${room.id}`, '0');
      }
    }
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

// Returns { h, s } only when EVERY colour-capable bulb in the room shares the
// same colour (within tolerance) — so the room can show a single colour dot.
// Returns null when bulbs disagree or none are colour-capable.
function roomUniformColour(roomDevices) {
  const cols = roomDevices.filter(d => d.color_xy != null).map(d => d.color_xy);
  if (cols.length === 0) return null;
  const [x0, y0] = cols[0];
  const same = cols.every(([x, y]) => Math.abs(x - x0) < 0.02 && Math.abs(y - y0) < 0.02);
  if (!same) return null;
  const { r, g, b } = xyToRgb(x0, y0, 254);
  return rgbToHsl(r, g, b);
}

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

  // Per-bulb effect indicator
  if (eff) {
    const overridden = eff.overrides.has(dev.device_id);
    const icon = EFFECT_ICONS[eff.effect_id] || DEFAULT_EFFECT_ICON;
    const btn = document.createElement('button');
    btn.className = 'device-effect-btn' + (overridden ? ' device-effect-overridden' : '');
    btn.title = overridden ? 'Excluded from effect — click to re-include' : 'In effect';
    btn.textContent = icon;
    btn.addEventListener('click', e => {
      e.stopPropagation();
      if (overridden) includeInEffect(roomId, dev.device_id);
      else excludeFromEffect(roomId, dev.device_id);
    });
    card.querySelector('.light-card-header-right')?.prepend(btn);
  }

  // Controls — only if online and has brightness
  if (!offline && dev.brightness != null) {
    const maybeExclude = () => {
      const e2 = roomEffectsMap.get(roomId);
      if (!e2 || e2.overrides.has(dev.device_id)) return;
      e2.overrides.add(dev.device_id);
      const btn2 = card.querySelector('.device-effect-btn');
      if (btn2) { btn2.classList.add('device-effect-overridden'); btn2.title = 'Excluded from effect — click to re-include'; }
      fetch(`/api/rooms/${encodeURIComponent(roomId)}/effect/override?token=${encodeURIComponent(tok())}`,
        { method: 'PATCH', headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ device_id: dev.device_id, excluded: true }) })
        .catch(err => {
          e2.overrides.delete(dev.device_id);
          if (btn2) { btn2.classList.remove('device-effect-overridden'); btn2.title = 'In effect'; }
          showToast(`Override error: ${err.message}`, true);
        });
    };

    const controls = buildLightControls(dev, {
      onOn:  () => { maybeExclude(); sendDeviceCommand(dev.device_id, { action: 'on' }); },
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
    if (!effectDragSrc) return;
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

async function deleteDevice(deviceId) {
  try {
    const res = await fetch(`/api/lights/${encodeURIComponent(deviceId)}?token=${encodeURIComponent(tok())}`, {
      method: 'DELETE',
    });
    if (!res.ok) showToast(`Delete device failed (${res.status})`, true);
  } catch (e) { showToast(`Delete device error: ${e.message}`, true); }
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

async function reorderRoomDevices(roomId, ids) {
  try {
    const res = await fetch(`/api/rooms/${encodeURIComponent(roomId)}/devices/reorder?token=${encodeURIComponent(tok())}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ids }),
    });
    if (!res.ok) showToast(`Device reorder failed (${res.status})`, true);
  } catch (e) { showToast(`Device reorder error: ${e.message}`, true); }
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

// ── Per-bulb effect overrides ─────────────────────────────────────────────────

async function setEffectOverride(roomId, deviceId, excluded) {
  const eff = roomEffectsMap.get(roomId);
  if (!eff) return;
  // Optimistic update.
  if (excluded) eff.overrides.add(deviceId); else eff.overrides.delete(deviceId);
  render();
  try {
    const res = await fetch(
      `/api/rooms/${encodeURIComponent(roomId)}/effect/override?token=${encodeURIComponent(tok())}`,
      { method: 'PATCH', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ device_id: deviceId, excluded }) },
    );
    if (!res.ok) throw new Error(`${res.status}`);
  } catch (e) {
    // Roll back the optimistic change.
    if (excluded) eff.overrides.delete(deviceId); else eff.overrides.add(deviceId);
    render();
    showToast(`Override error: ${e.message}`, true);
  }
}

function excludeFromEffect(roomId, deviceId) { return setEffectOverride(roomId, deviceId, true);  }
function includeInEffect(roomId, deviceId)  { return setEffectOverride(roomId, deviceId, false); }

async function activateEffect(roomId, effectId, params = null) {
  const room = roomsData.find(r => r.id === roomId);
  if (!room) return;

  lastEffectByRoom.delete(roomId); // resuming or fresh activation — clear paused state

  // Optimistic UI: stamp the active effect into the local map so the badge
  // appears immediately. The WS EffectUpdate that follows confirms it.
  roomEffectsMap.set(roomId, { effect_id: effectId, params: params ?? {}, overrides: new Set() });
  render();

  try {
    const body = params != null
      ? { effect_id: effectId, params }
      : { effect_id: effectId };
    const res = await fetch(`/api/rooms/${roomId}/effect?token=${encodeURIComponent(tok())}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      const detail = await res.text().catch(() => '');
      showToast(`Effect failed (${res.status}) ${detail}`.trim(), true);
    }
  } catch (e) { showToast(`Effect error: ${e.message}`, true); }
}

async function clearEffect(roomId) {
  // Remember the current effect so it can be resumed via the ghost badge.
  const eff = roomEffectsMap.get(roomId);
  if (eff) lastEffectByRoom.set(roomId, { effect_id: eff.effect_id, params: { ...eff.params } });

  roomEffectsMap.delete(roomId);
  render();

  try {
    const res = await fetch(`/api/rooms/${roomId}/effect?token=${encodeURIComponent(tok())}`, {
      method: 'DELETE',
    });
    if (!res.ok) showToast(`Effect disable failed (${res.status})`, true);
  } catch (e) { showToast(`Effect disable error: ${e.message}`, true); }
}

async function removeEffect(roomId) {
  lastEffectByRoom.delete(roomId);
  roomEffectsMap.delete(roomId);
  openEffectEditorRoomId = null;
  render();
  try {
    const res = await fetch(`/api/rooms/${roomId}/effect?token=${encodeURIComponent(tok())}`, {
      method: 'DELETE',
    });
    if (!res.ok) showToast(`Effect remove failed (${res.status})`, true);
  } catch (e) { showToast(`Effect remove error: ${e.message}`, true); }
}

async function sendRoomCommand(roomId, body, room, isGlobal = false) {
  if (!isGlobal) globalLightState = null;
  clearRoomActiveScene(roomId);
  // Optimistic update
  if (room) {
    for (const deviceId of room.device_ids) {
      const dev = devicesMap.get(deviceId);
      if (dev) {
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
  globalLightState = null;
  const owningRoom = roomsData.find(r => r.device_ids.includes(deviceId));
  if (owningRoom) clearRoomActiveScene(owningRoom.id);
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

  // Toggle: clicking the active scene reverts to pre-scene state.
  if (roomId && activeSceneByRoom.get(roomId) === id) {
    // Cancel any effect that may have been re-enabled since the scene was recalled.
    if (roomEffectsMap.has(roomId)) await clearEffect(roomId);
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

  // Snapshot BEFORE cancelling the effect so we capture the true pre-effect
  // light state, not the effect's last output which is still in devicesMap.
  if (roomId) {
    const room = roomsData.find(r => r.id === roomId);
    const snap = new Map();
    for (const deviceId of (room?.device_ids ?? [])) {
      const dev = devicesMap.get(deviceId);
      if (dev) snap.set(deviceId, { on: dev.on, brightness: dev.brightness ?? null, color_xy: dev.color_xy ?? null, color_temp: dev.color_temp ?? null });
    }
    preSceneStateByRoom.set(roomId, snap);
  }

  // Cancel any running effect — do this after the snapshot so the snapshot
  // reflects actual light state, not a post-cancel transition.
  if (roomId && roomEffectsMap.has(roomId)) {
    openEffectEditorRoomId = null; // close stale editor so stale params can't be re-applied
    await clearEffect(roomId);
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
