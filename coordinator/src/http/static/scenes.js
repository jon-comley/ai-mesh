// ── Scenes ─────────────────────────────────────────────────────────────────────
// The scenes domain: the save/recall scene section, the quick-scene chip drag
// reorder, per-bulb scene pause/resume, and the active-chip state. Split out of
// rooms.js; the room-card renderer calls the builders/wirers below, and this
// module calls back into the core (render, sendDeviceCommand) through injected
// refs (initScenes) rather than importing rooms.js — keeping the import graph
// one-directional (rooms → scenes → effects/actions → state, no cycle).
//
// recallScene cancels a running effect before recalling, so this module imports
// clearEffect from effects.js (one-way: scenes depend on effects, not vice
// versa). The open scene-name editor straddles this module and rooms.js's
// render(), so it lives in the state.sceneEdit holder; sceneDragId/the reorder
// timer are private here.

import { createPointerDrag, makeGhost, moveGhost, insertionBefore } from '/static/drag.js';
import { api } from '/static/api.js';
import { showToast } from '/static/util.js';
import { saveScene, deleteSceneApi, reorderScenes } from '/static/actions.js';
import { clearEffect, activateEffect, excludeFromEffect } from '/static/effects.js';
import { clearDotForRoom } from '/static/indicators.js';
import {
  model, devicesMap, pendingCommands,
  activeSceneByRoom, preSceneStateByRoom, pausedSceneDevices, roomEffectsMap,
  HUE_DEFAULT_ON, sceneEdit, effectEditor,
} from '/static/state.js';

// A scene settles every member to one fixed, known value — unlike an effect,
// which keeps moving, so forcing controls to track it would look like they're
// jumping around (deliberately not done for effects; see patchDeviceCards's
// `device-under-effect` freeze). Recalling (or reverting) a scene should win
// over whatever a control was showing a moment ago: drop any pendingCommands
// entry (or reconcilePending will keep re-asserting the old dragged value for
// up to PENDING_TTL_MS over the real one) and blur the input if the drag left
// it focused (patchDeviceCards skips any element that still has focus).
function clearPendingControlState(deviceId) {
  pendingCommands.delete(deviceId);
  const card = document.querySelector(`.room-device-card[data-device-id="${CSS.escape(deviceId)}"]`);
  if (!card) return;
  for (const ctrl of ['brightness', 'color_temp']) {
    const el = card.querySelector(`[data-ctrl="${ctrl}"]`);
    if (el && document.activeElement === el) el.blur();
  }
}

// A bulb ramping toward a scene's target sends several intermediate MQTT
// state reports along the way; patchDeviceCards snaps the slider to each one
// as it arrives, which reads as the slider jerking back and forth before
// settling. Holding the slider at the known final target (the scene's own
// saved snapshot) for a few seconds — same pendingCommands mechanism a live
// drag uses, just a longer TTL — hides the ramp and shows the true end state
// immediately instead.
const SCENE_SETTLE_TTL_MS = 5000;

function holdAtSceneTarget(deviceId, snap) {
  if (!snap) return;
  let fields = pendingCommands.get(deviceId);
  if (!fields) { fields = {}; pendingCommands.set(deviceId, fields); }
  const now = Date.now();
  if (snap.brightness != null) fields.brightness = { value: snap.brightness, ts: now, ttlMs: SCENE_SETTLE_TTL_MS };
  if (snap.color_temp != null) fields.color_temp = { value: snap.color_temp, ts: now, ttlMs: SCENE_SETTLE_TTL_MS };
}

// ── Scene divergence reconciliation ──────────────────────────────────────────
// A scene's "active" flag is pure client session state — the coordinator has
// no concept of it — so a light changed by ANY other source (a chat/intent
// command, a physical switch, another browser) needs the client to notice by
// itself rather than being told. Every incoming light-state update already
// carries the live value; SceneInfo now carries each scene's saved per-device
// values too, so this just compares the two — no new wire message, no
// coordinator-side scene tracking. Deliberately mirrors (never re-triggers)
// the manual pause/resume via the per-device 🎭 icon: same pausedSceneDevices
// bookkeeping, just detected instead of clicked. Never sends a command —
// whatever externally changed the device already did that.
const XY_EPSILON = 0.01;

