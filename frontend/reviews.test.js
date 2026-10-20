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

const { init, handleUpdate, scoreRows, parseTimes, formatTime, parseAliases, formatAliases, scheduleText, onDemandBody, answerHtml } =
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
      { id: 'f3', run_id: 2, repo: 'dashboard', path: 'src/vat.ts', line: 4, severity: 'low', kind: 'test',
        title: 'Test the VAT rounding', quote: 'const vat = round(net * 0.2);', scenario: 'nothing tests the half-penny case',
        fix: 'add a test', patch: '--- /dev/null\n+++ b/src/vat.test.ts\n@@ -0,0 +1 @@\n+<b>it</b>', patch_applies: true,
        verdict: 'confirmed', found_by: ['gemini-flash-latest@gemini'], status: 'open', first_seen: 1 },
    ],
    settings: { ntfy_topic_set: false, max_review_tokens: 32000, evening_max_tokens: 16000 },
    scorecard: [
      { reviewer: 'qwen3-coder@mac1', kind: 'bug', raised: 10, invented: 2, rejected: 3, unsure: 1, confirmed: 4, fixed: 3, dismissed: 1 },
      { reviewer: 'qwen3-coder@mac1', kind: 'test', raised: 2, invented: 0, rejected: 0, unsure: 0, confirmed: 2, fixed: 0, dismissed: 0 },
      { reviewer: 'gemini-flash-latest@gemini', kind: 'bug', raised: 4, invented: 0, rejected: 1, unsure: 0, confirmed: 3, fixed: 1, dismissed: 1 },
    ],
    questions: [
      { id: 'q2', repo: 'dashboard', question: 'Is VAT rounded per line?', status: 'thinking', sources: [], asked_at: 2 },
      { id: 'q1', repo: 'dashboard', question: 'Where is the invoice total worked out?', status: 'answered',
        answer: 'In `dashboard/src/services/invoices.ts:88` <b>bold?</b>', sources: ['dashboard/src/services/invoices.ts', 'guv/src/lib/invoice.ts'],
        worker: 'qwen3-coder:30b@mac1', asked_at: 1, answered_at: 3 },
    ],
    ...overrides,
  };
}

