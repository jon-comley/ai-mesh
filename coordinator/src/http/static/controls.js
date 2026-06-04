// ── Leaf input widgets ────────────────────────────────────────────────────────
// Pure, state-free DOM controls shared across the lighting UI: thumb-only range
// sliders, the hue/saturation colour wheel, and the pointer machinery they share.
// No imports, no module-level state — every widget is driven entirely by the
// caller's options/callbacks. (The temperature bar lives in lightcontrols.js
// because it needs layout.ctToHex for its gradient, and importing layout here
// would form a cycle with layout.js.)

// Prevent click-to-jump on any range slider — user must grab the thumb.
// Standalone helper for sliders not built via attachThumbSlider (e.g. the
// layout time-scrubber). Exported for layout.js.
export function lockSliderToThumb(slider) {
  slider.addEventListener('pointerdown', e => {
    const rect = slider.getBoundingClientRect();
    const ratio = (slider.value - slider.min) / (slider.max - slider.min);
    const thumbX = rect.left + ratio * rect.width;
    if (Math.abs(e.clientX - thumbX) > (e.pointerType === 'touch' ? 30 : 16)) {
      e.preventDefault();
      e.stopPropagation();
    }
  }, { capture: true });
}

// ── Shared slider core ────────────────────────────────────────────────────────
const SLIDER_THUMB_W = 18;

// The single thumb-only slider interaction used by every slider in the UI:
//  • value changes ONLY by grabbing the thumb (track clicks pass through so
//    card-level gestures like drag-to-reorder still fire)
//  • a value bubble follows the thumb while dragging
//  • the slider carries `.slider-active` (and the container `.dragging`) so
//    render() won't wipe the element out from under the user mid-drag
// slider: <input type=range>. opts: { format, bubble?, container?, onInput?(v), onChange(v) }
function attachThumbSlider(slider, { format, bubble, container, onInput, onChange }) {
  const positionBubble = () => {
    if (!bubble) return;
    const min = parseFloat(slider.min), max = parseFloat(slider.max);
    const ratio = (slider.value - min) / (max - min);
    const w = slider.getBoundingClientRect().width;
    const centre = SLIDER_THUMB_W / 2 + ratio * (w - SLIDER_THUMB_W);
    // offsetLeft is correct here because the bubble's offsetParent is the same
    // positioned ancestor offsetLeft is measured against, and no slider sits
    // inside a transformed parent. If that changes, switch to getBoundingClientRect
    // deltas (bubble vs offsetParent) for transform-safe positioning.
    bubble.style.left = `${slider.offsetLeft + centre}px`;
    bubble.textContent = format(parseInt(slider.value, 10));
  };

  slider.addEventListener('pointerdown', e => {
    const rect = slider.getBoundingClientRect();
    const min = parseFloat(slider.min), max = parseFloat(slider.max);
    const ratio = (slider.value - min) / (max - min);
    const thumbCentre = rect.left + SLIDER_THUMB_W / 2 + ratio * (rect.width - SLIDER_THUMB_W);
    const hitRadius = e.pointerType === 'touch' ? 26 : 16;
    if (Math.abs(e.clientX - thumbCentre) > hitRadius) { e.preventDefault(); return; }
    slider.classList.add('slider-active');
    container?.classList.add('dragging');
    bubble?.classList.add('visible');
    positionBubble();
  }, { capture: true });

  slider.addEventListener('input', () => {
    positionBubble();
    onInput?.(parseInt(slider.value, 10));
  });

  const finish = () => {
    slider.classList.remove('slider-active');
    container?.classList.remove('dragging');
    bubble?.classList.remove('visible');
  };
  slider.addEventListener('change', () => { finish(); onChange(parseInt(slider.value, 10)); });
  slider.addEventListener('pointercancel', finish);
}

// Full-width slider with its own label/value header and floating bubble.
// opts: { label, min, max, value, format(v)->string, onCommit(v)->void, onInput?(v)->void }
export function buildSlider(opts) {
  const { label, min, max, value, format, onCommit, onInput } = opts;

  const container = document.createElement('div');
  container.className = 'room-slider';

  const headerRow = document.createElement('div');
  headerRow.className = 'room-slider-header';
  const labelEl = document.createElement('span');
  labelEl.className = 'room-slider-label';
  labelEl.textContent = label;
  const valueEl = document.createElement('span');
  valueEl.className = 'room-slider-current-value';
  valueEl.textContent = format(value);
  headerRow.append(labelEl, valueEl);
  container.appendChild(headerRow);

  const track = document.createElement('div');
  track.className = 'room-slider-track';
  const bubble = document.createElement('div');
  bubble.className = 'room-slider-bubble';
  bubble.textContent = format(value);

  const slider = document.createElement('input');
  slider.type = 'range';
  slider.min = String(min);
  slider.max = String(max);
  slider.value = String(value);
  slider.className = 'light-slider room-slider-input';
  slider.autocomplete = 'off';
  slider.title = label;

  attachThumbSlider(slider, {
    format, bubble, container,
    onInput: v => { valueEl.textContent = format(v); onInput?.(v); },
    onChange: v => onCommit(v),
  });

  track.append(bubble, slider);
  container.appendChild(track);
  return container;
}

