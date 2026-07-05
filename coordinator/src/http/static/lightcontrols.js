// ── Common light control card ─────────────────────────────────────────────────
// The composite per-device light control widget shared by the lighting panel and
// the room device cards: on/off, brightness, and a tap-to-open temperature bar or
// hue/saturation wheel. Built from the leaf widgets in controls.js, painted via
// indicators.js. Lives in its own module (not controls.js) because it needs
// layout.ctToHex for the temperature gradient, and controls.js must stay
// import-free to avoid a cycle with layout.js.

import * as layout from '/static/layout.js';
import { clearDotForDevice, paintModeDot, paintDeviceButton } from '/static/indicators.js';
import { xyToRgb, rgbToHsl, hslToXy } from '/static/colormath.js';
import { buildColourWheel, makeLcSliderRow, wireDragSurface } from '/static/controls.js';
import { deviceDotDomain } from '/static/state.js';

// Collapse fn of the currently-open temp/colour section (only one at a time).
// rooms.js render() calls dismissOpenLightControl() before wiping cards.
let _lcOpenDismiss = null;

// Disarm the open temp/colour section before its card is removed, or its
// document-level pointerdown listener leaks against a detached card.
export function dismissOpenLightControl() {
  if (_lcOpenDismiss) _lcOpenDismiss();
}

