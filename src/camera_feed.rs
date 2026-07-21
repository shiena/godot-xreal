//! [`XrealCameraFeed`] — the XREAL glasses' RGB camera exposed as a Godot `CameraFeed`.
//!
//! Subclasses `CameraFeed` (the idiomatic custom-camera-source pattern): `activate_feed` starts the
//! XREAL RGB capture, `deactivate_feed` stops it, and `poll_frame()` — called each frame by a driver
//! (e.g. the addon's `xreal_camera.gd`) — publishes the latest frame as **Y + CbCr textures** for a
//! YCbCr→RGB shader (no CPU colour conversion). See `docs/plans/camera-feed-plan.md`.
//!
//! **Consumers read the custom getters, NOT the standard `CameraServer` texture route** — see the
//! class docs on [`XrealCameraFeed`] for the full story (this trips up readers who know `CameraFeed`).
//!
//! Usage (GDScript):
//! ```gdscript
//! var feed = ClassDB.instantiate(&"XrealCameraFeed")
//! CameraServer.add_feed(feed)
//! feed.set_active(true)          # -> activate_feed() starts the camera
//! # each frame:
//! feed.poll_frame()
//! # display: sample feed.get_y_texture() / feed.get_cbcr_texture() in a YCbCr->RGB shader
//! # (addons/godot_xreal/shaders/xreal_ycbcr*.gdshader) — NOT via CameraTexture.
//! ```

use godot::classes::image::Format;
use godot::classes::{CameraFeed, ICameraFeed, Image, ImageTexture, RenderingServer};
use godot::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// **TEMPORARY instrumentation** (see `crate::native::GrabTimings`): microseconds accumulated over
/// the frames since the last report, so the reported figure is a mean rather than one jittery
/// sample. Remove with the timing report in `poll_frame`.
#[derive(Default)]
struct PollTiming {
    grabs: u32,
    acquire_us: u64,
    planes_us: u64,
    interleave_us: u64,
    dispose_us: u64,
    packed_us: u64,
    image_us: u64,
    feed_us: u64,
    /// Split by plane: Y is 921,600 bytes and CbCr 460,800 — exactly 2:1. If the two times come out
    /// 2:1 the upload is bandwidth/copy bound; if they come out ~1:1 a fixed per-call cost (a sync
    /// or flush) dominates and shrinking the payload will not help.
    upload_y_us: u64,
    upload_cbcr_us: u64,
}

/// Feed-image plumbing:
/// - `poll_frame` grabs the XREAL frame as Y + interleaved CbCr and keeps two plain `ImageTexture`s
///   (`get_y_texture` / `get_cbcr_texture`) updated — the textures every addon shader samples
///   directly (matching the XREAL SDK's YUVTransRGB sample). Only with `feed_camera_server = true`
///   does it ALSO call `set_ycbcr_images` so the base `CameraFeed` carries data for standard
///   `CameraServer` consumers; that route is off by default because a `CameraTexture` bound to a
///   *script-fed* feed shows only the placeholder on this build, so the extra per-frame GPU upload
///   would feed a route nothing displays.
use crate::session;

