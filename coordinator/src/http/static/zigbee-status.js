// ── Zigbee bridge status ──────────────────────────────────────────────────────
// One decision, kept in its own leaf module so it can be tested directly: is
// the Zigbee bridge to be treated as offline? No imports, no state.

/// Decide whether the Zigbee bridge should be treated as offline.
///
/// `reported` is what the coordinator last sent on the `ZigbeeStatus` event —
/// `true` online, `false` offline, `null`/`undefined` unknown (its wire type is
/// `Option<bool>`, and no lighting node has reported yet). A report always wins;
/// the fallback only covers the window before one arrives.
///
/// **Deliberately does not infer "bridge down" from every light being offline.**
/// It used to, and the guess is unsound: a bulb switched off at the wall is
/// unreachable to zigbee2mqtt in exactly the same way as one behind a dead
/// bridge. The result was that the whole room UI disabled itself every night
/// once the lights went off — including the ✕ that removes a switch from a room
/// (2026-08-30; see ROADMAP.md). Lights being off is the normal state of a
/// house, not evidence of a fault.
export function deriveZigbeeStatus(reported, roomCount, lightCount) {
  if (reported !== null && reported !== undefined) return reported;
  // No report yet. Rooms exist but not one light has ever arrived — zigbee2mqtt
  // never connected. This inference is sound: it turns on lights never being
  // *known*, not on known lights being off.
  if (roomCount > 0 && lightCount === 0) return false;
  return true;
}
