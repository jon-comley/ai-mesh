import { describe, it, expect, beforeEach, vi } from 'vitest';
import { init, showGroup, showPanel, currentPanel, trackNew, GROUPS } from '/static/nav.js';

// The bottom bar's categories and the sub-tab strip, against the same panel
// markup index.html has.
function mount() {
  const panels = GROUPS.flatMap(g => g.panels.map(([p]) => p));
  document.body.innerHTML = `
    <nav class="subtab-bar" id="subtab-bar" hidden></nav>
    <main class="panels">${panels.map(p => `<section class="panel" id="panel-${p}"></section>`).join('')}</main>
    <nav class="tab-bar">${GROUPS.map(g => `<button class="tab" data-group="${g.id}">${g.label}</button>`).join('')}</nav>`;
  init({ tabs: document.querySelector('.tab-bar'), subtabs: document.getElementById('subtab-bar') });
}

const active = () => [...document.querySelectorAll('.panels > .panel.active')].map(p => p.id);
const subtabs = () => [...document.querySelectorAll('.subtab')].map(b => b.textContent);
const tab = id => document.querySelector(`.tab[data-group="${id}"]`);

beforeEach(() => {
  localStorage.clear();
});

describe('navigation', () => {
  it('has five categories and opens on Home', () => {
    mount();
    expect(GROUPS.map(g => g.label)).toEqual(['Home', 'Chat', 'Music', 'Tools', 'System']);
    expect(active()).toEqual(['panel-home']);
    expect(tab('home').classList.contains('active')).toBe(true);
    expect(subtabs()).toEqual(['Home', 'Devices']);
  });

  it('places every panel in exactly one category', () => {
    const all = GROUPS.flatMap(g => g.panels.map(([p]) => p));
    expect(new Set(all).size).toBe(all.length);
    expect(all.sort()).toEqual(['chat', 'chords', 'devices', 'ebay', 'errors', 'gateway', 'health',
      'home', 'models', 'nodes', 'reaper', 'reviews', 'security'].sort());
  });

  it('shows a category\'s sub-tabs and switches panels with them', () => {
    mount();
    tab('system').click();
    expect(subtabs()).toEqual(['Nodes', 'Health', 'Models', 'Online AI', 'Security', 'Errors']);
    expect(active()).toEqual(['panel-nodes']);
    [...document.querySelectorAll('.subtab')].find(b => b.textContent === 'Errors').click();
    expect(active()).toEqual(['panel-errors']);
    expect(currentPanel()).toBe('errors');
  });

  it('hides the strip for a category with one panel', () => {
    mount();
    tab('chat').click();
    expect(document.getElementById('subtab-bar').hidden).toBe(true);
    expect(active()).toEqual(['panel-chat']);
  });

  it('reopens a category on the sub-tab it was left on', () => {
    mount();
    tab('music').click();
    expect(active()).toEqual(['panel-chords']);
    [...document.querySelectorAll('.subtab')].find(b => b.textContent === 'REAPER').click();
    tab('home').click();
    tab('music').click();
    expect(active()).toEqual(['panel-reaper']);
    // …and still opens on Home after a reload.
    mount();
    expect(active()).toEqual(['panel-home']);
  });

  it('announces the panel it shows', () => {
    mount();
    const seen = vi.fn();
    document.addEventListener('panelshown', e => seen(e.detail.panel), { once: true });
    showGroup('music');
    expect(seen).toHaveBeenCalledWith('chords');
  });

  it('badges System for a new error until the Errors tab is opened', () => {
    mount();
    trackNew('errors', [100]); // the first feed ever seen is history, not news
    expect(tab('system').classList.contains('has-badge')).toBe(false);

    trackNew('errors', [100, 200]);
    expect(tab('system').classList.contains('has-badge')).toBe(true);
    tab('system').click();
    expect(document.querySelector('.subtab[data-panel="errors"]').classList.contains('has-badge')).toBe(true);

    showPanel('errors');
    expect(tab('system').classList.contains('has-badge')).toBe(false);
    trackNew('errors', [100, 200]); // nothing newer than what was looked at
    showPanel('home');
    expect(tab('system').classList.contains('has-badge')).toBe(false);
  });

  it('does not badge a feed that is already on screen', () => {
    mount();
    trackNew('security', [1]);
    showPanel('security');
    trackNew('security', [1, 5]);
    expect(tab('system').classList.contains('has-badge')).toBe(false);
  });
});
