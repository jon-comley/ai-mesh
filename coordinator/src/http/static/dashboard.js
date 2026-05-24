// ── Service worker registration ─────────────────────────────────────────────
if ('serviceWorker' in navigator) {
  navigator.serviceWorker.register('/service-worker.js').catch(err => {
    console.warn('Service worker registration failed:', err);
  });
}

// ── Tab switching (mobile) ──────────────────────────────────────────────────
const tabs = document.querySelectorAll('.tab');
const panels = document.querySelectorAll('.panel');

tabs.forEach(tab => {
  tab.addEventListener('click', () => {
    const target = tab.dataset.panel;
    tabs.forEach(t => {
      t.classList.toggle('active', t.dataset.panel === target);
      t.setAttribute('aria-selected', t.dataset.panel === target ? 'true' : 'false');
    });
    panels.forEach(p => {
      p.classList.toggle('active', p.id === `panel-${target}`);
    });
  });
});

// ── Connection status dot ───────────────────────────────────────────────────
const connDot = document.getElementById('conn-dot');

function setConnState(state) {
  connDot.className = `conn-dot ${state}`;
  connDot.title = { connected: 'Connected', connecting: 'Connecting…', disconnected: 'Disconnected' }[state] ?? state;
}

// ── WebSocket (Phase B — placeholder) ──────────────────────────────────────
// The WS connection and DashboardEvent dispatch are wired up in Phase B.
// For Phase A the dot stays red (disconnected) to indicate the feature is pending.
setConnState('disconnected');
