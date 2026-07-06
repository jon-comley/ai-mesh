// ── Room Layout Canvas ────────────────────────────────────────────────────────
// SVG top-down floor plan for placing bulbs and (Phase B) windows/doors.
// Coordinates are always 0–1 normalised; the SVG scales to any screen size.

import { buildSlider, buildColourWheel, lockSliderToThumb } from '/static/controls.js';
import { tok } from '/static/api.js';
import { hslToXy, xyToRgb, rgbToHsl } from '/static/colormath.js';
import { solarPosition, todaySunriseSunset, calculateSolarState } from '/static/solar.js';
import { layoutState, WALL_THICKNESS } from '/static/layoutstate.js';
import {
  initThree, teardownThree, syncBulbToThree, syncOpeningToThree,
  removeBulbFromThree, removeOpeningFromThree, threeUpdateSun, threeUpdateBulbColor,
  setView3D, is3DActive, initLayout3d,
} from '/static/layout3d.js';
import {
  initSunModels,
  renderConesModel, renderGradientConesModel, renderCausticModel, renderBrightPatchModel,
  renderParallelBeamModel, renderBeamFootprintModel, renderSoftBeamModel,
  renderWallGlowModel, renderSunArcModel,
} from '/static/sunmodels.js';

// Give the 3D module the core helpers it calls back into (popover open/close,
// sidebar close, device→colour). Hoisted function declarations, so available now.
initLayout3d({ openPopover, dismissPopover, closeSidebarSheet, devStateColor });
// Give the sun-effect models svgEl + live compass/lat/lon (read via getters).
initSunModels({
  svgEl,
  getCompassDeg: () => compassDeg,
  getMeshLat: () => meshLat,
  getMeshLon: () => meshLon,
});

// ── State ─────────────────────────────────────────────────────────────────────

// Shared canvas state (room, placed bulbs/openings, device map) lives in
// layoutstate.js so the extracted layout modules can mutate the same references.
let lastSolar = { azimuth: 180, elevation: -90 };
let undoStack = [];             // position snapshots for Ctrl+Z
let redoStack = [];
// Snap step is 1 cm of real-world distance — applied per-axis using the
// room's actual width_m / depth_m so the grid is uniform in cm regardless
// of canvas aspect ratio. See snapTo / snapX / snapY below.
let showLabels = true;
let activePopover = null;       // currently open popover element
let dragType = null;            // 'bulb' | 'opening' — set on dragstart
const bulbDragStarters = {};    // deviceId → fn(e) — called when drag begins externally

function setCanvasDragClass(on) {
  // Starting a drag (e.g. a chip from the device palette) closes the bottom
  // sheet so the canvas is visible to drop onto.
  if (on) closeSidebarSheet();
  const svg = document.getElementById('layout-canvas');
  if (!svg) return;
  svg.classList.toggle('layout-dragging', on);
  // Belt and braces: directly set pointer-events on the crosshair hit so the
  // drop is never absorbed mid-drag, regardless of CSS specificity quirks.
  const hit = svg.querySelector('.lc-crosshair-hit');
  if (hit) hit.setAttribute('pointer-events', on ? 'none' : 'auto');
}
// Global safety net: clear dragType if a drag is cancelled or ends outside the canvas
window.addEventListener('dragend', () => { dragType = null; });
window.addEventListener('drop', () => { dragType = null; });

// Popover dismiss — capture phase fires before any other handler, including the
// popover's own stopPropagation. If the touch lands inside the popover but a
// draggable bulb is underneath, we dismiss and directly start the bulb's drag
// engine via bulbDragStarters (avoids synthetic PointerEvent dispatch, which
// can trigger a pointercancel when the popover element is removed from the DOM
// while holding implicit touch capture).
document.addEventListener('pointerdown', e => {
  if (!e.isTrusted || !activePopover) return;
  if (!activePopover.contains(e.target)) {
    dismissPopover();
    return;
  }
  // Touch landed ON the popover — look for a bulb g element underneath it.
  const under = document.elementsFromPoint(e.clientX, e.clientY);
  const bulbG = under.find(el => el.tagName === 'g' && el.dataset?.deviceId);
  if (!bulbG) return;
  const starter = bulbDragStarters[bulbG.dataset.deviceId];
  if (!starter) return;
  dismissPopover();
  starter(e);
}, { capture: true });

// ── Phase D: compass + sun arc state ─────────────────────────────────────────
let compassDeg = 0;             // current orientation_degrees for the open room
let compassDragging = false;
let compassOrientTimer = null;
let compassUpdateFrozen = false;
let compassFreezeTimer = null;
let compassPrevAngle = 0;       // pointer angle at previous pointermove frame (delta rotation)
let scrubberLive = true;        // false while time scrubber is in preview mode
let scrubberRafPending = false;
let scrubberThrottleTimer = null;
let scrubberPlayTimer = null;   // non-null while auto-play is running
let scrubberPlayCmdTimer = null; // throttles Zigbee commands during playback
let phoneOrientActive = false;
let phoneHeadingSamples = [];
let meshLat = 51.5074;          // populated from GET /api/solar/config on first open
let meshLon = -0.1278;
let solarConfigLoaded = false;
let sunCalibMode = false;
let lightModel = 'parallel-beam';  // persists across room switches

// Wall-photo backdrop (Phase 4 tracing aid) — which wall's photo is shown
// and how visible it is. Session/UI-only, not persisted per room; resets to
// the same default on every room switch, same as the other UI-state lets above.
let activeWallEdge = 'N';
let wallPhotoOpacity = 0.45;

const LIGHT_MODELS = [
  { id: 'parallel-beam',   label: 'Parallel beam' },
  { id: 'beam-footprint',  label: 'Beam + footprint' },
  { id: 'soft-beam',       label: 'Soft beam' },
  { id: 'cone',            label: 'Cone' },
  { id: 'gradient-cone',   label: 'Gradient cone' },
  { id: 'caustic',         label: 'Caustic patch' },
  { id: 'bright-patch',    label: 'Bright patch' },
  { id: 'wall-glow',       label: 'Wall glow' },
  { id: 'sun-arc',         label: 'Sun arc' },
];

const FIXTURE_TYPES = [
  { id: 'ceiling_spot', label: 'Ceiling spot', defaultZ: 1.0 },
  { id: 'pendant',      label: 'Pendant',      defaultZ: 0.6 },
  { id: 'table_lamp',   label: 'Table lamp',   defaultZ: 0.3 },
  { id: 'floor_lamp',   label: 'Floor lamp',   defaultZ: 0.1 },
  { id: 'led_strip',    label: 'LED strip',    defaultZ: 0.5 },
];

// ── Public API ────────────────────────────────────────────────────────────────

export function init(devicesMap) {
  layoutState.devices = devicesMap;
}

export function currentLayoutRoomId() {
  return layoutState.room?.id ?? null;
}

export async function openLayout(room) {
  layoutState.room = room;
  layoutState.bulbs = {};
  layoutState.openings = {};
  layoutState.wallPhotos = {};
  undoStack = [];
  redoStack = [];
  scrubberLive = true;
  sunCalibMode = false;
  activeWallEdge = 'N';
  compassDeg = room.orientation_degrees ?? 0;

  const container = document.getElementById('home-list');
  for (const child of container.children) child.style.display = 'none';

  document.getElementById('panel-home')?.classList.add('layout-open');

  const view = buildLayoutView(room);
  container.appendChild(view);

  // Load solar config (lat/lon) once; used by JS solar position calculator
  if (!solarConfigLoaded) {
    try {
      const cfg = await fetch('/api/solar/config').then(r => r.json());
      meshLat = cfg.lat; meshLon = cfg.lon;
      solarConfigLoaded = true;
    } catch (_) {}
  }
  // Seed lastSolar from the JS calculator so the canvas looks correct before the first WS push
  { const s = solarPosition(Date.now(), meshLat, meshLon); lastSolar = { azimuth: s.azimuth, elevation: s.elevation }; }

  loadPlacedBulbs(room.id);
  loadPlacedOpenings(room.id).then(syncCeilingControl);
  loadWallPhotos(room.id);
  renderCrosshair(room);
  renderWallDims(room);
  initThree(room, lastSolar).catch(() => {});

  // Compass dial sets the room's orientation — useful regardless of whether
  // the room is currently solar-enabled, so render it always.
  renderCompassDial();
  wireCompass();
  wirePhoneCompass();

  if (isSolarActiveHere()) {
    wireSunCalib();
    wireScrubber();
    wireModelSelect();
    redrawSolarOverlay(lastSolar.azimuth, lastSolar.elevation);
    redrawLightEffect(lastSolar.azimuth, lastSolar.elevation);
    updateSunCalibButton();
  } else {
    // Hide solar-specific controls for rooms without solar effect
    const scrubBar = document.getElementById('lc-scrubber-bar');
    if (scrubBar) scrubBar.style.display = 'none';
  }

  document.addEventListener('keydown', onKeyDown);
}

export function closeLayout() {
  document.removeEventListener('keydown', onKeyDown);
  stopScrubberPlay();
  dismissPopover();
  teardownThree();

  document.getElementById('panel-home')?.classList.remove('layout-open');

  const container = document.getElementById('home-list');
  const view = container.querySelector('.layout-view');
  if (view) view.remove();

  for (const child of container.children) child.style.display = '';

  layoutState.room = null;
  layoutState.bulbs = {};
  layoutState.openings = {};
}

// Called by rooms.js when a LightingUpdate WS event arrives so canvas icons stay live.
export function notifyDeviceUpdate(deviceId, state) {
  const entry = layoutState.bulbs[deviceId];
  if (!entry || !scrubberLive || iconUpdatesFrozen) return;
  updateBulbIcon(entry, state);
  threeUpdateBulbColor(deviceId, state);
}

// Temporarily suppress WS-driven icon updates so bulk commands (All On / All Off)
// don't disturb the current canvas visual. Caller passes the freeze duration in ms.
let iconUpdatesFrozen = false;
let iconFreezeTimer = null;
export function freezeIconUpdates(ms = 3000) {
  iconUpdatesFrozen = true;
  clearTimeout(iconFreezeTimer);
  iconFreezeTimer = setTimeout(() => { iconUpdatesFrozen = false; }, ms);
}

// Per-room map of currently-active effect: { effectId, params }.
// Fed by notifyEffectActive so the layout panel can reflect effect params
// (e.g. solar min/max brightness) in the scrubber preview without importing rooms.js.
const activeEffectByRoom = new Map(); // roomId → { effectId, params }

// Called by rooms.js when a RoomsUpdate WS event arrives — updates the active room object.
export function notifyRoomUpdate(room) {
  if (layoutState.room && layoutState.room.id === room.id) {
    layoutState.room = room;
    redrawLightEffect(lastSolar.azimuth, lastSolar.elevation);
    renderCrosshair(room);
    renderWallDims(room);
  }
}

// Called by rooms.js when an EffectUpdate WS event arrives. `effectId === null`
// clears the room's active effect.
export function notifyEffectActive(roomId, effectId, params = {}) {
  if (effectId == null) activeEffectByRoom.delete(roomId);
  else activeEffectByRoom.set(roomId, { effectId, params });
}

function isSolarActiveHere() {
  return layoutState.room != null && activeEffectByRoom.get(layoutState.room.id)?.effectId === 'solar';
}

function getSolarParams() {
  if (!layoutState.room) return {};
  const entry = activeEffectByRoom.get(layoutState.room.id);
  if (!entry || entry.effectId !== 'solar') return {};
  return entry.params ?? {};
}

// Called by rooms.js when a SolarUpdate WS event arrives — redraws cones + arc.
export function notifySolarUpdate(azimuth, elevation) {
  lastSolar = { azimuth, elevation };
  if (scrubberLive) {
    redrawLightEffect(azimuth, elevation);
    redrawSolarOverlay(azimuth, elevation);
  }
  updateSunCalibButton();
  threeUpdateSun(azimuth, elevation);
}

// Called by rooms.js when a RoomsUpdate arrives with a new orientation for this room.
export function notifyOrientationUpdate(deg) {
  if (compassDragging || compassUpdateFrozen) return;
  compassDeg = deg;
  const dial = document.getElementById('lc-compass-dial');
  if (dial) dial.setAttribute('transform', `rotate(${dialAngle(deg)},925,75)`);
  if (scrubberLive) previewSolarState(lastSolar.azimuth, lastSolar.elevation);
}

// ── Phase D: Compass dial ─────────────────────────────────────────────────────

// compassDeg = the real-world bearing the top canvas wall faces (0=N, 90=E…).
// The rose rotates by the INVERSE so that when you spin the rose clockwise,
// N moves toward the direction where real-world North actually sits in the room.
// dialAngle() converts compassDeg → rose rotation angle (self-inverse function).
function dialAngle(deg) { return (360 - ((deg % 360) + 360) % 360) % 360; }

function renderCompassDial() {
  const g = document.getElementById('lc-compass');
  if (!g) return;
  g.innerHTML = '';

  // Outer ring
  g.appendChild(svgEl('circle', { cx: 925, cy: 75, r: 52, fill: 'rgba(0,0,0,0.6)', stroke: '#555', 'stroke-width': 1.5 }));

  // Fixed needle — always points up (= canvas top wall direction)
  g.appendChild(svgEl('polygon', { points: '925,29 929,50 921,50', fill: '#e8c84a', 'pointer-events': 'none' }));
  g.appendChild(svgEl('polygon', { points: '925,121 929,100 921,100', fill: '#555', 'pointer-events': 'none' }));
  g.appendChild(svgEl('circle', { cx: 925, cy: 75, r: 5, fill: '#fff', 'pointer-events': 'none' }));

  // Rotating compass rose — spin until N faces real-world north in your room
  const dial = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  dial.id = 'lc-compass-dial';
  dial.setAttribute('transform', `rotate(${dialAngle(compassDeg)},925,75)`);
  for (const [txt, x, y] of [['N', 925, 35], ['S', 925, 118], ['E', 969, 79], ['W', 881, 79]]) {
    const t = svgEl('text', { x, y, 'text-anchor': 'middle', 'dominant-baseline': 'central',
      'font-size': txt === 'N' ? 20 : 15, 'font-weight': 700,
      fill: txt === 'N' ? '#e8c84a' : '#aaa', 'pointer-events': 'none' });
    t.textContent = txt;
    dial.appendChild(t);
  }
  g.appendChild(dial);

  // Sun position dot — absolute compass bearing, hidden at night
  const sunDot = svgEl('circle', {
    id: 'lc-compass-sun', cx: 925, cy: 33, r: 5,
    fill: '#FFD700', 'pointer-events': 'none',
  });
  sunDot.style.display = 'none';
  g.appendChild(sunDot);

  // Invisible drag handle covering the whole compass circle
  const handle = svgEl('circle', { id: 'lc-compass-handle', cx: 925, cy: 75, r: 52,
    fill: 'transparent', style: 'cursor:grab' });
  const tip = document.createElementNS('http://www.w3.org/2000/svg', 'title');
  tip.textContent = 'Spin so N points toward real-world north in your room';
  handle.appendChild(tip);
  g.appendChild(handle);
}

function svgAngleFromClient(svg, clientX, clientY) {
  const pt = svg.createSVGPoint();
  pt.x = clientX; pt.y = clientY;
  const sp = pt.matrixTransform(svg.getScreenCTM().inverse());
  return Math.atan2(sp.y - 75, sp.x - 925) * 180 / Math.PI;
}

function freezeCompassUpdate(ms = 2000) {
  compassUpdateFrozen = true;
  clearTimeout(compassFreezeTimer);
  compassFreezeTimer = setTimeout(() => { compassUpdateFrozen = false; }, ms);
}

function wireCompass() {
  const svg = document.getElementById('layout-canvas');
  if (!svg) return;

  svg.addEventListener('pointerdown', e => {
    if (e.target.id !== 'lc-compass-handle') return;
    compassDragging = true;
    compassUpdateFrozen = true;   // freeze WS updates immediately on grab
    clearTimeout(compassFreezeTimer);
    svg.setPointerCapture(e.pointerId);
    e.target.style.cursor = 'grabbing';
    // Record pointer angle at drag start (for delta-based rotation)
    compassPrevAngle = svgAngleFromClient(svg, e.clientX, e.clientY);
  });

  svg.addEventListener('pointermove', e => {
    if (!compassDragging) return;
    const curAngle = svgAngleFromClient(svg, e.clientX, e.clientY);
    let delta = curAngle - compassPrevAngle;
    // Unwrap to [-180, 180] to handle the ±180° seam
    if (delta > 180) delta -= 360;
    if (delta < -180) delta += 360;
    compassPrevAngle = curAngle;
    // Rose rotates clockwise → compassDeg decreases by the same delta
    compassDeg = ((compassDeg - delta) % 360 + 360) % 360;
    const dial = document.getElementById('lc-compass-dial');
    if (dial) dial.setAttribute('transform', `rotate(${dialAngle(compassDeg)},925,75)`);
    previewSolarState(lastSolar.azimuth, lastSolar.elevation);
  });

  svg.addEventListener('pointerup', e => {
    if (!compassDragging) return;
    compassDragging = false;
    const handle = document.getElementById('lc-compass-handle');
    if (handle) handle.style.cursor = 'grab';
    clearTimeout(compassOrientTimer);
    compassOrientTimer = setTimeout(() => patchOrientation(layoutState.room?.id, compassDeg), 400);
    freezeCompassUpdate(2000);
  });

  svg.addEventListener('pointercancel', () => {
    if (!compassDragging) return;
    compassDragging = false;
    const handle = document.getElementById('lc-compass-handle');
    if (handle) handle.style.cursor = 'grab';
    freezeCompassUpdate(2000);
  });
}