// Renders a standardised control block usable in both the lighting panel and
// room device cards:
//   Row 1 (always): [On] [Off]  ──────────────────────────  [🌡][🎨 icons]
//   Row 2 (always): Brightness   ──────────●──────  78%
//   Row 3 (one of): Temperature OR Hue+Saturation — opened by tapping an icon
//
// Each supported domain gets a button: a glyph icon (🌡/🎨), or a live dot once
// you set that domain's value — the dot persists (see deviceDotDomain) showing
// the current value until brightness-aside changes clear it. Tapping a button
// opens that domain's control. Which control is open is persisted per device in
// localStorage (`mesh-mode-<id>`) — it can't be inferred from state because Hue
// bulbs always report both color_xy and color_temp.
//
// dev: LightStateReport-shaped object
// cb:  { onOn, onOff, onBrightness(v), onTemp(v), onColorXY(x,y) }
//      each callback fires on committed value (change event)
export function buildLightControls(dev, cb) {
  const hasTemp = dev.color_temp != null;

  const wrap = document.createElement('div');
  wrap.className = 'lc-wrap';

  // Repaints the colour/temp buttons from the current dot domain. Assigned to
  // applyMode() below for dual/single-capability bulbs; a no-op otherwise so the
  // on/off handlers can call it unconditionally.
  let refreshDots = () => {};

  // ── Row 1: on/off + toggle buttons ───────────────────────────────────────
  const row1 = document.createElement('div');
  row1.className = 'lc-row lc-row-controls';

  const onBtn = document.createElement('button');
  onBtn.className = 'light-toggle-btn light-toggle-on';
  onBtn.innerHTML = `<span class="badge ${dev.on ? 'badge-green' : 'badge-muted'}">On</span>`;
  // on/off clears this bulb's (and its room's) colour/temp dot back to an icon.
  onBtn.addEventListener('click', e => { e.stopPropagation(); clearDotForDevice(dev.device_id); refreshDots(); cb.onOn?.(); });

  const offBtn = document.createElement('button');
  offBtn.className = 'light-toggle-btn light-toggle-off';
  offBtn.innerHTML = `<span class="badge ${!dev.on ? 'badge-red' : 'badge-muted'}">Off</span>`;
  offBtn.addEventListener('click', e => { e.stopPropagation(); clearDotForDevice(dev.device_id); refreshDots(); cb.onOff?.(); });

  row1.appendChild(onBtn);
  row1.appendChild(offBtn);

  // Spacer
  const spacer = document.createElement('span');
  spacer.style.flex = '1';
  row1.appendChild(spacer);

  // ── Colour/temperature icons — one per supported domain ──────────────────
  // Hue bulbs always report BOTH color_xy and color_temp, so the open domain
  // can't be inferred from state — it's persisted per device in localStorage
  // (default Temperature). Adjusting a slider also pins that domain so the card
  // stays put after the next render.
  const supportsTemp   = hasTemp;
  const supportsColour = dev.color_xy != null;

  const modeKey = 'mesh-mode-' + dev.device_id;
  let lcMode = (!supportsTemp && !supportsColour) ? null
    : (localStorage.getItem(modeKey) || (supportsTemp ? 'temp' : 'colour'));
  if (lcMode === 'temp'   && !supportsTemp)   lcMode = 'colour';
  if (lcMode === 'colour' && !supportsColour) lcMode = 'temp';
  const setMode = (m) => { lcMode = m; localStorage.setItem(modeKey, m); };

  if (lcMode) {
    let h = 30, s = 80;
    if (dev.color_xy) {
      const [x, y] = dev.color_xy;
      const { r, g, b } = xyToRgb(x, y, dev.brightness ?? 254);
      ({ h, s } = rgbToHsl(r, g, b));
    }
    // One button per supported domain. At rest each shows its glyph icon; the
    // matching wheel/temp-bar drag handler turns it into a live dot and
    // applyMode() restores the icon. Tapping a button opens/closes its control.
    let tempBtn = null, colourBtn = null;
    const modeGroup = document.createElement('div');
    modeGroup.className = 'lc-mode-group';
    if (supportsTemp) {
      tempBtn = document.createElement('button');
      tempBtn.className = 'lc-mode-btn';
      tempBtn.dataset.domain = 'temp';
      modeGroup.appendChild(tempBtn);
    }
    if (supportsColour) {
      colourBtn = document.createElement('button');
      colourBtn.className = 'lc-mode-btn';
      colourBtn.dataset.domain = 'colour';
      modeGroup.appendChild(colourBtn);
    }
    row1.appendChild(modeGroup);

    // Secondary rows — only one visible at a time
    let tempRow = null, colourRows = null;

    if (supportsTemp) {
      tempRow = document.createElement('div');
      tempRow.className = 'lc-row lc-slider-row';
      const tempLabel = document.createElement('span');
      tempLabel.className = 'lc-label';
      tempLabel.textContent = 'Temperature';
      const tempVal = document.createElement('span');
      tempVal.className = 'lc-value';
      const fmtK = v => Math.round(1e6 / v) + 'K';
      tempVal.textContent = fmtK(dev.color_temp ?? 370);
      const tempBar = buildTempBar({
        mireds: dev.color_temp ?? 370,
        onInput: v => { tempVal.textContent = fmtK(v); paintModeDot(tempBtn, layout.ctToHex(v)); },
        onChange: v => { tempVal.textContent = fmtK(v); setMode('temp'); deviceDotDomain.set(dev.device_id, 'temp'); dev.color_temp = v; applyMode(); cb.onTemp?.(v); },
      });
      tempRow.append(tempLabel, tempBar, tempVal);
    }

    if (supportsColour) {
      colourRows = document.createElement('div');
      colourRows.className = 'lc-colour-wheel-wrap';
      colourRows.appendChild(buildColourWheel({
        hue: h, sat: s,
        onInput: (hh, ss) => paintModeDot(colourBtn, `hsl(${hh},${ss}%,50%)`),
        onChange: (hh, ss) => {
          setMode('colour'); deviceDotDomain.set(dev.device_id, 'colour');
          const { x, y } = hslToXy(hh, ss);
          dev.color_xy = [x, y];   // optimistic so the dot tints to the set colour now
          applyMode();
          cb.onColorXY?.(x, y);
        },
      }));
    }

    // The secondary control can be collapsed: tapping the active swatch again
    // hides it, and so does a tap anywhere outside the open section (popover
    // dismiss). Single-capability bulbs (no swatches) always show their control.
    let lcExpanded = false;
    let lcOutside = null;
    const disarmOutside = () => {
      if (lcOutside) { document.removeEventListener('pointerdown', lcOutside, true); lcOutside = null; }
    };
    const applyMode = () => {
      const showTemp   = supportsTemp   && lcMode === 'temp'   && lcExpanded;
      const showColour = supportsColour && lcMode === 'colour' && lcExpanded;
      if (tempRow)    tempRow.style.display    = showTemp   ? '' : 'none';
      if (colourRows) colourRows.style.display = showColour ? '' : 'none';
      // Each button shows its glyph icon, or a live dot if its domain is the one
      // last set for this bulb (persists until brightness-aside changes clear it).
      paintDeviceButton(tempBtn,   'temp',   dev);
      paintDeviceButton(colourBtn, 'colour', dev);
      // Ring the trigger whose control is currently open.
      tempBtn?.classList.toggle('active', showTemp);
      colourBtn?.classList.toggle('active', showColour);
    };
    refreshDots = applyMode;
    const collapse = () => {
      if (!lcExpanded) return;
      lcExpanded = false;
      applyMode();
      disarmOutside();
      if (_lcOpenDismiss === collapse) _lcOpenDismiss = null;
    };
    const expand = (mode) => {
      // Close any other card's open section first (one popover at a time).
      if (_lcOpenDismiss && _lcOpenDismiss !== collapse) _lcOpenDismiss();
      setMode(mode);
      lcExpanded = true;
      applyMode();
      _lcOpenDismiss = collapse;
      // Arm dismissal: a tap outside the open section (and off the swatches)
      // closes it. Capture phase so we see the tap before the wheel/bar's own
      // pointerdown calls stopPropagation.
      disarmOutside();
      lcOutside = (e) => {
        const sect = lcMode === 'temp' ? tempRow : colourRows;
        if (sect?.contains(e.target)) return;
        if (tempBtn?.contains(e.target) || colourBtn?.contains(e.target)) return;
        collapse();
      };
      document.addEventListener('pointerdown', lcOutside, true);
    };
    const pickMode = (mode) => {
      if (lcMode === mode && lcExpanded) collapse();   // tap active again → hide
      else expand(mode);
    };
    tempBtn?.addEventListener('click', e => { e.stopPropagation(); pickMode('temp'); });
    colourBtn?.addEventListener('click', e => { e.stopPropagation(); pickMode('colour'); });

    // Assemble once: controls row, brightness, then the active secondary section
    const briRow = makeLcSliderRow('Brightness', 1, 254, dev.brightness ?? 200,
      v => Math.round((v / 254) * 100) + '%',
      v => cb.onBrightness?.(v));
    wrap.append(row1, briRow);
    if (tempRow)    wrap.appendChild(tempRow);
    if (colourRows) wrap.appendChild(colourRows);
    applyMode();
  } else {
    // No secondary controls at all — just brightness
    const briRow = makeLcSliderRow('Brightness', 1, 254, dev.brightness ?? 200,
      v => Math.round((v / 254) * 100) + '%',
      v => cb.onBrightness?.(v));
    wrap.append(row1, briRow);
  }

  return wrap;
}

