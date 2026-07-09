// ── Switch → action bindings panel ──────────────────────────────────────────
// Minimal management UI for binding a switch's exact z2m action (button
// press, dial rotation) to a room/group light command — see
// coordinator/src/http/api/switch_bindings.rs for the backend. Lives under
// each Switch row in the Devices tab (devices.js), collapsed by default.

import { api } from '/static/api.js';
import { esc, showToast } from '/static/util.js';
import { model } from '/static/state.js';
import { getLastSeenAction, getSeenActions } from '/static/devicewidgets.js';

// A device id can contain characters that aren't safe in an HTML `id`
// attribute (mostly moot for the "0x..." Zigbee ids in practice, but a
// user-renamed or manually-entered device_id could contain anything).
function safeListId(deviceId) {
  return `switch-actions-${deviceId.replace(/[^a-zA-Z0-9_-]/g, '_')}`;
}

let allBindings = [];
let loaded = false;

async function loadBindings(force = false) {
  if (loaded && !force) return allBindings;
  try {
    const res = await api('/switch-bindings');
    if (res.ok) allBindings = await res.json();
  } catch {
    // Leave allBindings as whatever it was — a transient fetch failure
    // shouldn't blank out a previously-loaded list.
  }
  loaded = true;
  return allBindings;
}

function bindingsForDevice(deviceId) {
  return allBindings.filter(b => b.device_id === deviceId);
}

function targetLabel(binding) {
  if (binding.target_kind === 'room') {
    return model.rooms.find(r => r.id === binding.target_id)?.name ?? binding.target_id;
  }
  for (const room of model.rooms) {
    const group = (room.groups ?? []).find(g => g.id === binding.target_id);
    if (group) return `${room.name} / ${group.name}`;
  }
  return binding.target_id;
}

function commandLabel(binding) {
  if (binding.command === 'brightness_step') {
    const sign = binding.step_delta > 0 ? '+' : '';
    return `brightness ${sign}${binding.step_delta}`;
  }
  return binding.command;
}

async function createBinding(payload) {
  try {
    const res = await api('/switch-bindings', { method: 'POST', body: payload });
    if (!res.ok) {
      const text = await res.text().catch(() => '');
      showToast(`Binding failed (${res.status})${text ? ': ' + text : ''}`, true);
      return false;
    }
    return true;
  } catch (e) {
    showToast(`Binding error: ${e.message}`, true);
    return false;
  }
}

async function deleteBinding(id) {
  try {
    const res = await api(`/switch-bindings/${encodeURIComponent(id)}`, { method: 'DELETE' });
    if (!res.ok && res.status !== 404) {
      showToast(`Remove binding failed (${res.status})`, true);
      return false;
    }
    return true;
  } catch (e) {
    showToast(`Remove binding error: ${e.message}`, true);
    return false;
  }
}

// One <select> for both room and group targets — "room:<id>" or
// "group:<id>" so a single control can express either without a second
// dropdown that's only sometimes relevant.
function buildTargetSelect() {
  const select = document.createElement('select');
  select.className = 'device-room-select switch-binding-target-select';
  for (const room of model.rooms) {
    const opt = document.createElement('option');
    opt.value = `room:${room.id}`;
    opt.textContent = room.name;
    select.appendChild(opt);
    for (const group of room.groups ?? []) {
      const gopt = document.createElement('option');
      gopt.value = `group:${group.id}`;
      gopt.textContent = `↳ ${room.name} / ${group.name}`;
      select.appendChild(gopt);
    }
  }
  return select;
}

function buildBindingRow(binding, onRemoved) {
  const row = document.createElement('div');
  row.className = 'switch-binding-row';
  row.innerHTML = `
    <span class="switch-binding-action">${esc(binding.action)}</span>
    <span class="switch-binding-arrow">→</span>
    <span class="switch-binding-target">${esc(targetLabel(binding))}</span>
    <span class="switch-binding-command">${esc(commandLabel(binding))}</span>`;
  const delBtn = document.createElement('button');
  delBtn.className = 'device-row-btn device-row-btn-delete';
  delBtn.textContent = '✕';
  delBtn.title = 'Remove binding';
  delBtn.addEventListener('click', async () => {
    if (await deleteBinding(binding.id)) {
      allBindings = allBindings.filter(b => b.id !== binding.id);
      onRemoved();
    }
  });
  row.appendChild(delBtn);
  return row;
}

