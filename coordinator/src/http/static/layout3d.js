// ── Three.js 3D layout view ────────────────────────────────────────────────────
// The 3D visualisation of the room: a WebGL scene (floor/walls/grid), a bulb
// sphere + point light per placed fixture, opening planes, a sun directional
// light, and a colour-mixed ambient fill. View-only — all layout editing happens
// in the 2D SVG view; here a bulb tap opens its popover and orbit controls handle
// rotate/zoom. Split out of layout.js.
//
// Reads the shared canvas state via layoutstate.js. The handful of core helpers
// it needs (popover open/close, sidebar close, device→colour) are injected via
// initLayout3d() rather than imported, so the graph stays one-directional
// (layout.js → layout3d.js → layoutstate.js, no cycle). Three.js itself is
// dynamically imported on first use (resolved through the page import map).

import { layoutState } from '/static/layoutstate.js';

// ── Module state ───────────────────────────────────────────────────────────────
let THREE = null;
let ThreeOrbitControls = null;
let threeRenderer = null;
let threeScene = null;
let threeRoomGroup = null;
let threePerspCamera = null;
let threeControls = null;
let threeSunLight = null;
let threeAmbientHemi = null;  // HemisphereLight tinted by the mix of on-bulb colours
let threeBulbMeshes = {};     // deviceId → { mesh, ptLight, mat }
let threeOpeningMeshes = {};  // openingId → mesh
let threeNeedsRender = false; // on-demand rendering — only draw when something changed
let threeIs3D = false;
let threeAnimFrameId = null;
let threeRaycaster = null;
let threeFloorPlane = null;   // THREE.Plane at y=0

// Core helpers injected by layout.js (avoids importing layout.js back).
let _openPopover = () => {};
let _dismissPopover = () => {};
let _closeSidebarSheet = () => {};
let _devStateColor = () => '#111133';
export function initLayout3d({ openPopover, dismissPopover, closeSidebarSheet, devStateColor }) {
  _openPopover = openPopover;
  _dismissPopover = dismissPopover;
  _closeSidebarSheet = closeSidebarSheet;
  _devStateColor = devStateColor;
}

// Whether the 3D view is currently active (read by layout.js core).
export function is3DActive() { return threeIs3D; }

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