function stateMatchesScene(dev, snap) {
  if (dev.on !== snap.on) return false;
  if (!dev.on) return true; // both off — brightness/colour are moot
  if (snap.brightness != null && dev.brightness !== snap.brightness) return false;
  if (snap.color_temp != null && dev.color_temp !== snap.color_temp) return false;
  if (snap.color_xy != null) {
    if (dev.color_xy == null) return false;
    if (Math.abs(dev.color_xy[0] - snap.color_xy[0]) > XY_EPSILON) return false;
    if (Math.abs(dev.color_xy[1] - snap.color_xy[1]) > XY_EPSILON) return false;
  }
  return true;
}

// Called from rooms.js's notifyDevices with the freshly-reconciled light
// list on every LightingUpdate. Cheap no-op in the common case (bails
// immediately for any device whose room has no active scene).
export function reconcileSceneDivergence(devices) {
  let changed = false;
  for (const dev of devices) {
    if (dev.online === false) continue; // stale/last-known state, not a real signal
    const room = model.rooms.find(r => r.device_ids.includes(dev.device_id));
    if (!room) continue;
    // A device's own group's active scene takes precedence over the room's
    // — a device is rarely governed by both at once, but when it could be,
    // the more specific scope (its own group) is the one it's actually
    // tracking right now.
    const group = (room.groups ?? []).find(g => g.device_ids.includes(dev.device_id));
    const scopeId = (group && activeSceneByRoom.has(group.id)) ? group.id : room.id;
    const sceneId = activeSceneByRoom.get(scopeId);
    if (!sceneId) continue;
    const scene = model.scenes.find(s => s.id === sceneId);
    const snap = scene?.states?.find(s => s.device_id === dev.device_id);
    if (!snap) continue; // device isn't part of this scene's saved snapshot

    const set = pausedSceneDevices.get(scopeId);
    const wasPaused = set?.has(dev.device_id) ?? false;
    const matches = stateMatchesScene(dev, snap);

    if (matches && wasPaused) {
      set.delete(dev.device_id);
      updateSceneChipStates(room.id);
      changed = true;
    } else if (!matches && !wasPaused) {
      let s = pausedSceneDevices.get(scopeId);
      if (!s) { s = new Set(); pausedSceneDevices.set(scopeId, s); }
      s.add(dev.device_id);
      updateSceneChipStates(room.id);
      changed = true;
    }
  }
  // Room cards' per-device 🎭 icon is only set at build time (buildDeviceCard),
  // not patched incrementally like patchDeviceCards — a full render picks up
  // the new paused/resumed icon state. render() itself guards against
  // clobbering an in-progress drag, so this is safe to call unconditionally.
  if (changed) _render();
}

let sceneDragId = null;        // sceneId being dragged within the bar
let _sceneReorderTimer = null; // debounce for the reorder POST after a drag

// rooms.js owns render() + sendDeviceCommand(); injected so this module never
// imports rooms.js.
let _render = () => {};
let _sendDeviceCommand = () => {};
export function initScenes({ render, sendDeviceCommand }) {
  _render = render;
  _sendDeviceCommand = sendDeviceCommand;
}

// roomId locates the DOM card (chips for both the room's own scenes and all
// of its groups' scenes live inside it); each chip's active/paused look is
// computed against its OWN scope (room-wide or its group), not roomId's,
// since a room card can host several independently-active scopes at once.
function updateSceneChipStates(roomId) {
  const card = document.querySelector(`[data-room-id="${CSS.escape(roomId)}"]`);
  if (!card) return;
  const room = model.rooms.find(r => r.id === roomId);
  card.querySelectorAll('.room-quick-scene-chip[data-scene-id]').forEach(chip => {
    const scene = model.scenes.find(s => s.id === chip.dataset.sceneId);
    if (!scene) return;
    const group = scene.group_id ? room?.groups?.find(g => g.id === scene.group_id) : null;
    const scopeId = scene.group_id ?? scene.room_id;
    const activeId = activeSceneByRoom.get(scopeId);
    const active = chip.dataset.sceneId === activeId;
    // Pause coverage drives the active chip's dim/grey look (mirrors effect
    // ghost): none paused → solid active; some → partly-paused; all → all-paused.
    const memberCount = group ? group.device_ids.length : (room?.device_ids?.length ?? 0);
    const pausedCount = pausedSceneDevices.get(scopeId)?.size ?? 0;
    chip.classList.toggle('active', active);
    chip.classList.toggle('partly-paused', active && pausedCount > 0 && pausedCount < memberCount);
    chip.classList.toggle('all-paused', active && memberCount > 0 && pausedCount >= memberCount);
  });
}

