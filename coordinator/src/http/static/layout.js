// ── Room Layout Canvas ────────────────────────────────────────────────────────
// SVG top-down floor plan for placing bulbs and (Phase B) windows/doors.
// Coordinates are always 0–1 normalised; the SVG scales to any screen size.

import { buildSlider } from '/static/rooms.js';

// Prevent click-to-jump on any range slider — user must grab the thumb.
function lockSliderToThumb(slider) {
  slider.addEventListener('pointerdown', e => {
    const rect = slider.getBoundingClientRect();
    const ratio = (slider.value - slider.min) / (slider.max - slider.min);
    const thumbX = rect.left + ratio * rect.width;
    if (Math.abs(e.clientX - thumbX) > (e.pointerType === 'touch' ? 30 : 16))
      e.preventDefault();
  }, { capture: true });
}

// ── State ─────────────────────────────────────────────────────────────────────

let layoutRoom = null;          // RoomRecord currently in view
let devicesRef = new Map();     // reference to rooms.js devicesMap — set via init()
let placedBulbs = {};           // device_id → { x, y, z, fixture_type, el, labelEl }
let placedOpenings = {};        // opening_id → { opening_type, wall_edge, x_norm, width_norm, transmission, el }
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

// ── Three.js state ────────────────────────────────────────────────────────────
let THREE = null;
let ThreeOrbitControls = null;
let threeRenderer = null;
let threeScene = null;
let threeRoomGroup = null;
let threePerspCamera = null;
let threeControls = null;
let threeSunLight = null;
let threeBulbMeshes = {};     // deviceId → { mesh, ptLight, mat }
let threeOpeningMeshes = {};  // openingId → mesh
let threeNeedsRender = false; // on-demand rendering — only draw when something changed
let threeIs3D = false;
let threeAnimFrameId = null;
let threeRaycaster = null;
let threeFloorPlane = null;   // THREE.Plane at y=0
// 3D view is view-only — no drag-to-move, no sidebar drop target. Layout
// edits happen in the 2D view; 3D is purely for visualising effects/solar/scenes.

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
  devicesRef = devicesMap;
}

export function currentLayoutRoomId() {
  return layoutRoom?.id ?? null;
}

export async function openLayout(room) {
  layoutRoom = room;
  placedBulbs = {};
  placedOpenings = {};
  undoStack = [];
  redoStack = [];
  scrubberLive = true;
  sunCalibMode = false;
  compassDeg = room.orientation_degrees ?? 0;

  const container = document.getElementById('lighting-list');
  for (const child of container.children) child.style.display = 'none';

  document.getElementById('panel-lighting')?.classList.add('layout-open');

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
  { const s = solarPosition(Date.now()); lastSolar = { azimuth: s.azimuth, elevation: s.elevation }; }

  loadPlacedBulbs(room.id);
  loadPlacedOpenings(room.id);
  renderCrosshair(room);
  renderWallDims(room);
  initThree(room).catch(() => {});

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

  document.getElementById('panel-lighting')?.classList.remove('layout-open');

  const container = document.getElementById('lighting-list');
  const view = container.querySelector('.layout-view');
  if (view) view.remove();

  for (const child of container.children) child.style.display = '';

  layoutRoom = null;
  placedBulbs = {};
  placedOpenings = {};
}

