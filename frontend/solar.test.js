import { describe, it, expect } from 'vitest';
import { solarPosition, todaySunriseSunset, calculateSolarState } from '../coordinator/src/http/static/solar.js';

const LONDON = [51.5074, -0.1278];

describe('calculateSolarState', () => {
  // Deterministic elevation → { bri, ct } mapping (mirrors the Rust calculate_solar_state).
  it('full sun (90°) maxes brightness and goes coolest, clamped to 254', () => {
    expect(calculateSolarState(90)).toEqual({ bri: 254, ct: 153 });
  });

  it('horizon (0°) is dim and warmest', () => {
    expect(calculateSolarState(0)).toEqual({ bri: 30, ct: 500 });
  });

  it('mid-afternoon (45°) interpolates', () => {
    expect(calculateSolarState(45)).toEqual({ bri: 143, ct: 304 });
  });

  it('civil twilight floor (-18°) bottoms out at bri 1', () => {
    expect(calculateSolarState(-18)).toEqual({ bri: 1, ct: 500 });
  });

  it('well below horizon stays at the floor (no negative brightness)', () => {
    expect(calculateSolarState(-40)).toEqual({ bri: 1, ct: 500 });
  });

  it('ct_warmth scales colour temperature toward neutral (153)', () => {
    expect(calculateSolarState(0, { ct_warmth: 0 }).ct).toBe(153);
    expect(calculateSolarState(0, { ct_warmth: 0.5 }).ct).toBe(327);
    expect(calculateSolarState(0, { ct_warmth: 1 }).ct).toBe(500);
  });

  it('respects min/max brightness clamps', () => {
    expect(calculateSolarState(90, { max_brightness: 100 }).bri).toBe(100);
    expect(calculateSolarState(-40, { min_brightness: 40 }).bri).toBe(40);
  });
});

describe('solarPosition', () => {
  it('always returns azimuth in [0,360] and elevation in [-90,90]', () => {
    const base = Date.UTC(2026, 5, 21, 0, 0, 0);
    for (let h = 0; h < 24; h++) {
      const { azimuth, elevation } = solarPosition(base + h * 3600000, ...LONDON);
      expect(azimuth).toBeGreaterThanOrEqual(0);
      expect(azimuth).toBeLessThanOrEqual(360);
      expect(elevation).toBeGreaterThanOrEqual(-90);
      expect(elevation).toBeLessThanOrEqual(90);
    }
  });

  it('sun is higher near solar noon than near midnight (summer solstice, London)', () => {
    const noon = solarPosition(Date.UTC(2026, 5, 21, 11, 0, 0), ...LONDON);
    const midnight = solarPosition(Date.UTC(2026, 5, 21, 23, 0, 0), ...LONDON);
    expect(noon.elevation).toBeGreaterThan(midnight.elevation);
    expect(noon.elevation).toBeGreaterThan(40); // London summer noon sun is well up
  });
});

describe('todaySunriseSunset', () => {
  it('London is never polar and returns bearings in range', () => {
    const { sunriseAz, sunsetAz, polarDay, polarNight } = todaySunriseSunset(...LONDON);
    expect(polarDay).toBe(false);
    expect(polarNight).toBe(false);
    for (const az of [sunriseAz, sunsetAz]) {
      expect(az).toBeGreaterThanOrEqual(0);
      expect(az).toBeLessThanOrEqual(360);
    }
  });
});
