//! Perceptual blending between two bulb states.
//!
//! Used both for effect→effect handoff (1 s cross-fade) and for any other case
//! where the runner needs to interpolate between two known light states
//! (time-scrubbing, parameter preview, arbitrary-duration scene recall).
//!
//! Two-stage pipeline:
//! 1. **Normalise** — convert anything (CT, xy) to a single common colour
//!    representation (CIE xy with an implied Y=1 chromaticity).
//! 2. **Blend** — interpolate brightness linearly + colour in Oklab so a
//!    red→blue cross-fade passes through clean purple, not muddy grey.
//!
//! The Oklab pipeline is hand-rolled (not via the `palette` crate's `IntoColor`
//! traits) so the matrix coefficients are visible at the use-site and tests
//! can verify against reference values without crate-version churn.

// Oklab matrix coefficients are published at 10 digits in Ottosson's paper;
// keep them at source precision and let the f32 literal cast truncate.
#![allow(clippy::excessive_precision)]

use palette::Oklab;

/// A single point in the blend space. Whatever the source effect emitted
/// (brightness, CT, xy) is normalised to this shape before interpolation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlendPoint {
    pub on: bool,
    pub brightness: u8,
    pub color: ColorXy,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorXy {
    pub x: f32,
    pub y: f32,
}

impl ColorXy {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Linear-interpolate two u8 values without overflowing.
pub fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let t = t.clamp(0.0, 1.0);
    let v = a as f32 + (b as f32 - a as f32) * t;
    v.round().clamp(0.0, 255.0) as u8
}

// ── CT (mireds) → CIE xy ─────────────────────────────────────────────────────
//
// Eight reference points from 2000 K (mireds 500) to 6500 K (mireds 154),
// covering the full range Hue/Z2M bulbs accept. CIE xy values from the
// blackbody locus (BT.2020-ish references); used as a fast lookup with
// linear interpolation between adjacent entries.

const CT_TABLE: &[(u16, f32, f32)] = &[
    (2000, 0.5267, 0.4133),
    (2700, 0.4578, 0.4101),
    (3000, 0.4369, 0.4041),
    (3500, 0.4053, 0.3907),
    (4000, 0.3805, 0.3768),
    (5000, 0.3451, 0.3516),
    (5500, 0.3325, 0.3411),
    (6500, 0.3135, 0.3237),
];

pub fn mireds_to_xy(mireds: u16) -> ColorXy {
    let kelvin = if mireds == 0 {
        6500.0
    } else {
        1_000_000.0 / mireds as f32
    };
    ct_to_xy(kelvin)
}

pub fn ct_to_xy(kelvin: f32) -> ColorXy {
    // Clamp to table range.
    if kelvin <= CT_TABLE[0].0 as f32 {
        return ColorXy::new(CT_TABLE[0].1, CT_TABLE[0].2);
    }
    let last = CT_TABLE[CT_TABLE.len() - 1];
    if kelvin >= last.0 as f32 {
        return ColorXy::new(last.1, last.2);
    }
    for w in CT_TABLE.windows(2) {
        let (k1, x1, y1) = w[0];
        let (k2, x2, y2) = w[1];
        if kelvin >= k1 as f32 && kelvin <= k2 as f32 {
            let t = (kelvin - k1 as f32) / (k2 as f32 - k1 as f32);
            return ColorXy::new(x1 + (x2 - x1) * t, y1 + (y2 - y1) * t);
        }
    }
    ColorXy::new(last.1, last.2)
}

// ── Oklab pipeline: xy → Oklab → xy ──────────────────────────────────────────

/// CIE xy → Oklab (assumes Y=1 — we blend chromaticity only; brightness is
/// handled separately by `lerp_u8`).
fn xy_to_oklab(c: ColorXy) -> Oklab<f32> {
    let x = c.x;
    let y = c.y.max(1e-6);
    // xy → XYZ with Y=1
    let cap_x = x / y;
    let cap_y = 1.0_f32;
    let cap_z = (1.0 - x - y) / y;
    xyz_to_oklab(cap_x, cap_y, cap_z)
}