// Called by rooms.js when a LightingUpdate WS event arrives so canvas icons stay live.
export function notifyDeviceUpdate(deviceId, state) {
  const entry = placedBulbs[deviceId];
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
  if (layoutRoom && layoutRoom.id === room.id) {
    layoutRoom = room;
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
  return layoutRoom != null && activeEffectByRoom.get(layoutRoom.id)?.effectId === 'solar';
}

function getSolarParams() {
  if (!layoutRoom) return {};
  const entry = activeEffectByRoom.get(layoutRoom.id);
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
    compassOrientTimer = setTimeout(() => patchOrientation(layoutRoom?.id, compassDeg), 400);
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
        patchOrientation(layoutRoom?.id, compassDeg);
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

// NOAA simplified solar position (±1-2° accuracy — sufficient for arc preview).
// Returns { azimuth: 0-360, elevation: -90..90 }.
function solarPosition(dateUtc) {
  const lat = meshLat * Math.PI / 180;
  const lon = meshLon;
  const jd = dateUtc / 86400000 + 2440587.5;
  const n = jd - 2451545.0;
  const L = (280.46 + 0.9856474 * n) % 360;
  const g = (357.528 + 0.9856003 * n) % 360;
  const gr = g * Math.PI / 180;
  const lambda = (L + 1.915 * Math.sin(gr) + 0.020 * Math.sin(2 * gr)) * Math.PI / 180;
  const eps = 23.439 * Math.PI / 180;
  const sinDec = Math.sin(eps) * Math.sin(lambda);
  const dec = Math.asin(sinDec);
  const cosDec = Math.cos(dec);
  // Greenwich Mean Sidereal Time → hour angle
  const gmst = (18.697374558 + 24.06570982441908 * n) % 24;
  const lst = ((gmst + lon / 15) % 24 + 24) % 24;
  const ha = (lst - (Math.atan2(Math.sin(lambda), Math.cos(lambda) * Math.cos(eps)) * 12 / Math.PI) + 24) % 24;
  const haRad = ha * Math.PI / 12;
  const sinAlt = Math.sin(lat) * sinDec + Math.cos(lat) * cosDec * Math.cos(haRad);
  const elevation = Math.asin(Math.max(-1, Math.min(1, sinAlt))) * 180 / Math.PI;
  const cosAz = (sinDec - Math.sin(lat) * sinAlt) / (Math.cos(lat) * Math.cos(Math.asin(sinAlt)));
  let az = Math.acos(Math.max(-1, Math.min(1, cosAz))) * 180 / Math.PI;
  if (Math.sin(haRad) > 0) az = 360 - az;
  return { azimuth: az, elevation };
}

function todaySunriseSunset() {
  const base = new Date(); base.setHours(0, 0, 0, 0);
  let riseAz = null, setAz = null, wasUp = null;
  for (let m = 0; m <= 1440; m += 5) {
    const d = new Date(base.getTime() + m * 60000);
    const { azimuth, elevation } = solarPosition(d.getTime());
    const up = elevation > 0;
    if (wasUp === false && up)  riseAz = azimuth;
    if (wasUp === true  && !up) setAz  = azimuth;
    wasUp = up;
  }
  const polarDay   = riseAz == null && wasUp === true;
  const polarNight = riseAz == null && wasUp === false;
  return { sunriseAz: riseAz ?? 90, sunsetAz: setAz ?? 270, polarDay, polarNight };
}

function azimuthToCanvasPoint(az, orientDeg) {
  const adjusted = ((az - orientDeg) % 360 + 360) % 360;
  const rad = (adjusted - 90) * Math.PI / 180;
  return { x: 500 + 570 * Math.cos(rad), y: 500 + 570 * Math.sin(rad) };
}

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

// ── Phase D: client-side solar state (matches Rust calculate_solar_state) ────

function calculateSolarState(elevation, params = {}) {
  const minBri    = params.min_brightness ?? 1;
  const maxBri    = Math.max(minBri, params.max_brightness ?? 254);
  const ctWarmth  = Math.max(0, Math.min(1, params.ct_warmth ?? 1.0));

  let bri, ct;
  if (elevation <= 0) {
    const t = Math.max(0, Math.min(1, (elevation + 18) / 18));
    bri = Math.round(1 + t * 29);
    ct  = 500;
  } else {
    const t = Math.min(1, elevation / 90);
    bri = Math.round(30 + t * 225);
    ct  = Math.round(454 - t * 301);
  }

  bri = Math.max(minBri, Math.min(maxBri, bri));
  ct  = Math.round(153 + ctWarmth * (ct - 153));

  return { bri, ct };
}

function previewSolarState(azimuth, elevation) {
  lastSolar = { azimuth, elevation };  // keep in sync so model-change redraws use correct position
  const { bri, ct } = calculateSolarState(elevation, getSolarParams());
  for (const [id, entry] of Object.entries(placedBulbs)) {
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
      const { azimuth, elevation } = solarPosition(base.getTime());
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
    const s = solarPosition(now.getTime());
    lastSolar = { azimuth: s.azimuth, elevation: s.elevation };

    redrawSolarOverlay(lastSolar.azimuth, lastSolar.elevation);
    redrawLightEffect(lastSolar.azimuth, lastSolar.elevation);

    // Restore bulb icons to real states and push current solar to physical bulbs
    for (const [deviceId, entry] of Object.entries(placedBulbs)) {
      updateBulbIcon(entry, devicesRef.get(deviceId));
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
        const { azimuth, elevation } = solarPosition(base.getTime());
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
  const room = layoutRoom;
  if (!room || !room.device_ids) return;
  const { bri, ct } = calculateSolarState(elevation, getSolarParams());
  const t = tok();

  const targetIds = (onlyDeviceIds || room.device_ids).filter(id => placedBulbs[id]);

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
      patchOrientation(layoutRoom?.id, compassDeg);
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
    inp.value = layoutRoom?.name ?? title.textContent;
    title.replaceWith(inp);
    inp.select();
    const commit = () => {
      const name = inp.value.trim();
      if (name && layoutRoom && name !== layoutRoom.name) {
        layoutRoom.name = name;
        fetch(`/api/rooms/${encodeURIComponent(layoutRoom.id)}/name?token=${encodeURIComponent(tok())}`, {
          method: 'PATCH', headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ name }),
        }).catch(() => {});
      }
      title.textContent = layoutRoom?.name ?? name;
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

  const controls = document.createElement('div');
  controls.className = 'layout-header-controls';

  const labelToggle = document.createElement('label');
  labelToggle.className = 'layout-toggle';
  const labelCb = document.createElement('input');
  labelCb.type = 'checkbox';
  labelCb.checked = showLabels;
  labelCb.addEventListener('change', () => {
    showLabels = labelCb.checked;
    Object.values(placedBulbs).forEach(e => {
      if (!e.el) return;
      e.el.querySelectorAll('text, rect[fill="rgba(0,0,0,0.55)"]').forEach(el => {
        el.style.display = showLabels ? '' : 'none';
      });
    });
  });
  labelToggle.appendChild(labelCb);
  labelToggle.appendChild(document.createTextNode(' Labels'));
  controls.appendChild(labelToggle);

  const autoBtn = document.createElement('button');
  autoBtn.className = 'layout-auto-btn';
  autoBtn.textContent = 'Auto-arrange remaining';
  autoBtn.style.display = 'none';
  autoBtn.id = 'layout-auto-btn';
  autoBtn.addEventListener('click', autoArrange);
  controls.appendChild(autoBtn);

  const undoBtn = document.createElement('button');
  undoBtn.className = 'layout-undo-btn';
  undoBtn.textContent = '↩ Undo';
  undoBtn.addEventListener('click', undo);
  controls.appendChild(undoBtn);

  const redoBtn = document.createElement('button');
  redoBtn.className = 'layout-redo-btn';
  redoBtn.textContent = '↪ Redo';
  redoBtn.addEventListener('click', redo);
  controls.appendChild(redoBtn);

  const btn3d = document.createElement('button');
  btn3d.id = 'lc-3d-toggle';
  btn3d.className = 'layout-toolbar-btn';
  btn3d.textContent = '3D';
  btn3d.title = 'Switch to 3D perspective view';
  btn3d.addEventListener('click', () => toggle3D(btn3d));
  controls.appendChild(btn3d);

  header.appendChild(controls);
  view.appendChild(header);

  // Body: sidebar + resize handle + canvas
  const body = document.createElement('div');
  body.className = 'layout-body';

  const sidebar = buildSidebar(room);
  body.appendChild(sidebar);
  body.appendChild(buildSidebarResizeHandle(sidebar));
  body.appendChild(buildCanvas());

  view.appendChild(body);
  return view;
}

const SIDEBAR_WIDTH_KEY = 'mesh-layout-sidebar-width';
const SIDEBAR_MIN_PX = 80;
const SIDEBAR_MAX_PX = 320;

function buildSidebarResizeHandle(sidebar) {
  const handle = document.createElement('div');
  handle.className = 'layout-sidebar-resize';

  // Restore saved width.
  const saved = parseInt(localStorage.getItem(SIDEBAR_WIDTH_KEY), 10);
  if (saved >= SIDEBAR_MIN_PX && saved <= SIDEBAR_MAX_PX) {
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
  const unplaced = (layoutRoom?.device_ids || []).filter(id => !placedBulbs[id]);
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
      if (layoutRoom) {
        layoutRoom[key] = val;
        renderWallDims(layoutRoom);
        const body = {};
        body[key] = val;
        fetch(`/api/rooms/${encodeURIComponent(layoutRoom.id)}/dimensions?token=${encodeURIComponent(tok())}`, {
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

  const room = layoutRoom;
  const unplaced = (room.device_ids || []).filter(id => !placedBulbs[id]);

  const lightsSection = document.getElementById('layout-lights-section');
  if (unplaced.length === 0) {
    if (lightsSection) lightsSection.style.display = 'none';
  } else {
    if (lightsSection) lightsSection.style.display = '';
    for (const id of unplaced) {
      chips.appendChild(makeSidebarChip(id));
    }
  }

  // Show/hide auto-arrange button
  const autoBtn = document.getElementById('layout-auto-btn');
  if (autoBtn) {
    autoBtn.style.display =
      Object.keys(placedBulbs).length >= 2 && unplaced.length > 0 ? '' : 'none';
  }

  rebuildPlacedPanel();
}

function rebuildPlacedPanel() {
  const body = document.getElementById('layout-placed-body');
  if (!body) return;
  // Remove previous entries (keep the header)
  [...body.children].forEach(c => { if (!c.classList.contains('layout-placed-header')) c.remove(); });

  const room = layoutRoom;
  if (!room) return;
  const W = room.width_m || 3;
  const D = room.depth_m || 6;

  for (const [deviceId, entry] of Object.entries(placedBulbs)) {
    const dev = devicesRef.get(deviceId);
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
  const openingEntries = Object.entries(placedOpenings);
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
        if (!isFinite(v) || v < 0.1 || !layoutRoom) return;
        const body = {}; body[key + '_m'] = v;
        fetch(`/api/rooms/${encodeURIComponent(layoutRoom.id)}/dimensions?token=${encodeURIComponent(tok())}`, {
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
  const dev = devicesRef.get(deviceId);
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
  scrubBar.innerHTML = `
    <button id="lc-scrubber-play" title="Play through the day">▶</button>
    <span id="lc-scrubber-time">Now</span>
    <input type="range" id="lc-scrubber" min="0" max="1440" step="5" value="${nowMins}">
    <button id="lc-scrubber-live" title="Return to live">↺ Live</button>
    <button id="lc-sun-calib" title="Calibrate orientation from sun position" style="display:none">☀ Calibrate</button>
    <button id="lc-phone-compass-btn" title="Set orientation using phone compass">📱 Phone compass</button>
    <select id="lc-model-select" title="Light model">
      ${LIGHT_MODELS.map(m => `<option value="${m.id}">${m.label}</option>`).join('')}
    </select>
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
  if (!layoutRoom) return null;
  const ox = layoutRoom.origin_x ?? 0.5;
  const oy = layoutRoom.origin_y ?? 0.5;
  if (Math.hypot(nx - ox, ny - oy) < CROSSHAIR_MAGNET_RADIUS) {
    return { nx: ox, ny: oy };
  }
  return null;
}
function snapX(v) { return snapTo(v, layoutRoom?.width_m ?? 3); }
function snapY(v) { return snapTo(v, layoutRoom?.depth_m ?? 6); }
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
  for (const [id, entry] of Object.entries(placedBulbs)) {
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
    if (!wall || !layoutRoom) return;
    const rawPos = isHorizontalWall(wall) ? nx : ny;
    const xNorm = Math.abs(rawPos - 0.5) < 0.06 ? 0.5 : snapAlongWall(rawPos, wall);
    const transmission = payload === 'window' ? 1.0 : 0.1;
    postCreateOpening(layoutRoom.id, payload, wall, xNorm, 0.3, transmission);
    return;
  }
  if (kind === 'bulb') {
    const magnet = magnetToOrigin(nx, ny);
    const x = magnet ? magnet.nx : snapX(nx);
    const y = magnet ? magnet.ny : snapY(ny);
    const existing = placedBulbs[payload];
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

  chip.addEventListener('pointerdown', e => {
    // Mouse falls through to native HTML5 DnD (already wired). Touch/pen
    // take this path because dragstart never fires from them.
    if (e.pointerType === 'mouse') return;
    if (e.button !== 0 && e.button !== -1) return;
    startX = e.clientX;
    startY = e.clientY;
    pointerId = e.pointerId;
    dragging = false;
    chip.setPointerCapture(e.pointerId);
  });

  chip.addEventListener('pointermove', e => {
    if (e.pointerType === 'mouse') return;
    if (e.pointerId !== pointerId) return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;
    if (!dragging) {
      if (Math.hypot(dx, dy) < 8) return;
      // Commit to drag mode
      dragging = true;
      dragType = kind;
      setCanvasDragClass(true);
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
  });

  chip.addEventListener('pointerup', e => {
    if (e.pointerType === 'mouse') return;
    if (e.pointerId !== pointerId) return;
    if (chip.hasPointerCapture(e.pointerId)) chip.releasePointerCapture(e.pointerId);
    if (!dragging) { cleanup(); return; }
    const svg = document.getElementById('layout-canvas');
    if (svg) {
      const rect = svg.getBoundingClientRect();
      if (e.clientX >= rect.left && e.clientX <= rect.right &&
          e.clientY >= rect.top  && e.clientY <= rect.bottom) {
        const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
        commitDropAt(kind, payload, nx, ny);
      }
    }
    cleanup();
  });

  chip.addEventListener('pointercancel', e => {
    if (e.pointerType === 'mouse') return;
    if (e.pointerId !== pointerId) return;
    cleanup();
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
  if (placedBulbs[deviceId]?.el) {
    placedBulbs[deviceId].el.remove();
    placedBulbs[deviceId].labelEl?.remove();
  }

  const cx = x * 1000;
  const cy = y * 1000;

  const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  g.dataset.deviceId = deviceId;
  g.style.cursor = 'grab';
  g.addEventListener('click', e => e.stopPropagation());
  makeBulbDraggable(g, deviceId);

  drawFixtureIcon(g, cx, cy, z, fixtureType, devicesRef.get(deviceId));

  // Label: background pill + text, clear of the icon
  const dev = devicesRef.get(deviceId);
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

  placedBulbs[deviceId] = { x, y, z, fixture_type: fixtureType, el: g, labelEl };
  syncBulbToThree(deviceId, placedBulbs[deviceId], devicesRef.get(deviceId));

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

      const gDev = devicesRef.get(deviceId);
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
    const entry = placedBulbs[deviceId];
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
      const entry = placedBulbs[deviceId];
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

    const entry = placedBulbs[deviceId];
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
    const entry = placedBulbs[deviceId];
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
  const deviceId = Object.entries(placedBulbs).find(([, v]) => v === entry)?.[0] ?? '';
  const dev = devicesRef.get(deviceId);
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

function startInlineRename(deviceId, entry) {
  const labelEl = entry?.labelEl;
  const dev = devicesRef.get(deviceId);
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

  const entry = placedBulbs[deviceId];
  if (!entry) return;
  let dev = devicesRef.get(deviceId);

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
  function refreshDot() { dot.style.background = devStateColor(devicesRef.get(deviceId)); }

  // ── On / Off toggle ───────────────────────────────────────────────────────
  const toggleBtn = document.createElement('button');
  let isOn = dev?.on ?? true;
  toggleBtn.className = `layout-popover-toggle ${isOn ? 'is-on' : 'is-off'}`;
  toggleBtn.textContent = isOn ? '● On' : '○ Off';
  toggleBtn.addEventListener('click', () => {
    isOn = !isOn;
    const cur = devicesRef.get(deviceId) ?? {};
    devicesRef.set(deviceId, { ...cur, on: isOn });
    toggleBtn.className = `layout-popover-toggle ${isOn ? 'is-on' : 'is-off'}`;
    toggleBtn.textContent = isOn ? '● On' : '○ Off';
    updateBulbIcon(entry, devicesRef.get(deviceId));
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
        const cur = devicesRef.get(deviceId) ?? {};
        devicesRef.set(deviceId, { ...cur, brightness: v, on: true });
        updateBulbIcon(entry, devicesRef.get(deviceId));
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
        const cur = devicesRef.get(deviceId) ?? {};
        devicesRef.set(deviceId, { ...cur, color_temp: v });
        updateBulbIcon(entry, devicesRef.get(deviceId));
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
    drawFixtureIcon(entry.el, entry.x * 1000, entry.y * 1000, entry.z, newType, devicesRef.get(deviceId));
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
      drawFixtureIcon(entry.el, entry.x * 1000, entry.y * 1000, entry.z, entry.fixture_type, devicesRef.get(deviceId));
      syncBulbToThree(deviceId, entry, devicesRef.get(deviceId));
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
  const entry = placedBulbs[deviceId];
  if (!entry) return;
  entry.el?.remove();
  entry.labelEl?.remove();
  delete placedBulbs[deviceId];
  rebuildSidebar();
  // Server: post zero coords so the position record is cleared
  postPosition(deviceId, 0, 0, 0, null);
}

// ── Auto-arrange ──────────────────────────────────────────────────────────────

function autoArrange() {
  const room = layoutRoom;
  if (!room) return;

  const unplaced = (room.device_ids || []).filter(id => !placedBulbs[id]);
  if (unplaced.length === 0) return;

  pushUndo();

  const wallTypes = new Set(['floor_lamp', 'table_lamp', 'led_strip']);
  const n = unplaced.length;
  const cols = Math.ceil(Math.sqrt(n));

  unplaced.forEach((id, i) => {
    const fixtureType = guessFixtureType(id);
    const z = defaultZ(fixtureType);
    let x, y;

    if (wallTypes.has(fixtureType)) {
      // Distribute along the walls (perimeter)
      const perimeterFrac = i / Math.max(n - 1, 1);
      ({ x, y } = perimeterPoint(perimeterFrac));
    } else {
      // Uniform grid, centred, avoiding edges
      const col = i % cols;
      const row = Math.floor(i / cols);
      const rows = Math.ceil(n / cols);
      x = 0.15 + (col / Math.max(cols - 1, 1)) * 0.7;
      y = 0.15 + (row / Math.max(rows - 1, 1)) * 0.7;
    }

    x = snapX(x); y = snapY(y);
    placeBulb(id, x, y, z, fixtureType, true);
  });
}

function perimeterPoint(frac) {
  // Walk perimeter: top → right → bottom → left, inset by 0.1
  const p = frac * 4;
  const inset = 0.12;
  if (p < 1) return { x: inset + (1 - 2 * inset) * p, y: inset };
  if (p < 2) return { x: 1 - inset, y: inset + (1 - 2 * inset) * (p - 1) };
  if (p < 3) return { x: 1 - inset - (1 - 2 * inset) * (p - 2), y: 1 - inset };
  return { x: inset, y: 1 - inset - (1 - 2 * inset) * (p - 3) };
}

function guessFixtureType(deviceId) {
  // If user already placed it before and it had a type, reuse it.
  // Otherwise default to ceiling_spot.
  return placedBulbs[deviceId]?.fixture_type ?? 'ceiling_spot';
}

function defaultZ(fixtureType) {
  return FIXTURE_TYPES.find(f => f.id === fixtureType)?.defaultZ ?? 1.0;
}

// ── Undo / redo ───────────────────────────────────────────────────────────────

function snapshotPositions() {
  const snap = {};
  for (const [id, e] of Object.entries(placedBulbs)) {
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
  placedBulbs = {};

  for (const [id, pos] of Object.entries(snapshot)) {
    placeBulb(id, pos.x, pos.y, pos.z, pos.fixture_type, true);
  }
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

function tok() { return localStorage.getItem('meshToken') ?? ''; }

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

function ctToHex(mireds) {
  // Approximate colour temperature (mireds) → warm/cool white
  const t = ((mireds - 153) / (500 - 153));
  const r = Math.round(255);
  const g = Math.round(200 + (1 - t) * 55);
  const b = Math.round(100 + (1 - t) * 155);
  return `rgb(${r},${g},${b})`;
}

// ── Openings — geometry helpers ───────────────────────────────────────────────

const WALL_THICKNESS = 18;

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
    if (snapXId && placedBulbs[snapXId]) {
      const b = placedBulbs[snapXId];
      xSnapDot.setAttribute('cx', b.x * 1000); xSnapDot.setAttribute('cy', b.y * 1000);
      xSnapDot.setAttribute('opacity', 1);
    } else { xSnapDot.setAttribute('opacity', 0); }

    if (snapYId && placedBulbs[snapYId]) {
      const b = placedBulbs[snapYId];
      ySnapDot.setAttribute('cx', b.x * 1000); ySnapDot.setAttribute('cy', b.y * 1000);
      ySnapDot.setAttribute('opacity', snapYId === snapXId ? 0 : 1);
    } else { ySnapDot.setAttribute('opacity', 0); }
  }, sig);

  svg.addEventListener('pointerup', e => {
    if (!dragging) return;
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const { sx: ox, sy: oy } = snapCrosshairToBulbs(nx, ny);
    exitDrag(e.pointerId);
    if (layoutRoom) {
      layoutRoom.origin_x = ox;
      layoutRoom.origin_y = oy;
      fetch(`/api/rooms/${encodeURIComponent(layoutRoom.id)}/origin?token=${encodeURIComponent(tok())}`, {
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
  placedOpenings[o.id]?.el?.remove();

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
  placedOpenings[o.id] = { ...o, el: g };
  updateOpeningCone(o.id);
  syncOpeningToThree(o);
  rebuildPlacedPanel();
}

function updateOpeningRectAttrs(openingId) {
  const o = placedOpenings[openingId];
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
  updateOpeningCone(openingId);
  if (threeIs3D) syncOpeningToThree(placedOpenings[openingId]);

  // Keep the sidebar inputs in sync without a full panel rebuild.
  if (!layoutRoom) return;
  const W = layoutRoom.width_m || 3;
  const D = layoutRoom.depth_m || 6;
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

// Shared geometry for all opening-based models.
function openingCtx(o, azimuth, elevation) {
  // Civil twilight threshold: below -6° there is no usable solar illumination.
  if (elevation <= -6) return null;
  const wallCanvasDeg = { N: 0, E: 90, S: 180, W: 270 }[o.wall_edge] ?? 0;
  const wallRealDeg   = (wallCanvasDeg + compassDeg + 360) % 360;
  const diff = ((azimuth - wallRealDeg) + 360) % 360;
  const norm = diff > 180 ? 360 - diff : diff;
  if (norm >= 90) return null;
  const T = WALL_THICKNESS / 2;
  let ox, oy;
  switch (o.wall_edge) {
    case 'N': ox = o.x_norm * 1000; oy = T;         break;
    case 'S': ox = o.x_norm * 1000; oy = 1000 - T;  break;
    case 'E': ox = 1000 - T;        oy = o.x_norm * 1000; break;
    case 'W': ox = T;               oy = o.x_norm * 1000; break;
    default: return null;
  }
  const canvasInwardDeg = ((azimuth + 180 - compassDeg) % 360 + 360) % 360;
  const inwardAngle     = canvasInwardDeg * Math.PI / 180;
  // elevFactor: full intensity at 40°+; tapers through civil twilight (-6° → 0°)
  const elevFactor      = elevation <= 0
    ? Math.max(0, (elevation + 6) / 6) * 0.12   // civil twilight dim glow, max 12%
    : Math.min(1, elevation / 40);
  const dirFactor       = 1 - norm / 90;
  // wallTangent: the fixed axis along the wall — always [1,0] for N/S, [0,1] for E/W
  const wallTangent     = (o.wall_edge === 'E' || o.wall_edge === 'W') ? [0, 1] : [1, 0];
  return { ox, oy, inwardAngle, canvasInwardDeg, elevFactor, dirFactor, wallTangent };
}

// Helper: prepend a <defs> block into a layer (defs cleared with innerHTML each frame).
function layerDefs(layer) {
  const d = document.createElementNS('http://www.w3.org/2000/svg', 'defs');
  layer.insertBefore(d, layer.firstChild);
  return d;
}

// Helper: create a linearGradient element.
function mkLinearGrad(id, x1, y1, x2, y2, stops) {
  const g = document.createElementNS('http://www.w3.org/2000/svg', 'linearGradient');
  g.id = id;
  g.setAttribute('gradientUnits', 'userSpaceOnUse');
  g.setAttribute('x1', x1); g.setAttribute('y1', y1);
  g.setAttribute('x2', x2); g.setAttribute('y2', y2);
  for (const [offset, color, opacity] of stops) {
    const s = document.createElementNS('http://www.w3.org/2000/svg', 'stop');
    s.setAttribute('offset', offset);
    s.setAttribute('stop-color', color);
    s.setAttribute('stop-opacity', opacity);
    g.appendChild(s);
  }
  return g;
}

// Helper: create a radialGradient element (percentage coords).
function mkRadialGrad(id, stops) {
  const g = document.createElementNS('http://www.w3.org/2000/svg', 'radialGradient');
  g.id = id; g.setAttribute('cx', '50%'); g.setAttribute('cy', '50%'); g.setAttribute('r', '50%');
  for (const [offset, color, opacity] of stops) {
    const s = document.createElementNS('http://www.w3.org/2000/svg', 'stop');
    s.setAttribute('offset', offset);
    s.setAttribute('stop-color', color);
    s.setAttribute('stop-opacity', opacity);
    g.appendChild(s);
  }
  return g;
}

// ── Model 1: Cone ─────────────────────────────────────────────────────────────
function renderConesModel(layer, azimuth, elevation) {
  if (!layer) return;
  for (const [id, o] of Object.entries(placedOpenings)) {
    const c = openingCtx(o, azimuth, elevation);
    if (!c) continue;
    const { ox, oy, inwardAngle, elevFactor, dirFactor } = c;
    const len  = 200 + elevation * 3;
    const half = (20 + o.width_norm * 35) * Math.PI / 180;
    const op   = o.transmission * elevFactor * dirFactor * 0.55;
    if (op < 0.01) continue;
    const lx = ox + Math.sin(inwardAngle - half) * len;
    const ly = oy - Math.cos(inwardAngle - half) * len;
    const rx = ox + Math.sin(inwardAngle + half) * len;
    const ry = oy - Math.cos(inwardAngle + half) * len;
    layer.appendChild(svgEl('polygon', {
      points: `${ox.toFixed(1)},${oy.toFixed(1)} ${lx.toFixed(1)},${ly.toFixed(1)} ${rx.toFixed(1)},${ry.toFixed(1)}`,
      fill: `rgba(255,220,100,${op.toFixed(3)})`, 'pointer-events': 'none',
    }));
  }
}

// ── Model 2: Gradient cone ────────────────────────────────────────────────────
function renderGradientConesModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  for (const [id, o] of Object.entries(placedOpenings)) {
    const c = openingCtx(o, azimuth, elevation);
    if (!c) continue;
    const { ox, oy, inwardAngle, elevFactor, dirFactor } = c;
    const len  = 200 + elevation * 3;
    const half = (20 + o.width_norm * 35) * Math.PI / 180;
    const op   = o.transmission * elevFactor * dirFactor * 0.85;
    if (op < 0.01) continue;
    const tipX = ox + Math.sin(inwardAngle) * len;
    const tipY = oy - Math.cos(inwardAngle) * len;
    const lx = ox + Math.sin(inwardAngle - half) * len;
    const ly = oy - Math.cos(inwardAngle - half) * len;
    const rx = ox + Math.sin(inwardAngle + half) * len;
    const ry = oy - Math.cos(inwardAngle + half) * len;
    const gid = `lc-gc-${id}`;
    defs.appendChild(mkLinearGrad(gid,
      ox.toFixed(1), oy.toFixed(1), tipX.toFixed(1), tipY.toFixed(1),
      [['0%', '#FFDC64', op.toFixed(3)], ['100%', '#FF8C00', '0']]));
    layer.appendChild(svgEl('polygon', {
      points: `${ox.toFixed(1)},${oy.toFixed(1)} ${lx.toFixed(1)},${ly.toFixed(1)} ${rx.toFixed(1)},${ry.toFixed(1)}`,
      fill: `url(#${gid})`, 'pointer-events': 'none',
    }));
  }
}

// ── Model 3: Caustic patch ────────────────────────────────────────────────────
function renderCausticModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  for (const [id, o] of Object.entries(placedOpenings)) {
    const c = openingCtx(o, azimuth, elevation);
    if (!c) continue;
    const { ox, oy, inwardAngle, canvasInwardDeg, elevFactor, dirFactor } = c;
    const op = o.transmission * elevFactor * dirFactor * 0.9;
    if (op < 0.01) continue;
    const depth = 60 + 520 * (1 - elevFactor);              // low sun → far throw
    const cx_c  = ox + Math.sin(inwardAngle) * depth;
    const cy_c  = oy - Math.cos(inwardAngle) * depth;
    const winHW = o.width_norm * 350 + 50;
    const rx_c  = winHW * (0.6 + depth / 700);              // spreads with distance
    const ry_c  = Math.max(18, rx_c * Math.sin(elevation * Math.PI / 180)); // foreshorten
    const gid   = `lc-caustic-${id}`;
    defs.appendChild(mkRadialGrad(gid, [
      ['0%',   '#FFF0A0', op.toFixed(3)],
      ['55%',  '#FFAA30', (op * 0.45).toFixed(3)],
      ['100%', '#FF6000', '0'],
    ]));
    const el = document.createElementNS('http://www.w3.org/2000/svg', 'ellipse');
    el.setAttribute('cx', cx_c.toFixed(1)); el.setAttribute('cy', cy_c.toFixed(1));
    el.setAttribute('rx', rx_c.toFixed(1)); el.setAttribute('ry', ry_c.toFixed(1));
    el.setAttribute('fill', `url(#${gid})`);
    el.setAttribute('transform', `rotate(${canvasInwardDeg.toFixed(1)},${cx_c.toFixed(1)},${cy_c.toFixed(1)})`);
    el.setAttribute('pointer-events', 'none');
    layer.appendChild(el);
  }
}

// ── Model 4: Bright patch (parallelogram shaft, correct wall-axis width) ─────
function renderBrightPatchModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  for (const [id, o] of Object.entries(placedOpenings)) {
    const c = openingCtx(o, azimuth, elevation);
    if (!c) continue;
    const { ox, oy, inwardAngle, elevFactor, dirFactor, wallTangent } = c;
    const op = o.transmission * elevFactor * dirFactor * 0.9;
    if (op < 0.01) continue;
    const [tx, ty] = wallTangent;
    const winHW = o.width_norm * 500;
    const len   = Math.min(950, 350 / Math.tan(Math.max(5, elevation) * Math.PI / 180));
    const x1 = ox + tx * winHW, y1 = oy + ty * winHW;
    const x2 = ox - tx * winHW, y2 = oy - ty * winHW;
    const dx = Math.sin(inwardAngle) * len, dy = -Math.cos(inwardAngle) * len;
    const gid = `lc-bp-${id}`;
    defs.appendChild(mkLinearGrad(gid,
      ox.toFixed(1), oy.toFixed(1), (ox + dx).toFixed(1), (oy + dy).toFixed(1),
      [['0%', '#FFF4A0', op.toFixed(3)], ['100%', '#FFCC40', '0']]));
    layer.appendChild(svgEl('polygon', {
      points: `${x1.toFixed(1)},${y1.toFixed(1)} ${x2.toFixed(1)},${y2.toFixed(1)} ${(x2+dx).toFixed(1)},${(y2+dy).toFixed(1)} ${(x1+dx).toFixed(1)},${(y1+dy).toFixed(1)}`,
      fill: `url(#${gid})`, 'pointer-events': 'none',
    }));
  }
}

// ── Model 5: Parallel beam (physically correct — window width along wall axis) ─
function renderParallelBeamModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  for (const [id, o] of Object.entries(placedOpenings)) {
    const c = openingCtx(o, azimuth, elevation);
    if (!c) continue;
    const { ox, oy, inwardAngle, elevFactor, dirFactor, wallTangent } = c;
    const op = o.transmission * elevFactor * dirFactor * 0.8;
    if (op < 0.01) continue;
    const [tx, ty] = wallTangent;
    const winHW = o.width_norm * 500;
    const len   = Math.min(950, 350 / Math.tan(Math.max(5, elevation) * Math.PI / 180));
    const ax = ox + tx * winHW, ay = oy + ty * winHW;
    const bx = ox - tx * winHW, by = oy - ty * winHW;
    const bdx = Math.sin(inwardAngle) * len;
    const bdy = -Math.cos(inwardAngle) * len;
    const gid = `lc-pb-${id}`;
    defs.appendChild(mkLinearGrad(gid,
      ox.toFixed(1), oy.toFixed(1), (ox + bdx).toFixed(1), (oy + bdy).toFixed(1),
      [['0%', '#FFF8C0', op.toFixed(3)], ['70%', '#FFCC40', (op * 0.55).toFixed(3)], ['100%', '#FF8C00', '0']]));
    layer.appendChild(svgEl('polygon', {
      points: `${ax.toFixed(1)},${ay.toFixed(1)} ${bx.toFixed(1)},${by.toFixed(1)} ${(bx+bdx).toFixed(1)},${(by+bdy).toFixed(1)} ${(ax+bdx).toFixed(1)},${(ay+bdy).toFixed(1)}`,
      fill: `url(#${gid})`, 'pointer-events': 'none',
    }));
  }
}

// ── Model 6: Beam + footprint (parallel beam + bright landing patch) ──────────
function renderBeamFootprintModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  for (const [id, o] of Object.entries(placedOpenings)) {
    const c = openingCtx(o, azimuth, elevation);
    if (!c) continue;
    const { ox, oy, inwardAngle, canvasInwardDeg, elevFactor, dirFactor, wallTangent } = c;
    const op = o.transmission * elevFactor * dirFactor * 0.75;
    if (op < 0.01) continue;
    const [tx, ty] = wallTangent;
    const winHW = o.width_norm * 500;
    const len   = Math.min(950, 350 / Math.tan(Math.max(5, elevation) * Math.PI / 180));
    const bdx   = Math.sin(inwardAngle) * len;
    const bdy   = -Math.cos(inwardAngle) * len;
    const ax = ox + tx * winHW, ay = oy + ty * winHW;
    const bx = ox - tx * winHW, by = oy - ty * winHW;

    // Semi-transparent beam shaft
    const gbid = `lc-bfb-${id}`;
    defs.appendChild(mkLinearGrad(gbid,
      ox.toFixed(1), oy.toFixed(1), (ox + bdx).toFixed(1), (oy + bdy).toFixed(1),
      [['0%', '#FFF8C0', (op * 0.45).toFixed(3)], ['100%', '#FFCC40', '0']]));
    layer.appendChild(svgEl('polygon', {
      points: `${ax.toFixed(1)},${ay.toFixed(1)} ${bx.toFixed(1)},${by.toFixed(1)} ${(bx+bdx).toFixed(1)},${(by+bdy).toFixed(1)} ${(ax+bdx).toFixed(1)},${(ay+bdy).toFixed(1)}`,
      fill: `url(#${gbid})`, 'pointer-events': 'none',
    }));

    // Bright footprint ellipse where beam lands
    const fcx = ox + bdx, fcy = oy + bdy;
    const frx = winHW * (1 + 0.25 * len / 500);
    const fry = Math.max(14, frx * Math.sin(elevation * Math.PI / 180));
    const gfid = `lc-bff-${id}`;
    defs.appendChild(mkRadialGrad(gfid, [
      ['0%',   '#FFFCE0', (op * 0.95).toFixed(3)],
      ['55%',  '#FFCC40', (op * 0.45).toFixed(3)],
      ['100%', '#FF8C00', '0'],
    ]));
    const el = document.createElementNS('http://www.w3.org/2000/svg', 'ellipse');
    el.setAttribute('cx', fcx.toFixed(1)); el.setAttribute('cy', fcy.toFixed(1));
    el.setAttribute('rx', frx.toFixed(1)); el.setAttribute('ry', fry.toFixed(1));
    el.setAttribute('fill', `url(#${gfid})`);
    el.setAttribute('transform', `rotate(${canvasInwardDeg.toFixed(1)},${fcx.toFixed(1)},${fcy.toFixed(1)})`);
    el.setAttribute('pointer-events', 'none');
    layer.appendChild(el);
  }
}

// ── Model 7: Soft beam (parallel beam with Gaussian blur) ─────────────────────
function renderSoftBeamModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  const fid = 'lc-sb-filter';
  const filter = document.createElementNS('http://www.w3.org/2000/svg', 'filter');
  filter.id = fid;
  filter.setAttribute('x', '-25%'); filter.setAttribute('y', '-25%');
  filter.setAttribute('width', '150%'); filter.setAttribute('height', '150%');
  const blur = document.createElementNS('http://www.w3.org/2000/svg', 'feGaussianBlur');
  blur.setAttribute('stdDeviation', '22');
  filter.appendChild(blur);
  defs.appendChild(filter);

  for (const [id, o] of Object.entries(placedOpenings)) {
    const c = openingCtx(o, azimuth, elevation);
    if (!c) continue;
    const { ox, oy, inwardAngle, elevFactor, dirFactor, wallTangent } = c;
    const op = o.transmission * elevFactor * dirFactor * 1.1; // boosted to compensate blur dimming
    if (op < 0.01) continue;
    const [tx, ty] = wallTangent;
    const winHW = o.width_norm * 500;
    const len   = Math.min(950, 350 / Math.tan(Math.max(5, elevation) * Math.PI / 180));
    const ax = ox + tx * winHW, ay = oy + ty * winHW;
    const bx = ox - tx * winHW, by = oy - ty * winHW;
    const bdx = Math.sin(inwardAngle) * len;
    const bdy = -Math.cos(inwardAngle) * len;
    const gid = `lc-sbg-${id}`;
    defs.appendChild(mkLinearGrad(gid,
      ox.toFixed(1), oy.toFixed(1), (ox + bdx).toFixed(1), (oy + bdy).toFixed(1),
      [['0%', '#FFF8C0', op.toFixed(3)], ['65%', '#FFCC40', (op * 0.5).toFixed(3)], ['100%', '#FF8C00', '0']]));
    layer.appendChild(svgEl('polygon', {
      points: `${ax.toFixed(1)},${ay.toFixed(1)} ${bx.toFixed(1)},${by.toFixed(1)} ${(bx+bdx).toFixed(1)},${(by+bdy).toFixed(1)} ${(ax+bdx).toFixed(1)},${(ay+bdy).toFixed(1)}`,
      fill: `url(#${gid})`, filter: `url(#${fid})`, 'pointer-events': 'none',
    }));
  }
}

// ── Model 8: Wall glow ────────────────────────────────────────────────────────
function renderWallGlowModel(layer, azimuth, elevation) {
  if (!layer || elevation <= -6) return;
  const defs = layerDefs(layer);
  for (const [wid, facing, x1, y1, x2, y2, rx, ry, rw, rh] of [
    ['N', 0,   500, 0,    500, 220,  0,   0,   1000, 220],
    ['S', 180, 500, 1000, 500, 780,  0,   780, 1000, 220],
    ['E', 90,  1000,500,  780, 500,  780, 0,   220,  1000],
    ['W', 270, 0,   500,  220, 500,  0,   0,   220,  1000],
  ]) {
    const wallReal = (facing + compassDeg + 360) % 360;
    const diff = ((azimuth - wallReal) + 360) % 360;
    const norm = diff > 180 ? 360 - diff : diff;
    if (norm >= 90) continue;
    const dir  = Math.max(0, Math.cos(norm * Math.PI / 180));
    const elev = Math.min(1, Math.max(0, (elevation + 6) / 35));
    const intensity = dir * elev * 0.65;
    if (intensity < 0.02) continue;
    const gid = `lc-wg-${wid}`;
    defs.appendChild(mkLinearGrad(gid, x1, y1, x2, y2,
      [['0%', '#FFA020', intensity.toFixed(2)], ['100%', '#FFA020', '0']]));
    const rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
    rect.setAttribute('x', rx); rect.setAttribute('y', ry);
    rect.setAttribute('width', rw); rect.setAttribute('height', rh);
    rect.setAttribute('fill', `url(#${gid})`);
    rect.setAttribute('pointer-events', 'none');
    layer.appendChild(rect);
  }
}

// ── Model 6: Sun arc ──────────────────────────────────────────────────────────
function renderSunArcModel(layer, azimuth, elevation) {
  if (!layer) return;
  const { sunriseAz, sunsetAz, polarDay, polarNight } = todaySunriseSunset();
  if (polarNight) return;
  const R = 490;
  const isDaytime = elevation > 0;

  const azPt = az => {
    const adj = ((az - compassDeg) % 360 + 360) % 360;
    const r   = (adj - 90) * Math.PI / 180;
    return { x: 500 + R * Math.cos(r), y: 500 + R * Math.sin(r) };
  };

  if (polarDay) {
    layer.appendChild(svgEl('circle', { cx: 500, cy: 500, r: R,
      stroke: 'rgba(255,200,50,0.4)', 'stroke-width': 3, fill: 'none', 'pointer-events': 'none' }));
  } else {
    const rPt = azPt(sunriseAz), sPt = azPt(sunsetAz);
    layer.appendChild(svgEl('path', {
      d: `M ${rPt.x.toFixed(1)} ${rPt.y.toFixed(1)} A ${R} ${R} 0 1 1 ${sPt.x.toFixed(1)} ${sPt.y.toFixed(1)}`,
      stroke: isDaytime ? 'rgba(255,200,50,0.65)' : 'rgba(255,200,50,0.2)',
      'stroke-width': 3, fill: 'none',
      'stroke-dasharray': isDaytime ? 'none' : '8 6',
      'pointer-events': 'none',
    }));
    layer.appendChild(svgEl('circle', { cx: rPt.x.toFixed(1), cy: rPt.y.toFixed(1), r: 6,
      fill: 'rgba(255,160,50,0.85)', 'pointer-events': 'none' }));
    layer.appendChild(svgEl('circle', { cx: sPt.x.toFixed(1), cy: sPt.y.toFixed(1), r: 6,
      fill: 'rgba(255,80,50,0.85)',  'pointer-events': 'none' }));
  }

  if (isDaytime) {
    const cp  = azPt(azimuth);
    const dot = svgEl('circle', { cx: cp.x.toFixed(1), cy: cp.y.toFixed(1), r: 10,
      fill: '#FFD700', 'pointer-events': 'none' });
    dot.classList.add('lc-sun-dot');
    const lbl = svgEl('text', { x: cp.x.toFixed(1), y: (cp.y - 18).toFixed(1),
      'text-anchor': 'middle', 'font-size': 18, fill: '#FFD700', 'pointer-events': 'none' });
    lbl.textContent = '☀';
    layer.appendChild(dot);
    layer.appendChild(lbl);
  }
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

    const o = placedOpenings[openingId];
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
    const o = placedOpenings[openingId];
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
    const o = placedOpenings[openingId];
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
    const o = placedOpenings[openingId];
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
    const o = placedOpenings[openingId];
    if (o) patchOpening(openingId, { x_norm: o.x_norm, width_norm: o.width_norm });
  });

  handle.addEventListener('pointercancel', () => { dragging = false; });
}