// Wire the shared interaction onto an existing inline <input type=range> (device
// cards, where the row HTML is fixed).
// opts: { format(v)->string, onInput(v, valEl)->void, onChange(v)->void }
export function wireDeviceSlider(slider, opts) {
  const { format, onInput, onChange } = opts;
  const valEl = slider.parentElement?.querySelector('.light-detail-value');

  const bubble = document.createElement('div');
  bubble.className = 'room-slider-bubble device-slider-bubble';
  slider.parentElement?.insertBefore(bubble, slider);

  attachThumbSlider(slider, {
    format, bubble,
    onInput: v => { if (valEl) onInput(v, valEl); },
    onChange: v => onChange(v),
  });
}

// A labelled brightness/value row for the light-control card (label · slider · value).
export function makeLcSliderRow(label, min, max, value, format, onCommit) {
  const row = document.createElement('div');
  row.className = 'lc-row lc-slider-row';

  const labelEl = document.createElement('span');
  labelEl.className = 'lc-label';
  labelEl.textContent = label;

  const valEl = document.createElement('span');
  valEl.className = 'lc-value';
  valEl.textContent = format(value);

  const slider = document.createElement('input');
  slider.type = 'range';
  slider.min = String(min);
  slider.max = String(max);
  slider.value = String(value);
  slider.className = 'light-slider lc-slider';
  slider.autocomplete = 'off';

  wireDeviceSlider(slider, {
    format,
    onInput: (v, _) => { valEl.textContent = format(v); },
    onChange: onCommit,
  });

  row.appendChild(labelEl);
  row.appendChild(slider);
  row.appendChild(valEl);
  return row;
}

// ── Colour wheel ──────────────────────────────────────────────────────────────
// A Hue/Saturation wheel (the "ball thing" from Hue): angle around the circle is
// hue (0° at top, clockwise), distance from the centre is saturation (centre =
// white, edge = full). Grab the knob (or tap anywhere) and drag.
//   opts: { hue, sat, onInput?(h,s), onChange(h,s) }
// onInput fires live during the drag (for preview); onChange fires on release.
// While dragging, the wheel carries `.dragging` so render() won't wipe it mid-drag.
export function buildColourWheel({ hue, sat, onInput, onChange }) {
  const wheel = document.createElement('div');
  wheel.className = 'colour-wheel';

  const knob = document.createElement('div');
  knob.className = 'colour-wheel-knob';
  wheel.appendChild(knob);

  let curH = hue, curS = sat;

  const placeKnob = () => {
    const r = curS / 100;                 // 0..1 from centre
    const rad = (curH * Math.PI) / 180;   // 0° at top, clockwise
    const x = 50 + r * Math.sin(rad) * 50;
    const y = 50 - r * Math.cos(rad) * 50;
    knob.style.left = `${x}%`;
    knob.style.top = `${y}%`;
    knob.style.background = `hsl(${curH},${curS}%,50%)`;
  };

  const fromPointer = (e) => {
    const rect = wheel.getBoundingClientRect();
    const dx = e.clientX - (rect.left + rect.width / 2);
    const dy = e.clientY - (rect.top + rect.height / 2);
    const R = rect.width / 2;
    curS = Math.round(Math.min(Math.hypot(dx, dy) / R, 1) * 100);
    let deg = (Math.atan2(dx, -dy) * 180) / Math.PI; // 0 at top, clockwise
    if (deg < 0) deg += 360;
    curH = Math.round(deg);
    placeKnob();
  };

  wireDragSurface(wheel, {
    fromPointer,
    onInput: () => onInput?.(curH, curS),
    onChange: () => onChange(curH, curS),
  });

  placeKnob();
  return wheel;
}

// Shared pointer machinery for the direct-manipulation colour controls (the
// colour wheel and the temperature bar). Grabbing the surface captures the
// pointer, disables native drag on EVERY draggable ancestor (device card AND
// room card) — otherwise the gesture is hijacked into a card reorder, no
// pointerup fires, and the value never commits (symptom: handle snaps back,
// light doesn't change) — and marks `.dragging` so render() won't wipe the
// control out from under the user mid-drag.
//   el: the draggable surface. fromPointer(e) reads the pointer and updates
//   internal state + visuals. onInput fires live during the drag (preview);
//   onChange fires once on release (commit).
export function wireDragSurface(el, { fromPointer, onInput, onChange }) {
  let dragging = false;
  let suppressedDrags = [];
  const suppressAncestorDrags = () => {
    suppressedDrags = [];
    for (let p = el.parentElement; p; p = p.parentElement) {
      if (p.getAttribute && p.getAttribute('draggable') === 'true') {
        p.setAttribute('draggable', 'false');
        suppressedDrags.push(p);
      }
    }
  };
  const restoreAncestorDrags = () => {
    suppressedDrags.forEach(p => p.setAttribute('draggable', 'true'));
    suppressedDrags = [];
  };

  el.addEventListener('pointerdown', e => {
    e.preventDefault();
    e.stopPropagation();
    dragging = true;
    el.classList.add('dragging');
    suppressAncestorDrags();
    try { el.setPointerCapture(e.pointerId); } catch { /* older browsers */ }
    fromPointer(e);
    onInput?.();
  });
  el.addEventListener('pointermove', e => {
    if (!dragging) return;
    fromPointer(e);
    onInput?.();
  });
  // Belt-and-braces: cancel any native drag that still tries to start.
  el.addEventListener('dragstart', e => e.preventDefault());
  const end = (e) => {
    if (!dragging) return;
    dragging = false;
    el.classList.remove('dragging');
    restoreAncestorDrags();
    try { el.releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    onChange();
  };
  el.addEventListener('pointerup', end);
  el.addEventListener('pointercancel', end);
}
