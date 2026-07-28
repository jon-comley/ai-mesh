// ── REST actions ─────────────────────────────────────────────────────────────
// Thin fire-and-forget wrappers over the room/device/scene REST endpoints. Each
// posts a mutation and surfaces failures as a toast; the resulting state change
// arrives back over the WS channel and drives the re-render, so none of these
// touch the DOM, shared state, or render() — keeping this a pure leaf module.

import { api } from '/static/api.js';
import { showToast } from '/static/util.js';

// ── Rooms ──────────────────────────────────────────────────────────────────
export async function createRoom(name) {
  try {
    const res = await api('/rooms', { method: 'POST', body: { name } });
    if (!res.ok) showToast(`Create room failed (${res.status})`, true);
  } catch (e) { showToast(`Create room error: ${e.message}`, true); }
}

export async function deleteRoom(id) {
  try {
    const res = await api(`/rooms/${encodeURIComponent(id)}`, { method: 'DELETE' });
    if (!res.ok && res.status !== 404) showToast(`Delete room failed (${res.status})`, true);
  } catch (e) { showToast(`Delete room error: ${e.message}`, true); }
}

export async function renameRoom(id, name) {
  try {
    const res = await api(`/rooms/${encodeURIComponent(id)}/name`, { method: 'PATCH', body: { name } });
    if (!res.ok) showToast(`Rename failed (${res.status})`, true);
  } catch (e) { showToast(`Rename error: ${e.message}`, true); }
}

export async function reorderRooms(ids) {
  try {
    const res = await api('/rooms/reorder', { method: 'POST', body: { ids } });
    if (!res.ok) showToast(`Reorder failed (${res.status})`, true);
  } catch (e) { showToast(`Reorder error: ${e.message}`, true); }
}

// ── Devices in a room ────────────────────────────────────────────────────────
export async function addDeviceToRoom(roomId, deviceId) {
  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/devices`, { method: 'PATCH', body: { add: [deviceId], remove: [] } });
    if (!res.ok) showToast(`Add device failed (${res.status})`, true);
  } catch (e) { showToast(`Add device error: ${e.message}`, true); }
}

export async function removeDeviceFromRoom(roomId, deviceId) {
  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/devices`, { method: 'PATCH', body: { add: [], remove: [deviceId] } });
    if (!res.ok) showToast(`Remove device failed (${res.status})`, true);
  } catch (e) { showToast(`Remove device error: ${e.message}`, true); }
}

export async function reorderRoomDevices(roomId, ids) {
  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/devices/reorder`, { method: 'POST', body: { ids } });
    if (!res.ok) showToast(`Device reorder failed (${res.status})`, true);
  } catch (e) { showToast(`Device reorder error: ${e.message}`, true); }
}

// ── Room groups ──────────────────────────────────────────────────────────────
export async function createRoomGroup(roomId, name) {
  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/groups`, { method: 'POST', body: { name } });
    if (!res.ok) showToast(`Create group failed (${res.status})`, true);
  } catch (e) { showToast(`Create group error: ${e.message}`, true); }
}

export async function renameRoomGroup(roomId, groupId, name) {
  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/groups/${encodeURIComponent(groupId)}/name`, { method: 'PATCH', body: { name } });
    if (!res.ok) showToast(`Rename group failed (${res.status})`, true);
  } catch (e) { showToast(`Rename group error: ${e.message}`, true); }
}

export async function deleteRoomGroup(roomId, groupId) {
  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/groups/${encodeURIComponent(groupId)}`, { method: 'DELETE' });
    if (!res.ok && res.status !== 404) showToast(`Delete group failed (${res.status})`, true);
  } catch (e) { showToast(`Delete group error: ${e.message}`, true); }
}

export async function setDeviceGroup(roomId, deviceId, groupId) {
  try {
    const res = await api(`/rooms/${encodeURIComponent(roomId)}/devices/${encodeURIComponent(deviceId)}/group`,
      { method: 'PATCH', body: { group_id: groupId } });
    if (!res.ok) showToast(`Set group failed (${res.status})`, true);
  } catch (e) { showToast(`Set group error: ${e.message}`, true); }
}

// ── Devices ──────────────────────────────────────────────────────────────────
export async function deleteDevice(deviceId) {
  try {
    const res = await api(`/lights/${encodeURIComponent(deviceId)}`, { method: 'DELETE' });
    if (!res.ok) { showToast(`Delete device failed (${res.status})`, true); return; }
    // 200 (vs 204) means the registry was cleaned but the Zigbee-side unpair
    // request couldn't be sent — the device may still be joined to the
    // network. Surface that instead of a silent success (see delete_device
    // in coordinator/src/http/api/lights.rs).
    if (res.status === 200) {
      const { warning } = await res.json();
      if (warning) showToast(warning, true);
    }
  } catch (e) { showToast(`Delete device error: ${e.message}`, true); }
}

export async function patchDeviceName(deviceId, name) {
  try {
    await api(`/lights/${encodeURIComponent(deviceId)}/name`, { method: 'PATCH', body: { name } });
  } catch (e) { showToast(`Rename error: ${e.message}`, true); }
}

// ── Scenes ───────────────────────────────────────────────────────────────────
export async function saveScene(name, roomId, groupId) {
  try {
    const body = { name };
    if (roomId) body.room_id = roomId;
    if (groupId) body.group_id = groupId;
    const res = await api('/scenes', { method: 'POST', body });
    if (!res.ok) showToast(`Save scene failed (${res.status})`, true);
  } catch (e) { showToast(`Save scene error: ${e.message}`, true); }
}

export async function deleteSceneApi(id) {
  try {
    const res = await api(`/scenes/${encodeURIComponent(id)}`, { method: 'DELETE' });
    if (!res.ok && res.status !== 404) showToast(`Delete scene failed (${res.status})`, true);
  } catch (e) { showToast(`Delete scene error: ${e.message}`, true); }
}

export async function reorderScenes(ids) {
  try {
    const res = await api('/scenes/reorder', { method: 'POST', body: { ids } });
    if (!res.ok) showToast(`Scene reorder failed (${res.status})`, true);
  } catch (e) { showToast(`Scene reorder error: ${e.message}`, true); }
}
