import { describe, it, expect, vi, beforeEach } from 'vitest';

// `init` is the only way in — sortFinds and the ticker renderer are private,
// and testing them through the panel they actually build is the point: the
// three bugs this file covers were all "the DOM does not have the thing in it".
const apiCalls = [];
let hunts, finds, runNowResolve, rankReply;

vi.mock('/static/api.js', () => ({
  api: (path, opts) => {
    apiCalls.push({ path, opts });
    if (path === '/ebay/hunts') return Promise.resolve(json(hunts));
    if (path === '/ebay/finds') return Promise.resolve(json(finds));
    if (path === '/ebay/config') return Promise.resolve(json({ ntfy_topic: '' }));
    if (path.endsWith('/run-now')) return new Promise(r => { runNowResolve = () => r(json({ new_listings: 2 })); });
    if (path.endsWith('/rank')) return Promise.resolve(json(rankReply));
    if (path.endsWith('/reviewed')) return Promise.resolve(json({}));
    return Promise.resolve(json({}));
  },
}));
vi.mock('/static/util.js', () => ({ showToast: vi.fn() }));

function json(body) {
  return { ok: true, json: () => Promise.resolve(body), text: () => Promise.resolve('') };
}

const { init, keywordCoverage } = await import('/static/ebay.js');

// Fixed timestamps so the assertions are about order, not about "now".
const DAY = 86_400_000;
const T = Date.UTC(2026, 8, 14, 6, 0); // 14 Sep 2026, 06:00 UTC

