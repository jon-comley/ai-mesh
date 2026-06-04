// ── Solar math ───────────────────────────────────────────────────────────────
// Pure sun-position + sun-driven-light helpers, split out of layout.js. No DOM,
// no shared state — latitude/longitude are passed in, so these are deterministic
// and unit-testable (see frontend/solar.test.js). The DOM-touching pieces
// (redrawSolarOverlay, previewSolarState) stay in layout.js and call into these.

// NOAA simplified solar position (±1-2° accuracy — sufficient for arc preview).
// dateUtc is epoch milliseconds; latDeg/lonDeg in degrees.
// Returns { azimuth: 0-360, elevation: -90..90 }.
export function solarPosition(dateUtc, latDeg, lonDeg) {
  const lat = latDeg * Math.PI / 180;
  const lon = lonDeg;
  const jd = dateUtc / 86400000 + 2440587.5;
  const n = jd - 2451545.0;
  const L = (280.46 + 0.9856474 * n) % 360;
  const g = (357.528 + 0.9856003 * n) % 360;
  const gr = g * Math.PI / 180;
  const lambda = (L + 1.915 * Math.sin(gr) + 0.020 * Math.sin(2 * gr)) * Math.PI / 180;
  const eps = 23.439 * Math.PI / 180;
  const sinDec = Math.sin(eps) * Math.sin(lambda);
  const dec = Math.asin(sinDec);
  const cosDec = Math.cos(dec);
  // Greenwich Mean Sidereal Time → hour angle
  const gmst = (18.697374558 + 24.06570982441908 * n) % 24;
  const lst = ((gmst + lon / 15) % 24 + 24) % 24;
  const ha = (lst - (Math.atan2(Math.sin(lambda), Math.cos(lambda) * Math.cos(eps)) * 12 / Math.PI) + 24) % 24;
  const haRad = ha * Math.PI / 12;
  const sinAlt = Math.sin(lat) * sinDec + Math.cos(lat) * cosDec * Math.cos(haRad);
  const elevation = Math.asin(Math.max(-1, Math.min(1, sinAlt))) * 180 / Math.PI;
  const cosAz = (sinDec - Math.sin(lat) * sinAlt) / (Math.cos(lat) * Math.cos(Math.asin(sinAlt)));
  let az = Math.acos(Math.max(-1, Math.min(1, cosAz))) * 180 / Math.PI;
  if (Math.sin(haRad) > 0) az = 360 - az;
  return { azimuth: az, elevation };
}

// Scan today (local) in 5-min steps for the sunrise/sunset bearings.
export function todaySunriseSunset(latDeg, lonDeg) {
  const base = new Date(); base.setHours(0, 0, 0, 0);
  let riseAz = null, setAz = null, wasUp = null;
  for (let m = 0; m <= 1440; m += 5) {
    const d = new Date(base.getTime() + m * 60000);
    const { azimuth, elevation } = solarPosition(d.getTime(), latDeg, lonDeg);
    const up = elevation > 0;
    if (wasUp === false && up)  riseAz = azimuth;
    if (wasUp === true  && !up) setAz  = azimuth;
    wasUp = up;
  }
  const polarDay   = riseAz == null && wasUp === true;
  const polarNight = riseAz == null && wasUp === false;
  return { sunriseAz: riseAz ?? 90, sunsetAz: setAz ?? 270, polarDay, polarNight };
}

// Sun elevation → bulb brightness + colour temperature. Mirrors the Rust
// calculate_solar_state so the client preview matches the server's output.
export function calculateSolarState(elevation, params = {}) {
  const minBri    = params.min_brightness ?? 1;
  const maxBri    = Math.max(minBri, params.max_brightness ?? 254);
  const ctWarmth  = Math.max(0, Math.min(1, params.ct_warmth ?? 1.0));

  let bri, ct;
  if (elevation <= 0) {
    const t = Math.max(0, Math.min(1, (elevation + 18) / 18));
    bri = Math.round(1 + t * 29);
    ct  = 500;
  } else {
    const t = Math.min(1, elevation / 90);
    bri = Math.round(30 + t * 225);
    ct  = Math.round(454 - t * 301);
  }

  bri = Math.max(minBri, Math.min(maxBri, bri));
  ct  = Math.round(153 + ctWarmth * (ct - 153));

  return { bri, ct };
}
