import * as topology from '/static/topology.js';
import * as health from '/static/health.js';
import * as models from '/static/models.js';
import * as lighting from '/static/lighting.js';
import * as rooms from '/static/rooms.js';

// ── Service worker ──────────────────────────────────────────────────────────
if ('serviceWorker' in navigator) {
  navigator.serviceWorker.register('/service-worker.js').catch(err =>
    console.warn('Service worker registration failed:', err)
  );
}

// ── Tab switching ───────────────────────────────────────────────────────────
const tabs = document.querySelectorAll('.tab');
const panels = document.querySelectorAll('.panel');

tabs.forEach(tab => {
  tab.addEventListener('click', () => {
    const target = tab.dataset.panel;
    tabs.forEach(t => {
      t.classList.toggle('active', t.dataset.panel === target);
      t.setAttribute('aria-selected', t.dataset.panel === target ? 'true' : 'false');
    });
    panels.forEach(p => p.classList.toggle('active', p.id === `panel-${target}`));
  });
});

// ── Token management ────────────────────────────────────────────────────────
function getToken() {
  return localStorage.getItem('meshToken') ?? '';
}

function promptToken() {
  const existing = getToken();
  const entered = window.prompt('Enter MESH_AUTH_TOKEN (leave blank if auth is disabled):', existing);
  if (entered !== null) {
    localStorage.setItem('meshToken', entered.trim());
    return entered.trim();
  }
  return existing;
}

// ── Connection status ───────────────────────────────────────────────────────
const connDot = document.getElementById('conn-dot');

function setConnState(state) {
  connDot.className = `conn-dot ${state}`;
  connDot.title = { connected: 'Connected', connecting: 'Connecting…', disconnected: 'Disconnected' }[state] ?? state;
}

// ── Event dispatch ──────────────────────────────────────────────────────────
const handlers = {
  TopologyUpdate: evt => {
    topology.handleEvent(evt);
    health.handleTopology(evt.nodes);
  },
  HealthUpdate: evt => {
    health.handleHealth(evt);
    models.repaintModels();
  },
  ModelUpdate: evt => models.handleModelUpdate(evt),
  LightingUpdate: evt => {
    lighting.handleLightingUpdate(evt);
    rooms.notifyDevices(evt.devices);
  },
  RoomsUpdate: evt => rooms.handleRoomsUpdate(evt),
  ScenesUpdate: evt => rooms.handleScenesUpdate(evt),
  SolarUpdate: evt => rooms.notifySolar(evt.azimuth, evt.elevation),
};

function dispatch(evt) {
  const handler = handlers[evt.type];
  if (handler) handler(evt);
  else console.debug('unhandled dashboard event:', evt.type);
}

// ── WebSocket client ────────────────────────────────────────────────────────
let ws = null;
let reconnectTimer = null;

function connect() {
  if (ws && ws.readyState < WebSocket.CLOSING) return;

  const token = getToken();
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const url = `${proto}://${location.host}/ws?token=${encodeURIComponent(token)}`;

  setConnState('connecting');
  ws = new WebSocket(url);

  ws.onopen = () => {
    setConnState('connected');
    if (reconnectTimer) { clearTimeout(reconnectTimer); reconnectTimer = null; }
  };

  ws.onmessage = ({ data }) => {
    try { dispatch(JSON.parse(data)); }
    catch (e) { console.warn('bad WS message', e); }
  };

  ws.onclose = evt => {
    setConnState('disconnected');
    // 4001 = auth rejected — prompt for a new token before reconnecting.
    if (evt.code === 4001) {
      promptToken();
    }
    reconnectTimer = setTimeout(connect, 3000);
  };

  ws.onerror = () => setConnState('disconnected');
}

// ── Init ────────────────────────────────────────────────────────────────────
topology.init(document.getElementById('node-list'));
lighting.setRoomsActive();

// Ask for token on very first visit when auth is likely needed.
if (!localStorage.getItem('meshToken')) {
  promptToken();
}

connect();

// Tap the connection dot to re-enter the token.
connDot.style.cursor = 'pointer';
connDot.addEventListener('click', () => { promptToken(); connect(); });