async function patchOrientation(roomId, deg) {
  if (!roomId) return;
  try {
    await fetch(`/api/rooms/${encodeURIComponent(roomId)}/orientation?token=${encodeURIComponent(tok())}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ orientation_degrees: deg }),
    });
  } catch (_) {}
}

// ── Phase D: Phone compass wizard ─────────────────────────────────────────────

function wirePhoneCompass() {
  const btn = document.getElementById('lc-phone-compass-btn');
  if (!btn) return;

  // Hide on desktop where orientation API is absent
  if (!('ondeviceorientation' in window)) { btn.style.display = 'none'; return; }

  btn.addEventListener('click', () => showCompassWizard());
}

function showCompassWizard() {
  // Remove any existing wizard
  document.getElementById('lc-compass-wizard')?.remove();

  const overlay = document.createElement('div');
  overlay.id = 'lc-compass-wizard';
  overlay.className = 'lc-wizard-overlay';

  let currentStep = 1;
  let orientHandler = null;
  let lockedHeading = null;

  function renderStep() {
    overlay.innerHTML = '';
    const box = document.createElement('div');
    box.className = 'lc-wizard-box';

    const closeBtn = document.createElement('button');
    closeBtn.className = 'lc-wizard-close';
    closeBtn.textContent = '✕';
    closeBtn.addEventListener('click', closeWizard);
    box.appendChild(closeBtn);

    if (currentStep === 1) {
      // Step 1: intro
      box.innerHTML += `
        <div class="lc-wizard-step-label">Step 1 of 3</div>
        <h3 class="lc-wizard-title">Set room orientation with your phone</h3>
        <p class="lc-wizard-body">
          Stand in the centre of your room and hold your phone <strong>flat</strong>,
          screen facing up. Point the <strong>top edge</strong> of your phone
          toward the wall you want to mark as <strong>North</strong> on the canvas.
        </p>
        <p class="lc-wizard-body">
          Tap <em>Next</em> — your phone compass will begin reading automatically.
        </p>
      `;
      box.querySelector('.lc-wizard-close').remove();
      box.appendChild(closeBtn);
      const footer = document.createElement('div');
      footer.className = 'lc-wizard-footer';
      const cancelBtn = document.createElement('button');
      cancelBtn.className = 'lc-wizard-btn lc-wizard-btn-secondary';
      cancelBtn.textContent = 'Cancel';
      cancelBtn.addEventListener('click', closeWizard);
      const nextBtn = document.createElement('button');
      nextBtn.className = 'lc-wizard-btn lc-wizard-btn-primary';
      nextBtn.textContent = 'Next →';
      nextBtn.addEventListener('click', () => { currentStep = 2; renderStep(); });
      footer.appendChild(cancelBtn);
      footer.appendChild(nextBtn);
      box.appendChild(footer);

    } else if (currentStep === 2) {
      // Step 2: live reading
      box.innerHTML += `
        <div class="lc-wizard-step-label">Step 2 of 3</div>
        <h3 class="lc-wizard-title">Hold steady — reading compass</h3>
        <p class="lc-wizard-body">
          Keep your phone flat with its <strong>top edge</strong> pointing at your chosen North wall.
          Wait for the reading to settle, then tap <em>Lock heading</em>.
        </p>
        <div class="lc-wizard-heading-display" id="lc-wizard-heading">— °</div>
        <div class="lc-wizard-heading-label">averaged compass heading</div>
      `;
      box.querySelector('.lc-wizard-close').remove();
      box.appendChild(closeBtn);
      const footer = document.createElement('div');
      footer.className = 'lc-wizard-footer';
      const backBtn = document.createElement('button');
      backBtn.className = 'lc-wizard-btn lc-wizard-btn-secondary';
      backBtn.textContent = '← Back';
      backBtn.addEventListener('click', () => {
        stopOrientListener();
        currentStep = 1;
        renderStep();
      });
      const lockBtn = document.createElement('button');
      lockBtn.className = 'lc-wizard-btn lc-wizard-btn-primary';
      lockBtn.id = 'lc-wizard-lock-btn';
      lockBtn.textContent = 'Lock heading';
      lockBtn.addEventListener('click', () => {
        if (lockedHeading == null) return;
        stopOrientListener();
        currentStep = 3;
        renderStep();
      });
      footer.appendChild(backBtn);
      footer.appendChild(lockBtn);
      box.appendChild(footer);

      // Start listening
      startOrientListener();

    } else if (currentStep === 3) {
      // Step 3: confirm
      const deg = Math.round(lockedHeading ?? compassDeg);
      box.innerHTML += `
        <div class="lc-wizard-step-label">Step 3 of 3</div>
        <h3 class="lc-wizard-title">Confirm orientation</h3>
        <p class="lc-wizard-body">
          Your room will be set to face <strong>${deg}°</strong> (from true North).
          The compass dial and sun arc on the canvas will update immediately.
        </p>
        <p class="lc-wizard-body">Tap <em>Apply</em> to save.</p>
      `;
      box.querySelector('.lc-wizard-close').remove();
      box.appendChild(closeBtn);
      const footer = document.createElement('div');
      footer.className = 'lc-wizard-footer';
      const backBtn = document.createElement('button');
      backBtn.className = 'lc-wizard-btn lc-wizard-btn-secondary';
      backBtn.textContent = '← Redo';
      backBtn.addEventListener('click', () => {
        lockedHeading = null;
        currentStep = 2;
        renderStep();
      });
      const applyBtn = document.createElement('button');
      applyBtn.className = 'lc-wizard-btn lc-wizard-btn-primary';
      applyBtn.textContent = 'Apply';
      applyBtn.addEventListener('click', () => {
        compassDeg = lockedHeading ?? compassDeg;
        const dial = document.getElementById('lc-compass-dial');
        if (dial) dial.setAttribute('transform', `rotate(${dialAngle(compassDeg)},925,75)`);
        previewSolarState(lastSolar.azimuth, lastSolar.elevation);
        patchOrientation(layoutState.room?.id, compassDeg);
        closeWizard();
      });
      footer.appendChild(backBtn);
      footer.appendChild(applyBtn);
      box.appendChild(footer);
    }

    overlay.appendChild(box);
  }

  function startOrientListener() {
    phoneHeadingSamples = [];

    const requestAndStart = async () => {
      if (typeof DeviceOrientationEvent !== 'undefined' &&
          typeof DeviceOrientationEvent.requestPermission === 'function') {
        const perm = await DeviceOrientationEvent.requestPermission().catch(() => 'denied');
        if (perm !== 'granted') {
          document.getElementById('lc-wizard-heading').textContent = 'Permission denied';
          return;
        }
      }
      orientHandler = (e) => {
        const h = (e.webkitCompassHeading != null)
          ? e.webkitCompassHeading
          : (e.alpha != null ? ((e.alpha % 360 + 360) % 360) : null);
        if (h == null) return;

        phoneHeadingSamples.push(h);
        if (phoneHeadingSamples.length > 10) phoneHeadingSamples.shift();

        const sinSum = phoneHeadingSamples.reduce((s, a) => s + Math.sin(a * Math.PI / 180), 0);
        const cosSum = phoneHeadingSamples.reduce((s, a) => s + Math.cos(a * Math.PI / 180), 0);
        const avg = (Math.atan2(sinSum, cosSum) * 180 / Math.PI + 360) % 360;
        lockedHeading = avg;

        const display = document.getElementById('lc-wizard-heading');
        if (display) display.textContent = `${Math.round(avg)}°`;
      };
      window.addEventListener('deviceorientationabsolute', orientHandler);
      window.addEventListener('deviceorientation', orientHandler);
    };

    requestAndStart();
  }

  function stopOrientListener() {
    if (orientHandler) {
      window.removeEventListener('deviceorientationabsolute', orientHandler);
      window.removeEventListener('deviceorientation', orientHandler);
      orientHandler = null;
    }
  }

  function closeWizard() {
    stopOrientListener();
    overlay.remove();
  }

  renderStep();
  // Attach to the canvas-outer div so it floats above the canvas
  document.querySelector('.layout-canvas-outer')?.appendChild(overlay)
    ?? document.body.appendChild(overlay);
}

// ── Phase D: Sun arc overlay ──────────────────────────────────────────────────

function redrawSolarOverlay(azimuth, elevation) {
  // Compass sun dot — absolute bearing on the compass ring, hidden below civil twilight
  const sunDot = document.getElementById('lc-compass-sun');
  if (!sunDot) return;
  const visible = elevation > -6;
  sunDot.style.display = visible ? '' : 'none';
  if (visible) {
    // Position relative to the rotated rose so the dot tracks the correct
    // cardinal label. dialAngle(compassDeg) is the rose rotation, so the
    // dot must be placed at that same offset from the absolute azimuth.
    const rad = (dialAngle(compassDeg) + azimuth - 90) * Math.PI / 180;
    sunDot.setAttribute('cx', (925 + 42 * Math.cos(rad)).toFixed(1));
    sunDot.setAttribute('cy', (75  + 42 * Math.sin(rad)).toFixed(1));
  }
}

function previewSolarState(azimuth, elevation) {
  lastSolar = { azimuth, elevation };  // keep in sync so model-change redraws use correct position
  const { bri, ct } = calculateSolarState(elevation, getSolarParams());
  for (const [id, entry] of Object.entries(layoutState.bulbs)) {
    const state = { on: true, brightness: bri, color_temp: ct, color_xy: null };
    updateBulbIcon(entry, state);
    threeUpdateBulbColor(id, state);
  }
  redrawSolarOverlay(azimuth, elevation);
  redrawLightEffect(azimuth, elevation);
  threeUpdateSun(azimuth, elevation);
}

// ── Light model selector ──────────────────────────────────────────────────────

function wireModelSelect() {
  const sel = document.getElementById('lc-model-select');
  if (!sel) return;
  sel.value = lightModel;
  sel.addEventListener('change', () => {
    lightModel = sel.value;
    redrawLightEffect(lastSolar.azimuth, lastSolar.elevation);
  });
}

// ── Phase D: Time scrubber ────────────────────────────────────────────────────

function stopScrubberPlay() {
  if (scrubberPlayTimer) { clearInterval(scrubberPlayTimer); scrubberPlayTimer = null; }
  if (scrubberPlayCmdTimer) { clearTimeout(scrubberPlayCmdTimer); scrubberPlayCmdTimer = null; }
  const playBtn = document.getElementById('lc-scrubber-play');
  if (playBtn) playBtn.textContent = '▶';
}

function wireScrubber() {
  const scrubber = document.getElementById('lc-scrubber');
  const liveBtn  = document.getElementById('lc-scrubber-live');
  const timeEl   = document.getElementById('lc-scrubber-time');
  const playBtn  = document.getElementById('lc-scrubber-play');
  if (!scrubber || !liveBtn || !timeEl) return;

  lockSliderToThumb(scrubber);
  scrubber.addEventListener('input', () => {
    scrubberLive = false;
    setScrubberSimMode(true);
    if (scrubberRafPending) return;
    scrubberRafPending = true;
    requestAnimationFrame(() => {
      scrubberRafPending = false;
      const mins = parseInt(scrubber.value);
      const base = new Date(); base.setHours(0, mins, 0, 0);
      const { azimuth, elevation } = solarPosition(base.getTime(), meshLat, meshLon);
      const hh = String(Math.floor(mins / 60)).padStart(2, '0');
      const mm = String(mins % 60).padStart(2, '0');
      timeEl.textContent = `${hh}:${mm}`;
      previewSolarState(azimuth, elevation);

      // Trailing-edge debounce — only send physical commands after the user
      // pauses scrubbing. Resets on every RAF tick so rapid dragging produces
      // a single command batch fired 500 ms after the last movement.
      clearTimeout(scrubberThrottleTimer);
      scrubberThrottleTimer = setTimeout(() => {
        scrubberThrottleTimer = null;
        if (!scrubberLive) sendSimSolarCommands(lastSolar.elevation);
      }, 500);
    });
  });

  // On release: cancel the pending debounce and send immediately so the final
  // position is always reflected even if the user releases quickly.
  scrubber.addEventListener('change', () => {
    clearTimeout(scrubberThrottleTimer);
    scrubberThrottleTimer = null;
    if (!scrubberLive) sendSimSolarCommands(lastSolar.elevation);
  });

  liveBtn.addEventListener('click', () => {
    stopScrubberPlay();
    if (scrubberThrottleTimer) {
      clearTimeout(scrubberThrottleTimer);
      scrubberThrottleTimer = null;
    }
    scrubberLive = true;
    setScrubberSimMode(false);
    timeEl.textContent = 'Now';
    const now = new Date();
    scrubber.value = now.getHours() * 60 + now.getMinutes();

    // Use the real current position
    const s = solarPosition(now.getTime(), meshLat, meshLon);
    lastSolar = { azimuth: s.azimuth, elevation: s.elevation };

    redrawSolarOverlay(lastSolar.azimuth, lastSolar.elevation);
    redrawLightEffect(lastSolar.azimuth, lastSolar.elevation);

    // Restore bulb icons to real states and push current solar to physical bulbs
    for (const [deviceId, entry] of Object.entries(layoutState.bulbs)) {
      updateBulbIcon(entry, layoutState.devices.get(deviceId));
    }
    sendSimSolarCommands(lastSolar.elevation);
  });

  if (playBtn) {
    playBtn.addEventListener('click', () => {
      if (scrubberPlayTimer) {
        // Pause
        stopScrubberPlay();
        return;
      }
      // Start playing — enter sim mode if not already
      scrubberLive = false;
      setScrubberSimMode(true);
      playBtn.textContent = '⏸';

      // Advance 5 simulated minutes every 150 ms ≈ full day in ~72 s
      scrubberPlayTimer = setInterval(() => {
        const mins = (parseInt(scrubber.value) + 5) % 1440;
        scrubber.value = mins;
        const base = new Date(); base.setHours(0, mins, 0, 0);
        const { azimuth, elevation } = solarPosition(base.getTime(), meshLat, meshLon);
        const hh = String(Math.floor(mins / 60)).padStart(2, '0');
        const mm = String(mins % 60).padStart(2, '0');
        timeEl.textContent = `${hh}:${mm}`;
        previewSolarState(azimuth, elevation);

        // Send Zigbee commands at most once per second to avoid flooding the bus
        if (!scrubberPlayCmdTimer) {
          scrubberPlayCmdTimer = setTimeout(() => {
            scrubberPlayCmdTimer = null;
            if (scrubberPlayTimer) sendSimSolarCommands(lastSolar.elevation);
          }, 1000);
        }
      }, 150);
    });
  }
}

// Send the simulated solar brightness + colour temperature to all bulbs in the
// currently open room. Called on scrubber mouse-release only — not on every
// drag tick — to avoid flooding the Zigbee bus.
// Optional onlyDeviceIds: if provided, only these IDs are processed.
async function sendSimSolarCommands(elevation, onlyDeviceIds = null) {
  const room = layoutState.room;
  if (!room || !room.device_ids) return;
  const { bri, ct } = calculateSolarState(elevation, getSolarParams());
  const t = tok();

  const targetIds = (onlyDeviceIds || room.device_ids).filter(id => layoutState.bulbs[id]);

  for (const deviceId of targetIds) {
    // Match the successful "grab" sequence from rooms.js: ON, then color_temp, then brightness.
    // This ensures bulbs respond even if they were in an idle or off state.
    const url = `/api/lights/${encodeURIComponent(deviceId)}/command?token=${encodeURIComponent(t)}`;
    const post = (action, val, trans) => {
      const body = { action };
      if (val !== undefined) body.value = val;
      if (trans !== undefined) body.transition_secs = trans;
      fetch(url, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }).catch(() => {});
    };

    post('on');
    // 40ms offsets to ensure sequential processing at the coordinator/agent level
    setTimeout(() => post('color_temp', ct, 0.6), 40);
    setTimeout(() => post('brightness', bri, 0.6), 80);
  }
}
function setScrubberSimMode(active) {
  const canvas = document.getElementById('layout-canvas');
  const chip   = document.getElementById('lc-sim-chip');
  if (canvas) canvas.classList.toggle('lc-sim-mode', active);
  if (chip)   chip.style.display = active ? '' : 'none';
}

// ── Phase D: Sun calibration ──────────────────────────────────────────────────

function updateSunCalibButton() {
  const btn = document.getElementById('lc-sun-calib');
  if (btn) btn.style.display = lastSolar.elevation > 5 ? '' : 'none';
}

function wireSunCalib() {
  const btn = document.getElementById('lc-sun-calib');
  if (!btn) return;
  btn.addEventListener('click', () => {
    if (sunCalibMode) { exitSunCalibMode(); return; }
    enterSunCalibMode();
  });
}

function enterSunCalibMode() {
  sunCalibMode = true;
  const canvas = document.getElementById('layout-canvas');
  if (canvas) canvas.classList.add('lc-calib-mode');
  const preview = document.getElementById('lc-preview');
  if (!preview) return;
  preview.innerHTML = '';
  const walls = [
    { label: 'N', facing: 0,   d: 'M 0 0 L 1000 0',     hitX: 0,   hitY: 0,   hitW: 1000, hitH: 40 },
    { label: 'S', facing: 180, d: 'M 0 1000 L 1000 1000', hitX: 0,   hitY: 960, hitW: 1000, hitH: 40 },
    { label: 'E', facing: 90,  d: 'M 1000 0 L 1000 1000', hitX: 960, hitY: 0,   hitW: 40,   hitH: 1000 },
    { label: 'W', facing: 270, d: 'M 0 0 L 0 1000',       hitX: 0,   hitY: 0,   hitW: 40,   hitH: 1000 },
  ];
  for (const wall of walls) {
    const rect = svgEl('rect', {
      x: wall.hitX, y: wall.hitY, width: wall.hitW, height: wall.hitH,
      class: 'lc-wall-calib-target',
    });
    const lbl = svgEl('text', {
      x: wall.hitX + wall.hitW / 2,
      y: wall.hitY + wall.hitH / 2,
      'text-anchor': 'middle', 'dominant-baseline': 'central',
      'font-size': 40, fill: 'var(--amber)', 'pointer-events': 'none',
    });
    lbl.textContent = wall.label;
    rect.addEventListener('click', () => {
      const orientation = ((lastSolar.azimuth - wall.facing) % 360 + 360) % 360;
      exitSunCalibMode();
      compassDeg = orientation;
      const dial = document.getElementById('lc-compass-dial');
      if (dial) dial.setAttribute('transform', `rotate(${dialAngle(compassDeg)},925,75)`);
      redrawSolarOverlay(lastSolar.azimuth, lastSolar.elevation);
      patchOrientation(layoutState.room?.id, compassDeg);
    });
    preview.appendChild(rect);
    preview.appendChild(lbl);
  }
}

function exitSunCalibMode() {
  sunCalibMode = false;
  const canvas = document.getElementById('layout-canvas');
  if (canvas) canvas.classList.remove('lc-calib-mode');
  const preview = document.getElementById('lc-preview');
  if (preview) preview.innerHTML = '';
}

// ── View construction ─────────────────────────────────────────────────────────

function buildLayoutView(room) {
  const view = document.createElement('div');
  view.className = 'layout-view';

  // Header
  const header = document.createElement('div');
  header.className = 'layout-header';

  const backBtn = document.createElement('button');
  backBtn.className = 'layout-back-btn';
  backBtn.textContent = '← Rooms';
  backBtn.addEventListener('click', closeLayout);
  header.appendChild(backBtn);

  const title = document.createElement('span');
  title.className = 'layout-title';
  title.textContent = room.name;
  title.title = 'Tap to rename room';
  title.addEventListener('click', () => {
    const inp = document.createElement('input');
    inp.className = 'layout-title-input';
    inp.value = layoutState.room?.name ?? title.textContent;
    title.replaceWith(inp);
    inp.select();
    const commit = () => {
      const name = inp.value.trim();
      if (name && layoutState.room && name !== layoutState.room.name) {
        layoutState.room.name = name;
        fetch(`/api/rooms/${encodeURIComponent(layoutState.room.id)}/name?token=${encodeURIComponent(tok())}`, {
          method: 'PATCH', headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ name }),
        }).catch(() => {});
      }
      title.textContent = layoutState.room?.name ?? name;
      inp.replaceWith(title);
    };
    inp.addEventListener('blur', commit);
    inp.addEventListener('keydown', e => {
      if (e.key === 'Enter') { e.preventDefault(); inp.blur(); }
      if (e.key === 'Escape') { inp.replaceWith(title); }
    });
    inp.focus();
  });
  header.appendChild(title);
  view.appendChild(header);

  // Body: sidebar (a bottom sheet on phone) + resize handle + canvas
  const body = document.createElement('div');
  body.className = 'layout-body';

  const sidebar = buildSidebar(room);
  body.appendChild(sidebar);
  body.appendChild(buildSidebarResizeHandle(sidebar));
  body.appendChild(buildCanvas());
  view.appendChild(body);

  // Backdrop behind the device-palette sheet (phone only; hidden on desktop).
  const backdrop = document.createElement('div');
  backdrop.className = 'layout-sheet-backdrop';
  backdrop.addEventListener('click', closeSidebarSheet);
  view.appendChild(backdrop);

  // Bottom action bar — the primary one-handed controls. It sits in normal flow
  // as the last row of the layout view, resting just above the app tab bar, so
  // no fixed-position safe-area maths is needed.
  view.appendChild(buildActionBar(sidebar));

  return view;
}

// The phone-first control strip at the bottom of the layout view. On desktop the
// same bar still works but the sidebar is shown inline (see CSS), so ＋ Add is a
// no-op convenience there.
// Room lighting controls for the action bar: brightness / colour / temperature
// icons that reveal their control (slider / wheel) one at a time. Commands the
// whole room; the 2D icons + 3D render update live via the WS round-trip
// (notifyDeviceUpdate). Shown in both 2D and 3D.
function buildRoomLightControls() {
  const wrap = document.createElement('div');
  wrap.className = 'layout-light-group';

  const pop = document.createElement('div');
  pop.className = 'layout-light-popover';
  pop.style.display = 'none';

  const roomDevices = () => (layoutState.room?.device_ids ?? [])
    .map(id => layoutState.devices.get(id)).filter(Boolean);

  let openKind = null, outside = null;
  const close = () => {
    openKind = null;
    pop.style.display = 'none';
    pop.innerHTML = '';
    wrap.querySelectorAll('.layout-light-btn').forEach(b => b.classList.remove('active'));
    if (outside) { document.removeEventListener('pointerdown', outside, true); outside = null; }
  };
  const open = (kind, btn, buildControl) => {
    if (openKind === kind) { close(); return; }   // tap active again → hide
    close();
    openKind = kind;
    btn.classList.add('active');
    pop.appendChild(buildControl());
    pop.style.display = '';
    // Position above the icon (the action bar sits at the bottom of the layout
    // view), clamped to the viewport so it's never off-screen on laptop or mobile.
    const r = btn.getBoundingClientRect();
    const pw = pop.offsetWidth, ph = pop.offsetHeight;
    let top = r.top - ph - 8;
    if (top < 8) top = Math.min(r.bottom + 8, window.innerHeight - ph - 8); // flip below if no room above
    pop.style.left = `${Math.min(Math.max(r.left, 8), window.innerWidth - pw - 8)}px`;
    pop.style.top = `${Math.max(8, top)}px`;
    // Capture-phase outside-pointerdown dismiss (one open at a time).
    outside = (e) => {
      if (pop.contains(e.target) || e.target.closest?.('.layout-light-btn')) return;
      close();
    };
    document.addEventListener('pointerdown', outside, true);
  };
  const icon = (glyph, title, kind, buildControl) => {
    const btn = document.createElement('button');
    btn.className = 'layout-act-btn layout-light-btn';
    btn.textContent = glyph;
    btn.title = title;
    btn.addEventListener('click', e => { e.stopPropagation(); open(kind, btn, buildControl); });
    wrap.appendChild(btn);
  };

  // Brightness — always available.
  icon('🔆', 'Room brightness', 'bri', () => {
    const ds = roomDevices().filter(d => d.brightness != null);
    const avg = ds.length ? Math.round(ds.reduce((s, d) => s + (d.brightness ?? 0), 0) / ds.length) : 200;
    return buildSlider({
      label: 'Brightness', min: 1, max: 254, value: avg,
      format: v => Math.round(v / 254 * 100) + '%',
      onCommit: v => sendLayoutRoomCommand({ action: 'brightness', value: v }),
    });
  });

  // Colour — only if some bulb reports colour.
  if (roomDevices().some(d => d.color_xy != null)) {
    icon('🎨', 'Room colour', 'colour', () => {
      const wheelWrap = document.createElement('div');
      wheelWrap.className = 'lc-colour-wheel-wrap';
      const dev = roomDevices().find(d => d.color_xy != null);
      let h = 30, s = 80;
      if (dev) {
        const [x, y] = dev.color_xy;
        const { r, g, b } = xyToRgb(x, y, dev.brightness ?? 254);
        ({ h, s } = rgbToHsl(r, g, b));
      }
      wheelWrap.appendChild(buildColourWheel({
        hue: h, sat: s,
        onChange: (hh, ss) => {
          const { x, y } = hslToXy(hh, ss);
          sendLayoutRoomCommand({ action: 'color_xy', x, y });
        },
      }));
      return wheelWrap;
    });
  }

  // Temperature — only if some bulb reports colour temp.
  if (roomDevices().some(d => d.color_temp != null)) {
    icon('\u{1F321}\u{FE0F}', 'Room temperature', 'temp', () => {
      const ds = roomDevices().filter(d => d.color_temp != null);
      const avg = ds.length ? Math.round(ds.reduce((s, d) => s + (d.color_temp ?? 0), 0) / ds.length) : 370;
      return buildSlider({
        label: 'Temperature', min: 154, max: 500, value: avg,
        format: v => Math.round(1e6 / v) + 'K',
        onCommit: v => sendLayoutRoomCommand({ action: 'color_temp', value: v }),
      });
    });
  }

  wrap.appendChild(pop);
  return wrap;
}

function buildActionBar(sidebar) {
  const bar = document.createElement('div');
  bar.className = 'layout-action-bar';

  // 2D / 3D segmented toggle
  const seg = document.createElement('div');
  seg.className = 'layout-seg';
  const seg2d = document.createElement('button');
  seg2d.className = 'layout-seg-btn active';
  seg2d.textContent = '2D';
  const seg3d = document.createElement('button');
  seg3d.className = 'layout-seg-btn';
  seg3d.textContent = '3D';
  seg2d.addEventListener('click', () => setView3D(false, seg2d, seg3d));
  seg3d.addEventListener('click', () => setView3D(true, seg2d, seg3d));
  seg.append(seg2d, seg3d);
  bar.appendChild(seg);

  // Room lighting controls (brightness / colour / temp) — both 2D and 3D.
  bar.appendChild(buildRoomLightControls());

  // Undo / Redo — shown in both 2D and 3D (undo a move and see it in the 3D view).
  const undoBtn = document.createElement('button');
  undoBtn.className = 'layout-act-btn';
  undoBtn.textContent = '↩';
  undoBtn.title = 'Undo';
  undoBtn.addEventListener('click', undo);
  bar.appendChild(undoBtn);

  const redoBtn = document.createElement('button');
  redoBtn.className = 'layout-act-btn';
  redoBtn.textContent = '↪';
  redoBtn.title = 'Redo';
  redoBtn.addEventListener('click', redo);
  bar.appendChild(redoBtn);

  // Labels toggle — 2D only (the 3D view never shows labels).
  const labelsBtn = document.createElement('button');
  labelsBtn.className = 'layout-act-btn layout-2d-only' + (showLabels ? ' active' : '');
  labelsBtn.textContent = '🏷';
  labelsBtn.title = 'Show device labels';
  labelsBtn.addEventListener('click', () => {
    setShowLabels(!showLabels);
    labelsBtn.classList.toggle('active', showLabels);
  });
  bar.appendChild(labelsBtn);

  const spacer = document.createElement('span');
  spacer.className = 'layout-act-spacer';
  bar.appendChild(spacer);

  // ＋ Add — opens the device-palette sheet (phone). Editing only.
  const addBtn = document.createElement('button');
  addBtn.className = 'layout-act-btn layout-add-btn layout-edit-only';
  addBtn.textContent = '＋ Add';
  addBtn.title = 'Add lights & openings';
  addBtn.addEventListener('click', () => toggleSidebarSheet(sidebar));
  bar.appendChild(addBtn);

  return bar;
}

// Show/hide device labels in the 2D SVG. The 3D view never shows labels (the
// toggle is hidden there).
function setShowLabels(v) {
  showLabels = v;
  Object.values(layoutState.bulbs).forEach(e => {
    if (!e.el) return;
    e.el.querySelectorAll('text, rect[fill="rgba(0,0,0,0.55)"]').forEach(el => {
      el.style.display = showLabels ? '' : 'none';
    });
  });
}

// Open/close the device palette (phone). It opens as a centred modal (CSS-driven,
// see the media query) — the backdrop dims and taps to close. On desktop the
// sidebar is inline, the backdrop is hidden, and this just toggles a class.
function toggleSidebarSheet(sidebar) {
  const open = !sidebar.classList.contains('sheet-open');
  if (open) sidebar.classList.remove('collapsed'); // collapsed hides all sections
  sidebar.classList.toggle('sheet-open', open);
  document.querySelector('.layout-sheet-backdrop')?.classList.toggle('visible', open);
}
function closeSidebarSheet() {
  document.querySelector('.layout-sidebar')?.classList.remove('sheet-open');
  document.querySelector('.layout-sheet-backdrop')?.classList.remove('visible');
}

const SIDEBAR_WIDTH_KEY = 'mesh-layout-sidebar-width';
const SIDEBAR_MIN_PX = 80;
const SIDEBAR_MAX_PX = 320;

function buildSidebarResizeHandle(sidebar) {
  const handle = document.createElement('div');
  handle.className = 'layout-sidebar-resize';

  // Restore saved width — desktop only, so an inline width never overrides the
  // mobile bottom-sheet layout (which is full-width).
  const saved = parseInt(localStorage.getItem(SIDEBAR_WIDTH_KEY), 10);
  if (saved >= SIDEBAR_MIN_PX && saved <= SIDEBAR_MAX_PX
      && window.matchMedia('(min-width: 900px)').matches) {
    sidebar.style.width = saved + 'px';
  }

  let dragging = false;
  let startX = 0;
  let startW = 0;

  handle.addEventListener('pointerdown', e => {
    if (sidebar.classList.contains('collapsed')) return;
    dragging = true;
    startX = e.clientX;
    startW = sidebar.getBoundingClientRect().width;
    handle.setPointerCapture(e.pointerId);
    document.body.style.cursor = 'col-resize';
    e.preventDefault();
  });

  handle.addEventListener('pointermove', e => {
    if (!dragging) return;
    const w = Math.min(SIDEBAR_MAX_PX, Math.max(SIDEBAR_MIN_PX, startW + (e.clientX - startX)));
    sidebar.style.width = w + 'px';
  });

  const stopDrag = e => {
    if (!dragging) return;
    dragging = false;
    document.body.style.cursor = '';
    handle.releasePointerCapture(e.pointerId);
    const w = sidebar.getBoundingClientRect().width;
    localStorage.setItem(SIDEBAR_WIDTH_KEY, Math.round(w));
  };
  handle.addEventListener('pointerup', stopDrag);
  handle.addEventListener('pointercancel', stopDrag);

  return handle;
}

function makeCollapsibleSection(titleText, storageKey, defaultCollapsed = false) {
  const wrap = document.createElement('div');
  wrap.className = 'layout-sidebar-section';

  const head = document.createElement('button');
  head.type = 'button';
  head.className = 'layout-sidebar-section-head';
  const chev = document.createElement('span');
  chev.className = 'layout-sidebar-section-chevron';
  const titleEl = document.createElement('span');
  titleEl.className = 'layout-sidebar-section-title';
  titleEl.textContent = titleText;
  head.appendChild(chev);
  head.appendChild(titleEl);

  const body = document.createElement('div');
  body.className = 'layout-sidebar-section-body';

  const stored = localStorage.getItem(storageKey);
  const isCollapsed = stored === '1' || (stored === null && defaultCollapsed);
  chev.textContent = isCollapsed ? '▶' : '▼';
  if (isCollapsed) body.classList.add('collapsed');

  head.addEventListener('click', () => {
    const willCollapse = !body.classList.contains('collapsed');
    body.classList.toggle('collapsed', willCollapse);
    chev.textContent = willCollapse ? '▶' : '▼';
    localStorage.setItem(storageKey, willCollapse ? '1' : '0');
  });

  wrap.appendChild(head);
  wrap.appendChild(body);
  return { wrap, body };
}

function expandLightsSection() {
  const body = document.getElementById('layout-chips-section-body');
  if (!body || !body.classList.contains('collapsed')) return;
  body.classList.remove('collapsed');
  localStorage.setItem('mesh-layout-sb-bulbs', '0');
  const chev = body.parentElement?.querySelector('.layout-sidebar-section-chevron');
  if (chev) chev.textContent = '▼';
}

function showLightsDropzone() {
  const section = document.getElementById('layout-lights-section');
  if (section) section.style.display = '';
  expandLightsSection();
  const chips = document.getElementById('layout-sidebar-chips');
  if (chips && !document.getElementById('layout-chips-dropzone')) {
    const dz = document.createElement('div');
    dz.id = 'layout-chips-dropzone';
    dz.className = 'layout-chips-dropzone';
    dz.textContent = '↑ Drag a light here to unplace';
    chips.appendChild(dz);
  }
}

function hideLightsDropzone() {
  document.getElementById('layout-chips-dropzone')?.remove();
  const unplaced = (layoutState.room?.device_ids || []).filter(id => !layoutState.bulbs[id]);
  const section = document.getElementById('layout-lights-section');
  if (section) section.style.display = unplaced.length === 0 ? 'none' : '';
}

function buildSidebar(room) {
  const sidebar = document.createElement('div');
  sidebar.className = 'layout-sidebar';

  // Top-level collapse — slides the whole panel down to a thin strip so the
  // canvas gets the freed space. Click the chevron to expand again.
  const toggle = document.createElement('button');
  toggle.type = 'button';
  toggle.className = 'layout-sidebar-toggle';
  toggle.title = 'Show / hide menu';
  const sidebarCollapsedKey = 'mesh-layout-sidebar-collapsed';
  const updateToggle = () => {
    const collapsed = sidebar.classList.contains('collapsed');
    toggle.textContent = collapsed ? '▶' : '◀';
    toggle.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
  };
  if (localStorage.getItem(sidebarCollapsedKey) === '1') {
    sidebar.classList.add('collapsed');
  }
  updateToggle();
  toggle.addEventListener('click', () => {
    const nowCollapsed = !sidebar.classList.contains('collapsed');
    sidebar.classList.toggle('collapsed', nowCollapsed);
    localStorage.setItem(sidebarCollapsedKey, nowCollapsed ? '1' : '0');
    updateToggle();
  });
  sidebar.appendChild(toggle);

  const bulbs = makeCollapsibleSection('Lights', 'mesh-layout-sb-bulbs');
  bulbs.wrap.id = 'layout-lights-section';
  sidebar.appendChild(bulbs.wrap);
  bulbs.body.id = 'layout-chips-section-body';

  const chips = document.createElement('div');
  chips.className = 'layout-sidebar-chips';
  chips.id = 'layout-sidebar-chips';
  bulbs.body.appendChild(chips);

  const openings = makeCollapsibleSection('Openings', 'mesh-layout-sb-openings', true);
  sidebar.appendChild(openings.wrap);

  const openingChips = document.createElement('div');
  openingChips.className = 'layout-sidebar-chips';
  for (const { type, label: lbl } of [
    { type: 'window', label: '⬜ Window' },
    { type: 'door',   label: '▯ Door'   },
  ]) {
    const chip = document.createElement('div');
    chip.className = 'layout-chip layout-opening-chip';
    chip.draggable = true;
    chip.textContent = lbl;
    chip.addEventListener('dragstart', e => {
      dragType = 'opening';
      setCanvasDragClass(true);
      e.dataTransfer.effectAllowed = 'copy';
      e.dataTransfer.setData('text/plain', `opening:${type}`);
    });
    chip.addEventListener('dragend', () => { dragType = null; setCanvasDragClass(false); });
    wireChipTouchDrag(chip, 'opening', type);
    openingChips.appendChild(chip);
  }
  openings.body.appendChild(openingChips);

  // Glass/partial-glass ceiling (skylight/conservatory roof): unlike a
  // window/door, it has no compass-facing wall to drop onto, so it's a
  // simple dropdown rather than a draggable canvas chip — see
  // setCeilingGlazing/syncCeilingControl below.
  const ceiling = makeCollapsibleSection('Ceiling', 'mesh-layout-sb-ceiling', true);
  sidebar.appendChild(ceiling.wrap);
  const ceilingSelect = document.createElement('select');
  ceilingSelect.id = 'layout-ceiling-select';
  ceilingSelect.className = 'layout-ceiling-select';
  for (const [value, label] of [
    ['none', 'None'],
    ['partial', 'Partial glass'],
    ['full', 'Full glass'],
  ]) {
    const opt = document.createElement('option');
    opt.value = value;
    opt.textContent = label;
    ceilingSelect.appendChild(opt);
  }
  ceilingSelect.value = 'none';
  ceilingSelect.addEventListener('change', () => setCeilingGlazing(ceilingSelect.value));
  ceiling.body.appendChild(ceilingSelect);

  sidebar.appendChild(buildWallPhotoSection());

  const dims = makeCollapsibleSection('Room size (m)', 'mesh-layout-sb-dims', true);
  sidebar.appendChild(dims.wrap);

  const dimsRow = document.createElement('div');
  dimsRow.className = 'layout-dims-row';
  for (const { key, label: lbl, def } of [
    { key: 'width_m',  label: 'W', def: room.width_m  ?? 3.0 },
    { key: 'depth_m',  label: 'D', def: room.depth_m  ?? 6.0 },
    { key: 'height_m', label: 'H', def: room.height_m ?? 2.5 },
  ]) {
    const wrap = document.createElement('label');
    wrap.className = 'layout-dims-field';
    wrap.textContent = lbl + ' ';
    const inp = document.createElement('input');
    inp.type = 'number';
    inp.min = '0.5';
    inp.max = '50';
    inp.step = '0.1';
    inp.value = def;
    inp.addEventListener('change', () => {
      const val = parseFloat(inp.value);
      if (!isFinite(val) || val < 0.1) return;
      if (layoutState.room) {
        layoutState.room[key] = val;
        renderWallDims(layoutState.room);
        const body = {};
        body[key] = val;
        fetch(`/api/rooms/${encodeURIComponent(layoutState.room.id)}/dimensions?token=${encodeURIComponent(tok())}`, {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        }).catch(() => {});
      }
    });
    wrap.appendChild(inp);
    dimsRow.appendChild(wrap);
  }
  dims.body.appendChild(dimsRow);

  // Placed bulbs — position panel below room size
  const placed = makeCollapsibleSection('Placed', 'mesh-layout-sb-placed', false);
  sidebar.appendChild(placed.wrap);
  const placedBody = document.createElement('div');
  placedBody.id = 'layout-placed-body';
  const placedHdr = document.createElement('div');
  placedHdr.className = 'layout-placed-header';
  placedHdr.textContent = '⊕ x, y from ↖ corner (m)';
  placedBody.appendChild(placedHdr);
  placed.body.appendChild(placedBody);

  sidebar._room = room;
  return sidebar;
}

function rebuildSidebar() {
  const chips = document.getElementById('layout-sidebar-chips');
  if (!chips) return;
  chips.innerHTML = '';

  const room = layoutState.room;
  const unplaced = (room.device_ids || []).filter(id => !layoutState.bulbs[id]);

  const lightsSection = document.getElementById('layout-lights-section');
  if (unplaced.length === 0) {
    if (lightsSection) lightsSection.style.display = 'none';
  } else {
    if (lightsSection) lightsSection.style.display = '';
    for (const id of unplaced) {
      chips.appendChild(makeSidebarChip(id));
    }
  }

  rebuildPlacedPanel();
}

function rebuildPlacedPanel() {
  const body = document.getElementById('layout-placed-body');
  if (!body) return;
  // Remove previous entries (keep the header)
  [...body.children].forEach(c => { if (!c.classList.contains('layout-placed-header')) c.remove(); });

  const room = layoutState.room;
  if (!room) return;
  const W = room.width_m || 3;
  const D = room.depth_m || 6;

  for (const [deviceId, entry] of Object.entries(layoutState.bulbs)) {
    const dev = layoutState.devices.get(deviceId);
    const name = dev?.friendly_name ?? deviceId;

    const row = document.createElement('div');
    row.className = 'layout-placed-entry';

    const nameEl = document.createElement('div');
    nameEl.className = 'layout-placed-name';
    nameEl.textContent = name.length > 14 ? name.slice(0, 13) + '…' : name;
    row.appendChild(nameEl);

    const coordsRow = document.createElement('div');
    coordsRow.className = 'layout-placed-coords';

    for (const { axis, label, getValue, setValue } of [
      { axis: 'x', label: 'x', getValue: () => (entry.x * W), setValue: v => { entry.x = Math.max(0, Math.min(1, v / W)); } },
      { axis: 'y', label: 'y', getValue: () => (entry.y * D), setValue: v => { entry.y = Math.max(0, Math.min(1, v / D)); } },
    ]) {
      const field = document.createElement('label');
      field.className = 'layout-placed-coord-field';
      field.textContent = label + ' ';
      const inp = document.createElement('input');
      inp.type = 'number';
      inp.min = '0'; inp.step = '0.01';
      inp.max = axis === 'x' ? String(W) : String(D);
      inp.value = getValue().toFixed(2);
      inp.addEventListener('change', () => {
        const v = parseFloat(inp.value);
        if (!isFinite(v)) return;
        pushUndo();
        setValue(v);
        placeBulb(deviceId, entry.x, entry.y, entry.z, entry.fixture_type, true);
      });
      field.appendChild(inp);
      coordsRow.appendChild(field);
    }

    row.appendChild(coordsRow);
    body.appendChild(row);
  }

  // ── Openings (doors & windows) ──────────────────────────────────────────────
  const openingEntries = Object.entries(layoutState.openings);
  if (openingEntries.length > 0) {
    const divider = document.createElement('div');
    divider.className = 'layout-placed-divider';
    divider.textContent = 'Doors & Windows';
    body.appendChild(divider);

    for (const [id, o] of openingEntries) {
      const wallLen = (o.wall_edge === 'N' || o.wall_edge === 'S') ? W : D;
      const icon = o.opening_type === 'window' ? '⬜' : '▯';
      const label = `${icon} ${o.opening_type === 'window' ? 'Window' : 'Door'} — ${o.wall_edge} wall`;

      const row = document.createElement('div');
      row.className = 'layout-placed-entry';

      const nameEl = document.createElement('div');
      nameEl.className = 'layout-placed-name';
      nameEl.textContent = label;
      row.appendChild(nameEl);

      const coordsRow = document.createElement('div');
      coordsRow.className = 'layout-placed-coords';

      // pos = centre position along wall in metres
      const posField = document.createElement('label');
      posField.className = 'layout-placed-coord-field';
      posField.textContent = 'pos ';
      const posInp = document.createElement('input');
      posInp.type = 'number';
      posInp.min = '0'; posInp.step = '0.01';
      posInp.max = String(wallLen);
      posInp.value = (o.x_norm * wallLen).toFixed(2);
      posInp.dataset.openingPos = id;
      posInp.addEventListener('change', () => {
        const v = parseFloat(posInp.value);
        if (!isFinite(v)) return;
        const half = o.width_norm / 2;
        o.x_norm = Math.min(1 - half - 0.02, Math.max(half + 0.02, v / wallLen));
        posInp.value = (o.x_norm * wallLen).toFixed(2);
        updateOpeningRectAttrs(id);
        patchOpening(id, { x_norm: o.x_norm });
      });
      posField.appendChild(posInp);
      coordsRow.appendChild(posField);

      // width in metres
      const widField = document.createElement('label');
      widField.className = 'layout-placed-coord-field';
      widField.textContent = 'w ';
      const widInp = document.createElement('input');
      widInp.type = 'number';
      widInp.min = '0.05'; widInp.step = '0.01';
      widInp.max = String(wallLen);
      widInp.value = (o.width_norm * wallLen).toFixed(2);
      widInp.dataset.openingWid = id;
      widInp.addEventListener('change', () => {
        const v = parseFloat(widInp.value);
        if (!isFinite(v)) return;
        o.width_norm = Math.min(0.96, Math.max(0.05, v / wallLen));
        // Keep centre within bounds after width change
        const half = o.width_norm / 2;
        o.x_norm = Math.min(1 - half - 0.02, Math.max(half + 0.02, o.x_norm));
        widInp.value = (o.width_norm * wallLen).toFixed(2);
        posInp.value = (o.x_norm * wallLen).toFixed(2);
        updateOpeningRectAttrs(id);
        patchOpening(id, { x_norm: o.x_norm, width_norm: o.width_norm });
      });
      widField.appendChild(widInp);
      coordsRow.appendChild(widField);

      row.appendChild(coordsRow);
      body.appendChild(row);
    }
  }
}

// ── Wall dimension labels ─────────────────────────────────────────────────────
// Renders/refreshes clickable W and D labels along the bottom and right walls.
// Clicking opens a small floating input to edit the value directly.
function renderWallDims(room) {
  const layer = document.getElementById('lc-wall-dims');
  if (!layer) return;
  layer.innerHTML = '';

  const W = room.width_m || 3;
  const D = room.depth_m || 6;

  const mkLabel = (x, y, text, rotate, key, currentVal, maxVal) => {
    const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
    g.setAttribute('class', 'lc-wall-dim');
    if (rotate) g.setAttribute('transform', `rotate(-90,${x},${y})`);

    const bg = svgEl('rect', { x: x - 52, y: y - 14, width: 104, height: 26, rx: 5,
      fill: 'rgba(0,0,0,0.5)', class: 'lc-wall-dim-bg', 'pointer-events': 'none' });
    const txt = svgEl('text', { x, y: y + 1, 'text-anchor': 'middle', 'dominant-baseline': 'middle',
      fill: 'rgba(255,255,255,0.65)', 'font-size': 20, 'font-family': 'monospace',
      class: 'lc-wall-dim-text', 'pointer-events': 'none' });
    txt.textContent = text;
    g.appendChild(bg); g.appendChild(txt);

    g.addEventListener('click', e => {
      e.stopPropagation();
      const svg = document.getElementById('layout-canvas');
      const pt = svg.createSVGPoint();
      pt.x = x; pt.y = y;
      const sc = pt.matrixTransform(svg.getScreenCTM());

      const overlay = document.createElement('div');
      overlay.className = 'layout-wall-dim-edit';
      overlay.style.left = `${sc.x - 55}px`;
      overlay.style.top  = `${Math.min(sc.y - 36, window.innerHeight - 60)}px`;
      const lbl = document.createElement('span');
      lbl.textContent = key.toUpperCase() + ' (m):';
      const inp = document.createElement('input');
      inp.type = 'number'; inp.min = '0.5'; inp.max = '50'; inp.step = '0.1';
      inp.value = currentVal.toFixed(2);
      overlay.appendChild(lbl); overlay.appendChild(inp);
      document.body.appendChild(overlay);
      inp.focus(); inp.select();

      const commit = () => {
        const v = parseFloat(inp.value);
        overlay.remove();
        if (!isFinite(v) || v < 0.1 || !layoutState.room) return;
        const body = {}; body[key + '_m'] = v;
        fetch(`/api/rooms/${encodeURIComponent(layoutState.room.id)}/dimensions?token=${encodeURIComponent(tok())}`, {
          method: 'PATCH', headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        }).catch(() => {});
      };
      inp.addEventListener('blur', commit);
      inp.addEventListener('keydown', e2 => {
        if (e2.key === 'Enter') { e2.preventDefault(); inp.blur(); }
        if (e2.key === 'Escape') { overlay.remove(); }
      });
    });
    return g;
  };

  layer.appendChild(mkLabel(500, 978, `↔ ${W.toFixed(1)} m`, false, 'width', W, 50));
  layer.appendChild(mkLabel(978, 500, `↕ ${D.toFixed(1)} m`, true,  'depth', D, 50));
}

function makeSidebarChip(deviceId) {
  const dev = layoutState.devices.get(deviceId);
  const chip = document.createElement('div');
  chip.className = 'layout-chip';
  chip.draggable = true;
  chip.dataset.deviceId = deviceId;
  chip.textContent = dev ? dev.friendly_name ?? deviceId : deviceId;

  if (dev) {
    chip.style.setProperty('--chip-color', devStateColor(dev));
  }

  chip.addEventListener('dragstart', e => {
    dragType = 'bulb';
    setCanvasDragClass(true);
    e.dataTransfer.effectAllowed = 'copy';
    e.dataTransfer.setData('text/plain', `bulb:${deviceId}`);
    // Trigger pulse-on-grab via rooms.js exported function
    if (typeof window.__roomsStartPulse === 'function') {
      window.__roomsStartPulse(deviceId);
    }
  });
  chip.addEventListener('dragend', () => {
    dragType = null;
    setCanvasDragClass(false);
    if (typeof window.__roomsStopPulse === 'function') {
      window.__roomsStopPulse(true);
    }
  });
  wireChipTouchDrag(chip, 'bulb', deviceId);
  return chip;
}

function buildCanvas() {
  const outer = document.createElement('div');
  outer.className = 'layout-canvas-outer';

  // Simulation mode chip — floats above canvas
  const simChip = document.createElement('div');
  simChip.id = 'lc-sim-chip';
  simChip.textContent = '⏱ Simulation';
  simChip.style.display = 'none';
  outer.appendChild(simChip);

  const wrap = document.createElement('div');
  wrap.className = 'layout-canvas-wrap';

  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.id = 'layout-canvas';
  svg.setAttribute('viewBox', '0 0 1000 1000');
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');

  // Background floor
  const floor = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
  floor.setAttribute('x', '0'); floor.setAttribute('y', '0');
  floor.setAttribute('width', '1000'); floor.setAttribute('height', '1000');
  floor.setAttribute('fill', 'var(--layout-floor, #1a1a2e)');
  floor.setAttribute('rx', '8');
  svg.appendChild(floor);

  // Wall-photo backdrop (Phase 4 tracing aid) — a manual reference photo
  // shown semi-transparent behind the walls/openings/bulbs layers, purely so
  // proportions can be eyeballed while dragging the existing markers into
  // place. Sits right above the floor, below everything interactive.
  // pointer-events:none since it's decorative only — canvas clicks must
  // always reach the floor/openings/bulbs beneath, never this image.
  const backdrop = document.createElementNS('http://www.w3.org/2000/svg', 'image');
  backdrop.id = 'lc-wall-backdrop';
  backdrop.setAttribute('x', '0'); backdrop.setAttribute('y', '0');
  backdrop.setAttribute('width', '1000'); backdrop.setAttribute('height', '1000');
  backdrop.setAttribute('preserveAspectRatio', 'xMidYMid slice');
  backdrop.setAttribute('opacity', '0');
  backdrop.style.pointerEvents = 'none';
  svg.appendChild(backdrop);

  // Layer order: wall-glow behind everything, compass on top.
  // `lc-crosshair-hit` sits above bulbs so the crosshair grab handle remains
  // grabbable even when a bulb has been dropped on the origin — visually the
  // bulb is on top of the crosshair ring (good), but the user can still tap
  // the centre to drag the crosshair away.
  for (const id of ['lc-sun-arc', 'lc-openings', 'lc-shadow', 'lc-crosshair', 'lc-bulbs', 'lc-preview', 'lc-crosshair-hit', 'lc-wall-dims', 'lc-compass']) {
    const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
    g.id = id;
    svg.appendChild(g);
  }


  svg.addEventListener('dragover', onCanvasDragOver);
  svg.addEventListener('dragleave', onCanvasDragLeave);
  svg.addEventListener('drop', onCanvasDrop);
  svg.addEventListener('click', onCanvasClick);

  wrap.appendChild(svg);

  // Three.js container — hidden by default, shown when 3D toggle is active
  const canvas3d = document.createElement('div');
  canvas3d.id = 'lc-3d-container';
  canvas3d.style.display = 'none';
  wrap.appendChild(canvas3d);

  outer.appendChild(wrap);

  // Scrubber bar below canvas
  const scrubBar = document.createElement('div');
  scrubBar.id = 'lc-scrubber-bar';
  const nowMins = new Date().getHours() * 60 + new Date().getMinutes();
  // Two rows on phone: the time scrubber gets a full-width track row of its own
  // (a range can't live inside a horizontal-scroll strip — it collapses and the
  // drag fights the scroll), while the secondary buttons sit in a scrollable
  // strip so they never clip. At ≥900px both rows fold back onto one line.
  scrubBar.innerHTML = `
    <div class="lc-scrub-track-row">
      <button id="lc-scrubber-play" title="Play through the day">▶</button>
      <span id="lc-scrubber-time">Now</span>
      <input type="range" id="lc-scrubber" min="0" max="1440" step="5" value="${nowMins}">
    </div>
    <div class="lc-scrub-btn-row">
      <button id="lc-scrubber-live" title="Return to live">↺ Live</button>
      <button id="lc-sun-calib" title="Calibrate orientation from sun position" style="display:none">☀ Calibrate</button>
      <button id="lc-phone-compass-btn" title="Set orientation using phone compass">📱 Phone compass</button>
      <select id="lc-model-select" title="Light model">
        ${LIGHT_MODELS.map(m => `<option value="${m.id}">${m.label}</option>`).join('')}
      </select>
    </div>
  `;
  outer.appendChild(scrubBar);

  return outer;
}

// ── Drag / drop ───────────────────────────────────────────────────────────────

function svgPoint(svg, clientX, clientY) {
  const pt = svg.createSVGPoint();
  pt.x = clientX;
  pt.y = clientY;
  const svgP = pt.matrixTransform(svg.getScreenCTM().inverse());
  return {
    nx: svgP.x / 1000,
    ny: svgP.y / 1000,
  };
}

// Round a normalised coord (0..1) along an axis of length `length_m` (metres)
// to the nearest 1 cm in real-world distance. e.g. snapTo(0.5, 3) on a 3 m
// width snaps to (150 cm) / (300 cm) = 0.5 exactly.
function snapTo(v, length_m) {
  if (!length_m || length_m <= 0) return v;
  const cm = Math.round(v * length_m * 100);
  return cm / (length_m * 100);
}

// Crosshair magnet: when placing a bulb near the origin snap precisely to it.
// Radius is generous so touch users can reliably hit the target.
const CROSSHAIR_MAGNET_RADIUS = 40 / 1000;
function magnetToOrigin(nx, ny) {
  if (!layoutState.room) return null;
  const ox = layoutState.room.origin_x ?? 0.5;
  const oy = layoutState.room.origin_y ?? 0.5;
  if (Math.hypot(nx - ox, ny - oy) < CROSSHAIR_MAGNET_RADIUS) {
    return { nx: ox, ny: oy };
  }
  return null;
}
function snapX(v) { return snapTo(v, layoutState.room?.width_m ?? 3); }
function snapY(v) { return snapTo(v, layoutState.room?.depth_m ?? 6); }
// Walls run along one room axis: top/bottom along width, left/right along depth.
function snapAlongWall(v, wall) {
  return isHorizontalWall(wall) ? snapX(v) : snapY(v);
}

// Crosshair alignment snap: when dragging the crosshair, loosely stick to the
// X or Y coordinate of any placed bulb if within threshold. Falls back to 1cm
// grid snap for both axes. Returns snapped coords + which bulb ids triggered.
const BULB_ALIGN_THRESHOLD = 28 / 1000;
function snapCrosshairToBulbs(nx, ny) {
  let sx = snapX(nx), sy = snapY(ny);
  let snapXId = null, snapYId = null;
  let bestDx = BULB_ALIGN_THRESHOLD, bestDy = BULB_ALIGN_THRESHOLD;
  for (const [id, entry] of Object.entries(layoutState.bulbs)) {
    const dx = Math.abs(nx - entry.x);
    const dy = Math.abs(ny - entry.y);
    if (dx < bestDx) { bestDx = dx; sx = entry.x; snapXId = id; }
    if (dy < bestDy) { bestDy = dy; sy = entry.y; snapYId = id; }
  }
  return { sx: Math.max(0, Math.min(1, sx)), sy: Math.max(0, Math.min(1, sy)), snapXId, snapYId };
}

function renderDragPreviewAt(nx, ny, kind) {
  const preview = document.getElementById('lc-preview');
  if (!preview) return;
  preview.innerHTML = '';
  if (kind === 'opening') {
    const wall = detectWall(nx, ny);
    if (wall) {
      const rawPos = isHorizontalWall(wall) ? nx : ny;
      const xNorm = Math.abs(rawPos - 0.5) < 0.06 ? 0.5 : snapAlongWall(rawPos, wall);
      const r = openingToSvgRect({ wall_edge: wall, x_norm: xNorm, width_norm: 0.3 });
      preview.appendChild(svgEl('rect', {
        x: r.x, y: r.y, width: r.w, height: r.h, rx: 3,
        fill: 'rgba(100,200,255,0.45)', stroke: 'rgba(100,200,255,0.9)',
        'stroke-width': 2, 'pointer-events': 'none',
      }));
    } else {
      const reject = svgEl('text', {
        x: snapX(nx) * 1000, y: snapY(ny) * 1000,
        'text-anchor': 'middle', 'dominant-baseline': 'central',
        fill: 'rgba(255,80,80,0.85)', 'font-size': 48, 'pointer-events': 'none',
      });
      reject.textContent = '✕';
      preview.appendChild(reject);
    }
  } else {
    const magnet = magnetToOrigin(nx, ny);
    const sx = magnet ? magnet.nx : snapX(nx);
    const sy = magnet ? magnet.ny : snapY(ny);
    preview.appendChild(svgEl('circle', {
      cx: sx * 1000, cy: sy * 1000, r: 28,
      fill: magnet ? 'rgba(0,255,255,0.30)' : 'rgba(255,255,200,0.25)',
      stroke: magnet ? 'rgba(0,255,255,0.9)' : 'rgba(255,255,200,0.7)',
      'stroke-width': magnet ? 3 : 2,
      'pointer-events': 'none',
    }));
  }
}

function clearDragPreview() {
  const preview = document.getElementById('lc-preview');
  if (preview) preview.innerHTML = '';
}

function onCanvasDragOver(e) {
  e.preventDefault();
  if (!e.dataTransfer.types.includes('text/plain')) return;
  const svg = document.getElementById('layout-canvas');
  const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
  renderDragPreviewAt(nx, ny, dragType);
}

function onCanvasDragLeave() {
  const preview = document.getElementById('lc-preview');
  if (preview) preview.innerHTML = '';
}

function commitDropAt(kind, payload, nx, ny) {
  if (kind === 'opening') {
    const wall = detectWall(nx, ny);
    if (!wall || !layoutState.room) return;
    const rawPos = isHorizontalWall(wall) ? nx : ny;
    const xNorm = Math.abs(rawPos - 0.5) < 0.06 ? 0.5 : snapAlongWall(rawPos, wall);
    const transmission = payload === 'window' ? 1.0 : 0.1;
    postCreateOpening(layoutState.room.id, payload, wall, xNorm, 0.3, transmission);
    return;
  }
  if (kind === 'bulb') {
    const magnet = magnetToOrigin(nx, ny);
    const x = magnet ? magnet.nx : snapX(nx);
    const y = magnet ? magnet.ny : snapY(ny);
    const existing = layoutState.bulbs[payload];
    const fixtureType = existing?.fixture_type ?? 'ceiling_spot';
    const z = existing?.z ?? defaultZ(fixtureType);
    pushUndo();
    placeBulb(payload, x, y, z, fixtureType, true);
  }
}

function onCanvasDrop(e) {
  e.preventDefault();
  dragType = null;
  clearDragPreview();
  const raw = e.dataTransfer.getData('text/plain');
  const svg = document.getElementById('layout-canvas');
  const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
  if (raw.startsWith('opening:')) commitDropAt('opening', raw.slice(8), nx, ny);
  else if (raw.startsWith('bulb:')) commitDropAt('bulb', raw.slice(5), nx, ny);
}

// Touch / pen drag for sidebar chips. HTML5 native drag-and-drop never fires
// from a touch event, so on phones the chips would otherwise be inert. This
// path uses pointer events directly: on movement past a threshold a floating
// ghost element follows the finger, the canvas preview is updated as the
// finger moves over it, and release commits the drop via commitDropAt().
function wireChipTouchDrag(chip, kind, payload) {
  let startX = 0, startY = 0;
  let dragging = false;
  let ghost = null;
  let pointerId = null;

  const cleanup = () => {
    document.removeEventListener('pointermove', onMove);
    document.removeEventListener('pointerup',   onUp);
    document.removeEventListener('pointercancel', onCancel);
    if (ghost) { ghost.remove(); ghost = null; }
    clearDragPreview();
    dragType = null;
    dragging = false;
    pointerId = null;
    setCanvasDragClass(false);
    if (kind === 'bulb' && typeof window.__roomsStopPulse === 'function') {
      window.__roomsStopPulse(true);
    }
  };

  // Named functions so cleanup can removeEventListener by reference.
  function onMove(e) {
    if (e.pointerId !== pointerId) return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;
    if (!dragging) {
      if (Math.hypot(dx, dy) < 8) return;
      // Commit to drag mode
      dragging = true;
      dragType = kind;
      ghost = chip.cloneNode(true);
      ghost.style.position = 'fixed';
      ghost.style.left = `${e.clientX}px`;
      ghost.style.top = `${e.clientY}px`;
      ghost.style.transform = 'translate(-50%, -50%)';
      ghost.style.pointerEvents = 'none';
      ghost.style.opacity = '0.85';
      ghost.style.zIndex = '9999';
      ghost.style.boxShadow = '0 4px 16px rgba(0,0,0,0.4)';
      document.body.appendChild(ghost);
      if (kind === 'bulb' && typeof window.__roomsStartPulse === 'function') {
        window.__roomsStartPulse(payload);
      }
      // Close the sidebar sheet AFTER ghost is in the DOM. Capture stays on
      // document.body (never display:none) so the sheet hiding the chip element
      // cannot trigger pointercancel.
      setCanvasDragClass(true);
      e.preventDefault();
    }
    if (dragging) {
      ghost.style.left = `${e.clientX}px`;
      ghost.style.top = `${e.clientY}px`;
      const svg = document.getElementById('layout-canvas');
      if (svg) {
        const rect = svg.getBoundingClientRect();
        if (e.clientX >= rect.left && e.clientX <= rect.right &&
            e.clientY >= rect.top  && e.clientY <= rect.bottom) {
          const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
          renderDragPreviewAt(nx, ny, kind);
        } else {
          clearDragPreview();
        }
      }
    }
  }

  function onUp(e) {
    if (e.pointerId !== pointerId) return;
    if (dragging) {
      const svg = document.getElementById('layout-canvas');
      if (svg) {
        const rect = svg.getBoundingClientRect();
        if (e.clientX >= rect.left && e.clientX <= rect.right &&
            e.clientY >= rect.top  && e.clientY <= rect.bottom) {
          const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
          commitDropAt(kind, payload, nx, ny);
        }
      }
    }
    cleanup();
  }

  function onCancel(e) {
    if (e.pointerId !== pointerId) return;
    cleanup();
  }

  chip.addEventListener('pointerdown', e => {
    // Mouse falls through to native HTML5 DnD (already wired). Touch/pen
    // take this path because dragstart never fires from them.
    if (e.pointerType === 'mouse') return;
    if (e.button !== 0 && e.button !== -1) return;
    startX = e.clientX;
    startY = e.clientY;
    pointerId = e.pointerId;
    dragging = false;
    // Capture on document.body rather than chip so that closeSidebarSheet()
    // hiding the chip (display:none) cannot fire pointercancel mid-drag.
    document.body.setPointerCapture(e.pointerId);
    document.addEventListener('pointermove', onMove, { passive: false });
    document.addEventListener('pointerup',   onUp);
    document.addEventListener('pointercancel', onCancel);
  });
}

function onCanvasClick(e) {
  // Close popover if clicking outside it
  if (activePopover && !activePopover.contains(e.target)) {
    dismissPopover();
  }
}

// ── Bulb placement ────────────────────────────────────────────────────────────

function placeBulb(deviceId, x, y, z, fixtureType, postToServer) {
  const svg = document.getElementById('layout-canvas');
  const layer = document.getElementById('lc-bulbs');
  if (!svg || !layer) return;

  // Remove existing element if re-placing
  if (layoutState.bulbs[deviceId]?.el) {
    layoutState.bulbs[deviceId].el.remove();
    layoutState.bulbs[deviceId].labelEl?.remove();
  }

  const cx = x * 1000;
  const cy = y * 1000;

  const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  g.dataset.deviceId = deviceId;
  g.style.cursor = 'grab';
  g.addEventListener('click', e => e.stopPropagation());
  makeBulbDraggable(g, deviceId);

  drawFixtureIcon(g, cx, cy, z, fixtureType, layoutState.devices.get(deviceId));

  // Label: background pill + text, clear of the icon
  const dev = layoutState.devices.get(deviceId);
  const name = dev?.friendly_name ?? deviceId;
  const labelText = name.length > 14 ? name.slice(0, 13) + '…' : name;
  const labelY = cy + 58;

  const labelBg = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
  labelBg.setAttribute('x', cx - 52); labelBg.setAttribute('y', labelY - 18);
  labelBg.setAttribute('width', '104'); labelBg.setAttribute('height', '22');
  labelBg.setAttribute('rx', '6');
  labelBg.setAttribute('fill', 'rgba(0,0,0,0.55)');
  labelBg.setAttribute('pointer-events', 'none');
  labelBg.style.display = showLabels ? '' : 'none';
  g.appendChild(labelBg);

  const labelEl = document.createElementNS('http://www.w3.org/2000/svg', 'text');
  labelEl.setAttribute('x', cx);
  labelEl.setAttribute('y', labelY - 2);
  labelEl.setAttribute('text-anchor', 'middle');
  labelEl.setAttribute('font-size', '18');
  labelEl.setAttribute('fill', 'rgba(255,255,255,0.85)');
  labelEl.setAttribute('pointer-events', 'all');
  labelEl.style.cursor = 'text';
  labelEl.textContent = labelText;
  labelEl.style.display = showLabels ? '' : 'none';
  g.appendChild(labelEl);

  layer.appendChild(g);

  layoutState.bulbs[deviceId] = { x, y, z, fixture_type: fixtureType, el: g, labelEl };
  syncBulbToThree(deviceId, layoutState.bulbs[deviceId], layoutState.devices.get(deviceId));

  if (postToServer) {
    postPosition(deviceId, x, y, z, fixtureType);
  }

  rebuildSidebar();
}

function svgEl(tag, attrs) {
  const el = document.createElementNS('http://www.w3.org/2000/svg', tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  return el;
}

function drawFixtureIcon(g, cx, cy, z, fixtureType, dev) {
  const color = dev ? devStateColor(dev) : '#666';
  const on = dev?.on ?? false;
  const alpha = on ? 1 : 0.35;

  // Clear previous icon children (keep device-name text/rect labels; remove solar indicators too)
  [...g.children].forEach(c => {
    if (c.classList?.contains('lc-bulb-solar-indicator')) { c.remove(); return; }
    if (c.tagName !== 'text' && c.getAttribute('fill') !== 'rgba(0,0,0,0.55)') c.remove();
  });

  const els = [];

  if (fixtureType === 'led_strip') {
    // Wide pill — represents a strip mounted on a wall or ceiling
    const halo = svgEl('rect', { x: cx - 44, y: cy - 8, width: 88, height: 16, rx: 8,
      fill: color, opacity: on ? 0.2 : 0.1 });
    halo.classList.add('lc-bulb-halo');
    halo.dataset.baseOpacity = '0.2';
    els.push(halo);
    const strip = svgEl('rect', { x: cx - 40, y: cy - 5, width: 80, height: 10, rx: 5,
      fill: color, opacity: alpha });
    strip.classList.add('lc-bulb-shape');
    els.push(strip);

  } else if (fixtureType === 'pendant') {
    // Hanging cord from ceiling, then bulb circle
    const cordLen = 20 + (1 - z) * 40;
    els.push(svgEl('line', { x1: cx, y1: cy - 16, x2: cx, y2: cy - 16 - cordLen,
      stroke: 'rgba(255,255,255,0.35)', 'stroke-width': 2 }));
    // Glow halo
    const halo = svgEl('circle', { cx, cy, r: 22, fill: color, opacity: on ? 0.15 : 0.05 });
    halo.classList.add('lc-bulb-halo');
    halo.dataset.baseOpacity = '0.15';
    els.push(halo);
    const bulb = svgEl('circle', { cx, cy, r: 14, fill: color, opacity: alpha });
    bulb.classList.add('lc-bulb-shape');
    els.push(bulb);
    // Ring outline
    const ring = svgEl('circle', { cx, cy, r: 14, fill: 'none',
      stroke: color, 'stroke-width': 2, opacity: Math.min(alpha + 0.3, 1) });
    ring.classList.add('lc-bulb-ring');
    els.push(ring);

  } else if (fixtureType === 'table_lamp') {
    // Shade (downward triangle) + short stem + base
    const shade = svgEl('polygon', {
      points: `${cx},${cy - 8} ${cx - 16},${cy + 10} ${cx + 16},${cy + 10}`,
      fill: color, opacity: alpha });
    shade.classList.add('lc-bulb-shape');
    els.push(shade);
    els.push(svgEl('line', { x1: cx, y1: cy + 10, x2: cx, y2: cy + 22,
      stroke: 'rgba(255,255,255,0.4)', 'stroke-width': 3 }));
    els.push(svgEl('rect', { x: cx - 10, y: cy + 22, width: 20, height: 4, rx: 2,
      fill: 'rgba(255,255,255,0.35)' }));

  } else if (fixtureType === 'floor_lamp') {
    // Tall arc-head lamp
    els.push(svgEl('line', { x1: cx, y1: cy + 28, x2: cx, y2: cy - 4,
      stroke: 'rgba(255,255,255,0.4)', 'stroke-width': 3 }));
    // Shade arc (semi-circle open downwards)
    const halo = svgEl('path', {
      d: `M ${cx - 16} ${cy - 4} A 16 16 0 0 1 ${cx + 16} ${cy - 4}`,
      fill: color, opacity: alpha });
    halo.classList.add('lc-bulb-halo');
    els.push(halo);
    const head = svgEl('circle', { cx, cy: cy - 4, r: 7, fill: color, opacity: alpha });
    head.classList.add('lc-bulb-shape');
    els.push(head);
    els.push(svgEl('rect', { x: cx - 8, y: cy + 28, width: 16, height: 4, rx: 2,
      fill: 'rgba(255,255,255,0.35)' }));

  } else {
    // ceiling_spot (default) — downlight: halo ring + filled dot
    const halo = svgEl('circle', { cx, cy, r: 26, fill: color, opacity: on ? 0.12 : 0.04 });
    halo.classList.add('lc-bulb-halo');
    halo.dataset.baseOpacity = '0.12';
    els.push(halo);
    const ring = svgEl('circle', { cx, cy, r: 18, fill: 'none',
      stroke: color, 'stroke-width': 2, opacity: on ? 0.5 : 0.2 });
    ring.classList.add('lc-bulb-ring');
    els.push(ring);
    const dot = svgEl('circle', { cx, cy, r: 10, fill: color, opacity: alpha });
    dot.classList.add('lc-bulb-shape');
    els.push(dot);
    // Cross-hatch tick marks like a recessed light symbol
    const t1 = svgEl('line', { x1: cx - 18, y1: cy, x2: cx - 10, y2: cy,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 });
    t1.classList.add('lc-bulb-ring');
    els.push(t1);
    const t2 = svgEl('line', { x1: cx + 10, y1: cy, x2: cx + 18, y2: cy,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 });
    t2.classList.add('lc-bulb-ring');
    els.push(t2);
    const t3 = svgEl('line', { x1: cx, y1: cy - 18, x2: cx, y2: cy - 10,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 });
    t3.classList.add('lc-bulb-ring');
    els.push(t3);
    const t4 = svgEl('line', { x1: cx, y1: cy + 10, x2: cx, y2: cy + 18,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 });
    t4.classList.add('lc-bulb-ring');
    els.push(t4);
  }


  // Insert before any text/label children
  const firstLabel = [...g.children].find(c => c.tagName === 'text' && c.getAttribute('fill') !== 'var(--amber)');
  for (const el of els) {
    if (firstLabel) g.insertBefore(el, firstLabel);
    else g.appendChild(el);
  }
}

function makeBulbDraggable(g, deviceId) {
  let dragging = false;
  let moved = false;
  let dropzoneShown = false;
  let ghost = null;
  let tapTarget = null;
  let capturedPointerId = null;
  let startNx, startNy, startBulbX, startBulbY;

  // The layout SVG is embedded inside a room card that has draggable="true".
  // When the user starts a pointer drag on a bulb, the browser fires dragstart
  // on the draggable room card ancestor, which triggers pointercancel and kills
  // our drag. Block dragstart for the duration of the pointer drag.
  function onPreventDragStart(e) {
    e.preventDefault();
    e.stopPropagation();
  }

  function resetCrosshairRing() {
    const xRing = document.querySelector('#lc-crosshair-marker circle');
    if (xRing) {
      xRing.setAttribute('r', 16);
      xRing.setAttribute('fill', 'rgba(0, 200, 220, 0.12)');
      xRing.setAttribute('stroke-width', 2.5);
    }
  }

  function cleanup() {
    document.removeEventListener('pointermove', onDocMove);
    document.removeEventListener('pointerup',   onDocUp);
    document.removeEventListener('pointercancel', onDocCancel);
    document.removeEventListener('dragstart', onPreventDragStart, true);
    if (capturedPointerId !== null) {
      const svg = document.getElementById('layout-canvas');
      try { svg?.releasePointerCapture(capturedPointerId); } catch (_) {}
    }
    dragging = false;
    capturedPointerId = null;
    g.style.cursor = 'grab';
  }

  function onDocMove(e) {
    if (e.pointerId !== capturedPointerId) return;
    e.preventDefault();

    const svg = document.getElementById('layout-canvas');
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const dx = nx - startNx;
    const dy = ny - startNy;

    if (Math.abs(dx) > 0.005 || Math.abs(dy) > 0.005) moved = true;
    if (!moved) return;

    if (!dropzoneShown) {
      dropzoneShown = true;
      showLightsDropzone();

      const gDev = layoutState.devices.get(deviceId);
      const gName = gDev?.friendly_name ?? deviceId;
      ghost = document.createElement('div');
      ghost.className = 'layout-drag-ghost';
      const ghostChip = document.createElement('div');
      ghostChip.className = 'layout-chip';
      ghostChip.style.setProperty('--chip-color', gDev ? devStateColor(gDev) : 'var(--accent)');
      ghostChip.style.cursor = 'grabbing';
      ghostChip.textContent = gName.length > 14 ? gName.slice(0, 13) + '…' : gName;
      ghost.appendChild(ghostChip);
      document.body.appendChild(ghost);
    }

    if (ghost) {
      ghost.style.left = `${e.clientX - 44}px`;
      ghost.style.top  = `${e.clientY - 14}px`;
      const svgEl = document.getElementById('layout-canvas');
      const sr = svgEl?.getBoundingClientRect();
      const outsideSvg = !sr ||
        e.clientX < sr.left || e.clientX > sr.right ||
        e.clientY < sr.top  || e.clientY > sr.bottom;
      ghost.style.opacity = outsideSvg ? '1' : '0';
    }

    const candidateX = startBulbX + dx;
    const candidateY = startBulbY + dy;
    const magnet = magnetToOrigin(candidateX, candidateY);
    const newX = magnet ? magnet.nx : Math.max(0, Math.min(1, snapX(candidateX)));
    const newY = magnet ? magnet.ny : Math.max(0, Math.min(1, snapY(candidateY)));
    const entry = layoutState.bulbs[deviceId];
    const tx = (newX - entry.x) * 1000;
    const ty = (newY - entry.y) * 1000;
    g.setAttribute('transform', `translate(${tx},${ty})`);

    const xRing = document.querySelector('#lc-crosshair-marker circle');
    if (xRing) {
      if (magnet) {
        xRing.setAttribute('r', 24);
        xRing.setAttribute('fill', 'rgba(0, 255, 255, 0.3)');
        xRing.setAttribute('stroke-width', 3.5);
      } else {
        resetCrosshairRing();
      }
    }

    const dzHover = document.getElementById('layout-chips-dropzone');
    if (dzHover) {
      const r = dzHover.getBoundingClientRect();
      dzHover.classList.toggle('dz-hover',
        e.clientX >= r.left && e.clientX <= r.right &&
        e.clientY >= r.top  && e.clientY <= r.bottom);
    }
  }

  function onDocUp(e) {
    if (e.pointerId !== capturedPointerId) return;
    const wasMoved = moved;
    cleanup();

    if (!wasMoved) {
      const entry = layoutState.bulbs[deviceId];
      if (tapTarget && entry && tapTarget === entry.labelEl) {
        if (typeof window.__roomsStopPulse === 'function') window.__roomsStopPulse(false);
        startInlineRename(deviceId, entry);
      } else if (tapTarget !== 'from-dismiss') {
        openPopover(deviceId, g);
      }
      return;
    }

    const dzUp = document.getElementById('layout-chips-dropzone');
    if (dzUp) {
      dzUp.classList.remove('dz-hover');
      const r = dzUp.getBoundingClientRect();
      if (e.clientX >= r.left && e.clientX <= r.right &&
          e.clientY >= r.top  && e.clientY <= r.bottom) {
        if (typeof window.__roomsStopPulse === 'function') window.__roomsStopPulse(false);
        resetCrosshairRing();
        g.removeAttribute('transform');

        if (ghost) {
          const gr = ghost.getBoundingClientRect();
          const tdx = (r.left + r.width  / 2) - (gr.left + gr.width  / 2);
          const tdy = (r.top  + r.height / 2) - (gr.top  + gr.height / 2);
          ghost.style.transition = 'none';
          ghost.style.opacity = '1';
          const flyGhost = ghost; ghost = null;
          requestAnimationFrame(() => {
            flyGhost.style.transition =
              'transform 0.22s cubic-bezier(0.4,0,1,1), opacity 0.18s 0.04s';
            flyGhost.style.transform = `translate(${tdx}px,${tdy}px) scale(0.25)`;
            flyGhost.style.opacity = '0';
            setTimeout(() => flyGhost.remove(), 260);
          });
        }

        pushUndo();
        removeBulb(deviceId);
        return;
      }
    }

    if (ghost) { ghost.remove(); ghost = null; }
    hideLightsDropzone();
    if (typeof window.__roomsStopPulse === 'function') window.__roomsStopPulse(true);
    resetCrosshairRing();
    g.removeAttribute('transform');

    const svg = document.getElementById('layout-canvas');
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const candidateX = startBulbX + (nx - startNx);
    const candidateY = startBulbY + (ny - startNy);
    const magnet = magnetToOrigin(candidateX, candidateY);
    const newX = magnet ? magnet.nx : Math.max(0, Math.min(1, snapX(candidateX)));
    const newY = magnet ? magnet.ny : Math.max(0, Math.min(1, snapY(candidateY)));

    const entry = layoutState.bulbs[deviceId];
    if (newX !== entry.x || newY !== entry.y) {
      pushUndo();
      placeBulb(deviceId, newX, newY, entry.z, entry.fixture_type, true);
    }
  }

  function onDocCancel(e) {
    if (e.pointerId !== capturedPointerId) return;
    cleanup();
    dropzoneShown = false;
    g.removeAttribute('transform');
    resetCrosshairRing();
    if (typeof window.__roomsStopPulse === 'function') window.__roomsStopPulse(false);
    if (ghost) { ghost.remove(); ghost = null; }
    hideLightsDropzone();
  }

  function beginDrag(e) {
    const svg = document.getElementById('layout-canvas');
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const entry = layoutState.bulbs[deviceId];
    if (!entry) return;

    try { svg.setPointerCapture(e.pointerId); } catch (_) {}
    document.addEventListener('dragstart', onPreventDragStart, true);

    dragging = true;
    moved = false;
    dropzoneShown = false;
    startNx = nx; startNy = ny;
    startBulbX = entry.x; startBulbY = entry.y;
    capturedPointerId = e.pointerId;

    g.style.cursor = 'grabbing';

    document.addEventListener('pointermove',   onDocMove,   { passive: false });
    document.addEventListener('pointerup',     onDocUp);
    document.addEventListener('pointercancel', onDocCancel);

    if (typeof window.__roomsStartPulse === 'function') window.__roomsStartPulse(deviceId);
  }

  g.addEventListener('pointerdown', e => {
    if (e.button !== 0 && e.pointerType === 'mouse') return;
    e.stopPropagation();
    e.preventDefault();
    dismissPopover();
    tapTarget = e.target;
    beginDrag(e);
  });

  // Called by the document capture handler when a drag starts from underneath
  // an open popover. tapTarget='from-dismiss' suppresses popover-reopen on tap.
  bulbDragStarters[deviceId] = e => {
    tapTarget = 'from-dismiss';
    beginDrag(e);
  };
}

function updateBulbIcon(entry, state) {
  const deviceId = Object.entries(layoutState.bulbs).find(([, v]) => v === entry)?.[0] ?? '';
  const dev = layoutState.devices.get(deviceId);
  if (!entry.el) return;

  const color = devStateColor(state ?? dev);
  const on = (state ?? dev)?.on ?? false;
  const alpha = on ? 1 : 0.35;

  entry.el.querySelectorAll('.lc-bulb-shape').forEach(el => {
    el.setAttribute('fill', color);
    el.setAttribute('opacity', alpha);
  });

  entry.el.querySelectorAll('.lc-bulb-halo').forEach(el => {
    el.setAttribute('fill', color);
    const base = parseFloat(el.dataset.baseOpacity || '0.15');
    el.setAttribute('opacity', on ? base : base / 3);
  });

  entry.el.querySelectorAll('.lc-bulb-ring').forEach(el => {
    el.setAttribute('stroke', color);
    el.setAttribute('opacity', on ? 0.5 : 0.2);
  });

}

// ── Popover helpers ───────────────────────────────────────────────────────────

function sendLayoutDeviceCommand(deviceId, body) {
  return fetch(
    `/api/lights/${encodeURIComponent(deviceId)}/command?token=${encodeURIComponent(tok())}`,
    { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }
  ).catch(() => {});
}

// Whole-room command (all bulbs) for the action-bar lighting controls. POSTs to
// the room endpoint directly so layout.js needn't import rooms.js (which imports
// layout.js — that would be a cycle).
function sendLayoutRoomCommand(body) {
  const id = layoutState.room?.id;
  if (!id) return;
  return fetch(
    `/api/rooms/${encodeURIComponent(id)}/command?token=${encodeURIComponent(tok())}`,
    { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }
  ).catch(() => {});
}

function startInlineRename(deviceId, entry) {
  const labelEl = entry?.labelEl;
  const dev = layoutState.devices.get(deviceId);
  const currentName = dev?.friendly_name ?? deviceId;

  let cx = window.innerWidth / 2 - 80, cy = window.innerHeight / 2 - 16;
  if (labelEl) {
    const svg = document.getElementById('layout-canvas');
    if (svg) {
      const pt = svg.createSVGPoint();
      pt.x = parseFloat(labelEl.getAttribute('x') ?? 500);
      pt.y = parseFloat(labelEl.getAttribute('y') ?? 500);
      const screen = pt.matrixTransform(svg.getScreenCTM());
      cx = screen.x - 80; cy = screen.y - 16;
    }
  }

  const inp = document.createElement('input');
  inp.type = 'text';
  inp.className = 'layout-label-rename-input';
  inp.value = currentName;
  inp.style.left = `${Math.max(4, Math.min(cx, window.innerWidth - 164))}px`;
  inp.style.top = `${Math.max(4, Math.min(cy, window.innerHeight - 40))}px`;
  document.body.appendChild(inp);
  requestAnimationFrame(() => { inp.focus(); inp.select(); });

  let committed = false;
  function commit() {
    if (committed) return;
    committed = true;
    const n = inp.value.trim();
    inp.remove();
    if (!n || n === currentName) return;
    if (dev) dev.friendly_name = n;
    const labelText = n.length > 14 ? n.slice(0, 13) + '…' : n;
    if (labelEl) labelEl.textContent = labelText;
    fetch(`/api/lights/${encodeURIComponent(deviceId)}/name?token=${encodeURIComponent(tok())}`, {
      method: 'PATCH', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: n }),
    }).catch(() => {});
    rebuildSidebar();
  }

  inp.addEventListener('keydown', e => {
    if (e.key === 'Enter') { e.preventDefault(); commit(); }
    if (e.key === 'Escape') { committed = true; inp.remove(); }
  });
  inp.addEventListener('blur', commit);
}

// ── Popover ───────────────────────────────────────────────────────────────────

function openPopover(deviceId, anchorEl, screenX, screenY) {
  dismissPopover();

  const entry = layoutState.bulbs[deviceId];
  if (!entry) return;
  let dev = layoutState.devices.get(deviceId);

  const pop = document.createElement('div');
  pop.className = 'layout-popover';
  pop.addEventListener('pointerdown', e => e.stopPropagation());
  activePopover = pop;

  // ── Header: status dot + name (tap to rename) ─────────────────────────────
  const header = document.createElement('div');
  header.className = 'layout-popover-header';

  const dot = document.createElement('span');
  dot.className = 'layout-popover-status-dot';
  dot.style.background = devStateColor(dev);
  header.appendChild(dot);

  const nameBtn = document.createElement('button');
  nameBtn.className = 'layout-popover-name-btn';
  nameBtn.textContent = dev?.friendly_name ?? deviceId;
  nameBtn.title = 'Tap to rename';
  nameBtn.addEventListener('click', () => { dismissPopover(); startInlineRename(deviceId, entry); });
  header.appendChild(nameBtn);

  pop.appendChild(header);

  // Helper: refresh dot colour after state changes
  function refreshDot() { dot.style.background = devStateColor(layoutState.devices.get(deviceId)); }

  // ── On / Off toggle ───────────────────────────────────────────────────────
  const toggleBtn = document.createElement('button');
  let isOn = dev?.on ?? true;
  toggleBtn.className = `layout-popover-toggle ${isOn ? 'is-on' : 'is-off'}`;
  toggleBtn.textContent = isOn ? '● On' : '○ Off';
  toggleBtn.addEventListener('click', () => {
    isOn = !isOn;
    const cur = layoutState.devices.get(deviceId) ?? {};
    layoutState.devices.set(deviceId, { ...cur, on: isOn });
    toggleBtn.className = `layout-popover-toggle ${isOn ? 'is-on' : 'is-off'}`;
    toggleBtn.textContent = isOn ? '● On' : '○ Off';
    updateBulbIcon(entry, layoutState.devices.get(deviceId));
    refreshDot();
    if (briSlider) briSlider.disabled = !isOn;
    sendLayoutDeviceCommand(deviceId, { action: isOn ? 'on' : 'off' });
  });
  pop.appendChild(toggleBtn);

  // ── Brightness ────────────────────────────────────────────────────────────
  let briSlider = null;
  if (dev?.brightness != null) {
    let briTimer = null;
    const briEl = buildSlider({
      label: 'Brightness',
      min: 1, max: 255,
      value: dev.brightness,
      format: v => Math.round((v / 255) * 100) + '%',
      onInput: v => {
        const cur = layoutState.devices.get(deviceId) ?? {};
        layoutState.devices.set(deviceId, { ...cur, brightness: v, on: true });
        updateBulbIcon(entry, layoutState.devices.get(deviceId));
        refreshDot();
        clearTimeout(briTimer);
        briTimer = setTimeout(() =>
          sendLayoutDeviceCommand(deviceId, { action: 'brightness', value: v, transition_secs: 0.1 }), 80);
      },
      onCommit: v => {
        clearTimeout(briTimer);
        sendLayoutDeviceCommand(deviceId, { action: 'brightness', value: v, transition_secs: 0.2 });
      },
    });
    briSlider = briEl.querySelector('input');
    if (!(dev?.on ?? true)) briSlider.disabled = true;
    pop.appendChild(briEl);
  }

  // ── Colour temperature ────────────────────────────────────────────────────
  if (dev?.color_temp != null) {
    let ctTimer = null;
    pop.appendChild(buildSlider({
      label: 'Colour temp',
      min: 154, max: 500,
      value: dev.color_temp,
      format: v => Math.round(1e6 / v) + 'K',
      onInput: v => {
        const cur = layoutState.devices.get(deviceId) ?? {};
        layoutState.devices.set(deviceId, { ...cur, color_temp: v });
        updateBulbIcon(entry, layoutState.devices.get(deviceId));
        refreshDot();
        clearTimeout(ctTimer);
        ctTimer = setTimeout(() =>
          sendLayoutDeviceCommand(deviceId, { action: 'color_temp', value: v, transition_secs: 0.1 }), 80);
      },
      onCommit: v => {
        clearTimeout(ctTimer);
        sendLayoutDeviceCommand(deviceId, { action: 'color_temp', value: v, transition_secs: 0.2 });
      },
    }));
  }

  // ── Divider ───────────────────────────────────────────────────────────────
  const divider = document.createElement('div');
  divider.className = 'layout-popover-divider';
  pop.appendChild(divider);

  // ── Fixture type ──────────────────────────────────────────────────────────
  const typeLabel = document.createElement('div');
  typeLabel.className = 'layout-popover-label';
  typeLabel.textContent = 'Fixture type';
  pop.appendChild(typeLabel);

  const typeSelect = document.createElement('select');
  typeSelect.className = 'layout-popover-select';
  for (const ft of FIXTURE_TYPES) {
    const opt = document.createElement('option');
    opt.value = ft.id;
    opt.textContent = ft.label;
    opt.selected = ft.id === (entry.fixture_type ?? 'ceiling_spot');
    typeSelect.appendChild(opt);
  }
  typeSelect.addEventListener('change', () => {
    pushUndo();
    const newType = typeSelect.value;
    entry.fixture_type = newType;
    drawFixtureIcon(entry.el, entry.x * 1000, entry.y * 1000, entry.z, newType, layoutState.devices.get(deviceId));
    postPosition(deviceId, entry.x, entry.y, entry.z, newType);
  });
  pop.appendChild(typeSelect);

  // ── Height ────────────────────────────────────────────────────────────────
  pop.appendChild(buildSlider({
    label: 'Height',
    min: 0, max: 100,
    value: Math.round(entry.z * 100),
    format: v => v + '%',
    onInput: v => {
      entry.z = v / 100;
      drawFixtureIcon(entry.el, entry.x * 1000, entry.y * 1000, entry.z, entry.fixture_type, layoutState.devices.get(deviceId));
      syncBulbToThree(deviceId, entry, layoutState.devices.get(deviceId));
    },
    onCommit: () => {
      pushUndo();
      postPosition(deviceId, entry.x, entry.y, entry.z, entry.fixture_type);
    },
  }));

  // ── Remove ────────────────────────────────────────────────────────────────
  const removeBtn = document.createElement('button');
  removeBtn.className = 'layout-popover-remove';
  removeBtn.textContent = 'Remove from canvas';
  removeBtn.addEventListener('click', () => { pushUndo(); removeBulb(deviceId); dismissPopover(); });
  pop.appendChild(removeBtn);

  // ── Position ──────────────────────────────────────────────────────────────
  let cx, cy;
  if (screenX != null) {
    cx = screenX; cy = screenY;
  } else {
    const svg = document.getElementById('layout-canvas');
    const pt = svg.createSVGPoint();
    pt.x = entry.x * 1000; pt.y = entry.y * 1000;
    const screenPt = pt.matrixTransform(svg.getScreenCTM());
    cx = screenPt.x; cy = screenPt.y;
  }
  const pw = 240;
  pop.style.left = `${Math.min(Math.max(cx + 20, 8), window.innerWidth - pw - 8)}px`;
  pop.style.top = `${Math.min(Math.max(cy - 20, 8), window.innerHeight - 320)}px`;

  document.body.appendChild(pop);
}

function dismissPopover() {
  if (activePopover) { activePopover.remove(); activePopover = null; }
  if (typeof window.__roomsStopPulse === 'function') window.__roomsStopPulse(true);
}

// ── Remove bulb ───────────────────────────────────────────────────────────────

function removeBulb(deviceId) {
  const entry = layoutState.bulbs[deviceId];
  if (!entry) return;
  entry.el?.remove();
  entry.labelEl?.remove();
  delete layoutState.bulbs[deviceId];
  rebuildSidebar();
  // Server: post zero coords so the position record is cleared
  postPosition(deviceId, 0, 0, 0, null);
}

function defaultZ(fixtureType) {
  return FIXTURE_TYPES.find(f => f.id === fixtureType)?.defaultZ ?? 1.0;
}

// ── Undo / redo ───────────────────────────────────────────────────────────────

function snapshotPositions() {
  const snap = {};
  for (const [id, e] of Object.entries(layoutState.bulbs)) {
    snap[id] = { x: e.x, y: e.y, z: e.z, fixture_type: e.fixture_type };
  }
  return snap;
}

function pushUndo() {
  undoStack.push(snapshotPositions());
  redoStack = [];
}

function restoreSnapshot(snapshot) {
  // Clear current canvas bulbs
  const layer = document.getElementById('lc-bulbs');
  if (layer) layer.innerHTML = '';
  const oldIds = Object.keys(layoutState.bulbs);
  layoutState.bulbs = {};

  for (const [id, pos] of Object.entries(snapshot)) {
    placeBulb(id, pos.x, pos.y, pos.z, pos.fixture_type, true);
  }
  // placeBulb re-syncs the bulbs it re-places, but a bulb present before and
  // absent from the snapshot (e.g. undoing an add) would otherwise leave a stale
  // 3D mesh floating. Prune those. (no-op in 2D — its 3D meshes are torn down.)
  for (const id of oldIds) if (!(id in snapshot)) removeBulbFromThree(id);
}

function undo() {
  if (undoStack.length === 0) return;
  redoStack.push(snapshotPositions());
  restoreSnapshot(undoStack.pop());
}

function redo() {
  if (redoStack.length === 0) return;
  undoStack.push(snapshotPositions());
  restoreSnapshot(redoStack.pop());
}

function onKeyDown(e) {
  if (e.ctrlKey && !e.shiftKey && e.key === 'z') { e.preventDefault(); undo(); }
  if (e.ctrlKey && e.shiftKey  && e.key === 'z') { e.preventDefault(); redo(); }
  if (e.key === 'Escape') dismissPopover();
}

// ── Server I/O ────────────────────────────────────────────────────────────────
// tok() is imported from api.js.

async function loadPlacedBulbs(roomId) {
  try {
    const res = await fetch(`/api/rooms/${roomId}/positions?token=${encodeURIComponent(tok())}`);
    if (!res.ok) return;
    const items = await res.json();
    for (const item of items) {
      if (item.x === 0 && item.y === 0 && item.z === 0) continue; // unset
      placeBulb(item.device_id, item.x, item.y, item.z, item.fixture_type ?? 'ceiling_spot', false);
    }
  } catch (err) {
    console.warn('layout: failed to load positions', err);
  }
  rebuildSidebar();
}

async function postPosition(deviceId, x, y, z, fixtureType) {
  try {
    await fetch(`/api/lights/${encodeURIComponent(deviceId)}/position?token=${encodeURIComponent(tok())}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ x, y, z, fixture_type: fixtureType }),
    });
  } catch (err) {
    console.warn('layout: postPosition failed', err);
  }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function devStateColor(dev) {
  if (!dev || !dev.on) return '#444';
  if (dev.color_xy) {
    const [x, y] = dev.color_xy;
    return xyToHex(x, y);
  }
  if (dev.color_temp) return ctToHex(dev.color_temp);
  const b = dev.brightness ?? 200;
  const l = Math.round(30 + (b / 255) * 60);
  return `hsl(45,80%,${l}%)`;
}