/// The XREAL glasses' RGB camera exposed as a Godot `CameraFeed` (full colour, via the native C ABI).
///
/// Add it to the `CameraServer` and call `poll_frame()` each frame to grab the latest frame; sample
/// `get_y_texture()` (R8 luma) and `get_cbcr_texture()` (RG8 chroma) in a YCbCr→RGB shader to
/// display it (`addons/godot_xreal/shaders/xreal_ycbcr*.gdshader` do exactly that). Present only on
/// glasses that carry an RGB camera (e.g. the One Pro, not the Air 2 Ultra — gate on
/// `XrealSystem.is_camera_supported()`).
///
/// **Read frames via the custom getters, not `CameraTexture`.** If you know Godot's `CameraFeed`
/// this is the surprise: frames do NOT flow through the standard `CameraServer` texture route by
/// default. On this build a `CameraTexture` bound to a script-fed feed shows only the placeholder,
/// so the addon bypasses it — `poll_frame()` updates the two plain `ImageTexture`s above and every
/// consumer (camera panel, blend capture, stream, photo capture) samples those directly. Code that
/// consumes feeds through the standard `CameraFeed` API can opt in via `feed_camera_server`, which
/// additionally pushes each frame into the base feed (`set_ycbcr_images`) at the cost of a second
/// per-frame GPU upload.
#[derive(GodotClass)]
#[class(base = CameraFeed)]
pub struct XrealCameraFeed {
    base: Base<CameraFeed>,
    /// Capture handle from `StartRGBCameraDataCapture`, while active.
    capture_handle: Option<u64>,
    /// Frames actually grabbed, and `poll_frame()` calls made. The camera publishes slower than we
    /// poll, so `polls` runs ahead of `frames` — their ratio is how much duplicate work the
    /// timestamp gate in `rgb_camera_grab_yuv` is skipping.
    frames: u64,
    polls: u64,
    /// Timestamp of the last grabbed frame; gates the grab (see `XrealNative::rgb_camera_grab_yuv`).
    last_timestamp: u64,
    /// TEMPORARY per-stage timing accumulator; reported and reset with the periodic frame log.
    timing: PollTiming,
    /// Plain textures the shader samples directly (Y = R8, CbCr = RG8). Kept in sync with the frame.
    y_tex: Option<Gd<ImageTexture>>,
    cbcr_tex: Option<Gd<ImageTexture>>,
    /// Also push each frame into the base `CameraFeed` via `set_ycbcr_images` — the route standard
    /// `CameraServer` consumers read. **Off by default**: nothing in the addon reads it (a
    /// `CameraTexture` bound to this script-fed feed shows only the placeholder on this build) and
    /// it duplicates every frame's GPU upload. Turn on only for external code that consumes
    /// `CameraServer` feeds through the standard API.
    #[export]
    feed_camera_server: bool,
}

#[godot_api]
impl ICameraFeed for XrealCameraFeed {
    fn init(base: Base<CameraFeed>) -> Self {
        Self {
            base,
            capture_handle: None,
            frames: 0,
            polls: 0,
            last_timestamp: 0,
            timing: PollTiming::default(),
            y_tex: None,
            cbcr_tex: None,
            feed_camera_server: false,
        }
    }

    fn activate_feed(&mut self) -> bool {
        let Some(session) = session::shared() else {
            godot_warn!("[xreal] camera: no session yet; feed not activated");
            return false;
        };
        if !session.rgb_camera_available() {
            godot_warn!("[xreal] camera: RGB camera C ABI unavailable (libXREALXRPlugin.so)");
            return false;
        }
        match session.rgb_camera_start() {
            Some(handle) => {
                self.capture_handle = Some(handle);
                godot_print!("[xreal] camera: capture started (handle={handle})");
                true
            }
            None => {
                // Start returned the failure sentinel (see `XrealNative::rgb_camera_start`). On this
                // device that is a wedged glasses camera — an unclean prior exit (e.g. a render-thread
                // crash) left it holding the capture, so NRSDK rejects the new connection ("Recv
                // Frame, -99"). Recovery = re-plug the glasses USB AND restart the app: a replug
                // alone is not enough, because this process's native session stays bound to the old
                // connection, so retries keep failing (2026-07-21). (Or CAMERA permission denied.)
                godot_warn!(
                    "[xreal] camera: RGB capture did not start — glasses camera wedged (re-plug the USB and restart the app) or CAMERA permission denied"
                );
                false
            }
        }
    }

    fn deactivate_feed(&mut self) {
        if let (Some(session), Some(handle)) = (session::shared(), self.capture_handle.take()) {
            let ok = session.rgb_camera_stop(handle);
            godot_print!("[xreal] camera: capture stopped (handle={handle}, ok={ok})");
        }
    }
}

