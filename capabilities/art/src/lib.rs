//! Frame TV art-display capability — drives a fullscreen image viewer process
//! on this node (mirrors `capability-reaper`'s shape: manages an external
//! process rather than talking to Zigbee/MQTT). See
//! plans/frame-tv-art-display.md for the full design. `ArtShow` (v1) is a
//! single fullscreen image on demand; `ArtBatch` hands the node a whole set
//! to cycle through *locally*, one every `interval_secs`, with no further
//! coordinator round-trip per image — the general/default slideshow. A
//! specific `ArtShow` always takes precedence over an in-progress batch
//! (see `batch_generation`), and the coordinator decides when to hand a
//! fresh `ArtBatch` back (after a specific search has gone idle) — this
//! crate has no opinion on *when* to switch modes, only on cycling whatever
//! it was last told to.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use capability_core::Capability;
use image::{GenericImageView, Rgb, RgbImage, imageops};
use shared::{ArtBatchRequest, ArtShowRequest, ArtStatusReport, MeshMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};

/// A slow/hung source URL shouldn't hang an ArtShow request indefinitely —
/// the coordinator would just wait forever for the ArtStatus reply. 45s
/// (bumped from 15s after a live multi-MB original-resolution Met image
/// timed out mid-download on pi2's connection) gives real museum-API image
/// sizes enough headroom without being unbounded.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(45);
/// How many times to retry a failed download before giving up — confirmed
/// live that a node's WiFi link can intermittently drop a request or two
/// even when otherwise healthy (a working connection moments before and
/// after). Without a retry, one bad request used to mean the whole
/// slideshow sat frozen on the last successfully-shown image until the next
/// scheduled advance — many seconds, or on a busy rotation minutes — later,
/// with no visible explanation. A short, bounded retry covers the common
/// "one bad request" case without materially delaying a genuinely
/// unreachable host.
const DOWNLOAD_RETRIES: u32 = 3;
const DOWNLOAD_RETRY_DELAY: Duration = Duration::from_millis(750);

// ── Matte/border compositing ────────────────────────────────────────────────
// The real Frame TV's "art mode" look (a picture-frame border/mat around the
// image) only ever gets applied by Samsung's own Art Mode content pipeline —
// confirmed against a real showroom unit that a plain HDMI input signal (our
// approach, deliberately, to avoid that whole ecosystem — see plan §7) shows
// bare edge-to-edge, no matte, no border. Doing it ourselves here, once per
// downloaded image, is what plan §5's "generate a matte/border overlay to
// mimic the Frame's art-mode look" always called for — this is that step.
// All env-overridable so the look can be tuned without a rebuild.

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_f32(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_hex_rgb(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
    ])
}

fn env_rgb(name: &str, default: [u8; 3]) -> [u8; 3] {
    std::env::var(name)
        .ok()
        .and_then(|v| parse_hex_rgb(&v))
        .unwrap_or(default)
}

/// Fill a solid rectangle, clipped to the canvas bounds — used for the
/// "infill" panel that soaks up whatever gap contain-fit leaves between the
/// artwork and the frame's fixed interior boundary, coloured to match the
/// artwork's own edge rather than a flat fixed colour (see `compose_matte`
/// and `avg_row`/`avg_col`). Deliberately un-bevelled: that treatment
/// belongs to the frame itself (`draw_ring`, below), which never changes
/// size regardless of the artwork's own aspect ratio.
fn fill_rect(canvas: &mut RgbImage, x0: u32, y0: u32, w: u32, h: u32, rgb: [u8; 3]) {
    if w == 0 || h == 0 {
        return;
    }
    let color = Rgb(rgb);
    let (cw, ch) = canvas.dimensions();
    let x_end = (x0 + w).min(cw);
    let y_end = (y0 + h).min(ch);
    for yy in y0..y_end {
        for xx in x0..x_end {
            canvas.put_pixel(xx, yy, color);
        }
    }
}

/// Draw a rectangular outline (not filled) of `thickness` pixels, clipped to
/// the canvas bounds — the frame's own thin "fillet" line, always exactly
/// `frame_px` wide regardless of the artwork's aspect ratio (see
/// `compose_matte`).
fn draw_ring(
    canvas: &mut RgbImage,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    thickness: u32,
    rgb: [u8; 3],
) {
    if thickness == 0 || w == 0 || h == 0 {
        return;
    }
    let color = Rgb(rgb);
    let (cw, ch) = canvas.dimensions();
    let x_end = (x0 + w).min(cw);
    let y_end = (y0 + h).min(ch);
    for yy in y0..y_end {
        for xx in x0..x_end {
            let near_left = xx - x0 < thickness;
            let near_right = (x0 + w).saturating_sub(xx) <= thickness;
            let near_top = yy - y0 < thickness;
            let near_bottom = (y0 + h).saturating_sub(yy) <= thickness;
            if near_left || near_right || near_top || near_bottom {
                canvas.put_pixel(xx, yy, color);
            }
        }
    }
}

/// All tunable, env-overridable at the real call site (`MatteConfig::from_env`)
/// — kept as explicit fields rather than read directly from env vars inside
/// `compose_matte` so tests can pass small, fixed values instead of mutating
/// global process env state (which would race across parallel test threads).
struct MatteConfig {
    canvas_w: u32,
    canvas_h: u32,
    matte_percent: f32,
    /// Extra width added to the side (left/right) margins on top of the
    /// base matte, as a percentage of the base side margin — visually, an
    /// even-all-round mat still reads as slightly narrower at the sides
    /// than top/bottom on a real TV, so this nudges it back out. 0 = sides
    /// match top/bottom exactly.
    side_margin_boost_percent: f32,
    matte_rgb: [u8; 3],
    frame_rgb: [u8; 3],
    frame_px: u32,
    /// Strength (0-100) of the translucent white glaze blended over the
    /// artwork itself — see `apply_glaze`. 0 = off, matching today's plain
    /// display.
    glaze_percent: f32,
    glaze_rgb: [u8; 3],
    /// Strength (0-100) of the same translucent glaze, applied separately to
    /// the mat/frame border instead of the artwork — lets the two be washed
    /// out by different amounts, since a mat and a photo don't necessarily
    /// want the same glaze strength. Shares `glaze_rgb` as its colour.
    border_glaze_percent: f32,
    /// Overall brightness multiplier (percent) applied to the *entire*
    /// composed frame — artwork, mat, and border line alike — as the very
    /// last step before encoding. 100 = unchanged; below 100 dims
    /// everything uniformly, above 100 brightens it.
    brightness_percent: f32,
}

impl MatteConfig {
    fn from_env() -> Self {
        Self {
            canvas_w: env_u32("ART_CANVAS_WIDTH", 1920),
            canvas_h: env_u32("ART_CANVAS_HEIGHT", 1080),
            matte_percent: env_f32("ART_MATTE_PERCENT", 7.0),
            side_margin_boost_percent: env_f32("ART_SIDE_MARGIN_BOOST", 0.0),
            // A warm, off-white museum-mat colour by default — not stark
            // white, which would look harsher next to most artwork than a
            // real paper mat.
            matte_rgb: env_rgb("ART_MATTE_COLOR", [0xED, 0xE7, 0xDA]),
            frame_rgb: env_rgb("ART_FRAME_COLOR", [0x2B, 0x2B, 0x2B]),
            frame_px: env_u32("ART_FRAME_THICKNESS", 3),
            glaze_percent: env_f32("ART_GLAZE_PERCENT", 0.0),
            glaze_rgb: env_rgb("ART_GLAZE_COLOR", [0xFF, 0xFF, 0xFF]),
            border_glaze_percent: env_f32("ART_BORDER_GLAZE_PERCENT", 0.0),
            brightness_percent: env_f32("ART_BRIGHTNESS_PERCENT", 100.0),
        }
    }
}

/// Normalized (0..1 within their own axis, scaled by that edge's own border
/// thickness) distance from `(x,y)` to each of the canvas's four edges —
/// real frame moulding is four straight strips cut at 45° and joined at
/// each corner, and these four distances are what both the mitre seam and
/// the bevel gradient are built from.
fn border_edge_fracs(
    x: u32,
    y: u32,
    canvas_w: u32,
    canvas_h: u32,
    bw_x: u32,
    bw_y: u32,
) -> (f32, f32, f32, f32) {
    let left = x as f32 / bw_x.max(1) as f32;
    let right = canvas_w.saturating_sub(1).saturating_sub(x) as f32 / bw_x.max(1) as f32;
    let top = y as f32 / bw_y.max(1) as f32;
    let bottom = canvas_h.saturating_sub(1).saturating_sub(y) as f32 / bw_y.max(1) as f32;
    (top, bottom, left, right)
}