function xyToHex(x, y) {
  // CIE xy → approximate sRGB (simplified wide-gamut path)
  const z = 1 - x - y;
  const Y = 1;
  const X = (Y / y) * x;
  const Z = (Y / y) * z;
  let r =  X * 1.656492 - Y * 0.354851 - Z * 0.255038;
  let g = -X * 0.707196 + Y * 1.655397 + Z * 0.036152;
  let b =  X * 0.051713 - Y * 0.121364 + Z * 1.011530;
  const m = Math.max(r, g, b, 1);
  r = Math.round(Math.min(Math.max(r / m, 0), 1) * 255);
  g = Math.round(Math.min(Math.max(g / m, 0), 1) * 255);
  b = Math.round(Math.min(Math.max(b / m, 0), 1) * 255);
  return `rgb(${r},${g},${b})`;
}

export function ctToHex(mireds) {
  // Approximate colour temperature (mireds) → warm/cool white
  const t = ((mireds - 153) / (500 - 153));
  const r = Math.round(255);
  const g = Math.round(200 + (1 - t) * 55);
  const b = Math.round(100 + (1 - t) * 155);
  return `rgb(${r},${g},${b})`;
}

// ── Openings — geometry helpers ───────────────────────────────────────────────


function isHorizontalWall(wallEdge) {
  return wallEdge === 'N' || wallEdge === 'S';
}

