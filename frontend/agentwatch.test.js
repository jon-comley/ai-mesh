import { describe, it, expect } from 'vitest';
import { stateLabel, relTime, parseRoute, rowWhat, plain, tile, chatEvent } from '../agentwatch/static/agents.js';

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
});