/// How "lit" (as opposed to shadowed) a border pixel is, continuously from
/// 0 (fully shadowed) to 1 (fully lit). Finds the two *actually nearest*
/// edges (not all four — comparing e.g. a far-off top distance against a
/// close left one would leak vertical position into a purely horizontal
/// reading) and, only when they disagree on lit/shadow (a real corner),
/// blends between them over `BEVEL_TRANSITION`; beyond that gap the pixel
/// is solidly whichever of the two it's actually nearest to, exactly
/// matching the old discrete wedge classification away from every corner.
const BEVEL_TRANSITION: f32 = 0.15;

fn border_lit_amount(top: f32, bottom: f32, left: f32, right: f32) -> f32 {
    let mut candidates = [(top, true), (bottom, false), (left, true), (right, false)];
    candidates.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let (nearest, nearest_lit) = candidates[0];
    let (second, second_lit) = candidates[1];
    if nearest_lit == second_lit {
        return if nearest_lit { 1.0 } else { 0.0 };
    }
    // The two nearest edges disagree — a genuine corner. Blend from 0.5
    // (right at the tie) up to fully `nearest_lit`'s value as the gap grows
    // to `BEVEL_TRANSITION`.
    let t = ((second - nearest) / BEVEL_TRANSITION).clamp(0.0, 1.0);
    let nearest_weight = 0.5 + 0.5 * t;
    if nearest_lit {
        nearest_weight
    } else {
        1.0 - nearest_weight
    }
}

/// Strength of the bevel effect — how far each channel moves toward white
/// (fully lit) or black (fully shadowed), as a fraction of its remaining
/// headroom. Proportional rather than a flat per-channel delta so a channel
/// already near white/black eases smoothly toward the target instead of
/// clipping flat against it (a flat +18 add, for instance, already pins the
/// default matte colour's red channel — 237 — to the 255 ceiling).
const BEVEL_STRENGTH: f32 = 0.14;
/// How close the nearest and second-nearest edge distances (from
/// `border_edge_fracs`, normalized 0..1) need to be to count as sitting on a
/// mitre seam rather than solidly inside one wedge.
const SEAM_EPSILON: f32 = 0.035;
/// Mitre seam colour — a crisp near-black line reads as a real cut
/// regardless of the configured frame/matte colours.
const SEAM_RGB: [u8; 3] = [0x15, 0x15, 0x15];

/// Blend `rgb` toward white and toward black by `BEVEL_STRENGTH` of its
/// remaining headroom each way, then mix those two results by `lit_amount`
/// (1 = fully lit, 0 = fully shadowed, anything between blends smoothly
/// rather than flipping abruptly at a wedge boundary). Always strictly
/// between the original colour and whichever target it's blending toward,
/// so it can never clip the way a flat additive/subtractive delta can.
fn bevel_shade(rgb: [u8; 3], lit_amount: f32) -> [u8; 3] {
    let mut lit = rgb;
    let mut shadow = rgb;
    for (lit_channel, shadow_channel) in lit.iter_mut().zip(shadow.iter_mut()) {
        let current = *lit_channel as f32;
        *lit_channel = (current + (255.0 - current) * BEVEL_STRENGTH).round() as u8;
        *shadow_channel = (current - current * BEVEL_STRENGTH).round() as u8;
    }
    let mut out = [0u8; 3];
    for (out_channel, (lit_channel, shadow_channel)) in
        out.iter_mut().zip(lit.iter().zip(shadow.iter()))
    {
        *out_channel = (*lit_channel as f32 * lit_amount
            + *shadow_channel as f32 * (1.0 - lit_amount))
            .round() as u8;
    }
    out
}

/// Blend a translucent white (or `glaze_rgb`) wash over every pixel of the
/// artwork itself — the soft, matte "glare"/veil look Samsung's own Frame TV
/// applies over displayed art, distinct from (and orthogonal to) the mat
/// border: this touches only the picture's own pixels in place, so it never
/// changes the artwork's size the way widening the mat does.
fn apply_glaze(img: &mut RgbImage, percent: f32, glaze_rgb: [u8; 3]) {
    if percent <= 0.0 {
        return;
    }
    let alpha = (percent / 100.0).clamp(0.0, 1.0);
    for pixel in img.pixels_mut() {
        for (channel, glaze_channel) in pixel.0.iter_mut().zip(glaze_rgb) {
            *channel =
                (*channel as f32 * (1.0 - alpha) + glaze_channel as f32 * alpha).round() as u8;
        }
    }
}

/// Same wash as `apply_glaze`, but over the mat/frame border of `canvas`
/// instead of the artwork — every pixel outside the artwork rect
/// `(ix0,iy0)..(ix1,iy1)` gets blended toward `glaze_rgb`, independent of the
/// artwork's own glaze strength.
fn apply_border_glaze(
    canvas: &mut RgbImage,
    ix0: u32,
    iy0: u32,
    ix1: u32,
    iy1: u32,
    percent: f32,
    glaze_rgb: [u8; 3],
) {
    if percent <= 0.0 {
        return;
    }
    let alpha = (percent / 100.0).clamp(0.0, 1.0);
    let (cw, ch) = canvas.dimensions();
    for y in 0..ch {
        for x in 0..cw {
            if x >= ix0 && x < ix1 && y >= iy0 && y < iy1 {
                continue;
            }
            let mut blended = canvas.get_pixel(x, y).0;
            for (channel, glaze_channel) in blended.iter_mut().zip(glaze_rgb) {
                *channel =
                    (*channel as f32 * (1.0 - alpha) + glaze_channel as f32 * alpha).round() as u8;
            }
            canvas.put_pixel(x, y, Rgb(blended));
        }
    }
}

