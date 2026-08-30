// ── Switch → action bindings panel ──────────────────────────────────────────
// Minimal management UI for binding a switch's exact z2m action (button
// press, dial rotation) to a room/group light command — see
// coordinator/src/http/api/switch_bindings.rs for the backend. Lives under
// each Switch row in the Devices tab (devices.js), collapsed by default.

import { api } from '/static/api.js';
import { esc, showToast } from '/static/util.js';
import { model } from '/static/state.js';
import { getLastSeenAction, getSeenActions } from '/static/devicewidgets.js';

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
  if (binding.command === 'brightness_step_relative') {
    return binding.step_delta < 0 ? 'dim (dial-scaled)' : 'brighten (dial-scaled)';
  }
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

// Build the action <select>.
//
// `declaredActions` is the switch model's full z2m action vocabulary (see
// shared::DeviceEntry.actions) — every button/gesture it can ever emit, not
// just what has fired since the dashboard loaded. Where z2m has not given us
// one, fall back to the actions this switch has actually been *seen* emitting.
//
// **There is deliberately no free-text entry.** It used to accept any string,
// and a typed action that the device does not emit stores fine, lists fine and
// never fires — the Hue Smart Button spent an unknown length of time bound to
// `button_press_1`, which that model has no concept of (it declares
// on/off/press/hold/release). Nothing surfaced it. A binding you can only pick
// from a list cannot be wrong in that way. The server enforces the same rule.
//
// Returns `{ input, getValue, exhausted, empty }` — `empty` when there is no
// vocabulary to offer at all, which is a "press a button first" state, not an
// error.
function buildActionPicker(deviceId, declaredActions, boundActions) {
  const seen = getSeenActions(deviceId);
  const options = declaredActions.length > 0 ? declaredActions : seen;
  if (options.length === 0) {
    return { input: null, getValue: () => '', exhausted: false, empty: true };
  }

  const select = document.createElement('select');
  select.className = 'switch-binding-action-input switch-binding-action-select';
  const seenSet = new Set(seen);
  const lastSeen = getLastSeenAction(deviceId);
  // Prefer the action just pressed, but never land on one already bound — a
  // switch takes one binding per action, and the server refuses a duplicate.
  let defaulted = false;
  for (const action of options) {
    const opt = document.createElement('option');
    opt.value = action;
    const isBound = boundActions.has(action);
    // ● marks an action this switch has actually fired at least once —
    // reassurance the binding will really trigger, without hiding the rest of
    // the (equally real) vocabulary. ✓ marks one already bound.
    opt.textContent = isBound
      ? `${action} ✓ bound`
      : (seenSet.has(action) ? `${action} ●` : action);
    opt.disabled = isBound;
    if (!isBound && !defaulted && action === lastSeen) {
      opt.selected = true;
      defaulted = true;
    }
    select.appendChild(opt);
  }
  if (!defaulted) {
    const firstFree = [...select.options].find(o => !o.disabled);
    if (firstFree) firstFree.selected = true;
  }
  const allBound = [...select.options].every(o => o.disabled);
  return { input: select, getValue: () => select.value, exhausted: allBound, empty: false };
}

