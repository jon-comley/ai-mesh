// ── Room Layout Canvas ────────────────────────────────────────────────────────
// SVG top-down floor plan for placing bulbs and (Phase B) windows/doors.
// Coordinates are always 0–1 normalised; the SVG scales to any screen size.

// ── State ─────────────────────────────────────────────────────────────────────

let layoutRoom = null;          // RoomRecord currently in view
let devicesRef = new Map();     // reference to rooms.js devicesMap — set via init()
let placedBulbs = {};           // device_id → { x, y, z, fixture_type, el, labelEl }
let placedOpenings = {};        // opening_id → { opening_type, wall_edge, x_norm, width_norm, transmission, el }
let lastSolar = { azimuth: 180, elevation: -90 };
let undoStack = [];             // position snapshots for Ctrl+Z
let redoStack = [];
let snapDivisions = 20;         // invisible grid: 1/N of canvas width
let showLabels = true;
let activePopover = null;       // currently open popover element
let dragType = null;            // 'bulb' | 'opening' — set on dragstart since getData is unavailable in dragover
// Global safety net: clear dragType if a drag is cancelled or ends outside the canvas
window.addEventListener('dragend', () => { dragType = null; });
window.addEventListener('drop', () => { dragType = null; });

// ── Phase D: compass + sun arc state ─────────────────────────────────────────
let compassDeg = 0;             // current orientation_degrees for the open room
let compassDragging = false;
let compassOrientTimer = null;
let scrubberLive = true;        // false while time scrubber is in preview mode
let scrubberRafPending = false;
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

  if (room.solar_enabled) {
    // Init compass dial and sun arc now that the SVG exists
    renderCompassDial();
    wireCompass();
    wirePhoneCompass();
    wireSunCalib();
    wireScrubber();
    wireModelSelect();
    redrawSolarOverlay(lastSolar.azimuth, lastSolar.elevation);
    redrawLightEffect(lastSolar.azimuth, lastSolar.elevation);
    updateSunCalibButton();
  } else {
    // Hide solar controls for rooms without solar
    const scrubBar = document.getElementById('lc-scrubber-bar');
    if (scrubBar) scrubBar.style.display = 'none';
  }

  document.addEventListener('keydown', onKeyDown);
}

export function closeLayout() {
  document.removeEventListener('keydown', onKeyDown);
  dismissPopover();

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
  if (!entry) return;
  updateBulbIcon(entry, state);
}

// Called by rooms.js when a SolarUpdate WS event arrives — redraws cones + arc.
export function notifySolarUpdate(azimuth, elevation) {
  lastSolar = { azimuth, elevation };
  if (scrubberLive) {
    redrawLightEffect(azimuth, elevation);
    redrawSolarOverlay(azimuth, elevation);
  }
  updateSunCalibButton();
}

// Called by rooms.js when a RoomsUpdate arrives with a new orientation for this room.
export function notifyOrientationUpdate(deg) {
  compassDeg = deg;
  const dial = document.getElementById('lc-compass-dial');
  if (dial) dial.setAttribute('transform', `rotate(${deg})`);
  if (scrubberLive) previewSolarState(lastSolar.azimuth, lastSolar.elevation);
}

// ── Phase D: Compass dial ─────────────────────────────────────────────────────

function renderCompassDial() {
  const g = document.getElementById('lc-compass');
  if (!g) return;
  g.innerHTML = '';

  // Outer ring
  g.appendChild(svgEl('circle', { cx: 925, cy: 75, r: 52, fill: 'rgba(0,0,0,0.6)', stroke: '#555', 'stroke-width': 1.5 }));

  // Cardinal labels — pointer-events:none so they don't block the drag handle
  for (const [txt, x, y] of [['N', 925, 35], ['S', 925, 121], ['E', 969, 79], ['W', 881, 79]]) {
    const t = svgEl('text', { x, y, 'text-anchor': 'middle', 'dominant-baseline': 'central',
      'font-size': 18, 'font-weight': 700, fill: '#ccc', 'pointer-events': 'none' });
    t.textContent = txt;
    g.appendChild(t);
  }

  // Rotating dial group
  const dial = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  dial.id = 'lc-compass-dial';
  dial.setAttribute('transform', `rotate(${compassDeg},925,75)`);
  // N pointer (amber)
  dial.appendChild(svgEl('polygon', { points: '925,37 929,57 921,57', fill: '#e8c84a' }));
  // S pointer (grey)
  dial.appendChild(svgEl('polygon', { points: '925,113 929,93 921,93', fill: '#555' }));
  // Centre dot
  dial.appendChild(svgEl('circle', { cx: 925, cy: 75, r: 5, fill: '#fff' }));
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
  tip.textContent = 'Drag to set orientation: point N toward the real-world compass direction your top canvas wall actually faces.';
  handle.appendChild(tip);
  g.appendChild(handle);
}

