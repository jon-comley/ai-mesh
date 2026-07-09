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
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use capability_core::Capability;
use image::{GenericImageView, Rgb, RgbImage, imageops};
use shared::{ArtBatchRequest, ArtShowRequest, ArtStatusReport, MeshMessage};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};

/// A slow/hung source URL shouldn't hang an ArtShow request indefinitely —
/// the coordinator would just wait forever for the ArtStatus reply.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);

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

/// Draw a rectangular outline (not filled) of `thickness` pixels, clipped to
/// the canvas bounds — used for the thin "frame line" right at the image's
/// edge, a common real-world mat-and-frame detail (a "fillet") rather than
/// just the mat colour alone.
fn draw_border(
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
    matte_rgb: [u8; 3],
    frame_rgb: [u8; 3],
    frame_px: u32,
}

impl MatteConfig {
    fn from_env() -> Self {
        Self {
            canvas_w: env_u32("ART_CANVAS_WIDTH", 1920),
            canvas_h: env_u32("ART_CANVAS_HEIGHT", 1080),
            matte_percent: env_f32("ART_MATTE_PERCENT", 7.0),
            // A warm, off-white museum-mat colour by default — not stark
            // white, which would look harsher next to most artwork than a
            // real paper mat.
            matte_rgb: env_rgb("ART_MATTE_COLOR", [0xED, 0xE7, 0xDA]),
            frame_rgb: env_rgb("ART_FRAME_COLOR", [0x2B, 0x2B, 0x2B]),
            frame_px: env_u32("ART_FRAME_THICKNESS", 3),
        }
    }
}

/// Decode `bytes`, composite onto a matte-coloured canvas at the panel's
/// native resolution (fit-and-centre, preserving aspect ratio — a "contain"
/// fit, never cropping the artwork), draw a thin frame line at its edge, and
/// re-encode as JPEG. CPU-bound and synchronous by design — the caller runs
/// this via `spawn_blocking` rather than blocking the async runtime.
fn compose_matte(bytes: &[u8], config: &MatteConfig) -> Result<Vec<u8>, String> {
    let source =
        image::load_from_memory(bytes).map_err(|e| format!("could not decode image: {e}"))?;
    let (sw, sh) = source.dimensions();
    if sw == 0 || sh == 0 {
        return Err("decoded image has zero width or height".into());
    }
    let source_rgb = source.to_rgb8();

    let mut canvas = RgbImage::from_pixel(config.canvas_w, config.canvas_h, Rgb(config.matte_rgb));

    let pad_x = ((config.canvas_w as f32) * config.matte_percent / 100.0) as u32;
    let pad_y = ((config.canvas_h as f32) * config.matte_percent / 100.0) as u32;
    let inner_w = config.canvas_w.saturating_sub(pad_x * 2).max(1);
    let inner_h = config.canvas_h.saturating_sub(pad_y * 2).max(1);

    let scale = (inner_w as f32 / sw as f32).min(inner_h as f32 / sh as f32);
    let fit_w = ((sw as f32) * scale).round().max(1.0) as u32;
    let fit_h = ((sh as f32) * scale).round().max(1.0) as u32;
    let resized = imageops::resize(&source_rgb, fit_w, fit_h, imageops::FilterType::Lanczos3);

    let offset_x = pad_x + inner_w.saturating_sub(fit_w) / 2;
    let offset_y = pad_y + inner_h.saturating_sub(fit_h) / 2;

    draw_border(
        &mut canvas,
        offset_x.saturating_sub(config.frame_px),
        offset_y.saturating_sub(config.frame_px),
        fit_w + config.frame_px * 2,
        fit_h + config.frame_px * 2,
        config.frame_px,
        config.frame_rgb,
    );
    imageops::overlay(
        &mut canvas,
        &resized,
        i64::from(offset_x),
        i64::from(offset_y),
    );

    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(canvas)
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| format!("could not encode composed image: {e}"))?;
    Ok(out)
}

/// After killing the previous viewer, give the kernel a moment to actually
/// release `/dev/fb0` before the new one opens it — confirmed against the
/// real node that `pkill` returning doesn't guarantee the killed process has
/// finished releasing its file descriptors yet.
const RESPAWN_SETTLE: Duration = Duration::from_millis(150);

/// How long to give the old viewer to exit cleanly on SIGTERM before
/// escalating to SIGKILL — see `kill_existing_viewer`.
const KILL_GRACE_PERIOD: Duration = Duration::from_millis(200);