// declaredActions is this switch model's full z2m action vocabulary (see
// shared::DeviceEntry.actions) — every button/gesture it can ever emit,
// not just what's fired since the dashboard loaded. When z2m has given us
// that list, a real <select> replaces the old guess-and-press combo box
// entirely: no need to physically press a button before you can bind it.
function buildActionPicker(deviceId, declaredActions) {
  if (declaredActions.length > 0) {
    const select = document.createElement('select');
    select.className = 'switch-binding-action-input switch-binding-action-select';
    const seen = new Set(getSeenActions(deviceId));
    const lastSeen = getLastSeenAction(deviceId);
    for (const action of declaredActions) {
      const opt = document.createElement('option');
      opt.value = action;
      // A ● marks actions this switch has actually fired at least once —
      // reassurance the binding will really trigger, without hiding the
      // rest of the (equally real) vocabulary.
      opt.textContent = seen.has(action) ? `${action} ●` : action;
      if (action === lastSeen) opt.selected = true;
      select.appendChild(opt);
    }
    return { input: select, getValue: () => select.value };
  }

  // Fallback for a device z2m hasn't given us an action enum for (older
  // z2m, or a device type this crate hasn't seen exposes for yet): the
  // original combo box, built from whatever's actually fired so far.
  const listId = safeListId(deviceId);
  const actionInput = document.createElement('input');
  actionInput.type = 'text';
  actionInput.setAttribute('list', listId);
  actionInput.placeholder = 'z2m action, e.g. button_1_press';
  actionInput.className = 'switch-binding-action-input';
  actionInput.autocomplete = 'off';
  const lastSeen = getLastSeenAction(deviceId);
  if (lastSeen) actionInput.value = lastSeen;

  const actionList = document.createElement('datalist');
  actionList.id = listId;
  for (const action of getSeenActions(deviceId)) {
    const opt = document.createElement('option');
    opt.value = action;
    actionList.appendChild(opt);
  }
  return { input: actionInput, extra: actionList, getValue: () => actionInput.value.trim() };
}

function buildAddForm(deviceId, declaredActions, onAdded) {
  const form = document.createElement('div');
  form.className = 'switch-binding-form';

  const { input: actionInput, extra: actionList, getValue: getAction } =
    buildActionPicker(deviceId, declaredActions);

  const targetSelect = buildTargetSelect();

  const commandSelect = document.createElement('select');
  commandSelect.className = 'switch-binding-command-select';
  for (const [value, label] of [
    ['toggle', 'Toggle'],
    ['on', 'On'],
    ['off', 'Off'],
    ['brightness_step', 'Brightness step'],
  ]) {
    const o = document.createElement('option');
    o.value = value;
    o.textContent = label;
    commandSelect.appendChild(o);
  }

  const deltaInput = document.createElement('input');
  deltaInput.type = 'number';
  deltaInput.placeholder = 'e.g. 25 or -25';
  deltaInput.className = 'switch-binding-delta-input';
  deltaInput.hidden = true;
  commandSelect.addEventListener('change', () => {
    deltaInput.hidden = commandSelect.value !== 'brightness_step';
  });

  const addBtn = document.createElement('button');
  addBtn.className = 'device-row-btn';
  addBtn.textContent = '+ Bind';
  addBtn.addEventListener('click', async () => {
    const action = getAction();
    if (!action) {
      showToast('Enter the switch action first (press the button once to see it above)', true);
      return;
    }
    const [targetKind, targetId] = targetSelect.value.split(':');
    const command = commandSelect.value;
    let stepDelta;
    if (command === 'brightness_step') {
      stepDelta = parseInt(deltaInput.value, 10);
      if (Number.isNaN(stepDelta)) {
        showToast('Enter a step amount (e.g. 25 or -25)', true);
        return;
      }
    }
    const ok = await createBinding({
      device_id: deviceId,
      action,
      target_kind: targetKind,
      target_id: targetId,
      command,
      step_delta: stepDelta,
    });
    if (ok) {
      await loadBindings(true);
      onAdded();
    }
  });

  form.append(actionInput);
  if (actionList) form.append(actionList);
  form.append(targetSelect, commandSelect, deltaInput, addBtn);
  return form;
}

/// Returns { toggle, panel } — append `toggle` to the row's action bar and
/// `panel` as its own block underneath. The panel lazily fetches the full
/// binding list on first open rather than on every render (bindings change
/// rarely; no point re-fetching on every WS event that rebuilds the row).
/// `declaredActions` is the switch's full z2m action vocabulary (empty for
/// devices z2m hasn't reported one for) — see `buildActionPicker`.
export function buildBindingsPanel(deviceId, declaredActions = []) {
  const panel = document.createElement('div');
  panel.className = 'switch-bindings-panel';
  panel.hidden = true;

  const list = document.createElement('div');
  list.className = 'switch-bindings-list';
  panel.appendChild(list);

  const renderList = () => {
    list.innerHTML = '';
    const mine = bindingsForDevice(deviceId);
    if (mine.length === 0) {
      list.innerHTML = '<p class="placeholder">No bindings yet.</p>';
    } else {
      for (const binding of mine) list.appendChild(buildBindingRow(binding, renderList));
    }
  };

  panel.appendChild(buildAddForm(deviceId, declaredActions, renderList));

  const toggle = document.createElement('button');
  toggle.className = 'device-row-btn switch-bindings-toggle';
  toggle.textContent = '🔗 Bindings';
  toggle.title = 'Bind button presses / dial rotation to a light command';
  toggle.addEventListener('click', async () => {
    const opening = panel.hidden;
    if (opening) {
      await loadBindings();
      renderList();
    }
    panel.hidden = !opening;
  });

  return { toggle, panel };
}
