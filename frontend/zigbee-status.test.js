import { describe, it, expect } from 'vitest';
import { deriveZigbeeStatus } from '../coordinator/src/http/static/zigbee-status.js';

// Regression cover for 2026-08-30: every light in the house being offline is
// what a normal evening looks like once the wall switches are off, and the old
// heuristic read it as a dead bridge — disabling the whole room UI, including
// the ✕ that removes a switch from a room.
describe('deriveZigbeeStatus', () => {
  it('trusts a report of online', () => {
    expect(deriveZigbeeStatus(true, 5, 14)).toBe(true);
  });

  it('trusts a report of offline', () => {
    expect(deriveZigbeeStatus(false, 5, 14)).toBe(false);
  });

  it('trusts a report of online even when no lights are known yet', () => {
    expect(deriveZigbeeStatus(true, 5, 0)).toBe(true);
  });

  it('trusts a report of offline over a healthy-looking device list', () => {
    expect(deriveZigbeeStatus(false, 5, 14)).toBe(false);
  });

  it('treats an unknown report with rooms but zero lights as offline', () => {
    expect(deriveZigbeeStatus(null, 5, 0)).toBe(false);
    expect(deriveZigbeeStatus(undefined, 5, 0)).toBe(false);
  });

  it('treats an unknown report as online once lights are known', () => {
    expect(deriveZigbeeStatus(null, 5, 14)).toBe(true);
  });

  it('does not call the bridge offline just because no rooms exist yet', () => {
    expect(deriveZigbeeStatus(null, 0, 0)).toBe(true);
  });
});