/// Scale every channel of every pixel in the fully-composed frame by
/// `percent` — applied last, over artwork, mat, and frame border alike, so
/// dimming the display dims the whole thing uniformly rather than needing a
/// separate darkening pass per region.
fn apply_brightness(canvas: &mut RgbImage, percent: f32) {
    if (percent - 100.0).abs() < f32::EPSILON {
        return;
    }
    let factor = (percent / 100.0).max(0.0);
    for pixel in canvas.pixels_mut() {
        for channel in pixel.0.iter_mut() {
            *channel = (*channel as f32 * factor).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Bevel-shade (lit top/left, shadowed bottom/right — the standard trick for
/// reading a flat 2D border as a physically moulded 3D frame) and draw the
/// mitre seam lines, over every already-painted border pixel of `canvas`
/// outside the artwork rect `(ix0,iy0)..(ix1,iy1)` (left alone here; the
/// artwork gets overlaid on top afterwards regardless).
fn apply_bevel_and_seams(
    canvas: &mut RgbImage,
    ix0: u32,
    iy0: u32,
    ix1: u32,
    iy1: u32,
    bw_x: u32,
    bw_y: u32,
) {
    let (cw, ch) = canvas.dimensions();
    for y in 0..ch {
        for x in 0..cw {
            if x >= ix0 && x < ix1 && y >= iy0 && y < iy1 {
                continue;
            }
            let (top, bottom, left, right) = border_edge_fracs(x, y, cw, ch, bw_x, bw_y);
            let mut sorted = [top, bottom, left, right];
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let shaded = if sorted[1] - sorted[0] < SEAM_EPSILON {
                SEAM_RGB
            } else {
                let lit_amount = border_lit_amount(top, bottom, left, right);
                bevel_shade(canvas.get_pixel(x, y).0, lit_amount)
            };
            canvas.put_pixel(x, y, Rgb(shaded));
        }
    }
}

/// Scale `(sw, sh)` down/up preserving its own aspect ratio (never
/// distorted, never cropped) so it fits entirely within `(available_w,
/// available_h)` — the artwork's actual displayed size. Pure/pixel-free so
/// the aspect-ratio-preserving math is cheap to test directly.
fn contain_fit(sw: u32, sh: u32, available_w: u32, available_h: u32) -> (u32, u32) {
    let scale = (available_w as f32 / sw as f32).min(available_h as f32 / sh as f32);
    let pic_w = ((sw as f32) * scale)
        .round()
        .max(1.0)
        .min(available_w as f32) as u32;
    let pic_h = ((sh as f32) * scale)
        .round()
        .max(1.0)
        .min(available_h as f32) as u32;
    (pic_w, pic_h)
}

/// Average colour of one horizontal row of `img` — used to sink the
/// infill's own colour to whatever colour the artwork's own edge is (see
/// `compose_matte`), rather than a flat fixed colour, so it reads as the
/// picture bleeding out into the gap instead of a visibly separate panel.
fn avg_row(img: &RgbImage, y: u32) -> [u8; 3] {
    let w = img.width();
    let mut sum = [0u64; 3];
    for x in 0..w {
        let p = img.get_pixel(x, y).0;
        for (s, c) in sum.iter_mut().zip(p) {
            *s += c as u64;
        }
    }
    let count = (w as u64).max(1);
    [
        (sum[0] / count) as u8,
        (sum[1] / count) as u8,
        (sum[2] / count) as u8,
    ]
}

/// Average colour of one vertical column of `img` — see `avg_row`.
fn avg_col(img: &RgbImage, x: u32) -> [u8; 3] {
    let h = img.height();
    let mut sum = [0u64; 3];
    for y in 0..h {
        let p = img.get_pixel(x, y).0;
        for (s, c) in sum.iter_mut().zip(p) {
            *s += c as u64;
        }
    }
    let count = (h as u64).max(1);
    [
        (sum[0] / count) as u8,
        (sum[1] / count) as u8,
        (sum[2] / count) as u8,
    ]
}

/// Decode `bytes`, composite onto a matte-coloured canvas at the panel's
/// native resolution — a "contain" fit: scaled preserving the source's own
/// aspect ratio to fit entirely within the frame's fixed interior, the whole
/// artwork always visible, never cropped or distorted. Two earlier
/// approaches were tried and confirmed live to look wrong: stretching to
/// exactly fill the interior (visibly distorted proportions once enough
/// differently-shaped Met images had cycled through), then a "cover"
/// crop-to-fill (no distortion, but cropped a large fraction off portrait-
/// oriented artwork whenever the interior's own aspect ratio was very
/// different from the artwork's) — and a version after that which grew the
/// bevelled/mitred frame itself to absorb the slack, which read as the frame
/// changing size per image rather than staying the fixed, familiar assembly
/// it always was. This version keeps the frame (mat width, line, bevel,
/// mitre seams) exactly fixed-size regardless of the artwork's aspect ratio,
/// and instead gives whatever gap is left over between the artwork and the
/// frame's interior an infill coloured to match the artwork's own edge — see
/// `fill_rect`'s, `avg_row`'s, and `draw_ring`'s doc comments. CPU-bound and
/// synchronous by design — the caller runs this via `spawn_blocking` rather
/// than blocking the async runtime.
fn compose_matte(bytes: &[u8], config: &MatteConfig) -> Result<Vec<u8>, String> {
    let source =
        image::load_from_memory(bytes).map_err(|e| format!("could not decode image: {e}"))?;
    let (sw, sh) = source.dimensions();
    if sw == 0 || sh == 0 {
        return Err("decoded image has zero width or height".into());
    }
    let source_rgb = source.to_rgb8();

    let mut canvas = RgbImage::from_pixel(config.canvas_w, config.canvas_h, Rgb(config.matte_rgb));

    // A single pixel padding (derived from the wider dimension) applied to
    // both axes — a real picture-frame mat is an even width all the way
    // round, not a percentage of whichever axis happens to be shorter, which
    // on a 16:9 canvas would make the top/bottom mat visibly thinner than
    // the sides.
    let pad = ((config.canvas_w as f32) * config.matte_percent / 100.0) as u32;
    let pad_x = (pad as f32 * (1.0 + config.side_margin_boost_percent / 100.0)) as u32;
    let pad_y = pad;
    let fit_w = config.canvas_w.saturating_sub(pad_x * 2).max(1);
    let fit_h = config.canvas_h.saturating_sub(pad_y * 2).max(1);

    // The frame's own interior — offset_x/offset_y/fit_w/fit_h — never
    // changes size regardless of the artwork's aspect ratio; the mat width,
    // frame line, bevel and mitre seams below are all still computed exactly
    // as they always have been, fixed. Only where *inside* that interior the
    // artwork itself sits varies, since it's now contain-fit (preserving its
    // own aspect ratio, never cropped or distorted) rather than stretched or
    // cropped to fill the interior exactly.
    let offset_x = pad_x;
    let offset_y = pad_y;

    draw_ring(
        &mut canvas,
        offset_x.saturating_sub(config.frame_px),
        offset_y.saturating_sub(config.frame_px),
        fit_w + config.frame_px * 2,
        fit_h + config.frame_px * 2,
        config.frame_px,
        config.frame_rgb,
    );
    apply_border_glaze(
        &mut canvas,
        offset_x,
        offset_y,
        offset_x + fit_w,
        offset_y + fit_h,
        config.border_glaze_percent,
        config.glaze_rgb,
    );
    apply_bevel_and_seams(
        &mut canvas,
        offset_x,
        offset_y,
        offset_x + fit_w,
        offset_y + fit_h,
        pad_x + config.frame_px,
        pad_y + config.frame_px,
    );

    let (pic_w, pic_h) = contain_fit(sw, sh, fit_w, fit_h);
    let mut resized = imageops::resize(&source_rgb, pic_w, pic_h, imageops::FilterType::Lanczos3);
    apply_glaze(&mut resized, config.glaze_percent, config.glaze_rgb);
    let border_x = (fit_w - pic_w) / 2;
    let border_y = (fit_h - pic_h) / 2;
    let pic_offset_x = offset_x + border_x;
    let pic_offset_y = offset_y + border_y;
    // Whatever gap contain-fit leaves between the artwork and the frame's
    // fixed interior — zero on the axis the artwork actually fills, however
    // much its aspect ratio calls for on the other — gets filled with a
    // colour sampled from the artwork's own edge (averaged along that edge,
    // post-glaze) rather than a flat fixed colour, so it reads as the
    // picture bleeding out into the gap. contain_fit only ever leaves slack
    // on one axis at a time, never both, so at most one of these branches
    // does anything. Deliberately painted after the frame/bevel/mitre pass
    // above (which only touches *outside* this interior) and before the
    // artwork overlay, so it never disturbs the frame's own fixed-size
    // treatment.
    if border_y > 0 {
        let top_color = avg_row(&resized, 0);
        let bottom_color = avg_row(&resized, pic_h - 1);
        fill_rect(&mut canvas, offset_x, offset_y, fit_w, border_y, top_color);
        fill_rect(
            &mut canvas,
            offset_x,
            offset_y + fit_h - border_y,
            fit_w,
            border_y,
            bottom_color,
        );
    } else if border_x > 0 {
        let left_color = avg_col(&resized, 0);
        let right_color = avg_col(&resized, pic_w - 1);
        fill_rect(&mut canvas, offset_x, offset_y, border_x, fit_h, left_color);
        fill_rect(
            &mut canvas,
            offset_x + fit_w - border_x,
            offset_y,
            border_x,
            fit_h,
            right_color,
        );
    }
    imageops::overlay(
        &mut canvas,
        &resized,
        i64::from(pic_offset_x),
        i64::from(pic_offset_y),
    );
    apply_brightness(&mut canvas, config.brightness_percent);

    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(canvas)
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| format!("could not encode composed image: {e}"))?;
    Ok(out)
}

/// After killing a stuck/crashed viewer (the fallback path — see `show`),
/// give the kernel a moment to actually release the display before the
/// replacement opens it — confirmed against the real node that `pkill`
/// returning doesn't guarantee the killed process has finished releasing its
/// file descriptors yet.
const RESPAWN_SETTLE: Duration = Duration::from_millis(150);

/// How long to give the old viewer to exit cleanly on SIGTERM before
/// escalating to SIGKILL — see `kill_existing_viewer`.
const KILL_GRACE_PERIOD: Duration = Duration::from_millis(200);

/// How often to check whether `mpv` has created its IPC socket file yet —
/// see `launch_viewer`'s doc comment for why this is polled rather than a
/// fixed sleep.
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Give up waiting for the socket after this long — a launch that's this
/// broken will fail loudly on the first IPC attempt anyway (see `show`'s
/// fallback path) rather than hang forever.
const SOCKET_POLL_TIMEOUT: Duration = Duration::from_secs(5);

/// A `loadfile` command alone doesn't visibly redraw a still image once mpv
/// is just holding it frozen (its internal state/IPC events update
/// immediately, but the screen doesn't) — confirmed live on the real node. A
/// no-op absolute seek right after forces the redraw, but a seek sent before
/// mpv's own `playback-restart` event confirms the load actually landed can
/// silently no-op — also confirmed live, a fixed few-hundred-ms guess wasn't
/// reliably long enough. This bounds how long to wait for that event before
/// giving up and sending the seek anyway.
const IPC_REDRAW_TIMEOUT: Duration = Duration::from_secs(3);

pub struct ArtCapability {
    node_id: String,
    /// Serializes show() calls — the persistent `mpv` process is controlled
    /// over its IPC socket rather than via a held child handle (see `show`'s
    /// doc comment), so this just stops two overlapping ArtShow requests
    /// from racing on the launch-or-control decision.
    show_lock: Arc<Mutex<()>>,
    current_url: Arc<Mutex<Option<String>>>,
    /// Bumped on every `ArtShow` or `ArtBatch` received — a running batch
    /// loop captures its own value at spawn time and checks it before each
    /// step, exiting quietly the moment something newer supersedes it
    /// (either a specific show or a fresher batch). No coordinator
    /// involvement needed for this — it's purely local sequencing.
    generation: Arc<AtomicU64>,
}

impl ArtCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            show_lock: Arc::new(Mutex::new(())),
            current_url: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    fn viewer_bin() -> String {
        std::env::var("ART_VIEWER_BIN").unwrap_or_else(|_| "mpv".into())
    }

    /// One persistent viewer process holds the display for as long as the
    /// node's up — every image change after the first goes through this
    /// socket rather than killing/relaunching the process (see `show`'s doc
    /// comment for why).
    fn ipc_socket_path() -> PathBuf {
        if let Ok(p) = std::env::var("ART_MPV_IPC_SOCKET") {
            return PathBuf::from(p);
        }
        Self::cache_dir().join("mpv.sock")
    }

    fn cache_dir() -> PathBuf {
        if let Ok(d) = std::env::var("ART_CACHE_DIR") {
            return PathBuf::from(d);
        }
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ai-mesh")
            .join("art-cache")
    }

    /// Ping-pongs between two filenames rather than always overwriting one
    /// fixed name — confirmed live that `mpv` appears to cache decoded
    /// output keyed by path, so re-`loadfile`-ing the exact same path with
    /// freshly-written bytes underneath can just redisplay whatever it last
    /// showed for that path instead of genuinely re-reading it. Alternating
    /// guarantees consecutive shows always hand `mpv` a path distinct from
    /// the one it's currently displaying, forcing a real reload, while still
    /// bounding disk use to two files rather than growing unbounded over the
    /// agent's uptime.
    fn cache_path(generation: u64) -> PathBuf {
        Self::cache_dir().join(format!("current_art_{}", generation % 2))
    }

    /// Where a batch's images live, one file per index — cleared and
    /// recreated on every new `ArtBatch` (see `handle_batch`) so a smaller
    /// replacement batch doesn't leave a previous one's stale extra files
    /// sitting around on limited SD-card storage.
    fn batch_dir() -> PathBuf {
        Self::cache_dir().join("batch")
    }

    fn batch_item_path(index: usize) -> PathBuf {
        Self::batch_dir().join(index.to_string())
    }

    fn http_client() -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .timeout(DOWNLOAD_TIMEOUT)
            // Some hosts (Wikimedia Commons confirmed live) reject reqwest's
            // default User-Agent with a 403 — a descriptive one identifying
            // this as a personal, non-commercial project satisfies their
            // API etiquette policy and fixes the block.
            .user_agent("ai-mesh-frame-tv/1.0 (personal home project; non-commercial)")
            .build()
            .map_err(|e| format!("could not build HTTP client: {e}"))
    }

    /// One GET attempt, no retry — the actual network fetch, split out so
    /// `download_to` can retry it without re-running the (cheap, local)
    /// directory-creation step each time.
    async fn fetch_once(url: &str) -> Result<Vec<u8>, String> {
        let resp = Self::http_client()?
            .get(url)
            .send()
            .await
            .map_err(|e| format!("download failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("download failed: HTTP {}", resp.status()));
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("download failed reading body: {e}"))
    }

    /// Fetch `url` and write it to `dest`, creating `dest`'s parent dir if
    /// needed. Shared by the single-image path (`cache_path()`) and the
    /// batch path (`batch_item_path(i)`).
    async fn download_to(url: &str, dest: &Path) -> Result<(), String> {
        if let Some(dir) = dest.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| format!("could not create cache dir {}: {e}", dir.display()))?;
        }
        let mut last_err = String::new();
        let mut bytes = None;
        for attempt in 1..=DOWNLOAD_RETRIES {
            if attempt > 1 {
                tokio::time::sleep(DOWNLOAD_RETRY_DELAY).await;
            }
            match Self::fetch_once(url).await {
                Ok(b) => {
                    bytes = Some(b);
                    break;
                }
                Err(e) => {
                    warn!(error = %e, url, attempt, of = DOWNLOAD_RETRIES, "art: download attempt failed");
                    last_err = e;
                }
            }
        }
        let bytes = bytes.ok_or(last_err)?;

        // CPU-bound (decode/resize/re-encode) — spawn_blocking rather than
        // tying up the async runtime's worker threads for it. Falls back to
        // the raw downloaded bytes on any failure (a non-image content type,
        // a decode error, etc.) rather than leaving nothing to show at all.
        let for_processing = bytes.clone();
        let composed = tokio::task::spawn_blocking(move || {
            compose_matte(&for_processing, &MatteConfig::from_env())
        })
        .await
        .map_err(|e| format!("matte compositing task panicked: {e}"))?;
        let final_bytes: Vec<u8> = match composed {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, url = %url, "art: matte compositing failed, showing raw image instead");
                bytes.to_vec()
            }
        };

        tokio::fs::write(dest, &final_bytes)
            .await
            .map_err(|e| format!("could not write {}: {e}", dest.display()))?;
        Ok(())
    }

    async fn download(url: &str, generation: u64) -> Result<PathBuf, String> {
        let path = Self::cache_path(generation);
        Self::download_to(url, &path).await?;
        Ok(path)
    }

    /// Show `path`, reusing an already-running viewer over its IPC socket
    /// when there is one rather than killing and relaunching it.
    ///
    /// **Why this replaced the old kill-and-relaunch-`fbi`-per-image
    /// design**: that approach tore down and rebuilt the console's graphics
    /// mode on *every single image change*, which reads as a visible
    /// black-screen blink each time — confirmed live once a real TV was
    /// finally wired up to watch it (see plans/frame-tv-art-display.md).
    /// `mpv --vo=drm` holds the display continuously across many images
    /// instead: launch it once, then drive every later change through its
    /// JSON IPC socket (`--input-ipc-server`). The original objection to
    /// mpv (Debian's package drags in ~600 MB of GTK/X11 deps as unused
    /// linked dependencies, a bad fit for a 512 MB Pi Zero 2 W) no longer
    /// applies now that the Pi Zero migration has been dropped in favour of
    /// staying on a full Pi 4 — 600 MB of SD-card space there is a
    /// non-issue. `feh`/`pqiv` are still ruled out (X11-only, no desktop on
    /// this Lite install); mpv's `--vo=drm` needs no X11 or Wayland session
    /// and was confirmed live to work under the same `vc4-fkms-v3d` overlay
    /// `fbi` already required (see `docs/frame-tv-setup.md`), so no config
    /// change was needed switching from one to the other.
    ///
    /// **Confirmed live, two non-obvious gotchas**: (1) a bare `loadfile`
    /// command doesn't visibly redraw anything once mpv is just holding a
    /// frozen still image — its internal state and IPC events update
    /// immediately, but the actual screen doesn't, until a no-op absolute
    /// `seek` is sent right after (`send_ipc_show` below); (2) mpv creates
    /// its IPC socket file as `root` (it's launched via `sudo -n`, same
    /// NOPASSWD rationale as before, see `scripts/install-node-linux.sh`) —
    /// this process runs as a regular user, so the socket gets `chmod`'d
    /// open immediately after launch rather than needing every later
    /// message wrapped in its own `sudo` call.
    async fn show(show_lock: &Mutex<()>, path: &Path) -> Result<(), String> {
        let _guard = show_lock.lock().await;
        if Self::viewer_running().await {
            match Self::send_ipc_show(path).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!(error = %e, "art: mpv IPC control failed, restarting viewer fresh");
                    Self::kill_existing_viewer().await;
                    tokio::time::sleep(RESPAWN_SETTLE).await;
                }
            }
        }
        Self::launch_viewer(path).await
    }

    /// First-time (or fallback-after-a-dead-socket) launch: start the
    /// viewer fresh on `path` and open up its IPC socket for later control.
    /// `--idle=yes` keeps it alive with nothing loaded rather than exiting,
    /// though in practice a path is always given up front here too.
    async fn launch_viewer(path: &Path) -> Result<(), String> {
        let socket = Self::ipc_socket_path();
        // A stale socket file from a crashed prior instance would make mpv
        // fail to bind a fresh one on top of it.
        let _ = tokio::fs::remove_file(&socket).await;
        let mut child = Command::new("sudo")
            .arg("-n")
            .arg(Self::viewer_bin())
            .arg("--vo=drm")
            .arg("--fullscreen")
            .arg("--image-display-duration=inf")
            .arg("--idle=yes")
            .arg(format!("--input-ipc-server={}", socket.display()))
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to launch {}: {e}", Self::viewer_bin()))?;
        // Unlike `fbi` (which re-forks and detaches, so even a short-lived
        // parent process means it's already running independently), mpv
        // stays in the foreground holding the display itself — awaiting its
        // exit would block until the process is killed. A quick
        // `try_wait` just catches an immediate launch failure (bad sudoers
        // rule, missing binary); letting the `Child` drop afterwards is
        // deliberate fire-and-forget (tokio does not kill on drop unless
        // `kill_on_drop` is set), leaving the process running independently.
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "{} exited immediately with {status}",
                Self::viewer_bin()
            ));
        }
        // A fixed sleep here was confirmed live to be unreliable — mpv's
        // own startup time varies, and a chmod that runs before the socket
        // file actually exists leaves it root-only, silently failing every
        // later IPC connect attempt with "Permission denied" and falling
        // back to a full kill/relaunch (and its blink) on every single
        // subsequent image, defeating the entire point of this design. Poll
        // for the file to actually exist instead of guessing a delay.
        let deadline = tokio::time::Instant::now() + SOCKET_POLL_TIMEOUT;
        while tokio::fs::metadata(&socket).await.is_err() {
            if tokio::time::Instant::now() >= deadline {
                warn!(
                    "art: mpv IPC socket never appeared within {SOCKET_POLL_TIMEOUT:?} — later IPC control may fail"
                );
                break;
            }
            tokio::time::sleep(SOCKET_POLL_INTERVAL).await;
        }
        let _ = Command::new("sudo")
            .arg("-n")
            .arg("chmod")
            .arg("666")
            .arg(&socket)
            .status()
            .await;
        Ok(())
    }

    /// Tell an already-running viewer to switch to `path` over its IPC
    /// socket — see `show`'s doc comment for why this needs both a
    /// `loadfile` and a follow-up no-op `seek` to actually redraw, and
    /// `wait_for_playback_restart`'s doc comment for why the seek waits on
    /// mpv's own confirmation rather than a fixed delay.
    async fn send_ipc_show(path: &Path) -> Result<(), String> {
        let socket = Self::ipc_socket_path();
        let mut stream = UnixStream::connect(&socket)
            .await
            .map_err(|e| format!("could not connect to mpv IPC socket: {e}"))?;
        Self::send_ipc_command(
            &mut stream,
            &serde_json::json!({"command": ["loadfile", path.to_string_lossy()]}),
        )
        .await?;
        Self::wait_for_playback_restart(&mut stream).await;
        Self::send_ipc_command(
            &mut stream,
            &serde_json::json!({"command": ["seek", "0", "absolute"]}),
        )
        .await
    }

    /// Block (up to `IPC_REDRAW_TIMEOUT`) until mpv's own IPC event stream
    /// confirms the just-requested `loadfile` has actually finished loading
    /// and started rendering, rather than guessing a fixed delay — confirmed
    /// live that a seek sent too early (before this event) can silently
    /// no-op, leaving the previous image on screen despite `loadfile` having
    /// already updated mpv's own internal state. Gives up and returns
    /// (letting the caller send the seek anyway, on the chance it still
    /// helps) if the event never arrives in time.
    async fn wait_for_playback_restart(stream: &mut UnixStream) {
        let deadline = tokio::time::Instant::now() + IPC_REDRAW_TIMEOUT;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 512];
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                warn!(
                    "art: timed out waiting for mpv's playback-restart event, sending seek anyway"
                );
                return;
            }
            match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return, // connection closed, read error, or timed out
                Ok(Ok(n)) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if String::from_utf8_lossy(&buf).contains("\"event\":\"playback-restart\"") {
                        return;
                    }
                }
            }
        }
    }

    async fn send_ipc_command(
        stream: &mut UnixStream,
        cmd: &serde_json::Value,
    ) -> Result<(), String> {
        let mut line = cmd.to_string();
        line.push('\n');
        stream
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("mpv IPC write failed: {e}"))
    }

    /// SIGTERM first, escalating to SIGKILL only if it's still alive after a
    /// brief grace period — only used as a fallback when IPC control fails
    /// (see `show`), since the whole point of the persistent-process design
    /// is to not do this on every single image change any more.
    async fn kill_existing_viewer() {
        let _ = Command::new("sudo")
            .arg("-n")
            .arg("pkill")
            .arg("-x")
            .arg(Self::viewer_bin())
            .status()
            .await;
        tokio::time::sleep(KILL_GRACE_PERIOD).await;
        if Self::viewer_running().await {
            warn!("art: viewer didn't exit on SIGTERM in time, escalating to SIGKILL");
            let _ = Command::new("sudo")
                .arg("-n")
                .arg("pkill")
                .arg("-9")
                .arg("-x")
                .arg(Self::viewer_bin())
                .status()
                .await;
        }
    }

    async fn viewer_running() -> bool {
        matches!(
            Command::new("pgrep")
                .arg("-x")
                .arg(Self::viewer_bin())
                .status()
                .await,
            Ok(status) if status.success()
        )
    }

    /// A specific ArtShow always wins over whatever a batch loop is doing —
    /// bumping the generation here is what makes a running batch loop
    /// notice and exit next time it wakes (see `spawn_batch_loop`).
    async fn handle_show(&self, req: ArtShowRequest, tx: Sender<MeshMessage>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let result = async {
            let path = Self::download(&req.image_url, generation).await?;
            Self::show(&self.show_lock, &path).await?;
            Ok::<(), String>(())
        }
        .await;

        let error = match &result {
            Ok(()) => {
                *self.current_url.lock().await = Some(req.image_url.clone());
                info!(image_url = %req.image_url, "art: now showing");
                None
            }
            Err(e) => {
                warn!(error = %e, image_url = %req.image_url, "art: show failed");
                Some(e.clone())
            }
        };

        Self::report_status(&self.node_id, &self.current_url, error, tx).await;
    }

    async fn report_status(
        node_id: &str,
        current_url: &Mutex<Option<String>>,
        error: Option<String>,
        tx: Sender<MeshMessage>,
    ) {
        let status = ArtStatusReport {
            node_id: node_id.to_string(),
            viewer_running: Self::viewer_running().await,
            current_url: current_url.lock().await.clone(),
            error,
        };
        if tx.send(MeshMessage::ArtStatus(status)).await.is_err() {
            warn!("art: coordinator channel closed while sending status");
        }
    }

    /// Download every image in the batch (sequentially — kinder to a
    /// constrained node's RAM/bandwidth than fetching them all at once, and
    /// simpler to reason about than bounding concurrency), clearing out any
    /// previous batch's files first. Per-image failures are logged and
    /// skipped rather than aborting the whole batch — one bad URL out of a
    /// few dozen highlights shouldn't blank the display. Returns the local
    /// paths of whatever downloaded successfully, in order.
    async fn download_batch(urls: &[String]) -> Vec<PathBuf> {
        let dir = Self::batch_dir();
        if dir.exists()
            && let Err(e) = tokio::fs::remove_dir_all(&dir).await
        {
            warn!(error = %e, dir = %dir.display(), "art: could not clear previous batch cache");
        }
        let mut paths = Vec::with_capacity(urls.len());
        for (i, url) in urls.iter().enumerate() {
            let dest = Self::batch_item_path(i);
            match Self::download_to(url, &dest).await {
                Ok(()) => paths.push(dest),
                Err(e) => {
                    warn!(error = %e, url = %url, "art: batch image download failed, skipping")
                }
            }
        }
        paths
    }

    /// Cycle through `paths` locally, one every `interval`, wrapping at the
    /// end, until `expected_generation` no longer matches (superseded by a
    /// specific ArtShow or a newer ArtBatch — see the `generation` field's
    /// doc comment). This is what makes the general slideshow genuinely
    /// self-driving on the node: once this is running, no further
    /// coordinator messages are needed to keep it going.
    fn spawn_batch_loop(
        &self,
        paths: Vec<PathBuf>,
        interval: Duration,
        expected_generation: u64,
        tx: Sender<MeshMessage>,
    ) {
        if paths.is_empty() {
            return;
        }
        let show_lock = self.show_lock.clone();
        let current_url = self.current_url.clone();
        let generation = self.generation.clone();
        let node_id = self.node_id.clone();
        tokio::spawn(async move {
            let mut index = 0usize;
            loop {
                if generation.load(Ordering::SeqCst) != expected_generation {
                    return; // superseded — quietly stop, no cleanup needed
                }
                let path = &paths[index];
                let error = match Self::show(&show_lock, path).await {
                    Ok(()) => None,
                    Err(e) => {
                        warn!(error = %e, path = %path.display(), "art: batch show failed");
                        Some(e)
                    }
                };
                *current_url.lock().await = Some(path.display().to_string());
                Self::report_status(&node_id, &current_url, error, tx.clone()).await;
                index = (index + 1) % paths.len();
                tokio::time::sleep(interval).await;
            }
        });
    }

    /// A fresh batch always wins over a previous one — same reasoning as a
    /// specific ArtShow, just also replacing the local cache with the new
    /// image set before starting to cycle it.
    async fn handle_batch(&self, req: ArtBatchRequest, tx: Sender<MeshMessage>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let paths = Self::download_batch(&req.image_urls).await;
        if paths.is_empty() {
            Self::report_status(
                &self.node_id,
                &self.current_url,
                Some("no images in the batch downloaded successfully".into()),
                tx,
            )
            .await;
            return;
        }
        info!(
            count = paths.len(),
            interval_secs = req.interval_secs,
            "art: starting batch slideshow"
        );
        self.spawn_batch_loop(
            paths,
            Duration::from_secs(req.interval_secs.max(1)),
            generation,
            tx,
        );
    }
}

