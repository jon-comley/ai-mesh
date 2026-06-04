// ── Sun-effect render models ────────────────────────────────────────────────────
// The interchangeable SVG models that draw daylight through the room's openings
// on the 2D layout canvas: cones, gradient cones, caustic patches, bright/parallel
// beams, soft beams, wall glow, and the sun-arc overlay. Each takes a target SVG
// layer + the current sun azimuth/elevation and paints into it. Split out of
// layout.js; the dispatcher (redrawLightEffect, which picks the active model)
// stays there.
//
// Reads room/openings via layoutstate.js and sunrise/sunset via solar.js. The
// `svgEl` element helper and the live `compassDeg`/`meshLat`/`meshLon` values
// come from layout.js via initSunModels() injection (svgEl is a plain helper;
// the rest are read through getters because they change at runtime) — so no
// import back into layout.js, no cycle.

import { layoutState, WALL_THICKNESS } from '/static/layoutstate.js';
import { todaySunriseSunset } from '/static/solar.js';

let _svgEl = () => null;
let _getCompassDeg = () => 0;
let _getMeshLat = () => 51.5074;
let _getMeshLon = () => -0.1278;
export function initSunModels({ svgEl, getCompassDeg, getMeshLat, getMeshLon }) {
  _svgEl = svgEl;
  _getCompassDeg = getCompassDeg;
  _getMeshLat = getMeshLat;
  _getMeshLon = getMeshLon;
}

// Shared geometry for all opening-based models.
function openingCtx(o, azimuth, elevation) {
  // Civil twilight threshold: below -6° there is no usable solar illumination.
  if (elevation <= -6) return null;
  const compassDeg = _getCompassDeg();
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
export function renderConesModel(layer, azimuth, elevation) {
  if (!layer) return;
  for (const [id, o] of Object.entries(layoutState.openings)) {
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
    layer.appendChild(_svgEl('polygon', {
      points: `${ox.toFixed(1)},${oy.toFixed(1)} ${lx.toFixed(1)},${ly.toFixed(1)} ${rx.toFixed(1)},${ry.toFixed(1)}`,
      fill: `rgba(255,220,100,${op.toFixed(3)})`, 'pointer-events': 'none',
    }));
  }
}

// ── Model 2: Gradient cone ────────────────────────────────────────────────────
export function renderGradientConesModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  for (const [id, o] of Object.entries(layoutState.openings)) {
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
    layer.appendChild(_svgEl('polygon', {
      points: `${ox.toFixed(1)},${oy.toFixed(1)} ${lx.toFixed(1)},${ly.toFixed(1)} ${rx.toFixed(1)},${ry.toFixed(1)}`,
      fill: `url(#${gid})`, 'pointer-events': 'none',
    }));
  }
}

// ── Model 3: Caustic patch ────────────────────────────────────────────────────
export function renderCausticModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  for (const [id, o] of Object.entries(layoutState.openings)) {
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
export function renderBrightPatchModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  for (const [id, o] of Object.entries(layoutState.openings)) {
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
    layer.appendChild(_svgEl('polygon', {
      points: `${x1.toFixed(1)},${y1.toFixed(1)} ${x2.toFixed(1)},${y2.toFixed(1)} ${(x2+dx).toFixed(1)},${(y2+dy).toFixed(1)} ${(x1+dx).toFixed(1)},${(y1+dy).toFixed(1)}`,
      fill: `url(#${gid})`, 'pointer-events': 'none',
    }));
  }
}

// ── Model 5: Parallel beam (physically correct — window width along wall axis) ─
export function renderParallelBeamModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  for (const [id, o] of Object.entries(layoutState.openings)) {
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
    layer.appendChild(_svgEl('polygon', {
      points: `${ax.toFixed(1)},${ay.toFixed(1)} ${bx.toFixed(1)},${by.toFixed(1)} ${(bx+bdx).toFixed(1)},${(by+bdy).toFixed(1)} ${(ax+bdx).toFixed(1)},${(ay+bdy).toFixed(1)}`,
      fill: `url(#${gid})`, 'pointer-events': 'none',
    }));
  }
}