#[godot_api]
impl XrealCameraFeed {
    /// Poll the latest RGB-camera frame and refresh `get_y_texture()` / `get_cbcr_texture()` (the
    /// textures the YCbCr→RGB shaders sample — the display route). With `feed_camera_server` on it
    /// also pushes the frame into the base `CameraFeed` as **separate Y + CbCr** images
    /// (`FEED_YCBCR_SEP`) for standard `CameraServer` consumers. Returns `true` if a frame was
    /// grabbed this call. Call once per frame from a driver.
    ///
    /// The camera publishes slower than a 60 Hz render loop polls, so `false` is the normal result
    /// on the calls in between: the grab is gated on the SDK frame timestamp and does no copy or
    /// texture upload unless the frame actually changed. The textures keep their last contents,
    /// so a `false` return needs no handling by the caller.
    #[func]
    fn poll_frame(&mut self) -> bool {
        let Some(session) = session::shared() else {
            return false;
        };
        self.polls += 1;
        // `None` here is the normal "no new frame since the last poll" case, not an error.
        let mut grab = crate::native::GrabTimings::default();
        let Some((y, yw, yh, cbcr, cw, ch)) =
            session.rgb_camera_grab_yuv(&mut self.last_timestamp, &mut grab)
        else {
            return false;
        };
        self.frames += 1;
        let report = self.frames <= 3 || self.frames.is_multiple_of(120);
        // Read the luma diagnostic before the PBO path moves `y` onto the render thread.
        let mean = if report { mean_luma(&y) } else { 0 };

        // TEMPORARY (experiment E): once the textures exist, upload straight from the plane buffers
        // on the render thread through a pixel-unpack buffer — no PackedByteArray, no Image. The
        // Image path stays for the first frame (it is what creates the textures), while
        // `feed_camera_server` is on (that route needs the Images), and permanently after any PBO
        // failure.
        let pbo_path = self.y_tex.is_some()
            && self.cbcr_tex.is_some()
            && !self.feed_camera_server
            && !PBO_FAILED.load(Ordering::Relaxed);
        let (mut packed_us, mut image_us, mut feed_us) = (0u64, 0u64, 0u64);
        let (mut upload_y_us, mut upload_cbcr_us) = (0u64, 0u64);

        if pbo_path {
            let y_rid = self.y_tex.as_ref().expect("checked above").get_rid();
            let c_rid = self.cbcr_tex.as_ref().expect("checked above").get_rid();
            let callable = Callable::from_fn("xreal_camera_pbo_upload", move |_| {
                upload_planes_pbo(y_rid, yw, yh, &y, c_rid, cw, ch, &cbcr);
                Variant::nil()
            });
            RenderingServer::singleton().call_on_render_thread(&callable);
        } else {
            let t = Instant::now();
            let y_data = PackedByteArray::from(y.as_slice());
            let cbcr_data = PackedByteArray::from(cbcr.as_slice());
            packed_us = t.elapsed().as_micros() as u64;
            // Y = single-channel R8 (luma); CbCr = two-channel RG8 (Cb in R, Cr in G).
            let t = Instant::now();
            let Some(y_img) = Image::create_from_data(yw, yh, false, Format::R8, &y_data) else {
                return false;
            };
            let Some(cbcr_img) = Image::create_from_data(cw, ch, false, Format::RG8, &cbcr_data)
            else {
                return false;
            };
            image_us = t.elapsed().as_micros() as u64;
            // Keep the plain ImageTextures the shaders sample directly (the display route); feed the
            // base CameraFeed only on request (see the struct docs for why that route is off by default).
            let t = Instant::now();
            if self.feed_camera_server {
                self.base_mut().set_ycbcr_images(&y_img, &cbcr_img);
            }
            feed_us = t.elapsed().as_micros() as u64;
            let t = Instant::now();
            update_texture(&mut self.y_tex, &y_img);
            upload_y_us = t.elapsed().as_micros() as u64;
            let t = Instant::now();
            update_texture(&mut self.cbcr_tex, &cbcr_img);
            upload_cbcr_us = t.elapsed().as_micros() as u64;
        }

        // TEMPORARY: accumulate this grab's stages for the periodic mean below.
        let acc = &mut self.timing;
        acc.grabs += 1;
        acc.acquire_us += grab.acquire_us as u64;
        acc.planes_us += grab.planes_us as u64;
        acc.interleave_us += grab.interleave_us as u64;
        acc.dispose_us += grab.dispose_us as u64;
        acc.packed_us += packed_us;
        acc.image_us += image_us;
        acc.feed_us += feed_us;
        acc.upload_y_us += upload_y_us;
        acc.upload_cbcr_us += upload_cbcr_us;

        if report {
            godot_print!(
                "[xreal] camera frame #{} (polls={}) y={yw}x{yh} cbcr={cw}x{ch} mean_luma={mean}",
                self.frames,
                self.polls
            );
            // TEMPORARY: per-stage means over the frames since the last report, in microseconds.
            let acc = std::mem::take(&mut self.timing);
            let n = acc.grabs.max(1) as u64;
            let total = acc.acquire_us
                + acc.planes_us
                + acc.interleave_us
                + acc.dispose_us
                + acc.packed_us
                + acc.image_us
                + acc.feed_us
                + acc.upload_y_us
                + acc.upload_cbcr_us;
            godot_print!(
                "[xreal] camera timing/grab (us, n={}): acquire={} planes={} interleave={} dispose={} packed={} image={} feed={} upload_y={} upload_cbcr={} | total={}",
                acc.grabs,
                acc.acquire_us / n,
                acc.planes_us / n,
                acc.interleave_us / n,
                acc.dispose_us / n,
                acc.packed_us / n,
                acc.image_us / n,
                acc.feed_us / n,
                acc.upload_y_us / n,
                acc.upload_cbcr_us / n,
                total / n
            );
            // TEMPORARY: the render-thread half of experiment E. `main=` is what poll_frame still
            // costs; the PBO figures are measured inside the render-thread callback, so they are
            // *not* part of `total` above.
            let pbo_n = PBO_UPLOADS.swap(0, Ordering::Relaxed).max(1);
            let pbo_y = PBO_UPLOAD_Y_US.swap(0, Ordering::Relaxed) / pbo_n;
            let pbo_c = PBO_UPLOAD_CBCR_US.swap(0, Ordering::Relaxed) / pbo_n;
            godot_print!(
                "[xreal] camera upload path={} egl_ctx_on_main={:?} | render-thread pbo (us): y={} cbcr={} sum={}",
                if pbo_path { "pbo" } else { "image" },
                crate::gl::has_current_context(),
                pbo_y,
                pbo_c,
                pbo_y + pbo_c
            );
        }
        true
    }