#[async_trait]
impl Capability for ArtCapability {
    fn name(&self) -> &'static str {
        "art"
    }

    fn handles(&self, msg: &MeshMessage) -> bool {
        matches!(msg, MeshMessage::ArtShow(_) | MeshMessage::ArtBatch(_))
    }

    async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> {
        // Nothing to start eagerly — unlike REAPER (poll an always-on app) or
        // MQTT-based capabilities (an event loop), the viewer only runs once
        // told to show something. Cache dir is created lazily on first show.
        Ok(())
    }

    async fn handle(&self, msg: MeshMessage, tx: Sender<MeshMessage>) {
        match msg {
            MeshMessage::ArtShow(req) => self.handle_show(req, tx).await,
            MeshMessage::ArtBatch(req) => self.handle_batch(req, tx).await,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn send_ipc_command_writes_a_newline_terminated_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let accept = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 256];
            let n = stream.read(&mut buf).await.unwrap();
            buf.truncate(n);
            buf
        });

        let mut client = UnixStream::connect(&socket_path).await.unwrap();
        ArtCapability::send_ipc_command(
            &mut client,
            &serde_json::json!({"command": ["loadfile", "/tmp/x.jpg"]}),
        )
        .await
        .unwrap();

        let received = accept.await.unwrap();
        let text = String::from_utf8(received).unwrap();
        assert!(
            text.ends_with('\n'),
            "expected a newline-terminated line: {text:?}"
        );
        let parsed: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(parsed["command"][0], "loadfile");
        assert_eq!(parsed["command"][1], "/tmp/x.jpg");
    }

    #[tokio::test]
    async fn send_ipc_show_sends_loadfile_then_a_seek() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        // Safe in tests: this crate's only other env::set_var call sites are
        // absent, and each test here uses its own tempdir/socket so this
        // doesn't race with anything else.
        unsafe {
            std::env::set_var("ART_MPV_IPC_SOCKET", &socket_path);
        }
        let listener = UnixListener::bind(&socket_path).unwrap();

        let accept = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // send_ipc_show writes two separate lines over the same
            // connection, waiting in between for this mock server to send
            // back mpv's own playback-restart event — keep reading (rather
            // than a single `read` call, which would return after the first
            // line and let the connection close before the second write)
            // until both lines have arrived.
            let mut buf = Vec::new();
            let mut chunk = vec![0u8; 512];
            let mut sent_event = false;
            while buf.iter().filter(|&&b| b == b'\n').count() < 2 {
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0, "connection closed before two lines arrived");
                buf.extend_from_slice(&chunk[..n]);
                if !sent_event && buf.contains(&b'\n') {
                    stream
                        .write_all(b"{\"event\":\"playback-restart\"}\n")
                        .await
                        .unwrap();
                    sent_event = true;
                }
            }
            buf
        });

        ArtCapability::send_ipc_show(Path::new("/tmp/next.jpg"))
            .await
            .unwrap();
        unsafe {
            std::env::remove_var("ART_MPV_IPC_SOCKET");
        }

        let received = accept.await.unwrap();
        let text = String::from_utf8(received).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "expected exactly a loadfile then a seek: {lines:?}"
        );
        let loadfile: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(loadfile["command"][0], "loadfile");
        assert_eq!(loadfile["command"][1], "/tmp/next.jpg");
        let seek: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(seek["command"][0], "seek");
    }

    #[test]
    fn name_is_art() {
        let cap = ArtCapability::new("node1");
        assert_eq!(cap.name(), "art");
    }

    #[test]
    fn handles_art_show_and_art_batch_only() {
        let cap = ArtCapability::new("node1");
        assert!(cap.handles(&MeshMessage::ArtShow(ArtShowRequest {
            request_id: "r1".into(),
            image_url: "http://example.com/a.jpg".into(),
        })));
        assert!(cap.handles(&MeshMessage::ArtBatch(ArtBatchRequest {
            request_id: "r1".into(),
            image_urls: vec!["http://example.com/a.jpg".into()],
            interval_secs: 30,
        })));
        assert!(!cap.handles(&MeshMessage::Ping));
    }

    #[test]
    fn cache_path_alternates_between_two_files() {
        let a = ArtCapability::cache_path(1);
        let b = ArtCapability::cache_path(2);
        let c = ArtCapability::cache_path(3);
        assert_ne!(a, b, "consecutive generations should use different paths");
        assert_eq!(
            a, c,
            "the same parity should reuse the same path (bounded disk use)"
        );
    }

    #[tokio::test]
    async fn viewer_running_false_before_any_show() {
        assert!(!ArtCapability::viewer_running().await);
    }

    #[tokio::test]
    async fn download_to_retries_before_giving_up() {
        use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = attempts.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                attempts_clone.fetch_add(1, AtomicOrdering::SeqCst);
                let _ = stream
                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.jpg");
        let url = format!("http://{addr}/image.jpg");
        let result = ArtCapability::download_to(&url, &dest).await;
        assert!(result.is_err());
        assert_eq!(attempts.load(AtomicOrdering::SeqCst), DOWNLOAD_RETRIES);
    }

    #[tokio::test]
    async fn handle_show_reports_error_status_on_bad_url() {
        let cap = ArtCapability::new("node1");
        let (tx, mut rx) = mpsc::channel(4);
        cap.handle(
            MeshMessage::ArtShow(ArtShowRequest {
                request_id: "r1".into(),
                // Invalid scheme — reqwest::get fails fast without a real network call.
                image_url: "not-a-url".into(),
            }),
            tx,
        )
        .await;
        match rx.try_recv().unwrap() {
            MeshMessage::ArtStatus(status) => {
                assert_eq!(status.node_id, "node1");
                assert!(!status.viewer_running);
                assert!(status.error.is_some());
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_batch_reports_error_when_every_download_fails() {
        let cap = ArtCapability::new("node1");
        let (tx, mut rx) = mpsc::channel(4);
        cap.handle(
            MeshMessage::ArtBatch(ArtBatchRequest {
                request_id: "r1".into(),
                image_urls: vec!["not-a-url".into(), "also-not-a-url".into()],
                interval_secs: 30,
            }),
            tx,
        )
        .await;
        match rx.try_recv().unwrap() {
            MeshMessage::ArtStatus(status) => {
                assert_eq!(status.node_id, "node1");
                assert!(!status.viewer_running);
                assert!(status.error.is_some());
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_specific_art_show_bumps_generation_past_a_prior_batch() {
        // Verifies the supersession signal a running batch loop checks
        // against (see spawn_batch_loop) — not the loop's own timing, which
        // would need controlling real sleeps in a unit test.
        let cap = ArtCapability::new("node1");
        let before = cap.generation.load(Ordering::SeqCst);
        let (tx, mut rx) = mpsc::channel(4);
        cap.handle(
            MeshMessage::ArtShow(ArtShowRequest {
                request_id: "r1".into(),
                image_url: "not-a-url".into(),
            }),
            tx,
        )
        .await;
        rx.try_recv().ok();
        assert!(cap.generation.load(Ordering::SeqCst) > before);
    }

    // ── matte/border compositing ─────────────────────────────────────────────

    fn tiny_test_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_pixel(w, h, Rgb([200, 50, 50]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        out
    }

    /// JPEG is lossy — even a flat-colour region can drift a few units per
    /// channel after a compress/decompress round trip, so exact pixel
    /// equality isn't a meaningful assertion here.
    fn approx_eq_rgb(a: [u8; 3], b: [u8; 3], tolerance: u8) -> bool {
        a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.abs_diff(*y) <= tolerance)
    }

    fn small_config() -> MatteConfig {
        MatteConfig {
            canvas_w: 200,
            canvas_h: 120,
            matte_percent: 10.0,
            side_margin_boost_percent: 0.0,
            matte_rgb: [0xED, 0xE7, 0xDA],
            frame_rgb: [0x2B, 0x2B, 0x2B],
            frame_px: 3,
            glaze_percent: 0.0,
            glaze_rgb: [0xFF, 0xFF, 0xFF],
            border_glaze_percent: 0.0,
            brightness_percent: 100.0,
        }
    }

    #[test]
    fn compose_matte_output_matches_canvas_dimensions() {
        let src = tiny_test_jpeg(80, 40);
        let config = small_config();
        let out = compose_matte(&src, &config).unwrap();
        let decoded = image::load_from_memory(&out).unwrap();
        assert_eq!(decoded.dimensions(), (config.canvas_w, config.canvas_h));
    }

    #[test]
    fn bevel_shade_never_clips_even_near_the_ceiling() {
        // The default matte colour's red channel (237) plus a flat +18
        // delta used to pin straight to 255, making the lit wedge
        // indistinguishable from an even brighter matte. The proportional
        // blend should still move a mid-high channel upward, but a channel
        // already right at the ceiling must stay strictly below 255 rather
        // than clipping flat against it.
        let shaded = bevel_shade([237, 200, 254], 1.0);
        assert!(
            shaded[0] > 237 && shaded[0] < 255,
            "mid-high channel should move toward white without clipping: {shaded:?}"
        );
        assert!(shaded[1] > 200 && shaded[1] < 255);
        assert!(
            shaded[2] < 255,
            "near-ceiling channel should stay below 255: {shaded:?}"
        );
    }

    #[test]
    fn border_lit_amount_is_binary_away_from_any_corner() {
        // Comfortably inside a single wedge (both a lit and shadow edge are
        // clearly farther away than the nearest one), the result should
        // match the old discrete classification exactly: 1.0 for a lit
        // wedge, 0.0 for a shadow one.
        assert_eq!(border_lit_amount(0.1, 20.0, 20.0, 20.0), 1.0); // top wins, lit
        assert_eq!(border_lit_amount(20.0, 0.1, 20.0, 20.0), 0.0); // bottom wins, shadow
    }

    #[test]
    fn border_lit_amount_blends_smoothly_at_a_lit_shadow_corner() {
        // Top (lit) and right (shadow) are the two nearest edges and
        // disagree — right at the tie, the result should be the ambiguous
        // midpoint, not an abrupt flip.
        assert_eq!(border_lit_amount(1.0, 20.0, 20.0, 1.0), 0.5);
        // As right pulls away from top, the pixel should read as more
        // lit, monotonically, until it saturates at 1.0 once the gap
        // reaches BEVEL_TRANSITION.
        let closer = border_lit_amount(1.0, 20.0, 20.0, 1.05);
        let farther = border_lit_amount(1.0, 20.0, 20.0, 1.2);
        assert!(closer > 0.5 && closer < farther);
        assert_eq!(farther, 1.0);
    }

    #[test]
    fn border_lit_amount_does_not_leak_the_far_axis_into_a_near_tie() {
        // A pixel deep in the left band (clearly lit) but only mildly off
        // vertical-centre — top and bottom both moderately far, left very
        // close, right very far — must read as fully lit. Earlier logic
        // that compared raw min(top,left) against min(bottom,right)
        // (unbounded, no saturation) let a merely-larger-than-left bottom
        // distance still drag the result away from 1.0.
        assert_eq!(border_lit_amount(1.8, 1.79, 0.85, 8.2), 1.0);
    }

    #[test]
    fn compose_matte_top_edge_pixel_is_lightened_matte() {
        // A source image scaled to fit inside the padded area should never
        // reach the canvas edge. Sampled at the top-middle — well clear of
        // any corner mitre seam — a border pixel should read as the matte
        // colour lightened by the bevel shading (top is one of the two "lit"
        // wedges).
        let src = tiny_test_jpeg(80, 40);
        let config = small_config();
        let out = compose_matte(&src, &config).unwrap();
        let decoded = image::load_from_memory(&out).unwrap().to_rgb8();
        let expected = bevel_shade(config.matte_rgb, 1.0);
        let pixel = decoded.get_pixel(config.canvas_w / 2, 0).0;
        assert!(
            approx_eq_rgb(pixel, expected, 6),
            "top-edge pixel {:?} should be close to the lightened matte colour {:?} (JPEG re-encode allows small drift)",
            pixel,
            expected
        );
    }

    #[test]
    fn compose_matte_corners_are_mitre_seams() {
        // The exact outer corner sits right on the diagonal where the top
        // and left wedges meet — it should render as the dark seam colour,
        // not a flat matte or frame colour, mimicking a real mitred joint.
        let src = tiny_test_jpeg(80, 40);
        let config = small_config();
        let out = compose_matte(&src, &config).unwrap();
        let decoded = image::load_from_memory(&out).unwrap().to_rgb8();
        let pixel = decoded.get_pixel(0, 0).0;
        assert!(
            approx_eq_rgb(pixel, SEAM_RGB, 10),
            "corner pixel {pixel:?} should be close to the mitre seam colour {SEAM_RGB:?}"
        );
    }

    #[test]
    fn compose_matte_infill_absorbs_a_mismatched_ratio_without_touching_the_frame() {
        // A very wide (5:1) source into a roughly square frame interior
        // can't fill both axes without cropping or distorting the artwork —
        // the frame itself stays fixed-size; the leftover gap gets an infill
        // sampled from the artwork's own edge colour instead. tiny_test_jpeg
        // is a flat fill, so that sampled colour is exactly the source's own
        // [200, 50, 50] — verify a pixel well inside that gap matches it
        // (not bevel-tinted, and not the frame's own dark colour — that
        // treatment is reserved for the frame's own thin line, unaffected
        // by the artwork's aspect ratio).
        let src = tiny_test_jpeg(100, 20);
        let config = MatteConfig {
            canvas_w: 120,
            canvas_h: 120,
            matte_percent: 10.0,
            ..small_config()
        };
        let out = compose_matte(&src, &config).unwrap();
        let decoded = image::load_from_memory(&out).unwrap().to_rgb8();
        // pad=12, fit_w=fit_h=96. contain_fit(100,20,96,96): scale=
        // min(96/100,96/20)=min(0.96,4.8)=0.96 -> pic_w=96, pic_h=19. The
        // picture sits vertically centred within the interior (y=[50,69)) —
        // well above it, at y=30, is deep infill territory, clear of both
        // the frame's own ring (y<12) and the picture itself.
        let pixel = decoded.get_pixel(config.canvas_w / 2, 30).0;
        assert!(
            approx_eq_rgb(pixel, [200, 50, 50], 6),
            "expected the infill to match the artwork's own (flat) edge colour, got {pixel:?}"
        );
        // And the bevel-tinted mat colour, right outside the (always
        // fixed-size) frame interior, must be completely unaffected by the
        // artwork's unusual aspect ratio.
        let mat_pixel = decoded.get_pixel(config.canvas_w / 2, 0).0;
        let expected_mat = bevel_shade(config.matte_rgb, 1.0);
        assert!(
            approx_eq_rgb(mat_pixel, expected_mat, 6),
            "expected the fixed-size mat's own bevel untouched, got {mat_pixel:?} (expected {expected_mat:?})"
        );
    }

    #[test]
    fn contain_fit_preserves_the_source_aspect_ratio() {
        // A 4:1 source into a square target must scale uniformly (same
        // factor on both axes) — a distorting "stretch" would instead scale
        // each axis independently to fill both dimensions exactly.
        let (pic_w, pic_h) = contain_fit(200, 50, 100, 100);
        let source_ratio = 200.0 / 50.0;
        let pic_ratio = pic_w as f32 / pic_h as f32;
        assert!(
            (source_ratio - pic_ratio).abs() < 0.01,
            "source_ratio={source_ratio} pic_ratio={pic_ratio} (pic_w={pic_w}, pic_h={pic_h})"
        );
    }

    #[test]
    fn contain_fit_never_exceeds_the_available_space() {
        let (pic_w, pic_h) = contain_fit(200, 50, 100, 100);
        assert!(pic_w <= 100 && pic_h <= 100);
    }

    #[test]
    fn contain_fit_computes_the_exact_size_for_a_wide_source() {
        // scale = min(100/200, 100/50) = min(0.5, 2.0) = 0.5 -> (100, 25):
        // width fills the available space exactly, height is left over for
        // the dark frame to absorb.
        let (pic_w, pic_h) = contain_fit(200, 50, 100, 100);
        assert_eq!((pic_w, pic_h), (100, 25));
    }

    #[test]
    fn compose_matte_falls_back_gracefully_on_bad_bytes() {
        let config = small_config();
        assert!(compose_matte(b"not an image", &config).is_err());
    }

    #[test]
    fn apply_glaze_zero_percent_leaves_pixels_unchanged() {
        let mut img = RgbImage::from_pixel(4, 4, Rgb([200, 50, 50]));
        apply_glaze(&mut img, 0.0, [0xFF, 0xFF, 0xFF]);
        assert_eq!(img.get_pixel(0, 0).0, [200, 50, 50]);
    }

    #[test]
    fn apply_glaze_100_percent_fully_replaces_with_glaze_colour() {
        let mut img = RgbImage::from_pixel(4, 4, Rgb([200, 50, 50]));
        apply_glaze(&mut img, 100.0, [0xFF, 0xFF, 0xFF]);
        assert_eq!(img.get_pixel(0, 0).0, [255, 255, 255]);
    }

    #[test]
    fn apply_glaze_50_percent_blends_halfway() {
        let mut img = RgbImage::from_pixel(4, 4, Rgb([200, 50, 50]));
        apply_glaze(&mut img, 50.0, [0xFF, 0xFF, 0xFF]);
        let pixel = img.get_pixel(0, 0).0;
        assert!(approx_eq_rgb(pixel, [227, 152, 152], 1));
    }

    #[test]
    fn compose_matte_glaze_lightens_the_artwork_not_the_mat() {
        // The glaze should blend into the picture's own pixels only — the
        // mat/border colour, sampled well away from the image, must be
        // untouched by it.
        let src = tiny_test_jpeg(80, 40);
        let config = MatteConfig {
            glaze_percent: 50.0,
            ..small_config()
        };
        let out = compose_matte(&src, &config).unwrap();
        let decoded = image::load_from_memory(&out).unwrap().to_rgb8();
        let center = decoded
            .get_pixel(config.canvas_w / 2, config.canvas_h / 2)
            .0;
        let expected_center = {
            let mut px = [200u8, 50, 50];
            for c in px.iter_mut() {
                *c = (*c as f32 * 0.5 + 255.0 * 0.5).round() as u8;
            }
            px
        };
        assert!(
            approx_eq_rgb(center, expected_center, 8),
            "glazed centre pixel {center:?} should be close to {expected_center:?}"
        );
        let top_edge = decoded.get_pixel(config.canvas_w / 2, 0).0;
        let expected_matte = bevel_shade(config.matte_rgb, 1.0);
        assert!(
            approx_eq_rgb(top_edge, expected_matte, 6),
            "mat pixel {top_edge:?} should still be the (unglazed) lightened matte colour {expected_matte:?}"
        );
    }

    #[test]
    fn apply_border_glaze_skips_pixels_inside_the_artwork_rect() {
        let mut canvas = RgbImage::from_pixel(10, 10, Rgb([10, 10, 10]));
        apply_border_glaze(&mut canvas, 2, 2, 8, 8, 100.0, [0xFF, 0xFF, 0xFF]);
        assert_eq!(
            canvas.get_pixel(5, 5).0,
            [10, 10, 10],
            "inside the artwork rect must be untouched"
        );
        assert_eq!(
            canvas.get_pixel(0, 0).0,
            [255, 255, 255],
            "outside it, 100% glaze should be pure white"
        );
    }

    #[test]
    fn compose_matte_border_glaze_lightens_the_mat_not_the_artwork() {
        // The border glaze is independent of the artwork's own glaze — with
        // only border_glaze_percent set, the mat should wash toward white
        // while the artwork's own pixels stay untouched by it.
        let src = tiny_test_jpeg(80, 40);
        let config = MatteConfig {
            border_glaze_percent: 50.0,
            ..small_config()
        };
        let out = compose_matte(&src, &config).unwrap();
        let decoded = image::load_from_memory(&out).unwrap().to_rgb8();

        let center = decoded
            .get_pixel(config.canvas_w / 2, config.canvas_h / 2)
            .0;
        assert!(
            approx_eq_rgb(center, [200, 50, 50], 4),
            "artwork centre pixel {center:?} should be unaffected by the border glaze"
        );

        let top_edge = decoded.get_pixel(config.canvas_w / 2, 0).0;
        let washed_matte = {
            let mut px = config.matte_rgb;
            for c in px.iter_mut() {
                *c = (*c as f32 * 0.5 + 255.0 * 0.5).round() as u8;
            }
            px
        };
        let expected_matte = bevel_shade(washed_matte, 1.0);
        assert!(
            approx_eq_rgb(top_edge, expected_matte, 8),
            "mat pixel {top_edge:?} should be close to the border-glazed, bevel-lit matte colour {expected_matte:?}"
        );
    }

    #[test]
    fn apply_brightness_100_percent_leaves_pixels_unchanged() {
        let mut img = RgbImage::from_pixel(4, 4, Rgb([200, 100, 50]));
        apply_brightness(&mut img, 100.0);
        assert_eq!(img.get_pixel(0, 0).0, [200, 100, 50]);
    }

    #[test]
    fn apply_brightness_50_percent_halves_every_channel() {
        let mut img = RgbImage::from_pixel(4, 4, Rgb([200, 100, 50]));
        apply_brightness(&mut img, 50.0);
        assert_eq!(img.get_pixel(0, 0).0, [100, 50, 25]);
    }

    #[test]
    fn compose_matte_brightness_dims_both_artwork_and_border() {
        // Brightness is the very last step, applied over the whole composed
        // frame — both the artwork's own pixels and the mat/border colour
        // (sampled well clear of any corner seam) should come out dimmed by
        // the same factor.
        let src = tiny_test_jpeg(80, 40);
        let config = MatteConfig {
            brightness_percent: 50.0,
            ..small_config()
        };
        let out = compose_matte(&src, &config).unwrap();
        let decoded = image::load_from_memory(&out).unwrap().to_rgb8();

        let center = decoded
            .get_pixel(config.canvas_w / 2, config.canvas_h / 2)
            .0;
        let expected_center = [100u8, 25, 25]; // half of the tiny_test_jpeg fill [200,50,50]
        assert!(
            approx_eq_rgb(center, expected_center, 8),
            "dimmed artwork pixel {center:?} should be close to {expected_center:?}"
        );

        let top_edge = decoded.get_pixel(config.canvas_w / 2, 0).0;
        let lit_matte = bevel_shade(config.matte_rgb, 1.0);
        let expected_matte = [
            (lit_matte[0] as f32 * 0.5).round() as u8,
            (lit_matte[1] as f32 * 0.5).round() as u8,
            (lit_matte[2] as f32 * 0.5).round() as u8,
        ];
        assert!(
            approx_eq_rgb(top_edge, expected_matte, 6),
            "dimmed mat pixel {top_edge:?} should be close to {expected_matte:?}"
        );
    }

    #[test]
    fn parse_hex_rgb_reads_with_and_without_hash() {
        assert_eq!(parse_hex_rgb("#EDE7DA"), Some([0xED, 0xE7, 0xDA]));
        assert_eq!(parse_hex_rgb("2b2b2b"), Some([0x2B, 0x2B, 0x2B]));
        assert_eq!(parse_hex_rgb("not-hex"), None);
        assert_eq!(parse_hex_rgb("ABC"), None);
    }

    #[test]
    fn avg_row_averages_a_horizontal_line() {
        let mut img = RgbImage::new(2, 1);
        img.put_pixel(0, 0, Rgb([100, 0, 0]));
        img.put_pixel(1, 0, Rgb([200, 0, 0]));
        assert_eq!(avg_row(&img, 0), [150, 0, 0]);
    }

    #[test]
    fn avg_col_averages_a_vertical_line() {
        let mut img = RgbImage::new(1, 2);
        img.put_pixel(0, 0, Rgb([0, 100, 0]));
        img.put_pixel(0, 1, Rgb([0, 200, 0]));
        assert_eq!(avg_col(&img, 0), [0, 150, 0]);
    }

    #[test]
    fn fill_rect_paints_only_the_given_rect() {
        let mut canvas = RgbImage::from_pixel(20, 20, Rgb([0, 0, 0]));
        fill_rect(&mut canvas, 5, 5, 10, 10, [255, 255, 255]);
        // Inside the rect, including its own interior: painted.
        assert_eq!(canvas.get_pixel(5, 5).0, [255, 255, 255]);
        assert_eq!(canvas.get_pixel(10, 10).0, [255, 255, 255]);
        // Outside the rect entirely: untouched.
        assert_eq!(canvas.get_pixel(0, 0).0, [0, 0, 0]);
    }

    #[test]
    fn draw_ring_only_touches_the_outline_not_the_interior() {
        let mut canvas = RgbImage::from_pixel(20, 20, Rgb([0, 0, 0]));
        draw_ring(&mut canvas, 5, 5, 10, 10, 2, [255, 255, 255]);
        // Corner of the outline: painted.
        assert_eq!(canvas.get_pixel(5, 5).0, [255, 255, 255]);
        // Centre of the ringed rect: untouched (still background).
        assert_eq!(canvas.get_pixel(10, 10).0, [0, 0, 0]);
        // Outside the rect entirely: untouched.
        assert_eq!(canvas.get_pixel(0, 0).0, [0, 0, 0]);
    }
}