/// Inverse: Oklab → CIE xy.
fn oklab_to_xy(lab: Oklab<f32>) -> ColorXy {
    let (cap_x, cap_y, cap_z) = oklab_to_xyz(lab);
    let sum = (cap_x + cap_y + cap_z).max(1e-6);
    ColorXy::new(cap_x / sum, cap_y / sum)
}

/// XYZ (D65) → Oklab. Matrix coefficients from the Oklab paper (Björn Ottosson).
fn xyz_to_oklab(x: f32, y: f32, z: f32) -> Oklab<f32> {
    let l = 0.8189330101 * x + 0.3618667424 * y - 0.1288597137 * z;
    let m = 0.0329845436 * x + 0.9293118715 * y + 0.0361456387 * z;
    let s = 0.0482003018 * x + 0.2643662691 * y + 0.6338517070 * z;

    let l_ = cbrt_signed(l);
    let m_ = cbrt_signed(m);
    let s_ = cbrt_signed(s);

    Oklab::new(
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    )
}

/// Oklab → XYZ (D65). Inverse of `xyz_to_oklab`.
fn oklab_to_xyz(lab: Oklab<f32>) -> (f32, f32, f32) {
    let l_ = lab.l + 0.3963377774 * lab.a + 0.2158037573 * lab.b;
    let m_ = lab.l - 0.1055613458 * lab.a - 0.0638541728 * lab.b;
    let s_ = lab.l - 0.0894841775 * lab.a - 1.2914855480 * lab.b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    let x = 1.2270138511 * l - 0.5577999807 * m + 0.2812561490 * s;
    let y = -0.0405801784 * l + 1.1122568696 * m - 0.0716766787 * s;
    let z = -0.0763812845 * l - 0.4214819784 * m + 1.5861632204 * s;
    (x, y, z)
}

/// `f32::cbrt` exists but explicitly preserves sign — Oklab math assumes
/// signed cube root throughout.
fn cbrt_signed(v: f32) -> f32 {
    v.cbrt()
}

// ── Public blend API ─────────────────────────────────────────────────────────

/// Blend two BlendPoints. `t ∈ [0, 1]`. Brightness lerps linearly; colour
/// blends perceptually in Oklab so red→blue passes through purple. `on`
/// flips at the midpoint to avoid a 50%-power flicker.
pub fn blend(a: BlendPoint, b: BlendPoint, t: f32) -> BlendPoint {
    let t = t.clamp(0.0, 1.0);
    BlendPoint {
        on: if t < 0.5 { a.on } else { b.on },
        brightness: lerp_u8(a.brightness, b.brightness, t),
        color: oklab_lerp(a.color, b.color, t),
    }
}