// ── Model 6: Beam + footprint (parallel beam + bright landing patch) ──────────
export function renderBeamFootprintModel(layer, azimuth, elevation) {
  if (!layer) return;
  const defs = layerDefs(layer);
  for (const [id, o] of Object.entries(layoutState.openings)) {
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
    layer.appendChild(_svgEl('polygon', {
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
export function renderSoftBeamModel(layer, azimuth, elevation) {
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

  for (const [id, o] of Object.entries(layoutState.openings)) {
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
    layer.appendChild(_svgEl('polygon', {
      points: `${ax.toFixed(1)},${ay.toFixed(1)} ${bx.toFixed(1)},${by.toFixed(1)} ${(bx+bdx).toFixed(1)},${(by+bdy).toFixed(1)} ${(ax+bdx).toFixed(1)},${(ay+bdy).toFixed(1)}`,
      fill: `url(#${gid})`, filter: `url(#${fid})`, 'pointer-events': 'none',
    }));
  }
}

// ── Model 8: Wall glow ────────────────────────────────────────────────────────
export function renderWallGlowModel(layer, azimuth, elevation) {
  if (!layer || elevation <= -6) return;
  const compassDeg = _getCompassDeg();
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

// ── Model 9: Sun arc ──────────────────────────────────────────────────────────
export function renderSunArcModel(layer, azimuth, elevation) {
  if (!layer) return;
  const compassDeg = _getCompassDeg();
  const { sunriseAz, sunsetAz, polarDay, polarNight } = todaySunriseSunset(_getMeshLat(), _getMeshLon());
  if (polarNight) return;
  const R = 490;
  const isDaytime = elevation > 0;

  const azPt = az => {
    const adj = ((az - compassDeg) % 360 + 360) % 360;
    const r   = (adj - 90) * Math.PI / 180;
    return { x: 500 + R * Math.cos(r), y: 500 + R * Math.sin(r) };
  };

  if (polarDay) {
    layer.appendChild(_svgEl('circle', { cx: 500, cy: 500, r: R,
      stroke: 'rgba(255,200,50,0.4)', 'stroke-width': 3, fill: 'none', 'pointer-events': 'none' }));
  } else {
    const rPt = azPt(sunriseAz), sPt = azPt(sunsetAz);
    layer.appendChild(_svgEl('path', {
      d: `M ${rPt.x.toFixed(1)} ${rPt.y.toFixed(1)} A ${R} ${R} 0 1 1 ${sPt.x.toFixed(1)} ${sPt.y.toFixed(1)}`,
      stroke: isDaytime ? 'rgba(255,200,50,0.65)' : 'rgba(255,200,50,0.2)',
      'stroke-width': 3, fill: 'none',
      'stroke-dasharray': isDaytime ? 'none' : '8 6',
      'pointer-events': 'none',
    }));
    layer.appendChild(_svgEl('circle', { cx: rPt.x.toFixed(1), cy: rPt.y.toFixed(1), r: 6,
      fill: 'rgba(255,160,50,0.85)', 'pointer-events': 'none' }));
    layer.appendChild(_svgEl('circle', { cx: sPt.x.toFixed(1), cy: sPt.y.toFixed(1), r: 6,
      fill: 'rgba(255,80,50,0.85)',  'pointer-events': 'none' }));
  }

  if (isDaytime) {
    const cp  = azPt(azimuth);
    const dot = _svgEl('circle', { cx: cp.x.toFixed(1), cy: cp.y.toFixed(1), r: 10,
      fill: '#FFD700', 'pointer-events': 'none' });
    dot.classList.add('lc-sun-dot');
    const lbl = _svgEl('text', { x: cp.x.toFixed(1), y: (cp.y - 18).toFixed(1),
      'text-anchor': 'middle', 'font-size': 18, fill: '#FFD700', 'pointer-events': 'none' });
    lbl.textContent = '☀';
    layer.appendChild(dot);
    layer.appendChild(lbl);
  }
}
