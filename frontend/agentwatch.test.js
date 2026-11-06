import { describe, it, expect } from 'vitest';
import { stateLabel, relTime, parseRoute, rowWhat, plain, tile, chatEvent, memoryTileText, termText, applyLive, machineText, briefing, ticketHref, agentWhere, agentWhen, costTotals, spendCsv } from '../agentwatch/static/agents.js';

describe('agent watch page helpers', () => {
  it('labels every state in plain words', () => {
    expect(stateLabel('broken')).toBe('Did not finish');
    expect(stateLabel('new')).toBe('Not run yet');
    expect(stateLabel('something-else')).toBe('something-else');
  });

  it('says how long ago, or how long until', () => {
    const now = Date.parse('2026-11-03T12:00:00Z');
    expect(relTime('2026-11-03T11:59:30Z', now)).toBe('a minute ago');
    expect(relTime('2026-11-03T11:40:00Z', now)).toBe('20 min ago');
    expect(relTime('2026-11-03T07:00:00Z', now)).toBe('5 h ago');
    expect(relTime('2026-10-31T12:00:00Z', now)).toBe('3 days ago');
    expect(relTime('2026-11-03T16:00:00Z', now)).toBe('in 4 h');
    expect(relTime(null, now)).toBe('');
  });

  it('reads the route from the hash', () => {
    expect(parseRoute('')).toEqual({ view: 'tiles' });
    expect(parseRoute('#/')).toEqual({ view: 'tiles' });
    expect(parseRoute('#/agent/Desk')).toEqual({ view: 'agent', name: 'Desk' });
    expect(parseRoute('#/agent/Two%20Words')).toEqual({ view: 'agent', name: 'Two Words' });
    expect(parseRoute('#/settings')).toEqual({ view: 'settings' });
    expect(parseRoute('#/term/Desk')).toEqual({ view: 'term', name: 'Desk' });
    expect(parseRoute('#/schedule')).toEqual({ view: 'schedule' });
    expect(parseRoute('#/memory')).toEqual({ view: 'memory' });
    expect(parseRoute('#/memory/jon/be-punchy.md')).toEqual({ view: 'fact', path: 'jon/be-punchy.md' });
  });

  it('finds what a hub row is about', () => {
    const row = { headers: ['#', 'Raised', 'What', 'Type', 'Your reply'], cells: ['3', '2026-11-02', 'Pick a supplier', 'Decision', ''] };
    expect(rowWhat(row)).toBe('Pick a supplier');
    const noWhat = { headers: ['#', 'Date', 'Thing', 'Your reply'], cells: ['1', '2026-11-02', 'The longest cell here', ''] };
    expect(rowWhat(noWhat)).toBe('The longest cell here');
  });

  it('shows hub markdown as plain text', () => {
    expect(plain('Approve [IT-PRO-001 Setup](IT-PRO-001.md) **now** `x`')).toBe('Approve IT-PRO-001 Setup now x');
  });

  it('builds a tile with its light, purpose and waiting badge, and no HTML from the agent', () => {
    const t = tile({ name: 'Desk', purpose: '<b>What only Jon can do</b>', state: 'late', waiting: 2 });
    expect(t.getAttribute('href')).toBe('#/agent/Desk');
    expect(t.querySelector('.light').className).toBe('light late');
    expect(t.querySelector('p').textContent).toBe('<b>What only Jon can do</b>');
    expect(t.querySelector('b')).toBeNull();
    expect(t.querySelector('.badge').textContent).toBe('2');
    expect(tile({ name: 'X', purpose: 'p', state: 'ok', waiting: 0 }).querySelector('.badge')).toBeNull();
  });

  it('adds the worker words, its tool use and errors to the chat it was sent in', () => {
    const state = { name: 'Desk', messages: [], busy: true };
    chatEvent({ kind: 'text', text: 'Looking now.' }, state);
    chatEvent({ kind: 'tool', text: 'Editing HUB.md' }, state);
    chatEvent({ kind: 'done', session: 'abc', text: 'Looking now.' }, state);
    chatEvent({ kind: 'error', text: 'claude stopped' }, state);
    expect(state.messages).toEqual([
      { role: 'worker', text: 'Looking now.' },
      { role: 'tool', text: 'Editing HUB.md' },
      { role: 'error', text: 'claude stopped' },
    ]);
  });

  it('says on the Memory tile how much every worker knows', () => {
    expect(memoryTileText(null)).toBe('What every worker knows, by subject');
    expect(memoryTileText({ count: 85, folders: [1, 2, 3, 4, 5, 6] })).toBe('What every worker knows: 85 facts in 6 folders');
  });
});

describe('termText', () => {
  const t = { open: true, today_gbp: 0.4, month_gbp: 3.1, session_gbp: 0.4, over_session: false, over_day: false };
  it('says the phase, with no spend (that is on the Schedule tab)', () => {
    expect(termText({ ...t, state: { phase: 'done', line: 'x' } })).toBe('Done');
    expect(termText({ ...t, state: null })).toBe('Open');
  });
  it('says when it is over a limit', () => {
    expect(termText({ ...t, state: { phase: 'working' }, over_session: true })).toMatch(/session over its limit$/);
  });
  it('is empty without a terminal', () => expect(termText(null)).toBe(''));
});

