// ── Shared dashboard state ─────────────────────────────────────────────────────
// Single home for the module-level state the rooms/scenes/effects/indicators
// modules all read and mutate. Two kinds live here:
//
//  • const collections (Map/Set) — imported by name and mutated in place
//    (.set/.delete/.clear). ES modules forbid reassigning an imported binding,
//    but mutating the object it points to is fine, so these need no accessors.
//  • reassigned data (the arrays/maps swapped wholesale on each WS update) —
//    held as properties of `model` so writes (`model.rooms = …`) are visible
//    across every importer.
//
// Pure leaf: no imports.

// Reassigned-wholesale data model. Setters do `model.rooms = evt.rooms` etc.
export const model = {
  rooms: [],            // [{ id, name, device_ids, ... }]
  scenes: [],           // [{ id, name, room_id, ... }]
  names: new Map(),     // device_id → friendly name
  globalLight: null,    // 'on' | 'off' | null — last All On/Off press, cleared on any individual change
};

// Device snapshot map (device_id → dev). Mutated in place by notifyDevices.
export const devicesMap = new Map();

// ── Per-domain UI state (session-only) ─────────────────────────────────────────
export const lastEffectByRoom = new Map();   // roomId → { effect_id, params } — paused/remembered state
export const openPickerIds = new Set();      // device IDs whose colour picker is currently open
export const openRoomCtrlIds = new Set();    // room IDs whose 🎨 colour/temp panel is open (survives render)
export const activeSceneByRoom = new Map();      // roomId → sceneId of last-recalled scene
export const preSceneStateByRoom = new Map();    // roomId → Map<deviceId, snapshot> before last recall
// Devices the user has paused out of the room's active scene (session-only). The
// scene stays active for the rest; a paused light reverts to pre-scene (or warm
// white), and clicking its greyed scene icon resumes it. Mirrors effect overrides.
export const pausedSceneDevices = new Map();     // roomId → Set<deviceId>

// Which colour/temp domain currently shows a live DOT (vs its glyph icon), per
// target. A dot appears when you set a colour/temperature and PERSISTS, showing
// the live value. Brightness changes leave it alone; setting the other domain
// moves the dot there; on/off (of the target, a member bulb, or the whole room)
// or a scene clears it back to an icon. Session-only — both icons on a fresh load.
export const deviceDotDomain = new Map(); // device_id → 'colour' | 'temp'
export const roomDotDomain = new Map();   // room_id   → 'colour' | 'temp'

// Effect catalogue lookup (id → metadata) and per-room active effect.
export const effectsById = new Map();
export const roomEffectsMap = new Map();         // room_id → { effect_id, params }

// Transient effect UI state that straddles the effects module and the room-card
// renderer in rooms.js. Holder objects (not bare `let`s) so both modules mutate
// the same reference — ES-module imports are read-only bindings, so a plain
// exported `let` couldn't be reassigned from the importing module.
//   • effectDrag  — in-flight palette/badge drag: src set by the dragged chip,
//     read by the room-card drop zones; removeRoomId/isPermanent drive the
//     badge drag-to-remove document handlers.
//   • effectEditor.openRoomId — room whose param-editor popover is open; written
//     by the effects builders/recallScene, read by renderRoomCard.
export const effectDrag = { src: null, removeRoomId: null, isPermanent: false };
export const effectEditor = { openRoomId: null };

// Open scene-name editor, straddling the scenes module (builders + the
// Escape/click document listeners) and rooms.js's render() (which restores the
// open input after a rebuild). Holder object for the same read-only-binding
// reason as the effect holders above. `active` is { roomId, value } | null.
export const sceneEdit = { active: null };

// Pending optimistic command values per (deviceId, field). Each entry { value, ts }.
// Overlaid onto incoming WS snapshots so the slider doesn't snap back to the
// pre-command server value while the round-trip is still in flight.
export const pendingCommands = new Map();
export const PENDING_TTL_MS = 2000;

// ── Static display constants ───────────────────────────────────────────────────
export const COLOUR_ICON = '🎨', TEMP_ICON = '🌡';
export const EFFECT_ICONS = {                  // static icon per effect_id; falls back to ✨ for unknown
  solar: '☀',
  sunset: '\u{1F305}',       // 🌅
  sunrise: '\u{1F304}',      // 🌄
  candlelight: '\u{1F56F}',  // 🕯
  aurora: '\u{1F30C}',       // 🌌
  breathing: '\u{1FAC1}',    // 🫁
  snake: '\u{1F40D}',        // 🐍
};
export const DEFAULT_EFFECT_ICON = '✨';
export const SCENE_ICON = '\u{1F3AD}';     // 🎭 — per-light "in scene" marker (generic; tweakable)

// "On" powers a room OR a single bulb up to a consistent Hue default warm white
// (≈2700K), so on/off is predictable. Users wanting a soft on/off use brightness
// instead. Order matters: brightness/temp before the on flag. Shared by the room
// and device handlers (lighting.js imports it).
export const HUE_DEFAULT_ON = [
  { action: 'brightness', value: 200 },
  { action: 'color_temp', value: 370 },
  { action: 'on' },
];