function wireCompass() {
  const svg = document.getElementById('layout-canvas');
  if (!svg) return;

  svg.addEventListener('pointerdown', e => {
    if (e.target.id !== 'lc-compass-handle') return;
    compassDragging = true;
    // Capture to svg so pointermove/pointerup on svg still fire during fast drags
    svg.setPointerCapture(e.pointerId);
    e.target.style.cursor = 'grabbing';
  });

  svg.addEventListener('pointermove', e => {
    if (!compassDragging) return;
    const pt = svg.createSVGPoint();
    pt.x = e.clientX; pt.y = e.clientY;
    const sp = pt.matrixTransform(svg.getScreenCTM().inverse());
    const angle = Math.atan2(sp.y - 75, sp.x - 925) * 180 / Math.PI + 90;
    compassDeg = ((angle % 360) + 360) % 360;
    const dial = document.getElementById('lc-compass-dial');
    if (dial) dial.setAttribute('transform', `rotate(${compassDeg},925,75)`);
    previewSolarState(lastSolar.azimuth, lastSolar.elevation);
  });

  svg.addEventListener('pointerup', e => {
    if (!compassDragging) return;
    compassDragging = false;
    const handle = document.getElementById('lc-compass-handle');
    if (handle) handle.style.cursor = 'grab';
    clearTimeout(compassOrientTimer);
    compassOrientTimer = setTimeout(() => patchOrientation(layoutRoom?.id, compassDeg), 400);
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
        if (dial) dial.setAttribute('transform', `rotate(${compassDeg},925,75)`);
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
    const rad = (azimuth - 90) * Math.PI / 180;
    sunDot.setAttribute('cx', (925 + 42 * Math.cos(rad)).toFixed(1));
    sunDot.setAttribute('cy', (75  + 42 * Math.sin(rad)).toFixed(1));
  }
}

// ── Phase D: client-side solar state (matches Rust calculate_solar_state) ────

function calculateSolarState(elevation) {
  if (elevation <= 0) {
    const t = Math.max(0, Math.min(1, (elevation + 18) / 18));
    return { bri: Math.round(1 + t * 29), ct: 500 };
  }
  const t = Math.min(1, elevation / 90);
  return { bri: Math.round(30 + t * 225), ct: Math.round(454 - t * 301) };
}

function previewSolarState(azimuth, elevation) {
  lastSolar = { azimuth, elevation };  // keep in sync so model-change redraws use correct position
  const { bri, ct } = calculateSolarState(elevation);
  for (const [, entry] of Object.entries(placedBulbs)) {
    updateBulbIcon(entry, { on: bri > 5, brightness: bri, color_temp: ct, color_xy: null });
  }
  redrawSolarOverlay(azimuth, elevation);
  redrawLightEffect(azimuth, elevation);
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

function wireScrubber() {
  const scrubber = document.getElementById('lc-scrubber');
  const liveBtn  = document.getElementById('lc-scrubber-live');
  const timeEl   = document.getElementById('lc-scrubber-time');
  if (!scrubber || !liveBtn || !timeEl) return;

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
    });
  });

  // On release: push the simulated solar state to the real bulbs
  scrubber.addEventListener('change', () => {
    if (!scrubberLive) sendSimSolarCommands(lastSolar.elevation);
  });

  liveBtn.addEventListener('click', () => {
    scrubberLive = true;
    setScrubberSimMode(false);
    timeEl.textContent = 'Now';
    redrawSolarOverlay(lastSolar.azimuth, lastSolar.elevation);
    previewSolarState(lastSolar.azimuth, lastSolar.elevation);
  });
}