export async function initThree(room, lastSolar) {
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
    // Mid-tone floor (not near-black) so it actually catches and shows the
    // light the bulbs throw onto it.
    new THREE.MeshStandardMaterial({ color: 0x3a3a52, roughness: 0.85 })
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

  // Lighting. A small permanent neutral fill keeps an all-off room from going
  // pitch black; on top sits a HemisphereLight tinted by the mix of the bulbs
  // that are on (see recomputeRoomAmbient) so the room gently takes on the mood
  // of its lights without a saturated wash.
  threeScene.add(new THREE.AmbientLight(0x202028, 0.12));
  threeAmbientHemi = new THREE.HemisphereLight(0x202028, 0x101018, 0.0);
  threeScene.add(threeAmbientHemi);
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
  for (const [id, entry] of Object.entries(layoutState.bulbs)) {
    syncBulbToThree(id, entry, layoutState.devices.get(id));
  }
  recomputeRoomAmbient();

  // Sync all already-placed openings
  for (const o of Object.values(layoutState.openings)) {
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

export function teardownThree() {
  if (threeAnimFrameId) { cancelAnimationFrame(threeAnimFrameId); threeAnimFrameId = null; }
  if (threeControls) { threeControls.dispose(); threeControls = null; }
  if (threeRenderer) { threeRenderer.dispose(); threeRenderer.domElement.remove(); threeRenderer = null; }
  const c = document.getElementById('lc-3d-container');
  if (c?._ro) { c._ro.disconnect(); delete c._ro; }
  threeScene = null; threeRoomGroup = null; threePerspCamera = null;
  threeSunLight = null; threeAmbientHemi = null; threeBulbMeshes = {}; threeOpeningMeshes = {};
  threeIs3D = false;
  threeRaycaster = null; threeFloorPlane = null;
}

export function syncBulbToThree(deviceId, entry, dev) {
  if (!threeScene || !THREE || !threeRoomGroup || !layoutState.room) return;
  removeBulbFromThree(deviceId);

  const W = layoutState.room.width_m  || 3;
  const D = layoutState.room.depth_m  || 6;
  const H = layoutState.room.height_m || 2.5;

  const x3 = (entry.x - 0.5) * W;
  const y3 = (entry.z ?? 0.9) * H;
  const z3 = (entry.y - 0.5) * D;

  const colorStr = dev ? _devStateColor(dev) : '#111133';
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

  // Each bulb throws a real pool of light into the room. Generous intensity +
  // reach (decay kept low) so the fixtures visibly shine onto the floor and
  // walls; the room-wide colour mix is layered on top by recomputeRoomAmbient.
  const ptLight = new THREE.PointLight(color, bri * 18, W * 4, 1.4);
  ptLight.castShadow = false;
  mesh.add(ptLight);

  threeRoomGroup.add(mesh);
  threeBulbMeshes[deviceId] = { mesh, ptLight, mat };
  recomputeRoomAmbient();
  threeMarkDirty();
}

function removeBulbFromThree(deviceId) {
  const b = threeBulbMeshes[deviceId];
  if (!b) return;
  b.mesh.removeFromParent();
  b.mat.dispose();
  delete threeBulbMeshes[deviceId];
  recomputeRoomAmbient();
  threeMarkDirty();
}

export function syncOpeningToThree(o) {
  if (!threeScene || !THREE || !threeRoomGroup || !layoutState.room) return;
  removeOpeningFromThree(o.id);

  const W = layoutState.room.width_m  || 3;
  const D = layoutState.room.depth_m  || 6;
  const H = layoutState.room.height_m || 2.5;
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

export function removeOpeningFromThree(id) {
  const mesh = threeOpeningMeshes[id];
  if (!mesh) return;
  mesh.material.dispose();
  mesh.geometry.dispose();
  mesh.removeFromParent();
  delete threeOpeningMeshes[id];
  threeMarkDirty();
}

function threeMarkDirty() { threeNeedsRender = true; }

export function threeUpdateSun(azimuth, elevation) {
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

export function threeUpdateBulbColor(deviceId, dev) {
  const b = threeBulbMeshes[deviceId];
  if (!b || !THREE) return;
  const colorStr = dev ? _devStateColor(dev) : '#111133';
  const color = new THREE.Color(colorStr);
  const bri = dev?.on ? (dev.brightness ?? 200) / 254 : 0;
  b.mat.emissive.set(color);
  b.mat.emissiveIntensity = Math.max(bri, 0.1);
  b.ptLight.color.set(color);
  b.ptLight.intensity = bri * 18;
  recomputeRoomAmbient();
  threeMarkDirty();
}

// Gently fill the room with the MIXTURE of every on-bulb's colour: a
// brightness-weighted average of the bulb colours, desaturated toward white so
// a saturated bulb only faintly tints the room (realistic light bleed, not a
// disco wash). Drives the HemisphereLight; an all-off room falls back to the
// dim neutral floor so it reads as "off", not black.
function recomputeRoomAmbient() {
  if (!threeAmbientHemi || !THREE) return;
  let r = 0, g = 0, b = 0, wSum = 0;
  for (const id of Object.keys(threeBulbMeshes)) {
    const dev = layoutState.devices.get(id);
    if (!dev || !dev.on) continue;
    const c = new THREE.Color(_devStateColor(dev));
    const w = (dev.brightness ?? 200) / 254;
    r += c.r * w; g += c.g * w; b += c.b * w; wSum += w;
  }
  if (wSum <= 0) {
    threeAmbientHemi.color.setHex(0x202028);
    threeAmbientHemi.groundColor.setHex(0x101018);
    threeAmbientHemi.intensity = 0.0;
    return;
  }
  const mix = new THREE.Color(r / wSum, g / wSum, b / wSum);
  // Desaturate a little toward white so the tint reads as light, not paint, but
  // keep enough colour that the room visibly takes on the bulbs' hue.
  const hsl = { h: 0, s: 0, l: 0 };
  mix.getHSL(hsl);
  const sky = new THREE.Color().setHSL(hsl.h, hsl.s * 0.6, Math.max(hsl.l, 0.5));
  const ground = new THREE.Color().setHSL(hsl.h, hsl.s * 0.6, Math.max(hsl.l, 0.5) * 0.5);
  threeAmbientHemi.color.copy(sky);
  threeAmbientHemi.groundColor.copy(ground);
  // Ramp the room-fill up with total light so a lit room clearly glows.
  threeAmbientHemi.intensity = Math.min(2.2, 0.4 + wSum * 0.5);
}

// Switch the layout view to/from 3D. `is-3d` on the view drives CSS — hiding the
// editing-only controls, the device sidebar, and the 2D-only light-model select
// (3D is purely for visualising). The SVG/WebGL swap stays explicit here.
function toggle3D() {
  threeIs3D = !threeIs3D;
  const svg  = document.getElementById('layout-canvas');
  const c3d  = document.getElementById('lc-3d-container');
  const view = document.querySelector('.layout-view');
  view?.classList.toggle('is-3d', threeIs3D);
  if (threeIs3D) {
    _dismissPopover();
    _closeSidebarSheet();
    if (svg) svg.style.display = 'none';
    if (c3d) c3d.style.display = '';
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
    for (const o of Object.values(layoutState.openings)) {
      syncOpeningToThree(o);
    }
  } else {
    if (svg) svg.style.display = '';
    if (c3d) c3d.style.display = 'none';
  }
  threeMarkDirty();
}

// Drive the segmented 2D/3D control: flip the view if needed and sync the active
// pill regardless (so a programmatic call keeps the UI in step).
export function setView3D(want3D, seg2d, seg3d) {
  if (want3D !== threeIs3D) toggle3D();
  seg2d?.classList.toggle('active', !threeIs3D);
  seg3d?.classList.toggle('active', threeIs3D);
}

// ── Raycaster interactions ─────────────────────────────────────────────────────

function threeMouseNDC(e, el) {
  const r = el.getBoundingClientRect();
  return new THREE.Vector2(
    ((e.clientX - r.left) / r.width)  *  2 - 1,
    ((e.clientY - r.top)  / r.height) * -2 + 1,
  );
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
    _dismissPopover();

    const ndc = threeMouseNDC(e, el);
    threeRaycaster.setFromCamera(ndc, threePerspCamera);

    // Bulb tap → open popover (no drag)
    const bulbMeshList = Object.values(threeBulbMeshes).map(b => b.mesh);
    const bulbHits = threeRaycaster.intersectObjects(bulbMeshList);
    if (bulbHits.length > 0) {
      e.stopPropagation();
      const deviceId = bulbHits[0].object.userData.deviceId;
      _openPopover(deviceId, null, e.clientX, e.clientY);
      return;
    }

    // Openings are view-only in 3D — adding/removing/editing them happens in the
    // 2D view, so a tap on an opening here does nothing (orbit controls handle it).
  });
}
