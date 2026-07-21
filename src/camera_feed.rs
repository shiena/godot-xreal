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
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Reused CbCr interleave buffer for the direct path — the only per-frame pixel copy left there.
    cbcr_buf: Vec<u8>,
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
            cbcr_buf: Vec::new(),
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
        let mut grab = crate::native::GrabTimings::default();

        // Direct path: upload each plane straight out of the SDK's frame buffer, so the only pixel
        // copy left is the chroma interleave. It needs a current EGL context *on this thread* —
        // true on Android, where Godot's main loop is the GL thread (see `crate::gl`) — plus
        // textures to upload into. Everything else takes the Image path below, which is also what
        // creates those textures on the first frame, and which `feed_camera_server` needs anyway.
        if self.y_tex.is_some()
            && self.cbcr_tex.is_some()
            && !self.feed_camera_server
            && !PBO_FAILED.load(Ordering::Relaxed)
            && crate::gl::has_current_context() == Some(true)
        {
            return self.poll_frame_direct(session, &mut grab);
        }

        // `None` here is the normal "no new frame since the last poll" case, not an error.
        let Some((y, yw, yh, cbcr, cw, ch)) =
            session.rgb_camera_grab_yuv(&mut self.last_timestamp, &mut grab)
        else {
            return false;
        };
        self.frames += 1;
        let report = self.frames <= 3 || self.frames.is_multiple_of(120);
        let mean = if report { mean_luma(&y) } else { 0 };
        let t = Instant::now();
        let y_data = PackedByteArray::from(y.as_slice());
        let cbcr_data = PackedByteArray::from(cbcr.as_slice());
        let packed_us = t.elapsed().as_micros() as u64;
        // Y = single-channel R8 (luma); CbCr = two-channel RG8 (Cb in R, Cr in G).
        let t = Instant::now();
        let Some(y_img) = Image::create_from_data(yw, yh, false, Format::R8, &y_data) else {
            return false;
        };
        let Some(cbcr_img) = Image::create_from_data(cw, ch, false, Format::RG8, &cbcr_data) else {
            return false;
        };
        let image_us = t.elapsed().as_micros() as u64;
        // Keep the plain ImageTextures the shaders sample directly (the display route); feed the
        // base CameraFeed only on request (see the struct docs for why that route is off by default).
        let t = Instant::now();
        if self.feed_camera_server {
            self.base_mut().set_ycbcr_images(&y_img, &cbcr_img);
        }
        let feed_us = t.elapsed().as_micros() as u64;
        let t = Instant::now();
        update_texture(&mut self.y_tex, &y_img);
        let upload_y_us = t.elapsed().as_micros() as u64;
        let t = Instant::now();
        update_texture(&mut self.cbcr_tex, &cbcr_img);
        let upload_cbcr_us = t.elapsed().as_micros() as u64;

        self.accumulate(
            &grab,
            packed_us,
            image_us,
            feed_us,
            upload_y_us,
            upload_cbcr_us,
        );
        if report {
            self.report("image", yw, yh, cw, ch, mean);
        }
        true
    }

    /// The direct upload path (see the caller). Acquires the frame, pushes the luma plane to the GPU
    /// straight from the SDK's buffer, interleaves the chroma planes into the feed's reused buffer
    /// and pushes that too — no `PackedByteArray`, no `Image`, and no copy of the luma plane at all.
    /// A GL failure latches [`PBO_FAILED`], returning the feed to the Image path for good.
    fn poll_frame_direct(
        &mut self,
        session: &session::XrealSession,
        grab: &mut crate::native::GrabTimings,
    ) -> bool {
        let y_rid = self.y_tex.as_ref().expect("checked by caller").get_rid();
        let c_rid = self.cbcr_tex.as_ref().expect("checked by caller").get_rid();
        // Taken out so the closure can borrow it while `self` is not; put back below.
        let mut cbcr = std::mem::take(&mut self.cbcr_buf);
        let mut stage = DirectStages::default();

        let outcome = session.rgb_camera_with_frame(&mut self.last_timestamp, grab, |p| {
            let rs = RenderingServer::singleton();
            let y_tex = rs.texture_get_native_handle(y_rid) as u32;
            let c_tex = rs.texture_get_native_handle(c_rid) as u32;
            if y_tex == 0 || c_tex == 0 {
                // The textures exist but the renderer has not realised them yet. Returning `None`
                // leaves the timestamp unadvanced, so this frame is simply retried next poll.
                return None;
            }
            let t = Instant::now();
            let ok_y = crate::gl::upload_plane_pbo(
                0,
                y_tex,
                p.y_width,
                p.y_height,
                crate::gl::GL_RED,
                p.y,
            );
            stage.upload_y_us = t.elapsed().as_micros() as u64;

            let t = Instant::now();
            crate::native::interleave_cbcr(p.u, p.v, p.chroma_width, p.chroma_height, &mut cbcr);
            stage.interleave_us = t.elapsed().as_micros() as u64;

            let t = Instant::now();
            let ok_c = crate::gl::upload_plane_pbo(
                1,
                c_tex,
                p.chroma_width,
                p.chroma_height,
                crate::gl::GL_RG,
                &cbcr,
            );
            stage.upload_cbcr_us = t.elapsed().as_micros() as u64;

            stage.size = (p.y_width, p.y_height, p.chroma_width, p.chroma_height);
            stage.mean = mean_luma(p.y);
            Some(ok_y && ok_c)
        });
        self.cbcr_buf = cbcr;

        let Some(uploaded) = outcome else {
            return false; // no new frame, or the textures were not ready
        };
        if !uploaded {
            PBO_FAILED.store(true, Ordering::Relaxed);
        }
        self.frames += 1;
        // The interleave is the caller's work, not the grab's, so fold it in here.
        grab.interleave_us = stage.interleave_us as u32;
        self.accumulate(grab, 0, 0, 0, stage.upload_y_us, stage.upload_cbcr_us);
        if self.frames <= 3 || self.frames.is_multiple_of(120) {
            let (yw, yh, cw, ch) = stage.size;
            self.report("direct", yw, yh, cw, ch, stage.mean);
        }
        true
    }

    /// TEMPORARY: fold one grab's per-stage costs into the running means.
    fn accumulate(
        &mut self,
        grab: &crate::native::GrabTimings,
        packed_us: u64,
        image_us: u64,
        feed_us: u64,
        upload_y_us: u64,
        upload_cbcr_us: u64,
    ) {
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
    }

    /// TEMPORARY: print the frame line plus per-stage means since the last report, then reset.
    fn report(&mut self, path: &str, yw: i32, yh: i32, cw: i32, ch: i32, mean: u64) {
        godot_print!(
            "[xreal] camera frame #{} (polls={}) y={yw}x{yh} cbcr={cw}x{ch} mean_luma={mean} path={path}",
            self.frames,
            self.polls
        );
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

/// TEMPORARY: per-stage costs the direct path measures inside the frame closure, plus the frame
/// facts the periodic report needs (the planes themselves do not outlive that closure).
#[derive(Default)]
struct DirectStages {
    interleave_us: u64,
    upload_y_us: u64,
    upload_cbcr_us: u64,
    /// `(y_width, y_height, chroma_width, chroma_height)`.
    size: (i32, i32, i32, i32),
    mean: u64,
}

/// Latched on the first GL upload failure; the feed then stays on the Image path for the process's
/// life. The Image path is Godot's own and always works, so this is a safe permanent fallback.
static PBO_FAILED: AtomicBool = AtomicBool::new(false);

/// Create the `ImageTexture` on the first frame, then `update()` it in place (cheap, same size).
fn update_texture(slot: &mut Option<Gd<ImageTexture>>, img: &Gd<Image>) {
    match slot {
        Some(tex) => tex.update(img),
        None => *slot = ImageTexture::create_from_image(img),
    }
}