// ── Temperature bar ────────────────────────────────────────────────────────────
// A warm→cool track you can tap or drag anywhere along (no thumb-hit gate — the
// generic sliders are thumb-only, which is what made the old temperature control
// impossible to grab). The handle shows the live colour at the current
// temperature. Range is 154–500 mireds (cool ≈ 6500K → warm ≈ 2000K).
const TEMP_MIN_MIRED = 154, TEMP_MAX_MIRED = 500;
// Gradient sampled from the real mireds→colour mapping so the bar shows the
// actual perceived colour: cool/white on the left (154), warm/amber on the
// right (500). Single source of truth for the bar and the mode swatch.
const TEMP_GRADIENT = (() => {
  const stops = [];
  for (let i = 0; i <= 6; i++) {
    const m = TEMP_MIN_MIRED + (i / 6) * (TEMP_MAX_MIRED - TEMP_MIN_MIRED);
    stops.push(layout.ctToHex(m));
  }
  return `linear-gradient(to right, ${stops.join(', ')})`;
})();

// opts: { mireds, onInput?(m), onChange(m) }
// onInput fires live during the drag; onChange fires on release.
export function buildTempBar({ mireds, onInput, onChange }) {
  const bar = document.createElement('div');
  bar.className = 'temp-bar';
  bar.style.background = TEMP_GRADIENT;

  const handle = document.createElement('div');
  handle.className = 'temp-bar-handle';
  bar.appendChild(handle);

  let cur = mireds;
  const place = () => {
    const ratio = (cur - TEMP_MIN_MIRED) / (TEMP_MAX_MIRED - TEMP_MIN_MIRED);
    handle.style.left = `${ratio * 100}%`;
    handle.style.background = layout.ctToHex(cur);
  };
  const fromPointer = (e) => {
    const rect = bar.getBoundingClientRect();
    const ratio = Math.min(Math.max((e.clientX - rect.left) / rect.width, 0), 1);
    cur = Math.round(TEMP_MIN_MIRED + ratio * (TEMP_MAX_MIRED - TEMP_MIN_MIRED));
    place();
  };
  wireDragSurface(bar, {
    fromPointer,
    onInput: () => onInput?.(cur),
    onChange: () => onChange(cur),
  });
  place();
  return bar;
}