function find(id, { ageDays = 0, reviewed = false, verdict = null, title = null, term = 'thinkcentre', score = null } = {}) {
  return {
    id, hunt_id: 'h1', item_id: `item-${id}`, title: title ?? `Find ${id}`,
    price_minor: 12_345, currency: 'GBP', image_url: null,
    item_web_url: `https://example.test/${id}`, matched_term: term,
    verdict, score, found_ms: T - ageDays * DAY, reviewed,
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
  hunts = [{ id: 'h1', name: 'M920q', enabled: true, terms: [], timeslots: [], goal: 'headless CI runner' }];
  finds = [];
  rankReply = { scored: 2, considered: 2, refresh_error: null };
  window.confirm = () => true;
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

describe('keyword matching', () => {
  it('scores a title by how much of the matched term it carries', () => {
    const term = 'Lenovo ThinkCentre M920q i5 8500T';
    expect(keywordCoverage({ title: 'Lenovo ThinkCentre M920q i5 8500T Tiny', matched_term: term })).toBe(1);
    // Punctuation must not count against it: "i5-8500T" is the same machine.
    expect(keywordCoverage({ title: 'Lenovo ThinkCentre M920q, i5-8500T!', matched_term: term })).toBe(1);
    // Word order in an eBay title means nothing.
    expect(keywordCoverage({ title: '8500T i5 M920q ThinkCentre Lenovo', matched_term: term })).toBe(1);
    // 5 keywords in the term; the title carries Lenovo and ThinkCentre only.
    expect(keywordCoverage({ title: 'Lenovo ThinkCentre M710q i3', matched_term: term })).toBeCloseTo(2 / 5);
    expect(keywordCoverage({ title: 'Dell Optiplex', matched_term: term })).toBe(0);
  });

  it('scores 0 rather than dividing by zero on an empty term', () => {
    expect(keywordCoverage({ title: 'anything', matched_term: '' })).toBe(0);
    expect(keywordCoverage({})).toBe(0);
  });

  it('floats an exact keyword match above newer, looser ones', async () => {
    const term = 'ThinkCentre M920q 8500T';
    finds = [
      find('loose-newest', { ageDays: 0, title: 'ThinkCentre M710q i3', term }),
      find('exact-oldest', { ageDays: 6, title: 'Lenovo ThinkCentre M920q 8500T 16GB', term }),
      find('loose-older', { ageDays: 1, title: 'ThinkCentre M900 i5', term }),
    ];
    const panel = await mount();
    const titles = [...panel.querySelectorAll('.ebay-find-body a')].map(a => a.textContent);
    expect(titles[0]).toBe('Lenovo ThinkCentre M920q 8500T 16GB');
    // Everything below the exact tier stays newest-first.
    expect(titles.slice(1)).toEqual(['ThinkCentre M710q i3', 'ThinkCentre M900 i5']);
  });

  it('still keeps a dismissed exact match at the bottom', async () => {
    const term = 'ThinkCentre M920q';
    finds = [
      find('exact-dismissed', { ageDays: 0, title: 'Lenovo ThinkCentre M920q', term, reviewed: true }),
      find('live', { ageDays: 8, title: 'Dell Optiplex', term }),
    ];
    const panel = await mount();
    const titles = [...panel.querySelectorAll('.ebay-find-body a')].map(a => a.textContent);
    expect(titles).toEqual(['Dell Optiplex', 'Lenovo ThinkCentre M920q']);
  });

  it('badges an exact match and gives everything else its percentage', async () => {
    const term = 'ThinkCentre M920q 8500T';
    finds = [
      find('exact', { ageDays: 0, title: 'ThinkCentre M920q 8500T', term }),
      find('partial', { ageDays: 1, title: 'ThinkCentre M710q i3', term }),
    ];
    const panel = await mount();
    const rows = panel.querySelectorAll('.ebay-find');
    expect(rows[0].classList.contains('ebay-find-exact')).toBe(true);
    expect(rows[0].querySelector('.ebay-badge-exact').textContent).toBe('exact');
    expect(rows[0].querySelector('.ebay-match-pct')).toBeNull();
    expect(rows[1].classList.contains('ebay-find-exact')).toBe(false);
    expect(rows[1].querySelector('.ebay-match-pct').textContent).toBe('33% match');
  });
});

describe('deleting a hunt', () => {
  const openHunt = async panel => { panel.querySelector('.ebay-hunt-open').click(); await settle(); };

  it('clears the deleted hunt\'s finds from the ticker', async () => {
    finds = [find('a'), find('b')];
    const panel = await mount();
    expect(panel.querySelectorAll('.ebay-find-body').length).toBe(2);

    await openHunt(panel);
    // The server cascades the finds away with the hunt, so the reload sees none.
    finds = [];
    hunts = [];
    panel.querySelector('#ebay-delete').click();
    await settle();

    expect(apiCalls.some(c => c.path === '/ebay/hunts/h1' && c.opts.method === 'DELETE')).toBe(true);
    expect(panel.querySelectorAll('.ebay-find-body').length).toBe(0);
  });

  it('obeys a refusal at the confirm', async () => {
    window.confirm = () => false;
    const panel = await mount();
    await openHunt(panel);
    panel.querySelector('#ebay-delete').click();
    await settle();
    expect(apiCalls.some(c => c.opts?.method === 'DELETE')).toBe(false);
  });
});

describe('ranking', () => {
  const titles = panel => [...panel.querySelectorAll('.ebay-find-body a')].map(a => a.textContent);
  const openHunt = async panel => { panel.querySelector('.ebay-hunt-open').click(); await settle(); };

  it('carries the hunt goal into the editor and back out on save', async () => {
    const panel = await mount();
    await openHunt(panel);
    expect(panel.querySelector('#ebay-goal').value).toBe('headless CI runner');

    panel.querySelector('#ebay-goal').value = 'cores first, then RAM';
    panel.querySelector('#ebay-save').click();
    await settle();
    const patch = apiCalls.find(c => c.opts?.method === 'PATCH');
    expect(patch.opts.body.goal).toBe('cores first, then RAM');
  });

  it('posts rank and holds a busy state while it runs', async () => {
    const panel = await mount();
    await openHunt(panel);
    const btn = panel.querySelector('#ebay-rank');
    btn.click();
    await settle();
    expect(apiCalls.some(c => c.path === '/ebay/hunts/h1/rank' && c.opts.method === 'POST')).toBe(true);
    // Resolved by now, so the button must be back — the failure mode is a
    // control stuck on its busy label.
    expect(btn.disabled).toBe(false);
    expect(btn.textContent).toBe('Rank');
  });

  it('warns before ranking a hunt with no goal, and obeys a refusal', async () => {
    hunts = [{ id: 'h1', name: 'M920q', enabled: true, terms: [], timeslots: [], goal: '' }];
    window.confirm = () => false;
    const panel = await mount();
    await openHunt(panel);
    panel.querySelector('#ebay-rank').click();
    await settle();
    expect(apiCalls.some(c => c.path.endsWith('/rank'))).toBe(false);
  });

  it('switches to best-fit order after a rank', async () => {
    finds = [find('a', { ageDays: 0, score: 10 }), find('b', { ageDays: 4, score: 95 })];
    const panel = await mount();
    // The chosen order is module state and deliberately survives a re-mount —
    // it is the user's choice, not a property of the panel — so start from a
    // known one rather than assuming the default.
    panel.querySelector('.ebay-sort[data-sort="newest"]').click();
    await settle();
    expect(titles(panel)).toEqual(['Find a', 'Find b']); // newest first
    await openHunt(panel);
    panel.querySelector('#ebay-rank').click();
    await settle();
    expect(titles(panel)).toEqual(['Find b', 'Find a']);
    expect(panel.querySelector('.ebay-sort[data-sort="score"]').classList.contains('is-active')).toBe(true);
  });

  it('sorts unscored finds below scored ones rather than treating them as zero', async () => {
    finds = [
      find('unscored-newest', { ageDays: 0 }),
      find('scored-low', { ageDays: 5, score: 3 }),
      find('scored-high', { ageDays: 6, score: 90 }),
    ];
    const panel = await mount();
    panel.querySelector('.ebay-sort[data-sort="score"]').click();
    await settle();
    expect(titles(panel)).toEqual(['Find scored-high', 'Find scored-low', 'Find unscored-newest']);
  });

  it('keeps dismissed at the bottom in best-fit order too', async () => {
    finds = [find('top', { ageDays: 0, score: 99, reviewed: true }), find('live', { ageDays: 9, score: 1 })];
    const panel = await mount();
    panel.querySelector('.ebay-sort[data-sort="score"]').click();
    await settle();
    expect(titles(panel)).toEqual(['Find live', 'Find top']);
  });

  it('shows the score on a scored row and nothing on an unscored one', async () => {
    finds = [find('a', { score: 72 }), find('b', { ageDays: 1 })];
    const panel = await mount();
    panel.querySelector('.ebay-sort[data-sort="newest"]').click();
    await settle();
    const rows = panel.querySelectorAll('.ebay-find');
    expect(rows[0].querySelector('.ebay-score').textContent).toBe('72/100');
    expect(rows[1].querySelector('.ebay-score')).toBeNull();
  });

  it('says so when the pre-rank eBay refresh failed', async () => {
    const { showToast } = await import('/static/util.js');
    rankReply = { scored: 3, considered: 3, refresh_error: 'eBay rate limited' };
    const panel = await mount();
    await openHunt(panel);
    panel.querySelector('#ebay-rank').click();
    await settle();
    const [msg, isError] = showToast.mock.calls.at(-1);
    expect(msg).toContain('may include sold listings');
    expect(isError).toBe(true);
  });
});
