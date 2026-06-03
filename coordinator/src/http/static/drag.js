// ── Shared pointer-drag gesture machinery ────────────────────────────────────
// One state machine for every "pick up and move" drag in the dashboard (effect
// chips, scene chips, …). It distinguishes a tap/scroll from a drag via a
// movement threshold and/or a press-and-hold, captures the pointer ONLY once a
// drag is confirmed (so pre-threshold scrolling still works), and guarantees
// cleanup on pointerup/pointercancel. Callers supply the visuals (ghost,
// highlight, reorder) via onStart / onMove / onEnd.
//
// This replaces several near-identical hand-rolled handlers that were a recurring
// source of scroll-vs-drag / pointer-capture bugs. The continuous colour-wheel /
// temp-bar drag (wireDragSurface in rooms.js) is intentionally NOT routed through
// here — it's a different, stable interaction (immediate capture, in-place edit).

// opts:
//   skipMouse — ignore mouse pointers (those sites use native HTML5 DnD). Default true.
//   distance  — px of movement that confirms a drag. Default 8.
//   holdMs    — if >0, the drag only arms after a press-and-hold of this long; a
//               swipe that exceeds `distance` before then is treated as a scroll
//               and the gesture is abandoned. Default 0 (arm immediately).
//   onStart(e) — fired once when the drag is confirmed (pointer now captured).
//   onMove(e)  — fired on each move while dragging.
//   onEnd(e)   — fired on release/cancel, only if a drag actually started.
export function createPointerDrag(el, { skipMouse = true, distance = 8, holdMs = 0, onStart, onMove, onEnd } = {}) {
  let startX = 0, startY = 0, pointerId = null, dragging = false, armed = true, holdTimer = null;

  const clearHold = () => { if (holdTimer) { clearTimeout(holdTimer); holdTimer = null; } };
  const reset = () => { dragging = false; pointerId = null; armed = true; clearHold(); };

  el.addEventListener('pointerdown', (e) => {
    if (skipMouse && e.pointerType === 'mouse') return;
    if (e.button !== 0 && e.button !== -1) return;
    startX = e.clientX; startY = e.clientY;
    pointerId = e.pointerId;
    dragging = false;
    armed = holdMs === 0;
    if (holdMs > 0) holdTimer = setTimeout(() => { armed = true; }, holdMs);
  });

  el.addEventListener('pointermove', (e) => {
    if (e.pointerId !== pointerId) return;
    if (!dragging) {
      const moved = Math.hypot(e.clientX - startX, e.clientY - startY);
      if (!armed) {
        // Moved past the threshold before the hold armed it → it's a scroll;
        // drop this gesture so the browser pans the row.
        if (moved > distance) reset();
        return;
      }
      if (moved < distance) return;
      dragging = true;
      try { el.setPointerCapture(e.pointerId); } catch { /* older browsers */ }
      onStart?.(e);
      e.preventDefault();
    }
    onMove?.(e);
  });

  const finish = (e) => {
    if (e.pointerId !== pointerId) return;
    if (el.hasPointerCapture?.(e.pointerId)) {
      try { el.releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    }
    const dragged = dragging;
    reset();
    if (dragged) onEnd?.(e);
  };
  el.addEventListener('pointerup', finish);
  el.addEventListener('pointercancel', finish);
}

// Floating drag image cloned from the source element. Append once on drag start,
// reposition with moveGhost on each move, and `.remove()` on end.
export function makeGhost(el) {
  const ghost = el.cloneNode(true);
  ghost.style.cssText =
    'position:fixed;pointer-events:none;opacity:0.85;z-index:9999;'
    + 'transform:translate(-50%,-50%);box-shadow:0 4px 16px rgba(0,0,0,0.4)';
  document.body.appendChild(ghost);
  return ghost;
}

export function moveGhost(ghost, x, y) {
  if (!ghost) return;
  ghost.style.left = `${x}px`;
  ghost.style.top = `${y}px`;
}

// Horizontal reorder: the sibling the dragged element should be inserted before,
// or null to append. `selector` should exclude the element being dragged.
export function insertionBefore(container, selector, clientX) {
  const others = [...container.querySelectorAll(selector)];
  return others.reduce((closest, c) => {
    const box = c.getBoundingClientRect();
    const offset = clientX - box.left - box.width / 2;
    if (offset < 0 && offset > closest.offset) return { offset, element: c };
    return closest;
  }, { offset: Number.NEGATIVE_INFINITY }).element ?? null;
}
