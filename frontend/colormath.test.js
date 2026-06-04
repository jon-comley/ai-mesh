import { describe, it, expect } from 'vitest';
import { xyToRgb, rgbToHsl, hslToXy } from '../coordinator/src/http/static/colormath.js';

describe('colormath', () => {
  it('xyToRgb returns black when y is 0', () => {
    expect(xyToRgb(0.3, 0)).toEqual({ r: 0, g: 0, b: 0 });
  });

  it('rgbToHsl reports zero saturation for greys', () => {
    expect(rgbToHsl(128, 128, 128).s).toBe(0);
  });

  it('rgbToHsl puts pure red near hue 0', () => {
    const { h, s } = rgbToHsl(255, 0, 0);
    expect(h).toBe(0);
    expect(s).toBe(100);
  });

  it('hslToXy returns valid CIE xy in [0,1] for an arbitrary hue', () => {
    const { x, y } = hslToXy(200, 80);
    expect(x).toBeGreaterThan(0);
    expect(x).toBeLessThan(1);
    expect(y).toBeGreaterThan(0);
    expect(y).toBeLessThan(1);
  });

  it('hue survives an hsl -> xy -> rgb -> hsl round trip (within tolerance)', () => {
    const hue = 120;
    const { x, y } = hslToXy(hue, 100);
    const { r, g, b } = xyToRgb(x, y, 254);
    const back = rgbToHsl(r, g, b);
    expect(Math.abs(back.h - hue)).toBeLessThan(15);
  });
});