// `scopeId` is a room id (room-wide scene) or a group id (group scene) — see
// the Map comments in state.js. Resolves the owning room to refresh its chips.
export function clearRoomActiveScene(scopeId) {
  if (!activeSceneByRoom.has(scopeId)) return;
  activeSceneByRoom.delete(scopeId);
  pausedSceneDevices.delete(scopeId);
  const roomId = model.rooms.find(r => r.id === scopeId || (r.groups ?? []).some(g => g.id === scopeId))?.id;
  if (roomId) updateSceneChipStates(roomId);
}

export function cancelSceneEdit() {
  if (!sceneEdit.active) return;
  const { roomId, groupId } = sceneEdit.active;
  const card = document.querySelector(`[data-room-id="${CSS.escape(roomId)}"]`);
  // A room card can host several save-rows at once (the room-wide one plus
  // one per group) — each carries its own data-scope so only the row that's
  // actually open gets reset.
  const row = card?.querySelector(`.room-scene-save-row[data-scope="${CSS.escape(groupId ?? 'room')}"]`);
  row?.querySelector('.room-scene-name-input')?.style.setProperty('display', 'none');
  const sb = row?.querySelector('.room-scene-save-btn');
  if (sb) sb.style.display = '';
  sceneEdit.active = null;
}

export function handleScenesUpdate(evt) {
  model.scenes = evt.scenes ?? [];
  _render();
}

// ── Save scene row ────────────────────────────────────────────────────────────
// Shared by the room-wide scenes section below and each group cluster's own
// compact "+ Save scene" affordance (rooms.js) — groupId is omitted for the
// room-wide row, present for a group's own row. data-scope lets
// cancelSceneEdit() find the one actually-open row when a room card hosts
// several of these at once.
export function buildSceneSaveRow(roomId, groupId = null) {
  const saveRow = document.createElement('div');
  saveRow.className = 'room-scene-save-row';
  saveRow.dataset.scope = groupId ?? 'room';

  const saveBtn = document.createElement('button');
  saveBtn.className = 'room-scene-save-btn';
  saveBtn.textContent = '+ Save scene';
  saveRow.appendChild(saveBtn);

  const nameInput = document.createElement('input');
  nameInput.type = 'text';
  nameInput.className = 'room-scene-name-input';
  nameInput.name = 'scene-name';
  nameInput.autocomplete = 'off';
  nameInput.placeholder = 'Scene name…';
  nameInput.style.display = 'none';
  saveRow.appendChild(nameInput);

  saveBtn.addEventListener('click', e => {
    e.stopPropagation();
    sceneEdit.active = { roomId, groupId, value: '' };
    saveBtn.style.display = 'none';
    nameInput.style.display = '';
    nameInput.value = '';
    nameInput.focus();
  });

  nameInput.addEventListener('input', () => {
    if (sceneEdit.active) sceneEdit.active.value = nameInput.value;
  });

  let savingScene = false;
  const doSave = () => {
    if (savingScene) return;
    const name = nameInput.value.trim();
    nameInput.style.display = 'none';
    saveBtn.style.display = '';
    sceneEdit.active = null;
    if (!name) return;
    savingScene = true;
    saveScene(name, roomId, groupId).finally(() => { savingScene = false; });
  };
  nameInput.addEventListener('keydown', e => {
    if (e.key === 'Enter') { e.stopPropagation(); doSave(); }
    if (e.key === 'Escape') { e.stopPropagation(); cancelSceneEdit(); }
  });

  return saveRow;
}

