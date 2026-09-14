import { describe, it, expect, vi, beforeEach } from 'vitest';

// `init` is the only way in — sortFinds and the ticker renderer are private,
// and testing them through the panel they actually build is the point: the
// three bugs this file covers were all "the DOM does not have the thing in it".
const apiCalls = [];
let hunts, finds, runNowResolve;

vi.mock('/static/api.js', () => ({
  api: (path, opts) => {
    apiCalls.push({ path, opts });
    if (path === '/ebay/hunts') return Promise.resolve(json(hunts));
    if (path === '/ebay/finds') return Promise.resolve(json(finds));
    if (path === '/ebay/config') return Promise.resolve(json({ ntfy_topic: '' }));
    if (path.endsWith('/run-now')) return new Promise(r => { runNowResolve = () => r(json({ new_listings: 2 })); });
    if (path.endsWith('/reviewed')) return Promise.resolve(json({}));
    return Promise.resolve(json({}));
  },
}));
vi.mock('/static/util.js', () => ({ showToast: vi.fn() }));

function json(body) {
  return { ok: true, json: () => Promise.resolve(body), text: () => Promise.resolve('') };
}

const { init } = await import('/static/ebay.js');

// Fixed timestamps so the assertions are about order, not about "now".
const DAY = 86_400_000;
const T = Date.UTC(2026, 8, 14, 6, 0); // 14 Sep 2026, 06:00 UTC

function find(id, { ageDays = 0, reviewed = false, verdict = null } = {}) {
  return {
    id, hunt_id: 'h1', item_id: `item-${id}`, title: `Find ${id}`,
    price_minor: 12_345, currency: 'GBP', image_url: null,
    item_web_url: `https://example.test/${id}`, matched_term: 'thinkcentre',
    verdict, found_ms: T - ageDays * DAY, reviewed,
  };
}

// init() fires three un-awaited fetches; let their promise chains drain.
const settle = () => new Promise(r => setTimeout(r, 0));

async function mount() {
  const panel = document.createElement('div');
  document.body.append(panel);
  init(panel);
  await settle();
  return panel;
}

beforeEach(() => {
  document.body.innerHTML = '';
  apiCalls.length = 0;
  hunts = [{ id: 'h1', name: 'M920q', enabled: true, terms: [], timeslots: [] }];
  finds = [];
});

describe('hunt sidebar', () => {
  it('gives every hunt its own run button without opening the editor', async () => {
    hunts = [
      { id: 'h1', name: 'M920q', enabled: true, terms: [], timeslots: [] },
      { id: 'h2', name: 'Riser', enabled: false, terms: [], timeslots: [] },
    ];
    const panel = await mount();
    const runs = panel.querySelectorAll('.ebay-hunt-run');
    expect(runs).toHaveLength(2);
    expect([...runs].map(b => b.dataset.id)).toEqual(['h1', 'h2']);
    // The editor is still closed — the run control is reachable from the list.
    expect(panel.querySelector('#ebay-editor').hidden).toBe(true);
  });

  // The bug: a button cannot contain a button, so the old single-<button> row
  // could never have held this one.
  it('does not nest the run button inside another button', async () => {
    const panel = await mount();
    expect(panel.querySelector('button button')).toBeNull();
  });

  it('shows a held busy state for as long as the check is running', async () => {
    const panel = await mount();
    const btn = panel.querySelector('.ebay-hunt-run');
    expect(btn.disabled).toBe(false);

    btn.click();
    await settle();
    // Still in flight: this is the window that previously looked like nothing
    // had happened at all.
    expect(btn.disabled).toBe(true);
    expect(btn.classList.contains('is-busy')).toBe(true);
    expect(btn.textContent).toBe('Checking…');

    runNowResolve();
    await settle();
    expect(btn.disabled).toBe(false);
    expect(btn.classList.contains('is-busy')).toBe(false);
    expect(btn.textContent).toBe('Run');
  });

  it('posts run-now for the hunt whose button was pressed', async () => {
    hunts = [
      { id: 'h1', name: 'M920q', enabled: true, terms: [], timeslots: [] },
      { id: 'h2', name: 'Riser', enabled: true, terms: [], timeslots: [] },
    ];
    const panel = await mount();
    panel.querySelectorAll('.ebay-hunt-run')[1].click();
    await settle();
    expect(apiCalls.some(c => c.path === '/ebay/hunts/h2/run-now' && c.opts.method === 'POST')).toBe(true);
    runNowResolve();
  });
});