    /// The luma (Y) plane as an `R8` texture — sample `.r` for Y. `null` until the first frame.
    #[func]
    fn get_y_texture(&self) -> Option<Gd<ImageTexture>> {
        self.y_tex.clone()
    }

    /// The chroma plane as an `RG8` texture — `.r` = Cb (U), `.g` = Cr (V). `null` until first frame.
    #[func]
    fn get_cbcr_texture(&self) -> Option<Gd<ImageTexture>> {
        self.cbcr_tex.clone()
    }
}

/// Mean luma over a sparse sample of the plane — a cheap "is the image alive?" diagnostic.
fn mean_luma(y: &[u8]) -> u64 {
    let step = (y.len() / 4096).max(1);
    let (mut sum, mut n) = (0u64, 0u64);
    let mut i = 0;
    while i < y.len() {
        sum += y[i] as u64;
        n += 1;
        i += step;
    }
    sum.checked_div(n).unwrap_or(0)
}

/// TEMPORARY (experiment E): render-thread half of the PBO upload. Resolves both `ImageTexture`s to
/// their GL names and pushes each plane through a persistent pixel-unpack buffer, timing each into
/// the statics below. Any failure latches [`PBO_FAILED`], which returns the feed to the Image path.
#[allow(clippy::too_many_arguments)]
fn upload_planes_pbo(
    y_rid: Rid,
    yw: i32,
    yh: i32,
    y: &[u8],
    c_rid: Rid,
    cw: i32,
    ch: i32,
    cbcr: &[u8],
) {
    let rs = RenderingServer::singleton();
    let y_tex = rs.texture_get_native_handle(y_rid) as u32;
    let c_tex = rs.texture_get_native_handle(c_rid) as u32;

    let t = Instant::now();
    let ok_y = crate::gl::upload_plane_pbo(0, y_tex, yw, yh, crate::gl::GL_RED, y);
    PBO_UPLOAD_Y_US.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
    let t = Instant::now();
    let ok_c = crate::gl::upload_plane_pbo(1, c_tex, cw, ch, crate::gl::GL_RG, cbcr);
    PBO_UPLOAD_CBCR_US.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
    PBO_UPLOADS.fetch_add(1, Ordering::Relaxed);

    if !(ok_y && ok_c) {
        godot_warn!(
            "[xreal] camera: PBO upload failed (y_tex={y_tex} ok={ok_y}, cbcr_tex={c_tex} ok={ok_c}) — falling back to the Image path"
        );
        PBO_FAILED.store(true, Ordering::Relaxed);
    }
}

/// TEMPORARY: experiment-E accumulators, written on the render thread and drained by the periodic
/// report on the main thread.
static PBO_UPLOAD_Y_US: AtomicU64 = AtomicU64::new(0);
static PBO_UPLOAD_CBCR_US: AtomicU64 = AtomicU64::new(0);
static PBO_UPLOADS: AtomicU64 = AtomicU64::new(0);
/// Latched on the first PBO failure; the feed then stays on the Image path for the process's life.
static PBO_FAILED: AtomicBool = AtomicBool::new(false);

/// Create the `ImageTexture` on the first frame, then `update()` it in place (cheap, same size).
fn update_texture(slot: &mut Option<Gd<ImageTexture>>, img: &Gd<Image>) {
    match slot {
        Some(tex) => tex.update(img),
        None => *slot = ImageTexture::create_from_image(img),
    }
}
