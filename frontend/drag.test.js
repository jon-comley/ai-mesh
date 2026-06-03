import { describe, it, expect, vi, beforeEach } from 'vitest';
import { createPointerDrag, insertionBefore } from '../coordinator/src/http/static/drag.js';

// jsdom has no PointerEvent; synthesise a plain Event with the fields createPointerDrag reads.
function pointer(type, { x = 0, y = 0, id = 1, pointerType = 'touch', button = 0 } = {}) {
  const e = new Event(type, { bubbles: true, cancelable: true });
  Object.assign(e, { clientX: x, clientY: y, pointerId: id, pointerType, button });
  return e;
}

describe('createPointerDrag', () => {
  let el, calls;
  beforeEach(() => {
    el = document.createElement('div');
    document.body.appendChild(el);
    calls = { start: 0, move: 0, end: 0 };
    createPointerDrag(el, {
      distance: 8,
      onStart: () => { calls.start++; },
      onMove: () => { calls.move++; },
      onEnd: () => { calls.end++; },
    });
  });

  it('starts a drag once movement passes the threshold, and ends on release', () => {
    el.dispatchEvent(pointer('pointerdown', { x: 0, y: 0 }));
    el.dispatchEvent(pointer('pointermove', { x: 3, y: 0 })); // below threshold
    expect(calls.start).toBe(0);
    el.dispatchEvent(pointer('pointermove', { x: 20, y: 0 })); // crosses threshold
    expect(calls.start).toBe(1);
    expect(calls.move).toBe(1);
    el.dispatchEvent(pointer('pointermove', { x: 30, y: 0 }));
    expect(calls.move).toBe(2);
    el.dispatchEvent(pointer('pointerup', { x: 30, y: 0 }));
    expect(calls.end).toBe(1);
  });

  it('treats a sub-threshold press+release as a tap (no drag)', () => {
    el.dispatchEvent(pointer('pointerdown', { x: 0, y: 0 }));
    el.dispatchEvent(pointer('pointermove', { x: 2, y: 2 }));
    el.dispatchEvent(pointer('pointerup', { x: 2, y: 2 }));
    expect(calls.start).toBe(0);
    expect(calls.end).toBe(0);
  });

  it('ignores mouse pointers by default (native DnD handles those)', () => {
    el.dispatchEvent(pointer('pointerdown', { x: 0, y: 0, pointerType: 'mouse' }));
    el.dispatchEvent(pointer('pointermove', { x: 50, y: 0, pointerType: 'mouse' }));
    expect(calls.start).toBe(0);
  });

  it('with holdMs, a swipe before the hold fires is a scroll (no drag)', () => {
    vi.useFakeTimers();
    const el2 = document.createElement('div');
    document.body.appendChild(el2);
    let started = 0;
    createPointerDrag(el2, { holdMs: 150, distance: 8, onStart: () => { started++; } });

    el2.dispatchEvent(pointer('pointerdown', { x: 0, y: 0 }));
    el2.dispatchEvent(pointer('pointermove', { x: 40, y: 0 })); // swipe before hold → scroll
    expect(started).toBe(0);
    vi.advanceTimersByTime(200);
    el2.dispatchEvent(pointer('pointermove', { x: 80, y: 0 })); // gesture already abandoned
    expect(started).toBe(0);
    vi.useRealTimers();
  });

  it('with holdMs, a held press then move starts the drag', () => {
    vi.useFakeTimers();
    const el2 = document.createElement('div');
    document.body.appendChild(el2);
    let started = 0;
    createPointerDrag(el2, { holdMs: 150, distance: 8, onStart: () => { started++; } });

    el2.dispatchEvent(pointer('pointerdown', { x: 0, y: 0 }));
    vi.advanceTimersByTime(200); // hold arms the drag
    el2.dispatchEvent(pointer('pointermove', { x: 20, y: 0 }));
    expect(started).toBe(1);
    vi.useRealTimers();
  });
});

describe('insertionBefore', () => {
  it('returns null for an empty container', () => {
    const bar = document.createElement('div');
    expect(insertionBefore(bar, '.chip', 100)).toBe(null);
  });
});