function buildAddForm(deviceId, declaredActions, boundActions, onAdded) {
  const form = document.createElement('div');
  form.className = 'switch-binding-form';

  const { input: actionInput, getValue: getAction, exhausted, empty } =
    buildActionPicker(deviceId, declaredActions, boundActions);

  if (empty) {
    const note = document.createElement('p');
    note.className = 'placeholder';
    note.textContent =
      'No actions known for this switch yet — press one of its buttons once and it will appear here.';
    form.appendChild(note);
    return form;
  }
  if (exhausted) {
    const note = document.createElement('p');
    note.className = 'placeholder';
    note.textContent = 'Every action on this switch is bound. Remove one to rebind it.';
    form.appendChild(note);
    return form;
  }

  const targetSelect = buildTargetSelect();

  const commandSelect = document.createElement('select');
  commandSelect.className = 'switch-binding-command-select';
  for (const [value, label] of [
    ['toggle', 'Toggle'],
    ['on', 'On'],
    ['off', 'Off'],
    ['brightness_step_relative', 'Dim / brighten (dial)'],
    ['brightness_step', 'Brightness step (fixed)'],
  ]) {
    const o = document.createElement('option');
    o.value = value;
    o.textContent = label;
    commandSelect.appendChild(o);
  }

  const deltaInput = document.createElement('input');
  deltaInput.type = 'number';
  // min must be negative for iOS Safari (and most Android keyboards) to show
  // a minus key at all on the numeric pad — without it the field silently
  // can't accept a negative step no matter what's typed.
  deltaInput.min = '-254';
  deltaInput.max = '254';
  deltaInput.placeholder = 'e.g. 8 or -8';
  deltaInput.className = 'switch-binding-delta-input';
  deltaInput.autocomplete = 'off';
  deltaInput.name = 'switch-binding-step-delta';
  // Suppress mobile password-manager autofill prompts — some heuristically
  // flag any bare number input inside a form, autocomplete="off" alone.
  deltaInput.setAttribute('data-lpignore', 'true');
  deltaInput.setAttribute('data-1p-ignore', '');
  deltaInput.setAttribute('data-bwignore', 'true');
  deltaInput.hidden = true;

  // What the number means differs between the two brightness commands, so say
  // so rather than leaving one bare box to cover both.
  const deltaHint = document.createElement('span');
  deltaHint.className = 'switch-binding-delta-hint placeholder';
  deltaHint.hidden = true;

  const paintCommand = () => {
    const cmd = commandSelect.value;
    const isRelative = cmd === 'brightness_step_relative';
    const isFixed = cmd === 'brightness_step';
    deltaInput.hidden = !(isRelative || isFixed);
    deltaHint.hidden = deltaInput.hidden;
    if (isRelative) {
      // Only the sign matters here: the dial reports how far it was turned and
      // that figure is used, scaling with rotation speed the way Hue's own
      // system does. The number is just the fallback for a device that reports
      // nothing — 8 is the Hue Tap Dial's own smallest detent.
      deltaHint.textContent = 'sign sets direction; the dial supplies the amount';
      if (!deltaInput.value) deltaInput.value = '8';
    } else if (isFixed) {
      deltaHint.textContent = 'fixed amount per press, 1–254';
    }
  };
  commandSelect.addEventListener('change', paintCommand);
  paintCommand();

  const addBtn = document.createElement('button');
  addBtn.className = 'device-row-btn';
  addBtn.textContent = '+ Bind';
  addBtn.addEventListener('click', async () => {
    const action = getAction();
    if (!action) {
      showToast('Pick the switch action first', true);
      return;
    }
    // The server refuses a duplicate with 409; catching it here as well keeps
    // the free-text fallback path honest and gives a better message.
    if (boundActions.has(action)) {
      showToast(`'${action}' is already bound on this switch — remove that binding first`, true);
      return;
    }
    const [targetKind, targetId] = targetSelect.value.split(':');
    const command = commandSelect.value;
    let stepDelta;
    if (command === 'brightness_step' || command === 'brightness_step_relative') {
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

  form.append(actionInput, targetSelect, commandSelect, deltaInput, deltaHint, addBtn);
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

  // The add form is rebuilt alongside the list, not once: which actions are
  // still free changes every time a binding is added or removed, and a stale
  // picker would offer one that the server now refuses.
  const formWrap = document.createElement('div');
  panel.appendChild(formWrap);

  const renderAll = () => {
    const mine = bindingsForDevice(deviceId);
    list.innerHTML = '';
    if (mine.length === 0) {
      list.innerHTML = '<p class="placeholder">No bindings yet.</p>';
    } else {
      // Say the count out loud: a switch holds one binding per action, and a
      // Hue Tap Dial has 24 of them — four buttons and both dial directions
      // bind independently. The panel used to show a bare list, which read as
      // "this switch has a binding" rather than "it can have many".
      const count = document.createElement('p');
      count.className = 'switch-bindings-count placeholder';
      const total = declaredActions.length;
      count.textContent = total > 0
        ? `${mine.length} of ${total} actions bound`
        : `${mine.length} binding${mine.length === 1 ? '' : 's'}`;
      list.appendChild(count);
      for (const binding of mine) list.appendChild(buildBindingRow(binding, renderAll));
    }
    const boundActions = new Set(mine.map(b => b.action));
    formWrap.innerHTML = '';
    formWrap.appendChild(buildAddForm(deviceId, declaredActions, boundActions, renderAll));
  };

  const toggle = document.createElement('button');
  toggle.className = 'device-row-btn switch-bindings-toggle';
  toggle.textContent = '🔗 Bindings';
  toggle.title = 'Bind button presses / dial rotation to a light command';
  toggle.addEventListener('click', async () => {
    const opening = panel.hidden;
    if (opening) {
      await loadBindings();
      renderAll();
    }
    panel.hidden = !opening;
  });

  return { toggle, panel };
}