// ── Save/recall scene section ─────────────────────────────────────────────────
export function buildScenesSection(roomId) {
  const section = document.createElement('div');
  section.className = 'room-scenes';
  section.dataset.roomId = roomId;
  section.appendChild(buildSceneSaveRow(roomId));

  // Scene list — room-wide scenes only; each group's own scenes are managed
  // from its own compact save row instead (they still appear as chips in the
  // room's quick-scenes bar, just with a group-name prefix).
  const roomScenes = model.scenes
    .filter(s => s.room_id === roomId && !s.group_id)
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

// ── Quick-scene chip drag reorder ─────────────────────────────────────────────
// Touch reorder of scene chips: quick swipe scrolls the row, press-hold drags.
// Mouse uses the native HTML5 path in wireSceneBarDrag.
export function wireSceneChipTouchDrag(chip, bar) {
  let ghost = null;
  createPointerDrag(chip, {
    holdMs: 150, distance: 8,
    onStart: () => {
      sceneDragId = chip.dataset.sceneId;
      chip.classList.add('dragging');
      ghost = makeGhost(chip);
    },
    onMove: (e) => {
      moveGhost(ghost, e.clientX, e.clientY);
      const after = insertionBefore(bar, '.room-quick-scene-chip:not(.dragging)', e.clientX);
      if (after == null) bar.appendChild(chip);
      else bar.insertBefore(chip, after);
    },
    onEnd: () => {
      ghost?.remove(); ghost = null;
      chip.classList.remove('dragging');
      const ids = [...bar.querySelectorAll('.room-quick-scene-chip[data-scene-id]')]
        .map(c => c.dataset.sceneId);
      clearTimeout(_sceneReorderTimer);
      _sceneReorderTimer = setTimeout(() => reorderScenes(ids), 80);
      sceneDragId = null;
    },
  });
}

export function wireSceneBarDrag(bar, roomId) {
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

// ── Per-bulb scene pause/resume ───────────────────────────────────────────────
// Pausing a light out of the active scene reverts it to its pre-scene state, or —
// if that snapshot isn't available (e.g. after an app reload) — the Hue default
// warm white. keepScene:true so the room's scene stays active for the others.
function applyPreSceneOrWarmWhite(roomId, deviceId) {
  const snap = preSceneStateByRoom.get(roomId)?.get(deviceId);
  if (!snap) {
    for (const c of HUE_DEFAULT_ON) _sendDeviceCommand(deviceId, c, { keepScene: true });
    return;
  }
  if (!snap.on) { _sendDeviceCommand(deviceId, { action: 'off' }, { keepScene: true }); return; }
  if (snap.brightness != null)
    _sendDeviceCommand(deviceId, { action: 'brightness', value: snap.brightness, transition_secs: 0.8 }, { keepScene: true });
  else
    _sendDeviceCommand(deviceId, { action: 'on' }, { keepScene: true });
  if (snap.color_xy != null)
    _sendDeviceCommand(deviceId, { action: 'color_xy', x: snap.color_xy[0], y: snap.color_xy[1], transition_secs: 0.8 }, { keepScene: true });
  else if (snap.color_temp != null)
    _sendDeviceCommand(deviceId, { action: 'color_temp', value: snap.color_temp, transition_secs: 0.8 }, { keepScene: true });
}

function pauseSceneDevice(roomId, deviceId) {
  let set = pausedSceneDevices.get(roomId);
  if (!set) { set = new Set(); pausedSceneDevices.set(roomId, set); }
  set.add(deviceId);
  _render();
  applyPreSceneOrWarmWhite(roomId, deviceId);
}

// Resume re-applies the scene's stored value to just this device (server filters
// the recall to the one device_id). Optimistic + rollback, mirroring overrides.
async function resumeSceneDevice(roomId, deviceId) {
  const sceneId = activeSceneByRoom.get(roomId);
  if (!sceneId) return;
  const set = pausedSceneDevices.get(roomId);
  set?.delete(deviceId);
  _render();
  try {
    const res = await api(`/scenes/${encodeURIComponent(sceneId)}/recall`, {
      method: 'POST', body: { transition_secs: 0.8, device_id: deviceId },
    });
    if (!res.ok) throw new Error(`${res.status}`);
  } catch (e) {
    set?.add(deviceId);
    _render();
    showToast(`Resume error: ${e.message}`, true);
  }
}

export function toggleSceneDevice(roomId, deviceId) {
  if (pausedSceneDevices.get(roomId)?.has(deviceId)) resumeSceneDevice(roomId, deviceId);
  else pauseSceneDevice(roomId, deviceId);
}

// A group-scoped scene recall — smaller sibling of the room-wide path below.
// Scoped to just the group's own member devices (scene.states already only
// holds those, per save_scene in http/api/scenes.rs): the room's own active
// scene and any OTHER group's active scene are left completely untouched.
async function recallGroupScene(scene) {
  const groupId = scene.group_id;
  const room = model.rooms.find(r => r.id === scene.room_id);
  const group = room?.groups?.find(g => g.id === groupId);
  const memberIds = group?.device_ids ?? [];

  // A fresh recall/revert re-includes all — clear stale pauses from
  // whatever scene previously governed this group (mirrors the room-wide
  // path below), otherwise a device paused from a prior group scene would
  // wrongly still show paused against this one until reconciled.
  pausedSceneDevices.delete(groupId);

  // Toggle: clicking the active group scene reverts to pre-scene state.
  if (activeSceneByRoom.get(groupId) === scene.id) {
    const preState = preSceneStateByRoom.get(groupId);
    activeSceneByRoom.delete(groupId);
    preSceneStateByRoom.delete(groupId);
    if (room) updateSceneChipStates(room.id);
    if (preState) {
      for (const deviceId of memberIds) {
        const snap = preState.get(deviceId);
        if (!snap) continue;
        clearPendingControlState(deviceId);
        holdAtSceneTarget(deviceId, snap);
        if (!snap.on) { _sendDeviceCommand(deviceId, { action: 'off' }); continue; }
        if (snap.brightness != null)
          _sendDeviceCommand(deviceId, { action: 'brightness', value: snap.brightness, transition_secs: 0.8 });
        else
          _sendDeviceCommand(deviceId, { action: 'on' });
        if (snap.color_xy != null)
          _sendDeviceCommand(deviceId, { action: 'color_xy', x: snap.color_xy[0], y: snap.color_xy[1], transition_secs: 0.8 });
        else if (snap.color_temp != null)
          _sendDeviceCommand(deviceId, { action: 'color_temp', value: snap.color_temp, transition_secs: 0.8 });
      }
    }
    return;
  }

  const snap = new Map();
  for (const deviceId of memberIds) {
    const dev = devicesMap.get(deviceId);
    if (dev) snap.set(deviceId, { on: dev.on, brightness: dev.brightness ?? null, color_xy: dev.color_xy ?? null, color_temp: dev.color_temp ?? null });
  }
  preSceneStateByRoom.set(groupId, snap);

  // A room-wide effect would otherwise immediately drive this group's lights
  // back on its next tick — exclude just the group's own members, leaving
  // the effect running for the rest of the room (unlike a whole-room scene
  // recall, which cancels the effect entirely below).
  if (room && roomEffectsMap.has(room.id)) {
    for (const deviceId of memberIds) await excludeFromEffect(room.id, deviceId);
  }

  try {
    const res = await api(`/scenes/${encodeURIComponent(scene.id)}/recall`, { method: 'POST', body: { transition_secs: 1.0 } });
    if (res.ok || res.status === 503) {
      activeSceneByRoom.set(groupId, scene.id);
      if (room) updateSceneChipStates(room.id);
      for (const deviceId of memberIds) {
        clearPendingControlState(deviceId);
        holdAtSceneTarget(deviceId, scene.states?.find(s => s.device_id === deviceId));
      }
      if (res.status === 503) showToast('Some devices offline — others recalled', false);
    } else {
      preSceneStateByRoom.delete(groupId);
      showToast(`Recall failed (${res.status})`, true);
    }
  } catch (e) {
    preSceneStateByRoom.delete(groupId);
    showToast(`Recall error: ${e.message}`, true);
  }
}

// ── Recall ────────────────────────────────────────────────────────────────────
export async function recallScene(id) {
  const scene = model.scenes.find(s => s.id === id);
  if (scene?.group_id) { await recallGroupScene(scene); return; }
  const roomId = scene?.room_id;

  // A scene sets colour/temp wholesale — reset the room + member dots to icons,
  // and clear any per-light scene pauses (a fresh recall/revert re-includes all).
  if (roomId) {
    const room = model.rooms.find(r => r.id === roomId);
    if (room) clearDotForRoom(room);
    pausedSceneDevices.delete(roomId);
  }

  // Toggle: clicking the active scene reverts to pre-scene state.
  if (roomId && activeSceneByRoom.get(roomId) === id) {
    // Cancel any effect that may have been re-enabled since the scene was recalled.
    if (roomEffectsMap.has(roomId)) await clearEffect(roomId);
    const preState = preSceneStateByRoom.get(roomId);
    activeSceneByRoom.delete(roomId);
    preSceneStateByRoom.delete(roomId);
    updateSceneChipStates(roomId);
    if (preState) {
      const room = model.rooms.find(r => r.id === roomId);
      for (const deviceId of (room?.device_ids ?? [])) {
        const snap = preState.get(deviceId);
        if (!snap) continue;
        clearPendingControlState(deviceId);
        holdAtSceneTarget(deviceId, snap);
        if (!snap.on) { _sendDeviceCommand(deviceId, { action: 'off' }); continue; }
        if (snap.brightness != null)
          _sendDeviceCommand(deviceId, { action: 'brightness', value: snap.brightness, transition_secs: 0.8 });
        else
          _sendDeviceCommand(deviceId, { action: 'on' });
        if (snap.color_xy != null)
          _sendDeviceCommand(deviceId, { action: 'color_xy', x: snap.color_xy[0], y: snap.color_xy[1], transition_secs: 0.8 });
        else if (snap.color_temp != null)
          _sendDeviceCommand(deviceId, { action: 'color_temp', value: snap.color_temp, transition_secs: 0.8 });
      }
    }
    return;
  }

  // Snapshot BEFORE cancelling the effect so we capture the true pre-effect
  // light state, not the effect's last output which is still in devicesMap.
  if (roomId) {
    const room = model.rooms.find(r => r.id === roomId);
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
    effectEditor.openRoomId = null; // close stale editor so stale params can't be re-applied
    await clearEffect(roomId);
  }

  // A scene saved while an effect was active carries that effect + its
  // params rather than a frozen per-light snapshot for every member.
  // Reactivate it first so the non-overridden lights are driven by the
  // effect again; the /recall fan-out below then applies the saved explicit
  // values to just the handful of lights manually overridden out of it
  // (scene.states already holds only those — see save_scene in
  // http/api/scenes.rs). Excluding them here too, before recall fires their
  // commands, stops the freshly (re)activated effect from fighting the
  // explicit values on its next tick.
  if (roomId && scene?.effect_id) {
    await activateEffect(roomId, scene.effect_id, scene.effect_params ?? {});
    for (const snap of scene.states ?? []) {
      await excludeFromEffect(roomId, snap.device_id);
    }
  }

  try {
    const res = await api(`/scenes/${encodeURIComponent(id)}/recall`, { method: 'POST', body: { transition_secs: 1.0 } });
    if (res.ok || res.status === 503) {
      if (roomId) {
        activeSceneByRoom.set(roomId, id);
        updateSceneChipStates(roomId);
        const room = model.rooms.find(r => r.id === roomId);
        for (const deviceId of (room?.device_ids ?? [])) {
          clearPendingControlState(deviceId);
          holdAtSceneTarget(deviceId, scene.states?.find(s => s.device_id === deviceId));
        }
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