describe('find ticker', () => {
  const titles = panel => [...panel.querySelectorAll('.ebay-find-body a')].map(a => a.textContent);

  it('puts the newest find first regardless of verdict', async () => {
    // The old ordering would have floated the 5-day-old bargain to the top.
    finds = [
      find('old-bargain', { ageDays: 5, verdict: 'bargain: mis-listed' }),
      find('newest', { ageDays: 0 }),
      find('middle', { ageDays: 2, verdict: 'not a bargain: fairly priced' }),
    ];
    const panel = await mount();
    expect(titles(panel)).toEqual(['Find newest', 'Find middle', 'Find old-bargain']);
  });

  it('keeps already-dismissed finds at the bottom', async () => {
    finds = [
      find('dismissed-newest', { ageDays: 0, reviewed: true }),
      find('live-oldest', { ageDays: 9 }),
    ];
    const panel = await mount();
    expect(titles(panel)).toEqual(['Find live-oldest', 'Find dismissed-newest']);
  });

  it('sinks a find to the bottom on the press, not on the next reload', async () => {
    finds = [find('a', { ageDays: 0 }), find('b', { ageDays: 1 }), find('c', { ageDays: 2 })];
    const panel = await mount();
    expect(titles(panel)).toEqual(['Find a', 'Find b', 'Find c']);

    panel.querySelector('.ebay-dismiss').click(); // "Find a"
    await settle();
    expect(titles(panel)).toEqual(['Find b', 'Find c', 'Find a']);
    expect(panel.querySelectorAll('.ebay-find')[2].classList.contains('ebay-find-reviewed')).toBe(true);
  });

  it('dates every find so the latest is tellable at a glance', async () => {
    finds = [find('a', { ageDays: 0 }), find('b', { ageDays: 3 })];
    const panel = await mount();
    const whens = [...panel.querySelectorAll('.ebay-find-when')];
    expect(whens).toHaveLength(2);
    expect(whens[0].textContent).toMatch(/14 Sep|Sep 14/);
    expect(whens[1].textContent).toMatch(/11 Sep|Sep 11/);
    expect(whens[0].getAttribute('datetime')).toBe(new Date(T).toISOString());
  });

  it('says so rather than rendering Invalid Date when a timestamp is missing', async () => {
    finds = [{ ...find('a'), found_ms: null }];
    const panel = await mount();
    expect(panel.querySelector('.ebay-find-when').textContent).toBe('date unknown');
  });

  // Bargains lost their position in the sort, so the badge is now the only
  // thing carrying that verdict at a glance.
  it('badges a bargain in place', async () => {
    finds = [find('a', { verdict: 'bargain: far below typical' }), find('b', { ageDays: 1 })];
    const panel = await mount();
    const rows = panel.querySelectorAll('.ebay-find');
    expect(rows[0].classList.contains('ebay-find-bargain')).toBe(true);
    expect(rows[0].querySelector('.ebay-badge').textContent).toBe('bargain');
    expect(rows[1].querySelector('.ebay-badge')).toBeNull();
  });

  it('does not badge "not a bargain" on the prefix', async () => {
    finds = [find('a', { verdict: 'not a bargain: fairly priced' })];
    const panel = await mount();
    expect(panel.querySelector('.ebay-badge')).toBeNull();
    expect(panel.querySelector('.ebay-find').classList.contains('ebay-find-bargain')).toBe(false);
  });
});