pub struct ArtCapability {
    node_id: String,
    /// Serializes show() calls — `fbi` runs detached from its own launcher
    /// (see `show`'s doc comment), so there's no child handle to hold onto;
    /// this just stops two overlapping ArtShow requests from racing on the
    /// kill-then-respawn sequence.
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
        std::env::var("ART_VIEWER_BIN").unwrap_or_else(|_| "fbi".into())
    }

    fn fb_device() -> String {
        std::env::var("ART_FB_DEVICE").unwrap_or_else(|_| "/dev/fb0".into())
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

    /// Fixed filename, overwritten on every single ArtShow — no history to
    /// keep around for a one-off image, unlike the batch cache below.
    fn cache_path() -> PathBuf {
        Self::cache_dir().join("current_art")
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

    /// Fetch `url` and write it to `dest`, creating `dest`'s parent dir if
    /// needed. Shared by the single-image path (`cache_path()`) and the
    /// batch path (`batch_item_path(i)`).
    async fn download_to(url: &str, dest: &Path) -> Result<(), String> {
        if let Some(dir) = dest.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| format!("could not create cache dir {}: {e}", dir.display()))?;
        }
        let resp = Self::http_client()?
            .get(url)
            .send()
            .await
            .map_err(|e| format!("download failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("download failed: HTTP {}", resp.status()));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("download failed reading body: {e}"))?;

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

    async fn download(url: &str) -> Result<PathBuf, String> {
        let path = Self::cache_path();
        Self::download_to(url, &path).await?;
        Ok(path)
    }

    /// Kill whatever's currently showing (if anything) and start the viewer
    /// fresh on `path`. Uses `fbi` against the raw framebuffer, not
    /// feh/pqiv/mpv — a Raspberry Pi OS *Lite* install (no desktop,
    /// confirmed against the real hardware while bringing the first node
    /// up) has no X server, so an X11-only viewer like feh can't start at
    /// all; `mpv --vo=drm` does work, but Debian's `mpv` package drags in a
    /// full GTK/X11/audio stack as unused linked dependencies (~600 MB) —
    /// wildly disproportionate for a single-purpose kiosk display, and a bad
    /// fit for the eventual 512 MB Pi Zero 2 W. `fbi` needs only a handful
    /// of small deps and writes straight to `/dev/fb0`. That device isn't
    /// exposed by this SoC's default full-KMS driver at all — the node this
    /// was built against needed `dtoverlay=vc4-fkms-v3d` (legacy fake-KMS,
    /// still hardware-accelerated) plus `hdmi_force_hotplug=1` in
    /// `/boot/firmware/config.txt` before `/dev/fb0` appeared; see
    /// `docs/frame-tv-setup.md`. `-T 1` targets the primary VT, `-d` skips
    /// fbi's own failed DRM-dumb-buffer auto-probe (it tries DRM before
    /// falling back to fbdev otherwise), `-a` autozooms to fit the screen,
    /// `-noverbose` suppresses fbi's on-image filename/size overlay.
    ///
    /// Run via `sudo -n`: `/dev/fb0` is `root:video` (the agent's user is in
    /// `video`, so that part's fine on its own) but fbi also needs to
    /// control the active VT via `/dev/tty1`, and VT-switching ioctls need
    /// real root regardless of file permissions on that device — confirmed
    /// against the real node that even after a udev rule opened up
    /// `/dev/tty1` to the `video` group, fbi still silently exited without
    /// root. The agent's systemd unit already runs as an account with full
    /// passwordless sudo (see `scripts/install-node-linux.sh`'s existing
    /// NOPASSWD rationale for this solo home-lab setup), so this doesn't
    /// grant anything new.
    ///
    /// **Not tracked via a held child handle** — confirmed against the real
    /// node that `sudo`'s own process exits as soon as it has forked fbi
    /// (fbi itself immediately re-forks and detaches to hold the console),
    /// so a `Child` captured at spawn time is already gone moments later;
    /// killing it would do nothing and leave the real fbi process running
    /// forever, stacking a new one on every ArtShow. Killing by process
    /// name (`pkill -x`) and checking liveness by name (`pgrep -x`, see
    /// `viewer_running`) is what actually reaches the detached process.
    async fn show(show_lock: &Mutex<()>, path: &Path) -> Result<(), String> {
        let _guard = show_lock.lock().await;
        Self::kill_existing_viewer().await;
        tokio::time::sleep(RESPAWN_SETTLE).await;
        let output = Command::new("sudo")
            .arg("-n")
            .arg(Self::viewer_bin())
            .arg("-T")
            .arg("1")
            .arg("-d")
            .arg(Self::fb_device())
            .arg("-a")
            .arg("-noverbose")
            .arg(path)
            .output()
            .await
            .map_err(|e| format!("failed to launch {}: {e}", Self::viewer_bin()))?;
        // sudo's own exit here just means it successfully forked fbi (which
        // has by now already detached to hold the console) — a non-zero
        // status means the launch itself never happened (bad sudoers rule,
        // missing binary, etc.), which is worth surfacing distinctly.
        if !output.status.success() {
            return Err(format!(
                "{} launch failed: {}",
                Self::viewer_bin(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }

    /// SIGTERM first, escalating to SIGKILL only if it's still alive after a
    /// brief grace period. `fbi` puts the console into graphics mode via
    /// ioctl and only restores it on its own normal exit path — SIGKILL
    /// bypasses that entirely, which can leave the VT in a scrambled state.
    /// Since nothing here needs instant termination (there's already a
    /// settle delay before the next viewer spawns), giving it a fair chance
    /// to exit cleanly first is free.
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
        self.generation.fetch_add(1, Ordering::SeqCst);
        let result = async {
            let path = Self::download(&req.image_url).await?;
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
    use tokio::sync::mpsc;

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

    #[tokio::test]
    async fn viewer_running_false_before_any_show() {
        assert!(!ArtCapability::viewer_running().await);
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
            matte_rgb: [0xED, 0xE7, 0xDA],
            frame_rgb: [0x2B, 0x2B, 0x2B],
            frame_px: 3,
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
    fn compose_matte_corner_pixel_is_matte_colour() {
        // A source image scaled to fit inside the padded area should never
        // reach all the way to the canvas corner — the corner must still be
        // the matte colour, not part of the source image or its frame line.
        let src = tiny_test_jpeg(80, 40);
        let config = small_config();
        let out = compose_matte(&src, &config).unwrap();
        let decoded = image::load_from_memory(&out).unwrap().to_rgb8();
        assert!(
            approx_eq_rgb(decoded.get_pixel(0, 0).0, config.matte_rgb, 4),
            "corner pixel {:?} should be close to matte colour {:?} (JPEG re-encode allows small drift)",
            decoded.get_pixel(0, 0).0,
            config.matte_rgb
        );
    }

    #[test]
    fn compose_matte_preserves_source_aspect_ratio() {
        // A square source in a non-square canvas must not get stretched —
        // verify by checking the fitted region is itself square-ish by
        // sampling that the frame border forms an actual square, not a
        // rectangle skewed to the canvas's own aspect ratio.
        let src = tiny_test_jpeg(50, 50);
        let config = MatteConfig {
            canvas_w: 300,
            canvas_h: 120,
            ..small_config()
        };
        let out = compose_matte(&src, &config).unwrap();
        let decoded = image::load_from_memory(&out).unwrap().to_rgb8();
        // The fitted image is bounded by inner_h (canvas_h minus padding,
        // the tighter dimension for this canvas) — scan the middle row to
        // find where the frame-coloured pixels start/end horizontally and
        // confirm that span is roughly the same as the image's height
        // (i.e. square, matching the square source), not stretched to fill
        // the wide canvas.
        let mid_y = config.canvas_h / 2;
        let frame_cols: Vec<u32> = (0..config.canvas_w)
            .filter(|&x| approx_eq_rgb(decoded.get_pixel(x, mid_y).0, config.frame_rgb, 10))
            .collect();
        assert!(
            !frame_cols.is_empty(),
            "expected to find frame-coloured pixels on the middle row"
        );
        // Full outer-edge-to-outer-edge span of the frame line itself, which
        // is fit + 2*frame_px wide (the border sits *around* the fitted
        // image, not inside it) — a square source should give a span close
        // to the fitted height plus that same border margin, not to the
        // wide canvas's own aspect ratio.
        let span = frame_cols.last().unwrap() - frame_cols.first().unwrap();
        let pad_h = (config.canvas_h as f32 * config.matte_percent / 100.0) as u32;
        let pad_w = (config.canvas_w as f32 * config.matte_percent / 100.0) as u32;
        let inner_h = config.canvas_h - 2 * pad_h;
        let inner_w = config.canvas_w - 2 * pad_w;
        let scale = (inner_w as f32 / 50.0).min(inner_h as f32 / 50.0);
        let fit = (50.0 * scale).round() as u32;
        let expected_span = fit + 2 * config.frame_px;
        assert!(
            (span as i64 - expected_span as i64).abs() <= 3,
            "expected roughly square fit (span={span}, expected={expected_span})"
        );
    }

    #[test]
    fn compose_matte_falls_back_gracefully_on_bad_bytes() {
        let config = small_config();
        assert!(compose_matte(b"not an image", &config).is_err());
    }

    #[test]
    fn parse_hex_rgb_reads_with_and_without_hash() {
        assert_eq!(parse_hex_rgb("#EDE7DA"), Some([0xED, 0xE7, 0xDA]));
        assert_eq!(parse_hex_rgb("2b2b2b"), Some([0x2B, 0x2B, 0x2B]));
        assert_eq!(parse_hex_rgb("not-hex"), None);
        assert_eq!(parse_hex_rgb("ABC"), None);
    }

    #[test]
    fn draw_border_only_touches_the_outline_not_the_interior() {
        let mut canvas = RgbImage::from_pixel(20, 20, Rgb([0, 0, 0]));
        draw_border(&mut canvas, 5, 5, 10, 10, 2, [255, 255, 255]);
        // Corner of the outline: painted.
        assert_eq!(canvas.get_pixel(5, 5).0, [255, 255, 255]);
        // Centre of the bordered rect: untouched (still background).
        assert_eq!(canvas.get_pixel(10, 10).0, [0, 0, 0]);
        // Outside the rect entirely: untouched.
        assert_eq!(canvas.get_pixel(0, 0).0, [0, 0, 0]);
    }
}