describe('applyLive', () => {
  const a = { name: 'Orchestrator', state: 'ok', paused: false, term: null };
  it('shows a pause as soon as it happens, and its end', () => {
    const [p] = applyLive([a], { Orchestrator: { term: null, paused: true } });
    expect(p.state).toBe('paused');
    const [r] = applyLive([p], { Orchestrator: { term: null, paused: false } });
    expect(r.state).toBe('ok');
  });
  it('leaves a worker it has no news of alone', () => {
    expect(applyLive([a], {})[0]).toBe(a);
  });
});

describe('machineText', () => {
  it('says wl on or off, and nothing before the first check', () => {
    expect(machineText({ machine: 'wl', machine_on: true })).toBe('wl on');
    expect(machineText({ machine: 'wl', machine_on: false })).toBe('wl off');
    expect(machineText({ machine: 'wl', machine_on: null })).toBe('');
    expect(machineText({})).toBe('');
  });
});

describe('briefing', () => {
  it('says the last run, what waits on Jon, and what the terminal last said', () => {
    const lines = briefing({
      last_run: new Date(Date.now() - 3600_000).toISOString(), line: 'wrote IT-PRO-003',
      can_reply: false, waiting_lines: ['approve IT-POL-003'], term: { state: { line: 'Done: hub rewritten.' } },
    });
    expect(lines.map((l) => l.label)).toEqual(['Last run', 'Needs you', 'Terminal said']);
    expect(lines[0].text).toMatch(/wrote IT-PRO-003$/);
  });
  it("starts with the agent's own State and NEXT", () => {
    const lines = briefing({ status_lines: ['State: 3 drafts', 'NEXT: needs Jon: approve'], can_reply: false, waiting_lines: [] });
    expect(lines.slice(0, 2)).toEqual([{ label: 'State', text: '3 drafts' }, { label: 'NEXT', text: 'needs Jon: approve' }]);
  });
  it('keeps to five waiting items', () => {
    const lines = briefing({ can_reply: false, waiting_lines: ['a', 'b', 'c', 'd', 'e', 'f', 'g'] });
    expect(lines.at(-1).text).toBe('and 2 more');
  });
});

describe('schedule labels', () => {
  it('says where and when each agent runs', () => {
    expect(agentWhere({ kind: 'cloud' })).toBe('Anthropic cloud');
    expect(agentWhere({ kind: 'queue', machine: 'wl' })).toBe('lenovo1, for wl');
    expect(agentWhere({ kind: 'meter', machine: 'omnilink1' })).toBe('omnilink1');
    expect(agentWhen({ kind: 'meter' })).toBe('when Jon works there');
    expect(agentWhen({ kind: 'local', every: '20m' })).toBe('every 20m');
    expect(agentWhen({ kind: 'queue' })).toBe('when work is queued');
  });
});

describe('costTotals', () => {
  it('adds every terminal and skips agents without one', () => {
    const t = costTotals([{ term: { today_gbp: 0.4, month_gbp: 3 } }, { term: { today_gbp: 0.1, month_gbp: 1 } }, {}]);
    expect(t.today).toBeCloseTo(0.5);
    expect(t.month).toBeCloseTo(4);
  });
});

describe('costTotals for the books', () => {
  it('leaves out the work laptop', () => {
    const list = [{ kind: 'cloud', term: { today_gbp: 1, month_gbp: 2 } }, { kind: 'queue', term: { today_gbp: 5, month_gbp: 9 } }];
    expect(costTotals(list, true)).toEqual({ today: 1, month: 2 });
    expect(costTotals(list).month).toBe(11);
  });
});

describe('tickets', () => {
  it('routes a ticket path with its folders', () => {
    const href = ticketHref({ name: 'Work laptop' }, 'thingco/TICKET-musty-v0.2.0.md');
    expect(href).toBe('#/ticket/Work%20laptop/thingco/TICKET-musty-v0.2.0.md');
    expect(parseRoute(href)).toEqual({ view: 'ticket', name: 'Work laptop', path: 'thingco/TICKET-musty-v0.2.0.md' });
  });
});

describe('on-demand chat tiles', () => {
  it('say where and when a routine-less cloud tile runs', () => {
    const a = { kind: 'cloud', can_run: false, every: '1d' };
    expect(agentWhere(a)).toBe('its chat page, on demand');
    expect(agentWhen(a)).toBe('when used, checked every 1d');
    expect(agentWhere({ kind: 'cloud', can_run: true })).toBe('Anthropic cloud');
  });
});

describe('spendCsv', () => {
  it('writes a row per day with a column per agent', () => {
    const csv = spendCsv([
      { date: '2026-11-06', total_gbp: 1.5, books_gbp: 1, agents: { desk: 1, 'work-laptop': 0.5 } },
      { date: '2026-11-05', total_gbp: 2, books_gbp: 2, agents: { desk: 2 } },
    ]);
    expect(csv).toBe('date,total_gbp,books_gbp,desk,work-laptop\n2026-11-06,1.50,1.00,1.00,0.50\n2026-11-05,2.00,2.00,2.00,0.00\n');
  });
});