function detectWall(nx, ny) {
  const Z = 0.08;
  if (ny < Z) return 'N';
  if (ny > 1 - Z) return 'S';
  if (nx < Z) return 'W';
  if (nx > 1 - Z) return 'E';
  return null;
}

function openingToSvgRect(o) {
  const hw = (o.width_norm * 1000) / 2;
  switch (o.wall_edge) {
    case 'N': return { x: o.x_norm * 1000 - hw, y: 0,                         w: o.width_norm * 1000, h: WALL_THICKNESS };
    case 'S': return { x: o.x_norm * 1000 - hw, y: 1000 - WALL_THICKNESS,     w: o.width_norm * 1000, h: WALL_THICKNESS };
    case 'E': return { x: 1000 - WALL_THICKNESS, y: o.x_norm * 1000 - hw,     w: WALL_THICKNESS, h: o.width_norm * 1000 };
    case 'W': return { x: 0,                     y: o.x_norm * 1000 - hw,     w: WALL_THICKNESS, h: o.width_norm * 1000 };
    default:  return { x: 0, y: 0, w: 0, h: 0 };
  }
}

function openingHandlePositions(o, rect) {
  if (isHorizontalWall(o.wall_edge)) {
    const my = rect.y + rect.h / 2;
    return [{ x: rect.x + 5, y: my }, { x: rect.x + rect.w - 5, y: my }];
  } else {
    const mx = rect.x + rect.w / 2;
    return [{ x: mx, y: rect.y + 5 }, { x: mx, y: rect.y + rect.h - 5 }];
  }
}

