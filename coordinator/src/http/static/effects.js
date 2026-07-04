// ── Effects ────────────────────────────────────────────────────────────────────
// The room-effects domain: the draggable effects palette, the active/paused
// badges, the param-editor popover (JSON-Schema → form), and the activate/clear/
// remove/override REST actions. Split out of rooms.js; the room-card renderer in
// rooms.js calls the builders below, and this module calls back into the renderer
// through an injected `render` (initEffects) rather than importing rooms.js — so
// the import graph stays one-directional (rooms → effects → state, no cycle).
//
// Shared mutable UI state (the in-flight drag source, the open-editor room) lives
// in state.js holder objects so rooms.js and this module mutate the same refs;
// effectsCatalog is private here (written and read only within this module).

import * as layout from '/static/layout.js';
import { createPointerDrag, makeGhost, moveGhost } from '/static/drag.js';
import { api } from '/static/api.js';
import { showToast } from '/static/util.js';
import {
  model, effectsById, roomEffectsMap, lastEffectByRoom,
  EFFECT_ICONS, DEFAULT_EFFECT_ICON,
  effectDrag, effectEditor,
} from '/static/state.js';

// Catalogue is fetched once on dashboard load from GET /api/effects and never
// changes at runtime; the active-effect map (effectsById/roomEffectsMap) lives
// in state.js and is driven by EffectUpdate events.
let effectsCatalog = [];   // [{ id, display_name, description, category, params_schema, default_params }, ...]

// rooms.js owns render(); it's injected here so this module never imports rooms.
let _render = () => {};
export function initEffects({ render }) { _render = render; }

export async function fetchEffectsCatalog() {
  try {
    const res = await api('/effects');
    if (!res.ok) return;
    const list = await res.json();
    effectsCatalog = Array.isArray(list) ? list : [];
    effectsById.clear();
    for (const eff of effectsCatalog) effectsById.set(eff.id, eff);
    _render();
  } catch (_) {}
}

