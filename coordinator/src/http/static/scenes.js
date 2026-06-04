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
import { clearEffect } from '/static/effects.js';
import { clearDotForRoom } from '/static/indicators.js';
import {
  model, devicesMap,
  activeSceneByRoom, preSceneStateByRoom, pausedSceneDevices, roomEffectsMap,
  HUE_DEFAULT_ON, sceneEdit, effectEditor,
} from '/static/state.js';

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

function updateSceneChipStates(roomId) {
  const card = document.querySelector(`[data-room-id="${CSS.escape(roomId)}"]`);
  if (!card) return;
  const activeId = activeSceneByRoom.get(roomId);
  // Pause coverage drives the active chip's dim/grey look (mirrors effect ghost):
  // none paused → solid active; some → partly-paused; all members → all-paused.
  const room = model.rooms.find(r => r.id === roomId);
  const memberCount = room?.device_ids?.length ?? 0;
  const pausedCount = pausedSceneDevices.get(roomId)?.size ?? 0;
  card.querySelectorAll('.room-quick-scene-chip[data-scene-id]').forEach(chip => {
    const active = chip.dataset.sceneId === activeId;
    chip.classList.toggle('active', active);
    chip.classList.toggle('partly-paused', active && pausedCount > 0 && pausedCount < memberCount);
    chip.classList.toggle('all-paused', active && memberCount > 0 && pausedCount >= memberCount);
  });
}

export function clearRoomActiveScene(roomId) {
  if (!activeSceneByRoom.has(roomId)) return;
  activeSceneByRoom.delete(roomId);
  pausedSceneDevices.delete(roomId);
  updateSceneChipStates(roomId);
}

export function cancelSceneEdit() {
  if (!sceneEdit.active) return;
  const card = document.querySelector(`[data-room-id="${CSS.escape(sceneEdit.active.roomId)}"]`);
  card?.querySelector('.room-scene-name-input')?.style.setProperty('display', 'none');
  const sb = card?.querySelector('.room-scene-save-btn');
  if (sb) sb.style.display = '';
  sceneEdit.active = null;
}

export function handleScenesUpdate(evt) {
  model.scenes = evt.scenes ?? [];
  _render();
}

// ── Save/recall scene section ─────────────────────────────────────────────────
export function buildScenesSection(roomId) {
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
  nameInput.name = 'scene-name';
  nameInput.autocomplete = 'off';
  nameInput.placeholder = 'Scene name…';
  nameInput.style.display = 'none';
  saveRow.appendChild(nameInput);

  section.appendChild(saveRow);

  saveBtn.addEventListener('click', e => {
    e.stopPropagation();
    sceneEdit.active = { roomId, value: '' };
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
    saveScene(name, roomId).finally(() => { savingScene = false; });
  };
  nameInput.addEventListener('keydown', e => {
    if (e.key === 'Enter') { e.stopPropagation(); doSave(); }
    if (e.key === 'Escape') { e.stopPropagation(); cancelSceneEdit(); }
  });

  // Scene list
  const roomScenes = model.scenes
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

// ── Recall ────────────────────────────────────────────────────────────────────
export async function recallScene(id) {
  const scene = model.scenes.find(s => s.id === id);
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

  try {
    const res = await api(`/scenes/${encodeURIComponent(id)}/recall`, { method: 'POST', body: { transition_secs: 1.0 } });
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
