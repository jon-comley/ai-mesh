// ── Layout canvas shared state ─────────────────────────────────────────────────
// The mutable state the layout-canvas modules all read and write: the room in
// view, the placed-bulb and placed-opening entry maps, and the live device
// snapshot map. Held on a holder object so every importer (layout.js and its
// extracted leaves — layout3d.js, sunmodels.js, …) mutates the same reference:
// ES-module imports are read-only bindings, so a bare exported `let` couldn't be
// reassigned from an importing module (e.g. `room` on a room switch, `bulbs` on
// rebuild). Mirrors static/state.js. Pure leaf: no imports.
export const layoutState = {
  room: null,          // RoomRecord currently in view
  bulbs: {},           // device_id → { x, y, z, fixture_type, el, labelEl }
  openings: {},        // opening_id → { opening_type, wall_edge, x_norm, width_norm, transmission, el }
  devices: new Map(),  // reference to rooms.js devicesMap — set via layout.init()
};