export function handleEffectUpdate(evt) {
  const { room_id, effect_id, params, overrides } = evt;
  if (!room_id) return;
  if (effect_id == null) {
    const existing = roomEffectsMap.get(room_id);
    roomEffectsMap.delete(room_id);
    // An out-of-band clear (another device/script, or a device going offline)
    // pulled the effect out from under us — drop any open param editor for this
    // room so it can't silently resurrect when an effect is later re-activated.
    if (effectEditor.openRoomId === room_id) effectEditor.openRoomId = null;
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
  _render();
}

// ── Effect badge drag-to-remove ───────────────────────────────────────────────
// Dragging an active-effect badge off the room card removes the effect.
// The document-level handlers accept the drag everywhere so the user can drop
// on any empty area. Pressing Escape fires dragend without drop, cancelling.
document.addEventListener('dragover', e => {
  if (!effectDrag.removeRoomId) return;
  e.preventDefault();
  e.dataTransfer.dropEffect = 'move';
});
document.addEventListener('drop', e => {
  if (!effectDrag.removeRoomId) return;
  e.preventDefault();
  const roomId = effectDrag.removeRoomId;
  const permanent = effectDrag.isPermanent;
  effectDrag.removeRoomId = null;
  effectDrag.isPermanent = false;
  effectEditor.openRoomId = null;
  if (permanent) removeEffect(roomId);
  else clearEffect(roomId); // pause — remembers config
});

// ── Palette + chips ───────────────────────────────────────────────────────────
export function renderEffectsPalette() {
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

function buildEffectChip(meta) {
  const chip = document.createElement('div');
  chip.className = 'effect-chip';
  chip.setAttribute('draggable', 'true');
  chip.dataset.effect = meta.id;
  const icon = EFFECT_ICONS[meta.id] || DEFAULT_EFFECT_ICON;
  chip.textContent = `${icon} ${meta.display_name}`;
  chip.title = meta.description || `Drag onto a room to activate ${meta.display_name}`;

  chip.addEventListener('dragstart', e => {
    effectDrag.src = meta.id;
    e.dataTransfer.effectAllowed = 'copy';
    e.dataTransfer.setData('text/plain', `effect:${meta.id}`);
    requestAnimationFrame(() => chip.classList.add('dragging'));
  });
  chip.addEventListener('dragend', () => {
    effectDrag.src = null;
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
// Touch drag of an effect chip onto a room card. Mouse uses native HTML5 DnD.
function wireEffectChipTouchDrag(chip, effectId) {
  const EDGE = 80, MAX_SPEED = 16;                  // edge auto-scroll, mirrors the native-drag one
  let ghost = null, lastCard = null;
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
  // Auto-scroll the room list when the finger nears the top/bottom edge so a
  // target card below the fold can still be reached.
  const stopScroll = () => { if (scrollRaf) cancelAnimationFrame(scrollRaf); scrollRaf = null; scrollSpeed = 0; scrollTarget = null; };
  const edgeScroll = (y) => {
    const h = window.innerHeight;
    scrollSpeed = y < EDGE ? -MAX_SPEED * (1 - y / EDGE)
                : y > h - EDGE ?  MAX_SPEED * (1 - (h - y) / EDGE) : 0;
    if (!scrollSpeed) { stopScroll(); return; }
    if (!scrollTarget) scrollTarget = document.getElementById('panel-home') || document.scrollingElement || document.documentElement;
    if (!scrollRaf) {
      const tick = () => {
        if (!scrollSpeed) { scrollRaf = null; return; }
        scrollTarget?.scrollBy(0, scrollSpeed);
        scrollRaf = requestAnimationFrame(tick);
      };
      scrollRaf = requestAnimationFrame(tick);
    }
  };

  createPointerDrag(chip, {
    holdMs: 150, distance: 8,   // press-hold to drag; a quick swipe scrolls the palette
    onStart: () => { effectDrag.src = effectId; ghost = makeGhost(chip); },
    onMove: (e) => {
      moveGhost(ghost, e.clientX, e.clientY);
      highlight(cardUnder(e.clientX, e.clientY));
      edgeScroll(e.clientY);
    },
    onEnd: (e) => {
      const card = cardUnder(e.clientX, e.clientY);
      if (card) activateEffect(card.dataset.roomId, effectId);
      ghost?.remove(); ghost = null;
      highlight(null);
      stopScroll();
      effectDrag.src = null;
    },
  });
}

// ── Active / paused badges ────────────────────────────────────────────────────
export function buildEffectBadge(room, activeEffect) {
  const meta = effectsById.get(activeEffect.effect_id);
  const badge = document.createElement('span');
  badge.className = 'badge badge-effect';
  badge.dataset.effect = activeEffect.effect_id;
  badge.setAttribute('draggable', 'true');
  const icon = EFFECT_ICONS[activeEffect.effect_id] || DEFAULT_EFFECT_ICON;
  const name = meta?.display_name || activeEffect.effect_id;
  badge.textContent = `${icon} ${name}`;
  badge.style.cursor = 'pointer';
  badge.title = `${name} active — click for options, drag off to remove`;
  badge.addEventListener('click', e => {
    e.stopPropagation();
    effectEditor.openRoomId = effectEditor.openRoomId === room.id ? null : room.id;
    _render();
  });
  badge.addEventListener('dragstart', e => {
    e.stopPropagation();
    effectDrag.removeRoomId = room.id;
    effectDrag.isPermanent = true; // drag active badge off = permanent remove
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', `effect-remove:${room.id}`);
    requestAnimationFrame(() => badge.classList.add('dragging'));
  });
  badge.addEventListener('dragend', () => {
    effectDrag.removeRoomId = null;
    effectDrag.isPermanent = false;
    badge.classList.remove('dragging');
  });
  return badge;
}

export function buildEffectGhostBadge(room, last) {
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
    effectEditor.openRoomId = room.id;
  });
  badge.addEventListener('dragstart', e => {
    e.stopPropagation();
    effectDrag.removeRoomId = room.id;
    effectDrag.isPermanent = true; // drag ghost badge = permanent remove
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', `effect-remove:${room.id}`);
    requestAnimationFrame(() => badge.classList.add('dragging'));
  });
  badge.addEventListener('dragend', () => {
    effectDrag.removeRoomId = null;
    effectDrag.isPermanent = false;
    badge.classList.remove('dragging');
  });
  return badge;
}

// ── Param editor ──────────────────────────────────────────────────────────────
export function buildEffectEditor(room, activeEffect) {
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
    effectEditor.openRoomId = null;
    _render();
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
      _render();
    });
    btnRow.appendChild(defaultsBtn);

    const apply = document.createElement('button');
    apply.className = 'effect-editor-apply';
    apply.textContent = 'Apply';
    apply.addEventListener('click', e => {
      e.stopPropagation();
      if (dirty) activateEffect(room.id, activeEffect.effect_id, params);
      effectEditor.openRoomId = null;
      _render();
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
    input.autocomplete = 'off';
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
    cb.autocomplete = 'off';
    cb.addEventListener('change', () => { paramsObj[key] = cb.checked; onChange(); });
    row.appendChild(cb);
    paramsObj[key] = !!current;
    return row;
  }
  // Unsupported field — render nothing rather than 500 on apply.
  return null;
}

// ── Activate / clear / remove / override ──────────────────────────────────────
export async function activateEffect(roomId, effectId, params = null) {
  const room = model.rooms.find(r => r.id === roomId);
  if (!room) return;

  lastEffectByRoom.delete(roomId); // resuming or fresh activation — clear paused state

  // Optimistic UI: stamp the active effect into the local map so the badge
  // appears immediately. The WS EffectUpdate that follows confirms it.
  roomEffectsMap.set(roomId, { effect_id: effectId, params: params ?? {}, overrides: new Set() });
  _render();

  try {
    const body = params != null
      ? { effect_id: effectId, params }
      : { effect_id: effectId };
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/effect`, { method: 'POST', body });
    if (!res.ok) {
      const detail = await res.text().catch(() => '');
      showToast(`Effect failed (${res.status}) ${detail}`.trim(), true);
    }
  } catch (e) { showToast(`Effect error: ${e.message}`, true); }
}

export async function clearEffect(roomId) {
  // Remember the current effect so it can be resumed via the ghost badge.
  const eff = roomEffectsMap.get(roomId);
  if (eff) lastEffectByRoom.set(roomId, { effect_id: eff.effect_id, params: { ...eff.params } });

  roomEffectsMap.delete(roomId);
  _render();

  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/effect`, { method: 'DELETE' });
    if (!res.ok) showToast(`Effect disable failed (${res.status})`, true);
  } catch (e) { showToast(`Effect disable error: ${e.message}`, true); }
}

async function removeEffect(roomId) {
  lastEffectByRoom.delete(roomId);
  roomEffectsMap.delete(roomId);
  effectEditor.openRoomId = null;
  _render();
  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/effect`, { method: 'DELETE' });
    if (!res.ok) showToast(`Effect remove failed (${res.status})`, true);
  } catch (e) { showToast(`Effect remove error: ${e.message}`, true); }
}

async function setEffectOverride(roomId, deviceId, excluded) {
  const eff = roomEffectsMap.get(roomId);
  if (!eff) return;
  // Optimistic update.
  if (excluded) eff.overrides.add(deviceId); else eff.overrides.delete(deviceId);
  _render();
  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/effect/override`, {
      method: 'PATCH', body: { device_id: deviceId, excluded },
    });
    if (!res.ok) throw new Error(`${res.status}`);
  } catch (e) {
    // Roll back the optimistic change.
    if (excluded) eff.overrides.delete(deviceId); else eff.overrides.add(deviceId);
    _render();
    showToast(`Override error: ${e.message}`, true);
  }
}

export function excludeFromEffect(roomId, deviceId) { return setEffectOverride(roomId, deviceId, true);  }
export function includeInEffect(roomId, deviceId)  { return setEffectOverride(roomId, deviceId, false); }