// Send the simulated solar brightness + colour temperature to all solar-enabled
// devices in the currently open room. Called on scrubber mouse-release only —
// not on every drag tick — to avoid flooding the Zigbee bus.
async function sendSimSolarCommands(elevation) {
  const room = layoutRoom;
  if (!room) return;
  const { bri, ct } = calculateSolarState(elevation);
  const t = tok();
  for (const deviceId of room.device_ids) {
    const dev = devicesRef.get(deviceId);
    if (!dev?.solar_enabled) continue;
    // brightness command also implicitly turns the device on
    fetch(`/api/lights/${encodeURIComponent(deviceId)}/command?token=${encodeURIComponent(t)}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ action: 'brightness', value: bri, transition_secs: 1.5 }),
    }).catch(() => {});
    fetch(`/api/lights/${encodeURIComponent(deviceId)}/command?token=${encodeURIComponent(t)}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ action: 'color_temp', value: ct, transition_secs: 1.5 }),
    }).catch(() => {});
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
      if (dial) dial.setAttribute('transform', `rotate(${compassDeg},925,75)`);
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

  header.appendChild(controls);
  view.appendChild(header);

  // Body: sidebar + canvas
  const body = document.createElement('div');
  body.className = 'layout-body';

  body.appendChild(buildSidebar(room));
  body.appendChild(buildCanvas());

  view.appendChild(body);
  return view;
}

function buildSidebar(room) {
  const sidebar = document.createElement('div');
  sidebar.className = 'layout-sidebar';

  const label = document.createElement('div');
  label.className = 'layout-sidebar-label';
  label.textContent = 'Bulbs:';
  sidebar.appendChild(label);

  const chips = document.createElement('div');
  chips.className = 'layout-sidebar-chips';
  chips.id = 'layout-sidebar-chips';
  sidebar.appendChild(chips);

  // Openings section — always shown
  const openingsLabel = document.createElement('div');
  openingsLabel.className = 'layout-sidebar-label';
  openingsLabel.style.marginTop = '12px';
  openingsLabel.textContent = 'Openings (drop to wall):';
  sidebar.appendChild(openingsLabel);

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
      e.dataTransfer.effectAllowed = 'copy';
      e.dataTransfer.setData('text/plain', `opening:${type}`);
    });
    chip.addEventListener('dragend', () => { dragType = null; });
    openingChips.appendChild(chip);
  }
  sidebar.appendChild(openingChips);

  sidebar._room = room;
  return sidebar;
}

