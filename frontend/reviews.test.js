import { describe, it, expect, vi, beforeEach } from 'vitest';

const apiCalls = [];
let reviewsReply;

vi.mock('/static/api.js', () => ({
  api: (path, opts) => {
    apiCalls.push({ path, opts });
    if (path === '/reviews') return Promise.resolve(json(reviewsReply));
    if (path.endsWith('/report')) {
      return Promise.resolve({ ok: true, text: () => Promise.resolve('# Code review: dashboard') });
    }
    if (path === '/work/roles') return Promise.resolve(json(opts.body));
    return Promise.resolve(json({}));
  },
}));
vi.mock('/static/util.js', () => ({
  showToast: vi.fn(),
  esc: s => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;'),
}));

function json(body) {
  return { ok: true, status: 200, json: () => Promise.resolve(body), text: () => Promise.resolve('') };
}

const { init, handleUpdate, parseTimes, formatTime, parseAliases, formatAliases, scheduleText } =
  await import('/static/reviews.js');

const flush = () => new Promise(r => setTimeout(r, 0));

function snapshot(overrides = {}) {
  return {
    node_id: 'mac1',
    hostname: 'mac1',
    generated_at: 1,
    repos: [{
      name: 'dashboard', url: 'git@github-dashboard:jon-comley/dashboard.git', branch: 'main',
      timeslots: [135], sweep_day: 6, sweep_slot: 195, enabled: true,
      aliases: [{ prefix: '@app/', repo: 'guv', dir: 'src' }],
      last_reviewed_commit: 'cdcf9e8abc',
    }],
    runs: [
      { id: 3, repo: 'dashboard', kind: 'nightly', scope: 'Nightly: 2 new commits', status: 'running',
        started_at: 1, tasks_total: 5, tasks_done: 2,
        running: [{ kind: 'review', node_id: 'mac1', worker: 'qwen3-coder@mac1', label: 'src/pages/JobDetailPage.tsx +3 files' },
                  { kind: 'check', node_id: 'beelink1', worker: 'qwen2.5:7b@beelink1', label: 'dashboard/src/a.ts:12' }],
        counts: {} },
      { id: 2, repo: 'dashboard', kind: 'manual', scope: 'Run now', status: 'done', started_at: 1,
        tasks_total: 4, tasks_done: 4, running: [], counts: { high: 1, medium: 0, low: 2, unconfirmed: 1 } },
    ],
    findings: [
      { id: 'f1', run_id: 2, repo: 'dashboard', path: 'src/pages/JobDetailPage.tsx', line: 219, severity: 'high',
        title: 'Invoice bills the cheapest option', quote: 'const agreed = pricing?.amount;',
        scenario: 'accepted £1,900, billed £1,200', fix: 'use the accepted amount',
        verdict: 'confirmed', found_by: ['qwen3-coder@mac1'], status: 'open', first_seen: 1 },
      { id: 'f2', run_id: 2, repo: 'dashboard', path: 'src/x.ts', line: 3, severity: 'low',
        title: '<script>alert(1)</script>', quote: 'x', scenario: '', fix: '',
        found_by: [], status: 'open', first_seen: 1 },
    ],
    settings: { ntfy_topic_set: false, max_review_tokens: 100000, evening_max_tokens: 32000 },
    ...overrides,
  };
}

describe('helpers', () => {
  it('reads and writes times', () => {
    expect(parseTimes('02:15, 6:00, 25:00, nonsense')).toEqual([135, 360]);
    expect(formatTime(135)).toBe('02:15');
  });

  it('reads and writes import aliases', () => {
    const a = parseAliases('@app/ = guv:src\nbad line\n~/ = shared:lib');
    expect(a).toEqual([{ prefix: '@app/', repo: 'guv', dir: 'src' }, { prefix: '~/', repo: 'shared', dir: 'lib' }]);
    expect(formatAliases(a)).toBe('@app/ = guv:src\n~/ = shared:lib');
  });

  it('describes a schedule', () => {
    expect(scheduleText({ timeslots: [135], sweep_day: 6, sweep_slot: 195, enabled: true }))
      .toBe('nightly 02:15 · sweep Sunday 03:15');
    expect(scheduleText({ timeslots: [], enabled: true })).toBe('Run now only');
    expect(scheduleText({ timeslots: [135], enabled: false })).toBe('paused');
  });
});

