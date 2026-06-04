// ── Colour/temperature indicator model (shared by device + room cards) ───────
// Each colour/temp control shows its glyph ICON (🎨 colour / 🌡 temperature) by
// default. When you SET a colour or temperature, that domain becomes a live DOT
// showing the current value and PERSISTS (tracked in deviceDotDomain /
// roomDotDomain). Brightness changes leave it; setting the other domain moves
// the dot there; on/off or a scene clears it back to an icon. While a control is
// actively dragged it shows the live value under the finger too.

import { ctToHex } from '/static/layout.js';
import { xyToRgb, rgbToHsl } from '/static/colormath.js';
import {
  model, devicesMap, deviceDotDomain, roomDotDomain, COLOUR_ICON, TEMP_ICON,
} from '/static/state.js';

function roomIdForDevice(deviceId) {
  for (const r of model.rooms) if (r.device_ids?.includes(deviceId)) return r.id;
  return null;
}
// on/off of a bulb clears its own dot and its room's (the room is no longer the
// single colour/temperature the user last set).
export function clearDotForDevice(deviceId) {
  deviceDotDomain.delete(deviceId);
  const rid = roomIdForDevice(deviceId);
  if (rid) roomDotDomain.delete(rid);
}
// on/off or a scene at room level clears the room dot and every member bulb's.
export function clearDotForRoom(room) {
  roomDotDomain.delete(room.id);
  for (const id of (room.device_ids ?? [])) deviceDotDomain.delete(id);
}

// Representative colour (HSL) for a room, taken from its first colour-capable
// device. Falls back to a warm default when none report colour.
export function getRoomColourHsl(roomDevices) {
  const dev = roomDevices.find(d => d.color_xy != null);
  if (dev) {
    const [x, y] = dev.color_xy;
    const { r, g, b } = xyToRgb(x, y, dev.brightness ?? 254);
    const { h, s } = rgbToHsl(r, g, b);
    return { h, s };
  }
  return { h: 30, s: 80 };
}

// Paint a device mode button as its glyph icon.
function paintModeIcon(btn, domain) {
  if (!btn) return;
  btn.classList.remove('lc-mode-dot');
  btn.style.background = '';
  btn.textContent = domain === 'colour' ? COLOUR_ICON : TEMP_ICON;
  btn.title = domain === 'colour' ? 'Colour' : 'Temperature';
}

// Paint a device mode button as a live dot of the given colour.
export function paintModeDot(btn, bg) {
  if (!btn) return;
  btn.classList.add('lc-mode-dot');
  btn.textContent = '';
  btn.style.background = bg;
}

// Paint a device button: a live dot (from current state) when that domain is the
// one last set for this device, otherwise its glyph icon.
export function paintDeviceButton(btn, domain, dev) {
  if (!btn) return;
  const active = deviceDotDomain.get(dev.device_id) === domain;
  if (active && domain === 'colour' && dev.color_xy) {
    const [x, y] = dev.color_xy;
    const { r, g, b } = xyToRgb(x, y, dev.brightness ?? 254);
    const { h, s } = rgbToHsl(r, g, b);
    paintModeDot(btn, `hsl(${h},${s}%,50%)`);
  } else if (active && domain === 'temp' && dev.color_temp != null) {
    paintModeDot(btn, ctToHex(dev.color_temp));
  } else {
    paintModeIcon(btn, domain);
  }
}

// Repaint a device card's colour/temp buttons from the persisted dot domain —
// used by the live-patch paths so the dot re-tints when state changes.
export function repaintModeDots(card, dev) {
  if (dev.color_temp != null)
    paintDeviceButton(card.querySelector('.lc-mode-btn[data-domain="temp"]'), 'temp', dev);
  if (dev.color_xy != null)
    paintDeviceButton(card.querySelector('.lc-mode-btn[data-domain="colour"]'), 'colour', dev);
}

// Paint a room colour/temp trigger as its glyph icon.
function paintRoomTrigger(btn, domain) {
  if (!btn) return;
  btn.classList.remove('room-colour-dot', 'room-temp-dot');
  btn.style.background = '';
  btn.textContent = domain === 'colour' ? COLOUR_ICON : TEMP_ICON;
  btn.title = domain === 'colour' ? 'Colour' : 'Temperature';
}

// Paint a room trigger as a live dot tracking the given colour.
export function paintRoomDot(btn, domain, bg) {
  if (!btn) return;
  btn.classList.remove('room-colour-dot', 'room-temp-dot');
  btn.classList.add(domain === 'colour' ? 'room-colour-dot' : 'room-temp-dot');
  btn.textContent = '';
  btn.style.background = bg;
}

// Paint a room trigger: a live dot (from the room's representative value) when
// that domain is the one last set for this room, otherwise its glyph icon.
export function paintRoomButton(btn, domain, devices, roomId) {
  if (!btn) return;
  const active = roomDotDomain.get(roomId) === domain;
  if (active && domain === 'colour') {
    const { h, s } = getRoomColourHsl(devices);
    paintRoomDot(btn, 'colour', `hsl(${h},${s}%,50%)`);
  } else if (active && domain === 'temp') {
    const temps = devices.filter(d => d.color_temp != null).map(d => d.color_temp);
    if (temps.length) {
      const avg = Math.round(temps.reduce((a, b) => a + b, 0) / temps.length);
      paintRoomDot(btn, 'temp', ctToHex(avg));
    } else {
      paintRoomTrigger(btn, 'temp');
    }
  } else {
    paintRoomTrigger(btn, domain);
  }
}

// Re-tint each room's colour + temperature triggers from the persisted dot
// domain after a live patch.
export function refreshRoomTriggers() {
  for (const room of model.rooms) {
    const card = document.querySelector(`.room-card[data-room-id="${CSS.escape(room.id)}"]`);
    if (!card) continue;
    const devs = (room.device_ids ?? []).map(id => devicesMap.get(id)).filter(Boolean);
    paintRoomButton(card.querySelector('.room-ctrl-trigger[data-role="room-colour"]'), 'colour', devs, room.id);
    paintRoomButton(card.querySelector('.room-ctrl-trigger[data-role="room-temp"]'),   'temp',   devs, room.id);
  }
}
