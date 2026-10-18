// ── Navigation: five categories on the bottom bar, sub-tabs within each ─────
// Thirteen tabs no longer fit a phone's bottom bar, so it holds categories
// and a strip under the header switches between a category's panels. The
// panels themselves are untouched — this only decides which one is shown.

/** Categories in bar order; each panel is [panel id, sub-tab label]. */
export const GROUPS = [
  { id: 'home', label: 'Home', panels: [['home', 'Home'], ['devices', 'Devices']] },
  { id: 'chat', label: 'Chat', panels: [['chat', 'Chat']] },
  { id: 'music', label: 'Music', panels: [['chords', 'Songs'], ['reaper', 'REAPER']] },
  { id: 'tools', label: 'Tools', panels: [['ebay', 'Hunts'], ['reviews', 'Reviews']] },
  {
    id: 'system', label: 'System', panels: [
      ['nodes', 'Nodes'], ['health', 'Health'], ['models', 'Models'],
      ['gateway', 'Online AI'], ['security', 'Security'], ['errors', 'Errors'],
    ],
  },
];

const FIRST_GROUP = 'home';

function load(key) {
  try { return localStorage.getItem(key); } catch { return null; }
}
function store(key, value) {
  try { localStorage.setItem(key, value); } catch { /* preference only */ }
}

export function groupOf(panel) {
  return GROUPS.find(g => g.panels.some(([p]) => p === panel));
}

let tabBar = null;
let subBar = null;
let current = null;              // panel id on screen
const badged = new Set();        // panel ids with something new in them
const newest = new Map();        // panel id → newest timestamp seen in its feed

export function currentPanel() {
  return current;
}

/** Wire up the bottom bar and the sub-tab strip, and open the first category. */
export function init({ tabs, subtabs }) {
  tabBar = tabs;
  subBar = subtabs;
  tabBar.querySelectorAll('.tab[data-group]').forEach(b =>
    b.addEventListener('click', () => showGroup(b.dataset.group)));
  showGroup(FIRST_GROUP);
}

/** Open a category on the sub-tab it was last left on. */
export function showGroup(id) {
  const group = GROUPS.find(g => g.id === id);
  if (!group) return;
  const remembered = load(`mesh-tab-${id}`);
  const panel = group.panels.some(([p]) => p === remembered) ? remembered : group.panels[0][0];
  showPanel(panel);
}

/** Show one panel, selecting its category and sub-tab. */
export function showPanel(panel) {
  const group = groupOf(panel);
  if (!group) return;
  current = panel;
  store(`mesh-tab-${group.id}`, panel);

  tabBar.querySelectorAll('.tab[data-group]').forEach(b => {
    const on = b.dataset.group === group.id;
    b.classList.toggle('active', on);
    b.setAttribute('aria-selected', String(on));
  });
  document.querySelectorAll('.panels > .panel').forEach(p =>
    p.classList.toggle('active', p.id === `panel-${panel}`));

  // Looking at a feed is what makes its news old.
  badged.delete(panel);
  if (newest.has(panel)) store(`mesh-seen-${panel}`, String(newest.get(panel)));

  renderSubtabs(group);
  paintBadges();
  document.dispatchEvent(new CustomEvent('panelshown', { detail: { panel } }));
}

function renderSubtabs(group) {
  subBar.innerHTML = '';
  subBar.hidden = group.panels.length < 2;
  if (subBar.hidden) return;
  for (const [panel, label] of group.panels) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'subtab' + (panel === current ? ' active' : '');
    b.dataset.panel = panel;
    b.setAttribute('role', 'tab');
    b.setAttribute('aria-selected', String(panel === current));
    b.setAttribute('aria-controls', `panel-${panel}`);
    b.textContent = label;
    b.addEventListener('click', () => showPanel(panel));
    subBar.appendChild(b);
  }
  // On a phone System's six don't all fit; keep the open one in view.
  subBar.querySelector('.subtab.active')?.scrollIntoView?.({ block: 'nearest', inline: 'nearest' });
}

function paintBadges() {
  tabBar.querySelectorAll('.tab[data-group]').forEach(b => {
    const group = GROUPS.find(g => g.id === b.dataset.group);
    b.classList.toggle('has-badge', group.panels.some(([p]) => badged.has(p)));
  });
  subBar.querySelectorAll('.subtab').forEach(b =>
    b.classList.toggle('has-badge', badged.has(b.dataset.panel)));
}

/**
 * Note the timestamps in a feed (errors, security events) and badge its panel
 * when one is newer than the last time it was looked at. The very first feed
 * a browser ever sees counts as already read, so history isn't news.
 */
export function trackNew(panel, timestamps) {
  if (!timestamps.length) return;
  const latest = Math.max(...timestamps);
  newest.set(panel, latest);
  const seen = load(`mesh-seen-${panel}`);
  if (seen === null || current === panel) {
    store(`mesh-seen-${panel}`, String(latest));
    return;
  }
  if (latest > Number(seen)) {
    badged.add(panel);
    paintBadges();
  }
}