function rebuildSidebar() {
  const chips = document.getElementById('layout-sidebar-chips');
  if (!chips) return;
  chips.innerHTML = '';

  const room = layoutRoom;
  const unplaced = (room.device_ids || []).filter(id => !placedBulbs[id]);

  if (unplaced.length === 0) {
    const hint = document.createElement('span');
    hint.className = 'layout-sidebar-empty';
    hint.textContent = 'All bulbs placed';
    chips.appendChild(hint);
  } else {
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
    e.dataTransfer.effectAllowed = 'copy';
    e.dataTransfer.setData('text/plain', `bulb:${deviceId}`);
    // Trigger pulse-on-grab via rooms.js exported function
    if (typeof window.__roomsStartPulse === 'function') {
      window.__roomsStartPulse(deviceId);
    }
  });
  chip.addEventListener('dragend', () => {
    dragType = null;
    if (typeof window.__roomsStopPulse === 'function') {
      window.__roomsStopPulse(true);
    }
  });
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

  // Layer order: wall-glow behind everything, compass on top
  for (const id of ['lc-sun-arc', 'lc-openings', 'lc-shadow', 'lc-bulbs', 'lc-preview', 'lc-compass']) {
    const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
    g.id = id;
    svg.appendChild(g);
  }


  svg.addEventListener('dragover', onCanvasDragOver);
  svg.addEventListener('dragleave', onCanvasDragLeave);
  svg.addEventListener('drop', onCanvasDrop);
  svg.addEventListener('click', onCanvasClick);

  wrap.appendChild(svg);
  outer.appendChild(wrap);

  // Scrubber bar below canvas
  const scrubBar = document.createElement('div');
  scrubBar.id = 'lc-scrubber-bar';
  const nowMins = new Date().getHours() * 60 + new Date().getMinutes();
  scrubBar.innerHTML = `
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

function snap(v) {
  return Math.round(v * snapDivisions) / snapDivisions;
}

function onCanvasDragOver(e) {
  e.preventDefault();
  if (!e.dataTransfer.types.includes('text/plain')) return;

  const svg = document.getElementById('layout-canvas');
  const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
  const preview = document.getElementById('lc-preview');
  if (!preview) return;
  preview.innerHTML = '';

  if (dragType === 'opening') {
    const wall = detectWall(nx, ny);
    if (wall) {
      // Snap x_norm to midpoint (0.5) when close
      const rawPos = isHorizontalWall(wall) ? nx : ny;
      const xNorm = Math.abs(rawPos - 0.5) < 0.06 ? 0.5 : snap(rawPos);
      const r = openingToSvgRect({ wall_edge: wall, x_norm: xNorm, width_norm: 0.3 });
      const ghost = svgEl('rect', {
        x: r.x, y: r.y, width: r.w, height: r.h, rx: 3,
        fill: 'rgba(100,200,255,0.45)', stroke: 'rgba(100,200,255,0.9)',
        'stroke-width': 2, 'pointer-events': 'none',
      });
      preview.appendChild(ghost);
    } else {
      const reject = svgEl('text', {
        x: snap(nx) * 1000, y: snap(ny) * 1000,
        'text-anchor': 'middle', 'dominant-baseline': 'central',
        fill: 'rgba(255,80,80,0.85)', 'font-size': 48, 'pointer-events': 'none',
      });
      reject.textContent = '✕';
      preview.appendChild(reject);
    }
  } else {
    const sx = snap(nx), sy = snap(ny);
    const glow = svgEl('circle', {
      cx: sx * 1000, cy: sy * 1000, r: 28,
      fill: 'rgba(255,255,200,0.25)', stroke: 'rgba(255,255,200,0.7)', 'stroke-width': 2,
      'pointer-events': 'none',
    });
    preview.appendChild(glow);
  }
}

function onCanvasDragLeave() {
  const preview = document.getElementById('lc-preview');
  if (preview) preview.innerHTML = '';
}

function onCanvasDrop(e) {
  e.preventDefault();
  dragType = null;
  const preview = document.getElementById('lc-preview');
  if (preview) preview.innerHTML = '';

  const raw = e.dataTransfer.getData('text/plain');
  const svg = document.getElementById('layout-canvas');
  const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);

  if (raw.startsWith('opening:')) {
    const openingType = raw.slice(8);
    const wall = detectWall(nx, ny);
    if (!wall || !layoutRoom) return;
    const rawPos = isHorizontalWall(wall) ? nx : ny;
    const xNorm = Math.abs(rawPos - 0.5) < 0.06 ? 0.5 : snap(rawPos);
    const transmission = openingType === 'window' ? 1.0 : 0.1;
    postCreateOpening(layoutRoom.id, openingType, wall, xNorm, 0.3, transmission);
    return;
  }

  if (!raw.startsWith('bulb:')) return;
  const deviceId = raw.slice(5);

  const x = snap(nx);
  const y = snap(ny);

  const existing = placedBulbs[deviceId];
  const fixtureType = existing?.fixture_type ?? 'ceiling_spot';
  const z = existing?.z ?? defaultZ(fixtureType);

  pushUndo();
  placeBulb(deviceId, x, y, z, fixtureType, true);
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
  labelEl.setAttribute('pointer-events', 'none');
  labelEl.textContent = labelText;
  labelEl.style.display = showLabels ? '' : 'none';
  g.appendChild(labelEl);

  layer.appendChild(g);

  placedBulbs[deviceId] = { x, y, z, fixture_type: fixtureType, el: g, labelEl };

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

  // Clear previous icon children (keep text/rect labels)
  [...g.children].forEach(c => {
    if (c.tagName !== 'text' && c.getAttribute('fill') !== 'rgba(0,0,0,0.55)') c.remove();
  });

  const els = [];

  if (fixtureType === 'led_strip') {
    // Wide pill — represents a strip mounted on a wall or ceiling
    els.push(svgEl('rect', { x: cx - 44, y: cy - 8, width: 88, height: 16, rx: 8,
      fill: color, opacity: on ? 0.2 : 0.1 }));
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
    els.push(svgEl('circle', { cx, cy, r: 22, fill: color, opacity: on ? 0.15 : 0.05 }));
    const bulb = svgEl('circle', { cx, cy, r: 14, fill: color, opacity: alpha });
    bulb.classList.add('lc-bulb-shape');
    els.push(bulb);
    // Ring outline
    els.push(svgEl('circle', { cx, cy, r: 14, fill: 'none',
      stroke: color, 'stroke-width': 2, opacity: Math.min(alpha + 0.3, 1) }));

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
    els.push(svgEl('path', {
      d: `M ${cx - 16} ${cy - 4} A 16 16 0 0 1 ${cx + 16} ${cy - 4}`,
      fill: color, opacity: alpha }));
    const head = svgEl('circle', { cx, cy: cy - 4, r: 7, fill: color, opacity: alpha });
    head.classList.add('lc-bulb-shape');
    els.push(head);
    els.push(svgEl('rect', { x: cx - 8, y: cy + 28, width: 16, height: 4, rx: 2,
      fill: 'rgba(255,255,255,0.35)' }));

  } else {
    // ceiling_spot (default) — downlight: halo ring + filled dot
    els.push(svgEl('circle', { cx, cy, r: 26, fill: color, opacity: on ? 0.12 : 0.04 }));
    els.push(svgEl('circle', { cx, cy, r: 18, fill: 'none',
      stroke: color, 'stroke-width': 2, opacity: on ? 0.5 : 0.2 }));
    const dot = svgEl('circle', { cx, cy, r: 10, fill: color, opacity: alpha });
    dot.classList.add('lc-bulb-shape');
    els.push(dot);
    // Cross-hatch tick marks like a recessed light symbol
    els.push(svgEl('line', { x1: cx - 18, y1: cy, x2: cx - 10, y2: cy,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 }));
    els.push(svgEl('line', { x1: cx + 10, y1: cy, x2: cx + 18, y2: cy,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 }));
    els.push(svgEl('line', { x1: cx, y1: cy - 18, x2: cx, y2: cy - 10,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 }));
    els.push(svgEl('line', { x1: cx, y1: cy + 10, x2: cx, y2: cy + 18,
      stroke: color, 'stroke-width': 1.5, opacity: on ? 0.5 : 0.2 }));
  }

  // Insert before any text/label children
  const firstLabel = [...g.children].find(c => c.tagName === 'text' || c.getAttribute('fill') === 'rgba(0,0,0,0.55)');
  for (const el of els) {
    if (firstLabel) g.insertBefore(el, firstLabel);
    else g.appendChild(el);
  }
}

function makeBulbDraggable(g, deviceId) {
  let dragging = false;
  let moved = false;
  let startNx, startNy, startBulbX, startBulbY;

  g.addEventListener('pointerdown', e => {
    if (e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    dismissPopover();

    const svg = document.getElementById('layout-canvas');
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const entry = placedBulbs[deviceId];
    if (!entry) return;

    dragging = true;
    moved = false;
    startNx = nx; startNy = ny;
    startBulbX = entry.x; startBulbY = entry.y;

    g.setPointerCapture(e.pointerId);
    g.style.cursor = 'grabbing';

    if (typeof window.__roomsStartPulse === 'function') window.__roomsStartPulse(deviceId);
  });

  g.addEventListener('pointermove', e => {
    if (!dragging) return;
    e.stopPropagation();

    const svg = document.getElementById('layout-canvas');
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const dx = nx - startNx;
    const dy = ny - startNy;

    if (Math.abs(dx) > 0.005 || Math.abs(dy) > 0.005) moved = true;
    if (!moved) return;

    const newX = Math.max(0, Math.min(1, snap(startBulbX + dx)));
    const newY = Math.max(0, Math.min(1, snap(startBulbY + dy)));
    // Translate the group visually without re-creating it
    const entry = placedBulbs[deviceId];
    const tx = (newX - entry.x) * 1000;
    const ty = (newY - entry.y) * 1000;
    g.setAttribute('transform', `translate(${tx},${ty})`);
  });

  g.addEventListener('pointerup', e => {
    if (!dragging) return;
    dragging = false;
    g.style.cursor = 'grab';
    g.releasePointerCapture(e.pointerId);

    if (!moved) {
      // Tap — open popover; keep pulse running until popover is dismissed
      openPopover(deviceId, g);
      return;
    }

    if (typeof window.__roomsStopPulse === 'function') window.__roomsStopPulse(true);

    g.removeAttribute('transform');

    const svg = document.getElementById('layout-canvas');
    const { nx, ny } = svgPoint(svg, e.clientX, e.clientY);
    const newX = Math.max(0, Math.min(1, snap(startBulbX + (nx - startNx))));
    const newY = Math.max(0, Math.min(1, snap(startBulbY + (ny - startNy))));

    const entry = placedBulbs[deviceId];
    if (newX !== entry.x || newY !== entry.y) {
      pushUndo();
      placeBulb(deviceId, newX, newY, entry.z, entry.fixture_type, true);
    }
  });

  g.addEventListener('pointercancel', () => {
    if (!dragging) return;
    dragging = false;
    g.removeAttribute('transform');
    g.style.cursor = 'grab';
    if (typeof window.__roomsStopPulse === 'function') window.__roomsStopPulse(false);
  });
}

function updateBulbIcon(entry, state) {
  const dev = devicesRef.get(
    Object.entries(placedBulbs).find(([, v]) => v === entry)?.[0] ?? ''
  );
  if (!entry.el) return;
  const shape = entry.el.querySelector('.lc-bulb-shape');
  if (!shape) return;
  const color = devStateColor(state ?? dev);
  shape.setAttribute('fill', color);
  shape.setAttribute('opacity', (state ?? dev)?.on ? 1 : 0.45);
}

// ── Popover ───────────────────────────────────────────────────────────────────

function openPopover(deviceId, anchorEl) {
  dismissPopover();

  const entry = placedBulbs[deviceId];
  if (!entry) return;

  const pop = document.createElement('div');
  pop.className = 'layout-popover';
  activePopover = pop;

  // Fixture type picker
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

  // Height slider
  const heightLabel = document.createElement('div');
  heightLabel.className = 'layout-popover-label';
  heightLabel.textContent = `Height: ${Math.round(entry.z * 100)}%`;
  pop.appendChild(heightLabel);

  const slider = document.createElement('input');
  slider.type = 'range';
  slider.min = '0'; slider.max = '100';
  slider.value = Math.round(entry.z * 100);
  slider.className = 'layout-popover-slider';
  slider.addEventListener('input', () => {
    const z = parseInt(slider.value) / 100;
    heightLabel.textContent = `Height: ${slider.value}%`;
    entry.z = z;
    drawFixtureIcon(entry.el, entry.x * 1000, entry.y * 1000, z, entry.fixture_type, devicesRef.get(deviceId));
  });
  slider.addEventListener('change', () => {
    pushUndo();
    postPosition(deviceId, entry.x, entry.y, entry.z, entry.fixture_type);
  });
  pop.appendChild(slider);

  // Remove button
  const removeBtn = document.createElement('button');
  removeBtn.className = 'layout-popover-remove';
  removeBtn.textContent = 'Remove from canvas';
  removeBtn.addEventListener('click', () => {
    pushUndo();
    removeBulb(deviceId);
    dismissPopover();
  });
  pop.appendChild(removeBtn);

  // Position popover near the bulb using proper SVG coordinates
  const svg = document.getElementById('layout-canvas');
  const pt = svg.createSVGPoint();
  pt.x = entry.x * 1000;
  pt.y = entry.y * 1000;
  const screenPt = pt.matrixTransform(svg.getScreenCTM());
  const cx = screenPt.x;
  const cy = screenPt.y;
  pop.style.left = `${Math.min(cx + 30, window.innerWidth - 220)}px`;
  pop.style.top = `${Math.min(cy - 20, window.innerHeight - 260)}px`;

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

    x = snap(x); y = snap(y);
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
    o.x_norm = Math.min(1 - o.width_norm / 2 - 0.02, Math.max(o.width_norm / 2 + 0.02, snap(coord)));
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
      o.width_norm = Math.min(0.96, Math.max(0.05, snap(newWidth)));
      o.x_norm    = Math.min(0.97, Math.max(0.03, snap(newStart + o.width_norm / 2)));
    } else {
      const oldStart = startXNorm - startWidthNorm / 2;
      const newEnd = Math.min(0.98, Math.max(oldStart + 0.05, startXNorm + startWidthNorm / 2 + delta));
      const newWidth = Math.max(0.05, newEnd - oldStart);
      o.width_norm = Math.min(0.96, Math.max(0.05, snap(newWidth)));
      o.x_norm    = Math.min(0.97, Math.max(0.03, snap(oldStart + o.width_norm / 2)));
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
  delete placedOpenings[id];
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