// ── Openings — popover ────────────────────────────────────────────────────────

function openOpeningPopover(openingId, anchorEl) {
  dismissPopover();
  const o = placedOpenings[openingId];
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
    updateOpeningCone(openingId);
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
      updateOpeningCone(openingId);
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
  pop.style.left = `${Math.min(sp.x + window.scrollX + 12, window.innerWidth - 220)}px`;
  pop.style.top  = `${Math.min(sp.y + window.scrollY + 12, window.innerHeight - 300)}px`;
  document.body.appendChild(pop);
}

// ── Openings — remove ─────────────────────────────────────────────────────────

async function removeOpening(id) {
  const entry = placedOpenings[id];
  if (!entry) return;
  await apiDeleteOpening(id);
  entry.el?.remove();
  const cone = document.getElementById(`cone-${CSS.escape(id)}`);
  if (cone) cone.remove();
  removeOpeningFromThree(id);
  delete placedOpenings[id];
  rebuildPlacedPanel();
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
  const o = placedOpenings[id];
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
  const o = placedOpenings[id] ?? { room_id: layoutRoom?.id };
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

// ── Three.js 3D view ──────────────────────────────────────────────────────────

async function ensureThree() {
  if (THREE) return true;
  try {
    THREE = await import('three');
    const oc = await import('three/addons/controls/OrbitControls.js');
    ThreeOrbitControls = oc.OrbitControls;
    return true;
  } catch (e) {
    console.warn('[3D] Three.js failed to load:', e);
    return false;
  }
}

async function initThree(room) {
  if (!await ensureThree()) return;
  teardownThree();

  const container = document.getElementById('lc-3d-container');
  if (!container) return;

  const W = room.width_m  || 3;
  const D = room.depth_m  || 6;
  const H = room.height_m || 2.5;

  // Renderer
  threeRenderer = new THREE.WebGLRenderer({ antialias: true });
  threeRenderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
  threeRenderer.setSize(container.clientWidth || 600, container.clientHeight || 600);
  threeRenderer.shadowMap.enabled = true;
  threeRenderer.shadowMap.type = THREE.PCFSoftShadowMap;
  container.appendChild(threeRenderer.domElement);

  // Scene + room group
  threeScene = new THREE.Scene();
  threeScene.background = new THREE.Color(0x0d0d1a);
  threeRoomGroup = new THREE.Group();
  threeScene.add(threeRoomGroup);

  // Floor
  const floorMesh = new THREE.Mesh(
    new THREE.PlaneGeometry(W, D),
    new THREE.MeshStandardMaterial({ color: 0x1a1a2e, roughness: 0.9 })
  );
  floorMesh.rotation.x = -Math.PI / 2;
  floorMesh.receiveShadow = true;
  threeRoomGroup.add(floorMesh);

  // Walls — semi-transparent so you can see inside
  const wallMat = () => new THREE.MeshStandardMaterial({
    color: 0x22223b, roughness: 0.8, transparent: true, opacity: 0.4,
    side: THREE.DoubleSide, depthWrite: false,
  });
  const walls = [
    { geom: new THREE.PlaneGeometry(W, H), pos: [0, H/2, -D/2], ry: 0 },
    { geom: new THREE.PlaneGeometry(W, H), pos: [0, H/2,  D/2], ry: Math.PI },
    { geom: new THREE.PlaneGeometry(D, H), pos: [ W/2, H/2, 0], ry: -Math.PI/2 },
    { geom: new THREE.PlaneGeometry(D, H), pos: [-W/2, H/2, 0], ry:  Math.PI/2 },
  ];
  for (const { geom, pos, ry } of walls) {
    const m = new THREE.Mesh(geom, wallMat());
    m.position.set(...pos);
    m.rotation.y = ry;
    threeRoomGroup.add(m);
  }

  // Ceiling edges (wireframe box outline so room reads clearly)
  const edges = new THREE.EdgesGeometry(new THREE.BoxGeometry(W, H, D));
  const line = new THREE.LineSegments(edges, new THREE.LineBasicMaterial({ color: 0x334455 }));
  line.position.y = H / 2;
  threeRoomGroup.add(line);

  // Lighting
  threeScene.add(new THREE.AmbientLight(0xffffff, 0.35));
  threeSunLight = new THREE.DirectionalLight(0xfff8e0, 1.2);
  threeSunLight.castShadow = true;
  threeSunLight.shadow.mapSize.set(1024, 1024);
  threeSunLight.shadow.camera.near = 0.5;
  threeSunLight.shadow.camera.far = 200;
  // Frustum sized to room diagonal + margin for low-sun shadow stretch
  const shadowSpan = Math.hypot(W, D) / 2 * 1.5;
  threeSunLight.shadow.camera.left = -shadowSpan;
  threeSunLight.shadow.camera.right = shadowSpan;
  threeSunLight.shadow.camera.top = shadowSpan;
  threeSunLight.shadow.camera.bottom = -shadowSpan;
  threeScene.add(threeSunLight);
  threeScene.add(threeSunLight.target);
  threeUpdateSun(lastSolar.azimuth, lastSolar.elevation);

  // Grid on floor
  const grid = new THREE.GridHelper(Math.max(W, D) * 1.5, 10, 0x223344, 0x1a2233);
  grid.position.y = 0.001;
  threeRoomGroup.add(grid);

  // Crosshair on floor
  const cxW = (room.origin_x - 0.5) * W;
  const czD = (room.origin_y - 0.5) * D;
  const chMat = new THREE.LineBasicMaterial({ color: 0x00ffff });
  const hPts = [new THREE.Vector3(-W/2, 0.003, czD), new THREE.Vector3(W/2, 0.003, czD)];
  const vPts = [new THREE.Vector3(cxW, 0.003, -D/2), new THREE.Vector3(cxW, 0.003, D/2)];
  threeRoomGroup.add(new THREE.Line(new THREE.BufferGeometry().setFromPoints(hPts), chMat));
  threeRoomGroup.add(new THREE.Line(new THREE.BufferGeometry().setFromPoints(vPts), chMat.clone()));

  // Camera
  const aspect = (container.clientWidth || 600) / (container.clientHeight || 600);
  threePerspCamera = new THREE.PerspectiveCamera(45, aspect, 0.1, 500);
  threePerspCamera.position.set(W * 0.9, H * 2.2, D * 0.9);
  threePerspCamera.lookAt(0, H * 0.3, 0);

  // Orbit controls — with damping for smooth deceleration
  threeControls = new ThreeOrbitControls(threePerspCamera, threeRenderer.domElement);
  threeControls.target.set(0, H * 0.3, 0);
  threeControls.minDistance = 0.5;
  threeControls.maxDistance = Math.max(W, D) * 5;
  threeControls.enableDamping = true;
  threeControls.dampingFactor = 0.08;
  threeControls.addEventListener('change', threeMarkDirty);
  threeControls.update();

  // Sync all already-placed bulbs
  for (const [id, entry] of Object.entries(placedBulbs)) {
    syncBulbToThree(id, entry, devicesRef.get(id));
  }

  // Sync all already-placed openings
  for (const o of Object.values(placedOpenings)) {
    syncOpeningToThree(o);
  }

  // Raycaster + interactions
  threeRaycaster = new THREE.Raycaster();
  threeFloorPlane = new THREE.Plane(new THREE.Vector3(0, 1, 0), 0);
  wireThreeInteractions(container);

  // Resize observer
  const ro = new ResizeObserver(() => {
    if (!threeRenderer || !threePerspCamera) return;
    const w = container.clientWidth, h = container.clientHeight;
    if (!w || !h) return;
    threeRenderer.setSize(w, h);
    threePerspCamera.aspect = w / h;
    threePerspCamera.updateProjectionMatrix();
  });
  ro.observe(container);
  container._ro = ro;

  // Render loop
  threeNeedsRender = true;
  function animate() {
    threeAnimFrameId = requestAnimationFrame(animate);
    if (threeControls.update()) threeNeedsRender = true; // damping still settling
    if (!threeNeedsRender) return;
    threeNeedsRender = false;
    threeRenderer.render(threeScene, threePerspCamera);
  }
  animate();
}

function teardownThree() {
  if (threeAnimFrameId) { cancelAnimationFrame(threeAnimFrameId); threeAnimFrameId = null; }
  if (threeControls) { threeControls.dispose(); threeControls = null; }
  if (threeRenderer) { threeRenderer.dispose(); threeRenderer.domElement.remove(); threeRenderer = null; }
  const c = document.getElementById('lc-3d-container');
  if (c?._ro) { c._ro.disconnect(); delete c._ro; }
  threeScene = null; threeRoomGroup = null; threePerspCamera = null;
  threeSunLight = null; threeBulbMeshes = {}; threeOpeningMeshes = {};
  threeIs3D = false;
  threeRaycaster = null; threeFloorPlane = null;
}

function syncBulbToThree(deviceId, entry, dev) {
  if (!threeScene || !THREE || !threeRoomGroup || !layoutRoom) return;
  removeBulbFromThree(deviceId);

  const W = layoutRoom.width_m  || 3;
  const D = layoutRoom.depth_m  || 6;
  const H = layoutRoom.height_m || 2.5;

  const x3 = (entry.x - 0.5) * W;
  const y3 = (entry.z ?? 0.9) * H;
  const z3 = (entry.y - 0.5) * D;

  const colorStr = dev ? devStateColor(dev) : '#111133';
  const color = new THREE.Color(colorStr);
  const bri = dev?.on ? (dev.brightness ?? 200) / 254 : 0;

  const mat = new THREE.MeshStandardMaterial({
    color: 0x222222,
    emissive: color,
    emissiveIntensity: Math.max(bri, 0.1),
    roughness: 0.4,
  });
  const mesh = new THREE.Mesh(new THREE.SphereGeometry(0.07, 16, 16), mat);
  mesh.position.set(x3, y3, z3);
  mesh.castShadow = true;
  mesh.userData.deviceId = deviceId;

  const ptLight = new THREE.PointLight(color, bri * 3, W * 2.5);
  ptLight.castShadow = false;
  mesh.add(ptLight);

  threeRoomGroup.add(mesh);
  threeBulbMeshes[deviceId] = { mesh, ptLight, mat };
  threeMarkDirty();
}

function removeBulbFromThree(deviceId) {
  const b = threeBulbMeshes[deviceId];
  if (!b) return;
  b.mesh.removeFromParent();
  b.mat.dispose();
  delete threeBulbMeshes[deviceId];
  threeMarkDirty();
}

function syncOpeningToThree(o) {
  if (!threeScene || !THREE || !threeRoomGroup || !layoutRoom) return;
  removeOpeningFromThree(o.id);

  const W = layoutRoom.width_m  || 3;
  const D = layoutRoom.depth_m  || 6;
  const H = layoutRoom.height_m || 2.5;
  const isWindow = o.opening_type === 'window';

  // Physical dimensions
  const wallLen = (o.wall_edge === 'N' || o.wall_edge === 'S') ? W : D;
  const openW = o.width_norm * wallLen;
  const openH = isWindow ? H * 0.45 : H * 0.8;
  const openY = isWindow ? H * 0.65 : H * 0.4;   // centre height

  // Position along wall axis
  const posAlong = (o.x_norm - 0.5) * wallLen;
  const eps = 0.015; // offset from wall face to avoid z-fighting

  let x, y = openY, z, ry;
  switch (o.wall_edge) {
    case 'N': x = posAlong;  z = -D/2 + eps; ry = 0;           break;
    case 'S': x = posAlong;  z =  D/2 - eps; ry = Math.PI;     break;
    case 'E': x =  W/2 - eps; z = posAlong;  ry = -Math.PI/2;  break;
    case 'W': x = -W/2 + eps; z = posAlong;  ry =  Math.PI/2;  break;
    default: return;
  }

  const mat = new THREE.MeshStandardMaterial({
    color:       isWindow ? 0x88ccff : 0x7a5230,
    transparent: isWindow,
    opacity:     isWindow ? 0.45 : 1.0,
    side:        THREE.DoubleSide,
    roughness:   isWindow ? 0.05 : 0.8,
    metalness:   isWindow ? 0.1  : 0.0,
  });
  const mesh = new THREE.Mesh(new THREE.PlaneGeometry(openW, openH), mat);
  mesh.position.set(x, y, z);
  mesh.rotation.y = ry;
  threeRoomGroup.add(mesh);
  threeOpeningMeshes[o.id] = mesh;
  threeMarkDirty();
}

function removeOpeningFromThree(id) {
  const mesh = threeOpeningMeshes[id];
  if (!mesh) return;
  mesh.material.dispose();
  mesh.geometry.dispose();
  mesh.removeFromParent();
  delete threeOpeningMeshes[id];
  threeMarkDirty();
}

function threeMarkDirty() { threeNeedsRender = true; }

function threeUpdateSun(azimuth, elevation) {
  if (!threeSunLight) return;
  const phi   = (90 - elevation) * (Math.PI / 180);
  const theta = azimuth * (Math.PI / 180);
  const R = 50;
  threeSunLight.position.set(
    R * Math.sin(phi) * Math.sin(theta),
    R * Math.cos(phi),
    R * Math.sin(phi) * Math.cos(theta),
  );
  threeSunLight.intensity = Math.max(0, Math.sin(Math.max(elevation, 0) * Math.PI / 180)) * 1.5 + 0.1;
  threeMarkDirty();
}

function threeUpdateBulbColor(deviceId, dev) {
  const b = threeBulbMeshes[deviceId];
  if (!b || !THREE) return;
  const colorStr = dev ? devStateColor(dev) : '#111133';
  const color = new THREE.Color(colorStr);
  const bri = dev?.on ? (dev.brightness ?? 200) / 254 : 0;
  b.mat.emissive.set(color);
  b.mat.emissiveIntensity = Math.max(bri, 0.1);
  b.ptLight.color.set(color);
  b.ptLight.intensity = bri * 3;
  threeMarkDirty();
}

function toggle3D(btn) {
  threeIs3D = !threeIs3D;
  const svg     = document.getElementById('layout-canvas');
  const c3d     = document.getElementById('lc-3d-container');
  const sidebar = document.querySelector('.layout-sidebar');
  const modelSel = document.getElementById('lc-model-select');
  if (threeIs3D) {
    dismissPopover();
    if (svg) svg.style.display = 'none';
    if (c3d) c3d.style.display = '';
    if (sidebar) sidebar.style.display = 'none';
    if (modelSel) modelSel.style.display = 'none';
    btn.textContent = '2D';
    btn.title = 'Switch to 2D editing view';
    // Force a renderer resize now that the container is visible
    if (threeRenderer && threePerspCamera && c3d) {
      const w = c3d.clientWidth, h = c3d.clientHeight;
      if (w && h) {
        threeRenderer.setSize(w, h);
        threePerspCamera.aspect = w / h;
        threePerspCamera.updateProjectionMatrix();
      }
    }
    // Re-sync openings that may have moved while in 2D view
    for (const o of Object.values(placedOpenings)) {
      syncOpeningToThree(o);
    }
  } else {
    if (svg) svg.style.display = '';
    if (c3d) c3d.style.display = 'none';
    if (sidebar) sidebar.style.display = '';
    if (modelSel) modelSel.style.display = '';
    btn.textContent = '3D';
    btn.title = 'Switch to 3D perspective view';
  }
  threeMarkDirty();
}

// ── Three.js raycaster interactions ───────────────────────────────────────────

function threeMouseNDC(e, el) {
  const r = el.getBoundingClientRect();
  return new THREE.Vector2(
    ((e.clientX - r.left) / r.width)  *  2 - 1,
    ((e.clientY - r.top)  / r.height) * -2 + 1,
  );
}

function threeFloorHit(ndc) {
  if (!threeRaycaster || !threePerspCamera || !threeFloorPlane) return null;
  threeRaycaster.setFromCamera(ndc, threePerspCamera);
  const pt = new THREE.Vector3();
  return threeRaycaster.ray.intersectPlane(threeFloorPlane, pt) ? pt : null;
}

function wireThreeInteractions(_container) {
  const el = threeRenderer.domElement;

  // The 3D view is a demo / visualisation surface only — no layout editing.
  // Bulbs cannot be dragged or placed here; the user does layout work in the
  // 2D view and uses 3D to see effects / solar / scenes light up.
  // A tap on a bulb still opens its popover so the user can tweak the device
  // without switching views; orbit controls (rotate / zoom) remain active.
  el.addEventListener('pointerdown', e => {
    if (e.button !== 0) return;
    dismissPopover();
    document.getElementById('three-opening-menu')?.remove();

    const ndc = threeMouseNDC(e, el);
    threeRaycaster.setFromCamera(ndc, threePerspCamera);

    // Bulb tap → open popover (no drag)
    const bulbMeshList = Object.values(threeBulbMeshes).map(b => b.mesh);
    const bulbHits = threeRaycaster.intersectObjects(bulbMeshList);
    if (bulbHits.length > 0) {
      e.stopPropagation();
      const deviceId = bulbHits[0].object.userData.deviceId;
      openPopover(deviceId, null, e.clientX, e.clientY);
      return;
    }

    // Opening tap → context menu (toggle window/door state)
    const openingMeshList = Object.values(threeOpeningMeshes);
    const openingHits = threeRaycaster.intersectObjects(openingMeshList);
    if (openingHits.length > 0) {
      e.stopPropagation();
      const mesh = openingHits[0].object;
      const id = Object.keys(threeOpeningMeshes).find(k => threeOpeningMeshes[k] === mesh);
      if (id) showOpeningContextMenu(id, e.clientX, e.clientY);
    }
  });
}

function showOpeningContextMenu(openingId, screenX, screenY) {
  // Dismiss any existing menu
  document.getElementById('three-opening-menu')?.remove();

  const menu = document.createElement('div');
  menu.id = 'three-opening-menu';
  menu.className = 'layout-popover';
  menu.style.left = `${Math.min(screenX + 8, window.innerWidth - 160)}px`;
  menu.style.top  = `${Math.min(screenY - 8, window.innerHeight - 80)}px`;

  const o = placedOpenings[openingId];
  const label = document.createElement('div');
  label.className = 'layout-popover-label';
  label.textContent = o ? `${o.opening_type === 'window' ? 'Window' : 'Door'}` : 'Opening';
  menu.appendChild(label);

  const removeBtn = document.createElement('button');
  removeBtn.className = 'layout-popover-remove';
  removeBtn.textContent = 'Remove';
  removeBtn.addEventListener('click', () => {
    menu.remove();
    removeOpening(openingId);
  });
  menu.appendChild(removeBtn);

  document.body.appendChild(menu);

  const dismiss = e => {
    if (!menu.contains(e.target)) { menu.remove(); document.removeEventListener('pointerdown', dismiss); }
  };
  setTimeout(() => document.addEventListener('pointerdown', dismiss), 0);
}