function edgeCursor(wallEdge) {
  return isHorizontalWall(wallEdge) ? 'ew-resize' : 'ns-resize';
}

// ── Origin crosshair ─────────────────────────────────────────────────────────

function renderCrosshair(room) {
  if (!room) return;
  const layer = document.getElementById('lc-crosshair');
  if (!layer) return;
  while (layer.firstChild) layer.firstChild.remove();

  const ox0 = (room.origin_x ?? 0.5) * 1000;
  const oy0 = (room.origin_y ?? 0.5) * 1000;
  const W = room.width_m  || 3;
  const D = room.depth_m  || 6;

  const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  g.id = 'lc-crosshair-marker';

  // Full-width / full-height dashed guide lines
  const hLine = svgEl('line', { x1: 0, y1: oy0, x2: 1000, y2: oy0,
    stroke: '#0ff', 'stroke-width': 1.5, 'stroke-dasharray': '10 5',
    'pointer-events': 'none', opacity: 0.7 });
  const vLine = svgEl('line', { x1: ox0, y1: 0, x2: ox0, y2: 1000,
    stroke: '#0ff', 'stroke-width': 1.5, 'stroke-dasharray': '10 5',
    'pointer-events': 'none', opacity: 0.7 });

  // Visible ring + inner plus
  const ring = svgEl('circle', { cx: ox0, cy: oy0, r: 16,
    fill: 'rgba(0, 200, 220, 0.12)', stroke: '#0ff', 'stroke-width': 2.5,
    'pointer-events': 'none' });
  const plusH = svgEl('line', { x1: ox0 - 8, y1: oy0, x2: ox0 + 8, y2: oy0,
    stroke: '#0ff', 'stroke-width': 2, 'stroke-linecap': 'round',
    'pointer-events': 'none' });
  const plusV = svgEl('line', { x1: ox0, y1: oy0 - 8, x2: ox0, y2: oy0 + 8,
    stroke: '#0ff', 'stroke-width': 2, 'stroke-linecap': 'round',
    'pointer-events': 'none' });

  // Invisible hit area — large enough for a finger on mobile (r=44).
  // CSS sets pointer-events:none while a sidebar chip drag is in progress so
  // the hit never absorbs a drop intended for the canvas underneath.
  const hit = svgEl('circle', { cx: ox0, cy: oy0, r: 44,
    fill: 'rgba(0,0,0,0.001)', cursor: 'move', class: 'lc-crosshair-hit' });
  const hitTitle = document.createElementNS('http://www.w3.org/2000/svg', 'title');
  hitTitle.textContent = 'Origin point — double-tap (touch) or drag (mouse) to move';
  hit.appendChild(hitTitle);

  // Live dimension labels — hidden until drag
  const mkDim = () => {
    const t = svgEl('text', {
      fill: '#0ff', 'font-size': 26, 'font-family': 'monospace',
      'font-weight': 'bold', 'text-anchor': 'middle', 'dominant-baseline': 'middle',
      'pointer-events': 'none', 'paint-order': 'stroke',
      stroke: '#000', 'stroke-width': 4, 'stroke-linejoin': 'round',
      opacity: 0,
    });
    return t;
  };
  const topDim = mkDim();   // x label — centred on H guide, below the line
  const rightDim = mkDim(); // y label — left-anchored on V guide, to the right
  rightDim.setAttribute('text-anchor', 'start');

  // Snap guide dots — appear at the reference bulb when H or V line locks to it
  const mkSnapDot = () => svgEl('circle', {
    r: 10, fill: 'none', stroke: '#ffdd00', 'stroke-width': 2.5,
    opacity: 0, 'pointer-events': 'none',
  });
  const xSnapDot = mkSnapDot();
  const ySnapDot = mkSnapDot();

  g.appendChild(hLine);
  g.appendChild(vLine);
  g.appendChild(xSnapDot);
  g.appendChild(ySnapDot);
  g.appendChild(ring);
  g.appendChild(plusH);
  g.appendChild(plusV);
  g.appendChild(topDim);
  g.appendChild(rightDim);
  layer.appendChild(g);

  // Hit handle lives in a separate top-layer so it remains grabbable even when
  // a bulb has been dropped on the origin. Visible parts (ring, plus, dims)
  // stay in lc-crosshair below bulbs so a placed bulb naturally occludes them.
  const hitLayer = document.getElementById('lc-crosshair-hit');
  while (hitLayer && hitLayer.firstChild) hitLayer.firstChild.remove();
  hitLayer?.appendChild(hit);

  const setPos = (px, py) => {
    hLine.setAttribute('y1', py); hLine.setAttribute('y2', py);
    vLine.setAttribute('x1', px); vLine.setAttribute('x2', px);
    ring.setAttribute('cx', px); ring.setAttribute('cy', py);
    plusH.setAttribute('x1', px - 8); plusH.setAttribute('x2', px + 8);
    plusH.setAttribute('y1', py); plusH.setAttribute('y2', py);
    plusV.setAttribute('x1', px); plusV.setAttribute('x2', px);
    plusV.setAttribute('y1', py - 8); plusV.setAttribute('y2', py + 8);
    hit.setAttribute('cx', px); hit.setAttribute('cy', py);

    // X/Y from top-left corner — both labels sit in the 4th quadrant (below-right)
    const xFromLeft = (px / 1000) * W;
    const yFromTop  = (py / 1000) * D;
    // x label: on the H guide, to the right of origin, just below the line
    topDim.setAttribute('x', Math.min(960, Math.max(px + 60, (px + 1000) / 2)));
    topDim.setAttribute('y', Math.min(985, py + 30));
    topDim.textContent = `x: ${xFromLeft.toFixed(2)} m`;
    // y label: on the V guide, below origin, just to the right of the line
    rightDim.setAttribute('x', Math.min(985, px + 30));
    rightDim.setAttribute('y', Math.min(975, Math.max(py + 60, (py + 1000) / 2)));
    rightDim.textContent = `y: ${yFromTop.toFixed(2)} m`;
  };

  // Drag behaviour — attach to SVG using AbortController so old listeners are
  // cleaned up on the next renderCrosshair call (notifyRoomUpdate may re-render).
  const svg = document.getElementById('layout-canvas');
  if (svg._crosshairAbort) svg._crosshairAbort.abort();
  const ac = new AbortController();
  svg._crosshairAbort = ac;
  const sig = { signal: ac.signal };

  let dragging = false;

  const enterDrag = (pointerId) => {
    dragging = true;
    try { svg.setPointerCapture(pointerId); } catch (_) {}
    ring.setAttribute('fill', 'rgba(0, 255, 255, 0.35)');
    ring.setAttribute('r', 18);
    topDim.setAttribute('opacity', 1);
    rightDim.setAttribute('opacity', 1);
  };
  const exitDrag = (pointerId) => {
    dragging = false;
    try { svg.releasePointerCapture(pointerId); } catch (_) {}
    ring.setAttribute('fill', 'rgba(0, 200, 220, 0.12)');
    ring.setAttribute('r', 16);
    topDim.setAttribute('opacity', 0);
    rightDim.setAttribute('opacity', 0);
    hLine.setAttribute('stroke', '#0ff');
    vLine.setAttribute('stroke', '#0ff');
    xSnapDot.setAttribute('opacity', 0);
    ySnapDot.setAttribute('opacity', 0);
  };

  svg.addEventListener('pointerdown', e => {
    if (e.target !== hit) return;
    e.preventDefault();
    e.stopPropagation();
    enterDrag(e.pointerId);
  }, sig);

  svg.addEventListener('pointermove', e => {
    if (!dragging) return;
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const { sx, sy, snapXId, snapYId } = snapCrosshairToBulbs(nx, ny);
    setPos(sx * 1000, sy * 1000);

    // Yellow line = locked to a bulb column/row; cyan = normal grid
    hLine.setAttribute('stroke', snapYId ? '#ffdd00' : '#0ff');
    vLine.setAttribute('stroke', snapXId ? '#ffdd00' : '#0ff');

    // Show reference dot on the snapping bulb
    if (snapXId && layoutState.bulbs[snapXId]) {
      const b = layoutState.bulbs[snapXId];
      xSnapDot.setAttribute('cx', b.x * 1000); xSnapDot.setAttribute('cy', b.y * 1000);
      xSnapDot.setAttribute('opacity', 1);
    } else { xSnapDot.setAttribute('opacity', 0); }

    if (snapYId && layoutState.bulbs[snapYId]) {
      const b = layoutState.bulbs[snapYId];
      ySnapDot.setAttribute('cx', b.x * 1000); ySnapDot.setAttribute('cy', b.y * 1000);
      ySnapDot.setAttribute('opacity', snapYId === snapXId ? 0 : 1);
    } else { ySnapDot.setAttribute('opacity', 0); }
  }, sig);

  svg.addEventListener('pointerup', e => {
    if (!dragging) return;
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const { sx: ox, sy: oy } = snapCrosshairToBulbs(nx, ny);
    exitDrag(e.pointerId);
    if (layoutState.room) {
      layoutState.room.origin_x = ox;
      layoutState.room.origin_y = oy;
      fetch(`/api/rooms/${encodeURIComponent(layoutState.room.id)}/origin?token=${encodeURIComponent(tok())}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ origin_x: ox, origin_y: oy }),
      }).catch(() => {});
    }
  }, sig);

  svg.addEventListener('pointercancel', e => {
    if (!dragging) return;
    exitDrag(e.pointerId);
  }, sig);

  setPos(ox0, oy0);
}

// ── Openings — rendering ──────────────────────────────────────────────────────

function renderOpening(o) {
  const layer = document.getElementById('lc-openings');
  if (!layer) return;

  // Remove previous element for this opening
  layoutState.openings[o.id]?.el?.remove();

  const rect = openingToSvgRect(o);
  const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  g.dataset.openingId = o.id;

  const body = svgEl('rect', {
    x: rect.x, y: rect.y, width: rect.w, height: rect.h, rx: 3,
    class: o.opening_type === 'window' ? 'lc-opening-window' : 'lc-opening-door',
    cursor: 'grab',
  });
  makeMoveDraggable(body, o.id);
  g.addEventListener('click', e => e.stopPropagation());
  g.appendChild(body);

  // Resize handles at each end
  const [h1pos, h2pos] = openingHandlePositions(o, rect);
  for (const [pos, side] of [[h1pos, 'start'], [h2pos, 'end']]) {
    const h = svgEl('rect', {
      x: pos.x - 5, y: pos.y - 5, width: 10, height: 10,
      rx: 2, class: 'lc-resize-handle', cursor: edgeCursor(o.wall_edge),
    });
    makeResizeDraggable(h, o.id, side);
    g.appendChild(h);
  }

  layer.appendChild(g);
  layoutState.openings[o.id] = { ...o, el: g };
  updateOpeningCone();
  syncOpeningToThree(o);
  rebuildPlacedPanel();
}

function updateOpeningRectAttrs(openingId) {
  const o = layoutState.openings[openingId];
  if (!o?.el) return;
  const rect = openingToSvgRect(o);
  const body = o.el.querySelector('rect');
  if (body) {
    body.setAttribute('x', rect.x);
    body.setAttribute('y', rect.y);
    body.setAttribute('width', rect.w);
    body.setAttribute('height', rect.h);
  }
  const handles = [...o.el.querySelectorAll('.lc-resize-handle')];
  const [h1pos, h2pos] = openingHandlePositions(o, rect);
  const hCursor = edgeCursor(o.wall_edge);
  for (const [i, pos] of [[0, h1pos], [1, h2pos]]) {
    if (handles[i]) {
      handles[i].setAttribute('x', pos.x - 5);
      handles[i].setAttribute('y', pos.y - 5);
      handles[i].setAttribute('cursor', hCursor);
    }
  }
  updateOpeningCone();
  if (is3DActive()) syncOpeningToThree(layoutState.openings[openingId]);

  // Keep the sidebar inputs in sync without a full panel rebuild.
  if (!layoutState.room) return;
  const W = layoutState.room.width_m || 3;
  const D = layoutState.room.depth_m || 6;
  const wallLen = (o.wall_edge === 'N' || o.wall_edge === 'S') ? W : D;
  const posInp = document.querySelector(`[data-opening-pos="${CSS.escape(openingId)}"]`);
  if (posInp) posInp.value = (o.x_norm * wallLen).toFixed(2);
  const widInp = document.querySelector(`[data-opening-wid="${CSS.escape(openingId)}"]`);
  if (widInp) widInp.value = (o.width_norm * wallLen).toFixed(2);
}

// ── Light model system ────────────────────────────────────────────────────────

// Single entry point — clears both layers then delegates to current model.
function redrawLightEffect(azimuth, elevation) {
  const shadow = document.getElementById('lc-shadow');
  const arc    = document.getElementById('lc-sun-arc');
  if (shadow) shadow.innerHTML = '';
  if (arc)    arc.innerHTML    = '';
  switch (lightModel) {
    case 'parallel-beam':  renderParallelBeamModel(shadow, azimuth, elevation);  break;
    case 'beam-footprint': renderBeamFootprintModel(shadow, azimuth, elevation); break;
    case 'soft-beam':      renderSoftBeamModel(shadow, azimuth, elevation);      break;
    case 'cone':           renderConesModel(shadow, azimuth, elevation);         break;
    case 'gradient-cone':  renderGradientConesModel(shadow, azimuth, elevation); break;
    case 'caustic':        renderCausticModel(shadow, azimuth, elevation);       break;
    case 'bright-patch':   renderBrightPatchModel(shadow, azimuth, elevation);   break;
    case 'wall-glow':      renderWallGlowModel(arc, azimuth, elevation);         break;
    case 'sun-arc':        renderSunArcModel(arc, azimuth, elevation);           break;
  }
}

// updateOpeningCone now just redraws everything — cheap since openings are few.
function updateOpeningCone() {
  redrawLightEffect(lastSolar.azimuth, lastSolar.elevation);
}

// ── Openings — move drag ──────────────────────────────────────────────────────

function makeMoveDraggable(body, openingId) {
  let pressing = false;  // pointerdown received, drag not yet confirmed
  let moving = false;    // drag confirmed (past distance threshold)
  let startX = 0, startY = 0;

  body.addEventListener('pointerdown', e => {
    if (e.button !== 0) return;
    e.stopPropagation();
    pressing = true;
    moving = false;
    startX = e.clientX;
    startY = e.clientY;
    // Pointer capture deferred — held only while actually dragging so that
    // a short tap falls through to the click/popover path.
  });

  body.addEventListener('pointermove', e => {
    if (!pressing) return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;

    if (!moving) {
      if (Math.sqrt(dx * dx + dy * dy) < 6) return; // below drag threshold
      moving = true;
      body.setPointerCapture(e.pointerId);
      body.style.cursor = 'grabbing';
    }

    const o = layoutState.openings[openingId];
    if (!o) return;
    const svg = document.getElementById('layout-canvas');
    const pt = svgPoint(svg, e.clientX, e.clientY);
    const wall = detectWall(pt.nx, pt.ny) ?? o.wall_edge;
    const coord = isHorizontalWall(wall) ? pt.nx : pt.ny;
    o.wall_edge = wall;
    o.x_norm = Math.min(1 - o.width_norm / 2 - 0.02, Math.max(o.width_norm / 2 + 0.02, snapAlongWall(coord, wall)));
    updateOpeningRectAttrs(openingId);
  });

  body.addEventListener('pointerup', e => {
    if (!pressing) return;
    const wasDrag = moving;
    pressing = false;
    moving = false;
    body.style.cursor = 'grab';
    if (body.hasPointerCapture(e.pointerId)) body.releasePointerCapture(e.pointerId);

    if (!wasDrag) {
      openOpeningPopover(openingId, body);
      return;
    }
    const o = layoutState.openings[openingId];
    if (o) patchOpening(openingId, { wall_edge: o.wall_edge, x_norm: o.x_norm });
  });

  body.addEventListener('pointercancel', () => {
    pressing = false; moving = false; body.style.cursor = 'grab';
  });
}

// ── Openings — resize drag ────────────────────────────────────────────────────

function makeResizeDraggable(handle, openingId, side) {
  let dragging = false;
  let startCoord, startXNorm, startWidthNorm;

  handle.addEventListener('pointerdown', e => {
    e.stopPropagation(); e.preventDefault();
    const o = layoutState.openings[openingId];
    if (!o) return;
    dragging = true;
    const svg = document.getElementById('layout-canvas');
    const pt = svgPoint(svg, e.clientX, e.clientY);
    startCoord = isHorizontalWall(o.wall_edge) ? pt.nx : pt.ny;
    startXNorm = o.x_norm;
    startWidthNorm = o.width_norm;
    handle.setPointerCapture(e.pointerId);
  });

  handle.addEventListener('pointermove', e => {
    if (!dragging) return;
    const o = layoutState.openings[openingId];
    if (!o) return;
    const svg = document.getElementById('layout-canvas');
    const pt = svgPoint(svg, e.clientX, e.clientY);
    const coord = isHorizontalWall(o.wall_edge) ? pt.nx : pt.ny;
    const delta = coord - startCoord;

    if (side === 'start') {
      const oldEnd = startXNorm + startWidthNorm / 2;
      const newStart = Math.max(0.02, Math.min(oldEnd - 0.05, startXNorm - startWidthNorm / 2 + delta));
      const newWidth = Math.max(0.05, oldEnd - newStart);
      o.width_norm = Math.min(0.96, Math.max(0.05, snapAlongWall(newWidth, o.wall_edge)));
      o.x_norm    = Math.min(0.97, Math.max(0.03, snapAlongWall(newStart + o.width_norm / 2, o.wall_edge)));
    } else {
      const oldStart = startXNorm - startWidthNorm / 2;
      const newEnd = Math.min(0.98, Math.max(oldStart + 0.05, startXNorm + startWidthNorm / 2 + delta));
      const newWidth = Math.max(0.05, newEnd - oldStart);
      o.width_norm = Math.min(0.96, Math.max(0.05, snapAlongWall(newWidth, o.wall_edge)));
      o.x_norm    = Math.min(0.97, Math.max(0.03, snapAlongWall(oldStart + o.width_norm / 2, o.wall_edge)));
    }
    updateOpeningRectAttrs(openingId);
  });

  handle.addEventListener('pointerup', e => {
    if (!dragging) return;
    dragging = false;
    handle.releasePointerCapture(e.pointerId);
    const o = layoutState.openings[openingId];
    if (o) patchOpening(openingId, { x_norm: o.x_norm, width_norm: o.width_norm });
  });

  handle.addEventListener('pointercancel', () => { dragging = false; });
}

// ── Openings — popover ────────────────────────────────────────────────────────

function openOpeningPopover(openingId, anchorEl) {
  dismissPopover();
  const o = layoutState.openings[openingId];
  if (!o) return;

  const pop = document.createElement('div');
  pop.className = 'layout-popover';
  activePopover = pop;

  const typeRow = document.createElement('div');
  typeRow.className = 'layout-popover-label';
  typeRow.textContent = o.opening_type === 'window' ? '⬜ Window' : '▯ Door';
  typeRow.style.fontWeight = 'bold';
  pop.appendChild(typeRow);

  const transLabel = document.createElement('div');
  transLabel.className = 'layout-popover-label';
  transLabel.textContent = `Transmission: ${Math.round(o.transmission * 100)}%`;
  pop.appendChild(transLabel);

  const slider = document.createElement('input');
  slider.type = 'range'; slider.min = '0'; slider.max = '100';
  slider.value = Math.round(o.transmission * 100);
  slider.className = 'layout-popover-slider';
  slider.addEventListener('input', () => {
    o.transmission = parseInt(slider.value) / 100;
    transLabel.textContent = `Transmission: ${slider.value}%`;
    updateOpeningCone();
  });
  slider.addEventListener('change', () => {
    patchOpening(openingId, { transmission: o.transmission });
  });
  pop.appendChild(slider);

  const presets = o.opening_type === 'window'
    ? [{ label: 'Clear',       v: 1.0 }, { label: 'Frosted', v: 0.5 }, { label: 'Blind',       v: 0.05 }]
    : [{ label: 'Solid door',  v: 0.1 }, { label: '½ glazed', v: 0.3 }, { label: 'Full glazed', v: 0.7 }];
  const presetRow = document.createElement('div');
  presetRow.className = 'layout-popover-presets';
  for (const p of presets) {
    const btn = document.createElement('button');
    btn.textContent = p.label;
    btn.className = 'layout-popover-preset-btn';
    btn.addEventListener('click', () => {
      o.transmission = p.v;
      slider.value = Math.round(p.v * 100);
      transLabel.textContent = `Transmission: ${Math.round(p.v * 100)}%`;
      patchOpening(openingId, { transmission: p.v });
      updateOpeningCone();
    });
    presetRow.appendChild(btn);
  }
  pop.appendChild(presetRow);

  const removeBtn = document.createElement('button');
  removeBtn.className = 'layout-popover-remove';
  removeBtn.textContent = 'Remove opening';
  removeBtn.addEventListener('click', () => {
    removeOpening(openingId);
    dismissPopover();
  });
  pop.appendChild(removeBtn);

  // Position near opening
  const svg = document.getElementById('layout-canvas');
  const r = openingToSvgRect(o);
  const pt = svg.createSVGPoint();
  pt.x = r.x + r.w / 2;
  pt.y = r.y + r.h / 2;
  const sp = pt.matrixTransform(svg.getScreenCTM());
  // .layout-popover is position:fixed, so getScreenCTM's viewport coords are
  // already correct — do NOT add window.scrollX/scrollY (that would shift the
  // popover off the opening by the scroll amount). Matches the bulb popover.
  pop.style.left = `${Math.min(sp.x + 12, window.innerWidth - 220)}px`;
  pop.style.top  = `${Math.min(sp.y + 12, window.innerHeight - 300)}px`;
  document.body.appendChild(pop);
}

// ── Openings — remove ─────────────────────────────────────────────────────────

async function removeOpening(id) {
  const entry = layoutState.openings[id];
  if (!entry) return;
  await apiDeleteOpening(id);
  entry.el?.remove();
  const cone = document.getElementById(`cone-${CSS.escape(id)}`);
  if (cone) cone.remove();
  removeOpeningFromThree(id);
  delete layoutState.openings[id];
  rebuildPlacedPanel();
}

// ── Ceiling (glass / partial-glass) ──────────────────────────────────────────
// Mirrors the "C" sentinel in coordinator/src/http/api/rooms.rs
// (CEILING_WALL_EDGE) — a skylight opening with no compass-facing wall.
const CEILING_WALL_EDGE = 'C';

function findCeilingOpening() {
  return Object.values(layoutState.openings).find(o => o.wall_edge === CEILING_WALL_EDGE);
}

// Reflects the current room's ceiling opening (if any) into the sidebar
// dropdown — called once loadPlacedOpenings resolves so it sees the freshly
// loaded state, and again whenever setCeilingGlazing changes it.
function syncCeilingControl() {
  const sel = document.getElementById('layout-ceiling-select');
  if (!sel) return;
  const current = findCeilingOpening();
  sel.value = !current ? 'none' : current.width_norm >= 0.7 ? 'full' : 'partial';
}

async function setCeilingGlazing(kind) {
  const roomId = layoutState.room?.id;
  if (!roomId) return;
  const current = findCeilingOpening();
  if (kind === 'none') {
    if (current) await removeOpening(current.id);
    syncCeilingControl();
    return;
  }
  const widthNorm = kind === 'full' ? 1.0 : 0.4;
  if (current) {
    current.width_norm = widthNorm;
    await patchOpening(current.id, { width_norm: widthNorm });
  } else {
    // x_norm is meaningless for a ceiling opening (no wall position) — the
    // default is never read since openingToSvgRect/effect math skip it for
    // wall_edge === 'C'.
    await postCreateOpening(roomId, 'skylight', CEILING_WALL_EDGE, 0.5, widthNorm, 1.0);
  }
  syncCeilingControl();
}

// ── Wall photo backdrop (Phase 4 tracing aid) ─────────────────────────────────
// Not room scanning — a manual-tracing aid for this editor's existing
// dimensions/orientation/opening-placement flow. One photo per wall (N/S/E/W;
// the ceiling has no wall to photograph), shown as a semi-transparent
// backdrop behind the canvas so proportions can be eyeballed while dragging
// the existing markers into place. See plans/home-ui-redesign.md Phase 4.
const WALL_EDGES = ['N', 'S', 'E', 'W'];
// 1600px/q0.82 is plenty for eyeballing proportions on a phone screen and
// keeps a typical modern-phone photo's data URI in the low hundreds of KB
// (comfortably under MAX_WALL_PHOTO_DATA_URI_LEN server-side) — this is a
// tracing aid, not a photo gallery, so more resolution buys nothing.
const WALL_PHOTO_MAX_DIM = 1600;     // px, longest edge after downscale
const WALL_PHOTO_JPEG_QUALITY = 0.82;

function buildWallPhotoSection() {
  const section = makeCollapsibleSection('Wall photo', 'mesh-layout-sb-wallphoto', true);
  section.wrap.id = 'layout-wallphoto-section';

  const tabs = document.createElement('div');
  tabs.className = 'layout-wallphoto-tabs';
  for (const edge of WALL_EDGES) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'layout-wallphoto-tab';
    btn.dataset.wallEdge = edge;
    btn.textContent = edge;
    btn.addEventListener('click', () => {
      activeWallEdge = edge;
      refreshWallPhotoSidebar();
      updateWallPhotoBackdrop();
    });
    tabs.appendChild(btn);
  }
  section.body.appendChild(tabs);

  const thumbWrap = document.createElement('div');
  thumbWrap.className = 'layout-wallphoto-thumb-wrap';
  const thumb = document.createElement('img');
  thumb.className = 'layout-wallphoto-thumb';
  thumb.id = 'layout-wallphoto-thumb';
  thumbWrap.appendChild(thumb);
  section.body.appendChild(thumbWrap);

  const btnRow = document.createElement('div');
  btnRow.className = 'layout-wallphoto-btn-row';

  const fileInput = document.createElement('input');
  fileInput.type = 'file';
  fileInput.accept = 'image/*';
  fileInput.style.display = 'none';
  fileInput.addEventListener('change', async () => {
    const file = fileInput.files?.[0];
    fileInput.value = '';
    if (!file) return;
    const roomId = layoutState.room?.id;
    if (!roomId) return;
    let dataUri;
    try {
      dataUri = await downscaleImageFile(file);
    } catch (err) {
      console.warn('layout: failed to process wall photo', err);
      return;
    }
    layoutState.wallPhotos[activeWallEdge] = dataUri;
    refreshWallPhotoSidebar();
    updateWallPhotoBackdrop();
    await putWallPhoto(roomId, activeWallEdge, dataUri);
  });
  section.body.appendChild(fileInput);

  const addBtn = document.createElement('button');
  addBtn.type = 'button';
  addBtn.className = 'layout-wallphoto-btn';
  addBtn.id = 'layout-wallphoto-add-btn';
  addBtn.addEventListener('click', () => fileInput.click());
  btnRow.appendChild(addBtn);

  const removeBtn = document.createElement('button');
  removeBtn.type = 'button';
  removeBtn.className = 'layout-wallphoto-btn layout-wallphoto-remove-btn';
  removeBtn.id = 'layout-wallphoto-remove-btn';
  removeBtn.textContent = '✕ Remove';
  removeBtn.addEventListener('click', async () => {
    const roomId = layoutState.room?.id;
    if (!roomId) return;
    delete layoutState.wallPhotos[activeWallEdge];
    refreshWallPhotoSidebar();
    updateWallPhotoBackdrop();
    await deleteWallPhotoApi(roomId, activeWallEdge);
  });
  btnRow.appendChild(removeBtn);
  section.body.appendChild(btnRow);

  const opacityRow = document.createElement('label');
  opacityRow.className = 'layout-wallphoto-opacity-row';
  opacityRow.textContent = 'Opacity ';
  const opacitySlider = document.createElement('input');
  opacitySlider.type = 'range';
  opacitySlider.min = '0';
  opacitySlider.max = '100';
  opacitySlider.value = String(Math.round(wallPhotoOpacity * 100));
  opacitySlider.id = 'layout-wallphoto-opacity';
  opacitySlider.addEventListener('input', () => {
    wallPhotoOpacity = Number(opacitySlider.value) / 100;
    updateWallPhotoBackdrop();
  });
  opacityRow.appendChild(opacitySlider);
  section.body.appendChild(opacityRow);

  refreshWallPhotoSidebar(section.wrap);
  return section.wrap;
}

// Reflects layoutState.wallPhotos/activeWallEdge into the tab highlight,
// thumbnail, and add/remove button visibility. `root` defaults to the whole
// document so callers elsewhere in the module don't need the section element.
function refreshWallPhotoSidebar(root) {
  root = root ?? document.getElementById('layout-wallphoto-section');
  if (!root) return;
  const uri = layoutState.wallPhotos[activeWallEdge];
  root.querySelectorAll('.layout-wallphoto-tab').forEach(btn => {
    btn.classList.toggle('active', btn.dataset.wallEdge === activeWallEdge);
    btn.classList.toggle('has-photo', !!layoutState.wallPhotos[btn.dataset.wallEdge]);
  });
  const thumb = root.querySelector('#layout-wallphoto-thumb');
  if (thumb) {
    thumb.src = uri ?? '';
    thumb.style.display = uri ? '' : 'none';
  }
  const addBtn = root.querySelector('#layout-wallphoto-add-btn');
  if (addBtn) addBtn.textContent = uri ? `📷 Replace ${activeWallEdge} photo` : `📷 Add ${activeWallEdge} photo`;
  const removeBtn = root.querySelector('#layout-wallphoto-remove-btn');
  if (removeBtn) removeBtn.style.display = uri ? '' : 'none';
}

// Shows the active wall's photo behind the canvas at the configured opacity,
// or hides the backdrop entirely when that wall has none.
function updateWallPhotoBackdrop() {
  const img = document.getElementById('lc-wall-backdrop');
  if (!img) return;
  const uri = layoutState.wallPhotos[activeWallEdge];
  if (uri) {
    img.setAttributeNS('http://www.w3.org/1999/xlink', 'href', uri);
    img.setAttribute('href', uri);
    img.setAttribute('opacity', String(wallPhotoOpacity));
  } else {
    img.setAttribute('opacity', '0');
  }
}

// Downscales+re-encodes an uploaded photo client-side before it's ever sent
// to the coordinator — this is a proportions-eyeballing aid, not a gallery,
// so a modest resolution is plenty, and it keeps the stored data URI (and
// the request body) small regardless of what the phone's camera produced.
function downscaleImageFile(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error);
    reader.onload = () => {
      const img = new Image();
      img.onerror = () => reject(new Error('could not decode image'));
      img.onload = () => {
        const scale = Math.min(1, WALL_PHOTO_MAX_DIM / Math.max(img.width, img.height));
        const w = Math.max(1, Math.round(img.width * scale));
        const h = Math.max(1, Math.round(img.height * scale));
        const canvas = document.createElement('canvas');
        canvas.width = w; canvas.height = h;
        canvas.getContext('2d').drawImage(img, 0, 0, w, h);
        resolve(canvas.toDataURL('image/jpeg', WALL_PHOTO_JPEG_QUALITY));
      };
      img.src = reader.result;
    };
    reader.readAsDataURL(file);
  });
}

// ── Wall photo — server I/O ───────────────────────────────────────────────────

async function loadWallPhotos(roomId) {
  try {
    const res = await fetch(`/api/rooms/${encodeURIComponent(roomId)}/wall-photos?token=${encodeURIComponent(tok())}`);
    if (!res.ok) return;
    layoutState.wallPhotos = await res.json();
  } catch (err) {
    console.warn('layout: failed to load wall photos', err);
  }
  refreshWallPhotoSidebar();
  updateWallPhotoBackdrop();
}

async function putWallPhoto(roomId, wallEdge, dataUri) {
  try {
    const res = await fetch(
      `/api/rooms/${encodeURIComponent(roomId)}/wall-photos/${encodeURIComponent(wallEdge)}?token=${encodeURIComponent(tok())}`,
      { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ data_uri: dataUri }) }
    );
    if (!res.ok) console.warn('layout: wall photo upload failed', res.status);
  } catch (err) {
    console.warn('layout: wall photo upload error', err);
  }
}

async function deleteWallPhotoApi(roomId, wallEdge) {
  try {
    await fetch(
      `/api/rooms/${encodeURIComponent(roomId)}/wall-photos/${encodeURIComponent(wallEdge)}?token=${encodeURIComponent(tok())}`,
      { method: 'DELETE' }
    );
  } catch (err) {
    console.warn('layout: wall photo delete error', err);
  }
}

// ── Openings — server I/O ─────────────────────────────────────────────────────

async function loadPlacedOpenings(roomId) {
  try {
    const res = await fetch(`/api/rooms/${encodeURIComponent(roomId)}/openings?token=${encodeURIComponent(tok())}`);
    if (!res.ok) return;
    const items = await res.json();
    for (const item of items) renderOpening(item);
  } catch (err) {
    console.warn('layout: failed to load openings', err);
  }
}

async function postCreateOpening(roomId, openingType, wallEdge, xNorm, widthNorm, transmission) {
  try {
    const res = await fetch(
      `/api/rooms/${encodeURIComponent(roomId)}/openings?token=${encodeURIComponent(tok())}`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          opening_type: openingType,
          wall_edge: wallEdge,
          x_norm: xNorm,
          width_norm: widthNorm,
          transmission,
        }),
      }
    );
    if (!res.ok) return;
    const { id } = await res.json();
    renderOpening({ id, room_id: roomId, opening_type: openingType,
      wall_edge: wallEdge, x_norm: xNorm, width_norm: widthNorm, transmission });
  } catch (err) {
    console.warn('layout: postCreateOpening failed', err);
  }
}

async function patchOpening(id, data) {
  const o = layoutState.openings[id];
  if (!o) return;
  try {
    await fetch(
      `/api/rooms/${encodeURIComponent(o.room_id)}/openings/${encodeURIComponent(id)}?token=${encodeURIComponent(tok())}`,
      { method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(data) }
    );
  } catch (err) {
    console.warn('layout: patchOpening failed', err);
  }
}

async function apiDeleteOpening(id) {
  const o = layoutState.openings[id] ?? { room_id: layoutState.room?.id };
  if (!o.room_id) return;
  try {
    await fetch(
      `/api/rooms/${encodeURIComponent(o.room_id)}/openings/${encodeURIComponent(id)}?token=${encodeURIComponent(tok())}`,
      { method: 'DELETE' }
    );
  } catch (err) {
    console.warn('layout: apiDeleteOpening failed', err);
  }
}