/// Perceptual colour interpolation in Oklab space.
pub fn oklab_lerp(a: ColorXy, b: ColorXy, t: f32) -> ColorXy {
    let t = t.clamp(0.0, 1.0);
    let lab_a = xy_to_oklab(a);
    let lab_b = xy_to_oklab(b);
    let blended = Oklab::new(
        lab_a.l + (lab_b.l - lab_a.l) * t,
        lab_a.a + (lab_b.a - lab_a.a) * t,
        lab_a.b + (lab_b.b - lab_a.b) * t,
    );
    oklab_to_xy(blended)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn lerp_u8_endpoints() {
        assert_eq!(lerp_u8(0, 100, 0.0), 0);
        assert_eq!(lerp_u8(0, 100, 1.0), 100);
        assert_eq!(lerp_u8(80, 100, 0.5), 90);
    }

    #[test]
    fn lerp_u8_clamps_t() {
        assert_eq!(lerp_u8(0, 100, -1.0), 0);
        assert_eq!(lerp_u8(0, 100, 2.0), 100);
    }

    #[test]
    fn mireds_to_xy_warm_end() {
        // 500 mireds = 2000 K — first row of table.
        let xy = mireds_to_xy(500);
        assert!(approx(xy.x, 0.5267, 1e-4));
        assert!(approx(xy.y, 0.4133, 1e-4));
    }

    #[test]
    fn mireds_to_xy_cool_end() {
        // ~154 mireds = 6500 K — last row of table.
        let xy = mireds_to_xy(154);
        assert!(approx(xy.x, 0.3135, 1e-3));
        assert!(approx(xy.y, 0.3237, 1e-3));
    }

    #[test]
    fn mireds_to_xy_2700k() {
        let xy = mireds_to_xy(370); // 2700 K
        assert!(approx(xy.x, 0.4578, 5e-3));
        assert!(approx(xy.y, 0.4101, 5e-3));
    }

    #[test]
    fn mireds_to_xy_4000k() {
        let xy = mireds_to_xy(250); // 4000 K
        assert!(approx(xy.x, 0.3805, 5e-3));
        assert!(approx(xy.y, 0.3768, 5e-3));
    }

    #[test]
    fn blend_endpoints_exact() {
        let a = BlendPoint {
            on: true,
            brightness: 100,
            color: ColorXy::new(0.4, 0.5),
        };
        let b = BlendPoint {
            on: true,
            brightness: 200,
            color: ColorXy::new(0.3, 0.3),
        };
        assert_eq!(blend(a, b, 0.0).brightness, 100);
        assert_eq!(blend(a, b, 1.0).brightness, 200);
    }

    #[test]
    fn blend_brightness_monotone() {
        let a = BlendPoint {
            on: true,
            brightness: 0,
            color: ColorXy::new(0.4, 0.5),
        };
        let b = BlendPoint {
            on: true,
            brightness: 255,
            color: ColorXy::new(0.3, 0.3),
        };
        let prev = blend(a, b, 0.0).brightness;
        let mid = blend(a, b, 0.5).brightness;
        let end = blend(a, b, 1.0).brightness;
        assert!(prev < mid && mid < end);
    }

    #[test]
    fn blend_on_steps_at_midpoint() {
        let a = BlendPoint {
            on: false,
            brightness: 0,
            color: ColorXy::new(0.4, 0.5),
        };
        let b = BlendPoint {
            on: true,
            brightness: 100,
            color: ColorXy::new(0.3, 0.3),
        };
        assert!(!blend(a, b, 0.4).on);
        assert!(blend(a, b, 0.5).on);
        assert!(blend(a, b, 0.6).on);
    }

    #[test]
    fn red_to_blue_passes_through_purple_not_grey() {
        // Acid test of the Oklab pipeline: lerping red ↔ blue in plain xy
        // produces a desaturated grey midpoint; in Oklab it produces purple.
        let red = ColorXy::new(0.675, 0.322);
        let blue = ColorXy::new(0.167, 0.040);
        let mid = oklab_lerp(red, blue, 0.5);

        // Grey is roughly the D65 whitepoint at xy ≈ (0.31, 0.33).
        let grey_dist = ((mid.x - 0.31).powi(2) + (mid.y - 0.33).powi(2)).sqrt();
        // Magenta on the line red↔blue lives roughly around (0.32, 0.15) once
        // mapped back from Oklab.
        let magenta_dist = ((mid.x - 0.32).powi(2) + (mid.y - 0.15).powi(2)).sqrt();
        assert!(
            magenta_dist < grey_dist,
            "Oklab midpoint should be closer to magenta than to grey — got xy=({:.3}, {:.3}), grey_dist={:.4}, magenta_dist={:.4}",
            mid.x,
            mid.y,
            grey_dist,
            magenta_dist
        );
    }

    #[test]
    fn xy_oklab_roundtrip_preserves_input() {
        // A few representative chromaticities should survive a round trip.
        for c in [
            ColorXy::new(0.4, 0.5),
            ColorXy::new(0.31, 0.33),
            ColorXy::new(0.675, 0.322),
            ColorXy::new(0.167, 0.040),
        ] {
            let back = oklab_to_xy(xy_to_oklab(c));
            assert!(
                approx(back.x, c.x, 1e-3) && approx(back.y, c.y, 1e-3),
                "round trip drifted: in=({:.4}, {:.4}) out=({:.4}, {:.4})",
                c.x,
                c.y,
                back.x,
                back.y
            );
        }
    }
}