describe('Reviews tab', () => {
  let panel;
  beforeEach(async () => {
    apiCalls.length = 0;
    reviewsReply = { online: true, snapshot: snapshot(), roles: { control: [], work: [] }, models: ['qwen2.5:7b', 'qwen3-coder'] };
    document.body.innerHTML = '<section id="p"></section>';
    panel = document.getElementById('p');
    init(panel);
    await flush();
  });

  it('shows which machine is doing which task in the live run', () => {
    const text = panel.querySelector('#rv-runs').textContent;
    expect(text).toContain('running · 2 of 5 tasks');
    expect(text).toContain('review on qwen3-coder@mac1');
    expect(text).toContain('check on qwen2.5:7b@beelink1');
    expect(text).toContain('done · 1 high, 0 medium, 2 low, 1 not confirmed');
  });

  it('lists findings by severity and escapes model text', () => {
    const el = panel.querySelector('#rv-findings');
    expect(el.textContent).toContain('Invoice bills the cheapest option');
    expect(el.textContent).toContain('dashboard/src/pages/JobDetailPage.tsx:219');
    expect(el.querySelector('script')).toBeNull();
    expect(el.textContent).toContain('(not confirmed)');
  });

  it('Run now and Dismiss call the API', async () => {
    panel.querySelector('button[data-act="run"]').click();
    await flush();
    expect(apiCalls.find(c => c.path === '/reviews/run-now').opts.body).toEqual({ repo: 'dashboard', sweep: false });
    panel.querySelector('button[data-act="dismiss"][data-id="f1"]').click();
    await flush();
    expect(apiCalls.find(c => c.path === '/reviews/findings/f1').opts.body).toEqual({ status: 'dismissed' });
    expect(panel.querySelector('.rv-finding[data-id="f1"]')).toBeNull();
  });

  it('editing a repo sends the whole spec back', async () => {
    panel.querySelector('button[data-act="edit"]').click();
    panel.querySelector('#rv-e-times').value = '01:30, 04:00';
    panel.querySelector('button[data-act="save-repo"]').click();
    await flush();
    const body = apiCalls.find(c => c.path === '/reviews/repos').opts.body;
    expect(body.name).toBe('dashboard');
    expect(body.timeslots).toEqual([90, 240]);
    expect(body.sweep_day).toBe(6);
    expect(body.aliases).toEqual([{ prefix: '@app/', repo: 'guv', dir: 'src' }]);
  });

  it('a report is fetched and shown as text', async () => {
    panel.querySelector('button[data-act="report"]').click();
    await flush(); await flush();
    expect(panel.querySelector('.rv-report-text').textContent).toBe('# Code review: dashboard');
  });

  it('offline and notices are shown; a WebSocket update re-renders', () => {
    handleUpdate({ snapshot: snapshot({ notice: 'evil: only https://github.com/… URLs are accepted', runs: [] }) });
    expect(panel.querySelector('#rv-notice').hidden).toBe(false);
    expect(panel.querySelector('#rv-runs').textContent).toContain('No runs yet.');
    expect(panel.querySelector('#rv-status').textContent).toContain('running on mac1');
  });

  it('saves the model roles from the ticks', async () => {
    panel.querySelector('#rv-roles input[data-role="work"][value="qwen3-coder"]').checked = true;
    panel.querySelector('#rv-roles input[data-role="control"][value="qwen2.5:7b"]').checked = true;
    panel.querySelector('#rv-roles-save').click();
    await flush();
    expect(apiCalls.find(c => c.path === '/work/roles').opts.body)
      .toEqual({ control: ['qwen2.5:7b'], work: ['qwen3-coder'] });
  });
});