describe('helpers', () => {
  it('adds the scorecard up by reviewer or by kind, with rates', () => {
    const rows = [
      { reviewer: 'a@x', kind: 'bug', raised: 4, invented: 1, rejected: 1, unsure: 0, confirmed: 2, fixed: 0, dismissed: 0 },
      { reviewer: 'b@y', kind: 'bug', raised: 1, invented: 0, rejected: 0, unsure: 0, confirmed: 1, fixed: 1, dismissed: 0 },
    ];
    const byKind = scoreRows(rows, 'kind');
    expect(byKind).toHaveLength(1);
    expect(byKind[0]).toMatchObject({ kind: 'bug', raised: 5, confirmed: 3, confirmedPct: 60, fixedPct: 100 });
    const byReviewer = scoreRows(rows, 'reviewer');
    expect(byReviewer.map(r => r.reviewer)).toEqual(['a@x', 'b@y']);
    expect(byReviewer[0].fixedPct).toBeNull();
  });

  it('reads and writes times', () => {
    expect(parseTimes('02:15, 6:00, 25:00, nonsense')).toEqual([135, 360]);
    expect(formatTime(135)).toBe('02:15');
  });

  it('reads and writes import aliases', () => {
    const a = parseAliases('@app/ = guv:src\nbad line\n~/ = shared:lib');
    expect(a).toEqual([{ prefix: '@app/', repo: 'guv', dir: 'src' }, { prefix: '~/', repo: 'shared', dir: 'lib' }]);
    expect(formatAliases(a)).toBe('@app/ = guv:src\n~/ = shared:lib');
  });

  it('builds the on-demand review request', () => {
    expect(onDemandBody('guv', 'new', '')).toEqual({ repo: 'guv', sweep: false });
    expect(onDemandBody('guv', 'sweep', '')).toEqual({ repo: 'guv', sweep: true });
    expect(onDemandBody('guv', 'path', ' src/lib ')).toEqual({ repo: 'guv', sweep: false, path: 'src/lib' });
    expect(onDemandBody('guv', 'branch', 'feature-x')).toEqual({ repo: 'guv', sweep: false, branch: 'feature-x' });
    expect(onDemandBody('guv', 'path', '  ')).toBeNull();
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

  it('shows each reviewer\'s scorecard and the totals by kind', () => {
    const tables = panel.querySelectorAll('#rv-scorecard table');
    expect(tables).toHaveLength(2);
    const mac = [...tables[0].querySelectorAll('tbody tr')][0].textContent;
    expect(mac).toContain('qwen3-coder@mac1');
    expect(mac).toContain('50%'); // 6 confirmed of 12 raised
    expect(mac).toContain('75%'); // 3 fixed of 4 decided
    expect(tables[1].textContent).toContain('Bugs');
    expect(tables[1].textContent).toContain('Tests');
  });

  it('puts suggestions after the bugs, with their diff and who found them', () => {
    const el = panel.querySelector('#rv-findings');
    const headings = [...el.querySelectorAll('h4')].map(h => h.textContent);
    expect(headings).toEqual(['High bugs', 'Low bugs', 'Tests to add']);
    const test = el.querySelector('.rv-finding[data-id="f3"]');
    expect(test.querySelector('summary').textContent).toBe('Diff (applies cleanly)');
    expect(test.querySelector('details pre').textContent).toContain('+<b>it</b>');
    expect(test.querySelector('b')).toBeNull();
    expect(test.textContent).toContain('found by gemini-flash-latest@gemini');
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
    expect(panel.querySelector('#rv-e-cloud').checked).toBe(false);
    panel.querySelector('#rv-e-cloud').checked = true;
    panel.querySelector('button[data-act="save-repo"]').click();
    await flush();
    const body = apiCalls.find(c => c.path === '/reviews/repos').opts.body;
    expect(body.cloud).toBe(true);
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

  it('shows questions with their answers as text, and asks new ones', async () => {
    const el = panel.querySelector('#rv-questions');
    expect(el.textContent).toContain('reading the code…');
    expect(el.textContent).toContain('qwen3-coder:30b@mac1');
    expect(el.querySelector('b')).toBeNull();
    expect(el.textContent).toContain('2 files read');
    panel.querySelector('#rv-ask-text').value = 'What does parsePrice refuse?';
    panel.querySelector('#rv-ask-go').click();
    await flush();
    expect(apiCalls.find(c => c.path === '/reviews/ask').opts.body)
      .toEqual({ repo: 'dashboard', question: 'What does parsePrice refuse?' });
    expect(panel.querySelector('#rv-ask-text').value).toBe('');
  });

  it('starts an on-demand branch review', async () => {
    const what = panel.querySelector('#rv-od-what');
    what.value = 'branch';
    what.dispatchEvent(new Event('change'));
    expect(panel.querySelector('#rv-od-arg').hidden).toBe(false);
    panel.querySelector('#rv-od-arg').value = 'claude/adoring-cerf-h3nz8p';
    panel.querySelector('#rv-od-go').click();
    await flush();
    expect(apiCalls.find(c => c.path === '/reviews/run-now').opts.body)
      .toEqual({ repo: 'dashboard', sweep: false, branch: 'claude/adoring-cerf-h3nz8p' });
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

describe('answerHtml', () => {
  it('shows ``` fences as code blocks and escapes everything', () => {
    const html = answerHtml('Look here:\n```ts\nconst a = <b>1</b>;\n```\nDone.');
    expect(html).toBe('Look here:<pre class="rv-quote">const a = &lt;b&gt;1&lt;/b&gt;;</pre>Done.');
  });

  it('leaves an answer without fences as text', () => {
    expect(answerHtml('just <text>')).toBe('just &lt;text&gt;');
  });
});
