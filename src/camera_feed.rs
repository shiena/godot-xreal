//! [`XrealCameraFeed`], the XREAL glasses' RGB camera exposed as a Godot `CameraFeed`.
//!
//! It subclasses `CameraFeed`, the idiomatic custom-camera-source pattern: `activate_feed` starts
//! the XREAL RGB capture, `deactivate_feed` stops it, and `poll_frame()`, which a driver such as
//! the addon's `xreal_camera.gd` calls each frame, publishes the latest frame as **Y and CbCr
//! textures** for a YCbCr-to-RGB shader, with no CPU colour conversion. See
//! `docs/plans/camera-feed-plan.md`.
//!
//! **Consumers read the custom getters, NOT the standard `CameraServer` texture route.** The class
//! docs on [`XrealCameraFeed`] tell the full story; this trips up readers who know `CameraFeed`.
//!
//! Usage (GDScript):
//! ```gdscript
//! var feed = ClassDB.instantiate(&"XrealCameraFeed")
//! CameraServer.add_feed(feed)
//! feed.set_active(true)          # -> activate_feed() starts the camera
//! # each frame:
//! feed.poll_frame()
//! # display: sample feed.get_y_texture() / feed.get_cbcr_texture() in a YCbCr->RGB shader
//! # (addons/godot_xreal/shaders/xreal_ycbcr*.gdshader), NOT through CameraTexture.
//! ```

use godot::classes::image::Format;
use godot::classes::{CameraFeed, ICameraFeed, Image, ImageTexture, RenderingServer};
use godot::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Per-stage grab cost (see `crate::native::GrabTimings`): microseconds accumulated over the frames
/// since the last report, so the reported figure is a mean rather than one jittery sample.
///
/// Collecting this costs about 8 `Instant::now()` calls per grabbed frame, well under a microsecond
/// against a grab of roughly 525 us, so it always runs and only the report line is gated (see
/// [`TIMING_PROP`]). It is what found every win in the optimisation pass recorded in
/// `docs/plans/camera-feed-plan.md`, so it is kept rather than deleted.
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
    /// Split by plane: Y is 921,600 bytes and CbCr 460,800, exactly 2:1. When the two times come out
    /// 2:1 the upload is bound by bandwidth and copying; when they come out near 1:1 a fixed per-call
    /// cost, a sync or a flush, dominates and shrinking the payload will not help.
    upload_y_us: u64,
    upload_cbcr_us: u64,
    /// The `cpu_luma_step` retain copy, zero unless it is enabled.
    cpu_luma_us: u64,
}

/// Feed-image plumbing:
/// - `poll_frame` grabs the XREAL frame as Y plus interleaved CbCr and keeps two plain
///   `ImageTexture`s updated, `get_y_texture` and `get_cbcr_texture`, which are the textures every
///   addon shader samples directly, matching the XREAL SDK's YUVTransRGB sample. Only with
///   `feed_camera_server = true` does it ALSO call `set_ycbcr_images`, so the base `CameraFeed`
///   carries data for standard `CameraServer` consumers. That route is off by default, because a
///   `CameraTexture` bound to a *script-fed* feed shows only the placeholder on this build, so the
///   extra per-frame GPU upload would feed a route nothing displays.
use crate::session;

/// The XREAL glasses' RGB camera exposed as a Godot `CameraFeed`, in full colour, through the
/// native C ABI.
///
/// Add it to the `CameraServer` and call `poll_frame()` each frame to grab the latest frame, then
/// sample `get_y_texture()`, the R8 luma, and `get_cbcr_texture()`, the RG8 chroma, in a
/// YCbCr-to-RGB shader to display it; `addons/godot_xreal/shaders/xreal_ycbcr*.gdshader` do exactly
/// that. It is present only on glasses that carry an RGB camera, such as the One Pro but not the
/// Air 2 Ultra, so gate on `XrealSystem.is_camera_supported()`.
///
/// **Read frames through the custom getters, not `CameraTexture`.** If you know Godot's
/// `CameraFeed` this is the surprise: frames do NOT flow through the standard `CameraServer`
/// texture route by default. On this build a `CameraTexture` bound to a script-fed feed shows only
/// the placeholder, so the addon bypasses it: `poll_frame()` updates the two plain `ImageTexture`s
/// above and every consumer, whether the camera panel, blend capture, the stream or photo capture,
/// samples those directly. Code that consumes feeds through the standard `CameraFeed` API can opt
/// in with `feed_camera_server`, which additionally pushes each frame into the base feed through
/// `set_ycbcr_images`, at the cost of a second per-frame GPU upload.
#[derive(GodotClass)]
#[class(base = CameraFeed)]
pub struct XrealCameraFeed {
    base: Base<CameraFeed>,
    /// Capture handle from `StartRGBCameraDataCapture`, while active.
    capture_handle: Option<u64>,
    /// Frames actually grabbed, and `poll_frame()` calls made. The camera publishes slower than we
    /// poll, so `polls` runs ahead of `frames`, and their ratio is how much duplicate work the
    /// timestamp gate in `rgb_camera_grab_yuv` is skipping.
    frames: u64,
    polls: u64,
    /// Timestamp of the last grabbed frame; gates the grab (see `XrealNative::rgb_camera_grab_yuv`).
    last_timestamp: u64,
    /// Reused CbCr interleave buffer for the direct path, the only per-frame pixel copy left there.
    cbcr_buf: Vec<u8>,
    /// Retained CPU copy of the luma plane, filled only while `cpu_luma_step > 0`. It is deliberately a
    /// `PackedByteArray` rather than a `Vec`, because that is copy-on-write refcounted: `get_y_data()`
    /// becomes a refcount bump instead of a second 0.9 MB copy, and a worker thread holding a reference
    /// sees its snapshot fork rather than tear when the next frame overwrites this.
    y_cpu: PackedByteArray,
    y_cpu_size: Vector2i,
    /// Per-stage timing accumulator; reported and reset with the periodic frame log when
    /// [`TIMING_PROP`] is set.
    timing: PollTiming,
    /// Whether to print the per-stage timing line. It is sampled from [`TIMING_PROP`] at capture start,
    /// so a `setprop` followed by a camera off and on cycle is enough, with no rebuild.
    timing_report: bool,
    /// Plain textures the shader samples directly, Y as R8 and CbCr as RG8, kept in sync with the
    /// frame.
    y_tex: Option<Gd<ImageTexture>>,
    cbcr_tex: Option<Gd<ImageTexture>>,
    /// Also push each frame into the base `CameraFeed` through `set_ycbcr_images`, the route standard
    /// `CameraServer` consumers read. It is **off by default**, because nothing in the addon reads it,
    /// a `CameraTexture` bound to this script-fed feed showing only the placeholder on this build, and
    /// it duplicates every frame's GPU upload. Turn it on only for external code that consumes
    /// `CameraServer` feeds through the standard API.
    #[export]
    feed_camera_server: bool,
    /// Retain a CPU-readable copy of the **luma** plane each frame, for [`Self::get_y_data`], which is
    /// the path for OpenCV and friends, since the GPU-upload path deliberately keeps no CPU copy.
    ///
    /// `0`, the default, is off and makes no copy. `1` is the full 1280x720. `2` and `4` take every
    /// 2nd or 4th pixel and row, giving 640x360 or 320x180. The luma plane is already a dense 8-bit
    /// greyscale image, so the result needs no conversion: it is directly a `CV_8UC1` Mat.
    ///
    /// The cost is one copy per grabbed frame, measured at **about 296 us at `step = 1` and about
    /// 278 us at `step = 2`** on the X4000. Those are 6% apart, not 4x apart: a strided read still
    /// touches every cache line of the source, so this is read-bound whatever the step. **Do not raise
    /// `step` expecting to save copy time.** Its value is entirely downstream, in the 4x fewer pixels
    /// the CV code then has to process.
    ///
    /// Read the result from the `frame_changed` signal rather than polling the getters; see
    /// [`Self::get_y_data`] and `docs/plans/camera-feed-plan.md`.
    ///
    /// Luma only: the chroma is not retained. Colour CV would need the other two planes and a
    /// `cvtColor`, and note that the plane order is **YV12**, not I420. Its 1-3 ms would dominate this
    /// copy entirely, so it stays unimplemented until something actually needs it.
    #[export]
    cpu_luma_step: i32,
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
            y_cpu: PackedByteArray::new(),
            y_cpu_size: Vector2i::ZERO,
            timing: PollTiming::default(),
            timing_report: false,
            y_tex: None,
            cbcr_tex: None,
            feed_camera_server: false,
            cpu_luma_step: 0,
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
        // Sampled per capture start, so a `setprop` plus a camera off and on cycle takes effect at once.
        self.timing_report = crate::session::android_prop_i32(TIMING_PROP).unwrap_or(0) != 0;
        match session.rgb_camera_start() {
            Some(handle) => {
                self.capture_handle = Some(handle);
                godot_print!("[xreal] camera: capture started (handle={handle})");
                true
            }
            None => {
                // Start returned the failure sentinel (see `XrealNative::rgb_camera_start`). On this device that
                // means a wedged glasses camera: an unclean prior exit, such as a render-thread crash, left it
                // holding the capture, so NRSDK rejects the new connection with "Recv Frame, -99". Recovery takes
                // re-plugging the glasses USB AND restarting the app, because a replug alone leaves this process's
                // native session bound to the old connection and retries keep failing (2026-07-21). The other
                // possible cause is a denied CAMERA permission.
                godot_warn!(
                    "[xreal] camera: RGB capture did not start: glasses camera wedged (re-plug the USB and restart the app) or CAMERA permission denied"
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
    /// Poll the latest RGB-camera frame and refresh `get_y_texture()` and `get_cbcr_texture()`, the
    /// textures the YCbCr-to-RGB shaders sample, which is the display route. With `feed_camera_server`
    /// on it also pushes the frame into the base `CameraFeed` as **separate Y and CbCr** images
    /// (`FEED_YCBCR_SEP`) for standard `CameraServer` consumers. It returns `true` when a frame was
    /// grabbed on this call. Call it once per frame from a driver.
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

        // Direct path: upload each plane straight out of the SDK's frame buffer, so the only pixel copy
        // left is the chroma interleave. It needs a current EGL context *on this thread*, which holds on
        // Android, where Godot's main loop is the GL thread (see `crate::gl`), plus textures to upload
        // into. Everything else takes the Image path below, which also creates those textures on the
        // first frame and which `feed_camera_server` needs anyway.
        //
        // The renderer check comes FIRST, before the context probe, so under Vulkan this never touches
        // the GL/EGL loader at all. The context probe alone is not a renderer gate: once the
        // vulkan-path stage-2 bridge gives the render thread a private EGL context, a context that
        // happens to be current here would let this path treat `texture_get_native_handle`'s VkImage
        // as a GL texture name. Under Vulkan the feed always takes the renderer-agnostic Image path.
        if self.y_tex.is_some()
            && self.cbcr_tex.is_some()
            && !self.feed_camera_server
            && !PBO_FAILED.load(Ordering::Relaxed)
            && crate::gl::renderer_is_gl()
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
        // Y is single-channel R8 luma, and CbCr is two-channel RG8, with Cb in R and Cr in G.
        let t = Instant::now();
        let Some(y_img) = Image::create_from_data(yw, yh, false, Format::R8, &y_data) else {
            return false;
        };
        let Some(cbcr_img) = Image::create_from_data(cw, ch, false, Format::RG8, &cbcr_data) else {
            return false;
        };
        let image_us = t.elapsed().as_micros() as u64;
        let t = Instant::now();
        update_texture(&mut self.y_tex, &y_img);
        let upload_y_us = t.elapsed().as_micros() as u64;
        let t = Instant::now();
        update_texture(&mut self.cbcr_tex, &cbcr_img);
        let upload_cbcr_us = t.elapsed().as_micros() as u64;
        let t = Instant::now();
        if self.cpu_luma_step > 0 {
            let mut buf = std::mem::take(&mut self.y_cpu);
            self.y_cpu_size = copy_luma(&y, yw, yh, self.cpu_luma_step, &mut buf);
            self.y_cpu = buf;
        } else if !self.y_cpu.is_empty() {
            self.y_cpu = PackedByteArray::new();
            self.y_cpu_size = Vector2i::ZERO;
        }
        let cpu_luma_us = t.elapsed().as_micros() as u64;
        // Feed the base CameraFeed only on request; the struct docs explain why that route is off by
        // default. Do it *last*, because Godot may emit `frame_changed` from inside this call and a
        // handler must not observe textures or `get_y_data()` from the previous frame.
        let t = Instant::now();
        if self.feed_camera_server {
            self.base_mut().set_ycbcr_images(&y_img, &cbcr_img);
        }
        let feed_us = t.elapsed().as_micros() as u64;

        self.accumulate(
            &grab,
            (packed_us, image_us, feed_us),
            (upload_y_us, upload_cbcr_us),
            cpu_luma_us,
        );
        if report {
            self.report("image", yw, yh, cw, ch, mean);
        }
        self.emit_frame_changed();
        true
    }

    /// The direct upload path; see the caller. It acquires the frame, pushes the luma plane to the GPU
    /// straight from the SDK's buffer, interleaves the chroma planes into the feed's reused buffer and
    /// pushes that too, with no `PackedByteArray`, no `Image` and no copy of the luma plane at all. A
    /// GL failure latches [`PBO_FAILED`], returning the feed to the Image path for good.
    fn poll_frame_direct(
        &mut self,
        session: &session::XrealSession,
        grab: &mut crate::native::GrabTimings,
    ) -> bool {
        let y_rid = self.y_tex.as_ref().expect("checked by caller").get_rid();
        let c_rid = self.cbcr_tex.as_ref().expect("checked by caller").get_rid();
        // Taken out so the closure can borrow them while `self` is not, and put back below.
        let mut cbcr = std::mem::take(&mut self.cbcr_buf);
        let mut y_cpu = std::mem::take(&mut self.y_cpu);
        let luma_step = self.cpu_luma_step;
        let mut y_cpu_size = Vector2i::ZERO;
        let mut stage = DirectStages::default();

        let outcome = session.rgb_camera_with_frame(&mut self.last_timestamp, grab, |p| {
            let rs = RenderingServer::singleton();
            let y_tex = rs.texture_get_native_handle(y_rid) as u32;
            let c_tex = rs.texture_get_native_handle(c_rid) as u32;
            if y_tex == 0 || c_tex == 0 {
                // The textures exist but the renderer has not realised them yet. Returning `None` leaves the
                // timestamp unadvanced, so this frame is simply retried on the next poll.
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

            if luma_step > 0 {
                let t = Instant::now();
                y_cpu_size = copy_luma(p.y, p.y_width, p.y_height, luma_step, &mut y_cpu);
                stage.cpu_luma_us = t.elapsed().as_micros() as u64;
            }

            stage.size = (p.y_width, p.y_height, p.chroma_width, p.chroma_height);
            stage.mean = mean_luma(p.y);
            Some(ok_y && ok_c)
        });
        self.cbcr_buf = cbcr;
        self.y_cpu = y_cpu;
        if luma_step <= 0 && !self.y_cpu.is_empty() {
            // Turned off at runtime, so drop the snapshot and `get_y_data()` cannot return a stale one.
            self.y_cpu = PackedByteArray::new();
            self.y_cpu_size = Vector2i::ZERO;
        }

        let Some(uploaded) = outcome else {
            return false; // no new frame, or the textures were not ready
        };
        // Only now: on a gated poll the closure never ran, so `y_cpu_size` is still its ZERO initialiser
        // and assigning it above would have blanked a perfectly good size.
        if luma_step > 0 {
            self.y_cpu_size = y_cpu_size;
        }
        if !uploaded {
            PBO_FAILED.store(true, Ordering::Relaxed);
        }
        self.frames += 1;
        // The interleave is the caller's work, not the grab's, so fold it in here.
        grab.interleave_us = stage.interleave_us as u32;
        self.accumulate(
            grab,
            (0, 0, 0),
            (stage.upload_y_us, stage.upload_cbcr_us),
            stage.cpu_luma_us,
        );
        if self.frames <= 3 || self.frames.is_multiple_of(120) {
            let (yw, yh, cw, ch) = stage.size;
            self.report("direct", yw, yh, cw, ch, stage.mean);
        }
        self.emit_frame_changed();
        true
    }

    /// Emit `CameraFeed`'s own `frame_changed` as the last thing in a successful grab, so a handler
    /// sees the textures and `get_y_data()` already updated for *this* frame. Reading the data from
    /// this signal rather than polling the getters is the recommended pattern: the "no new frame this
    /// poll" state then simply cannot be observed, and that is the whole class of staleness bug
    /// `get_y_data_size()` briefly had.
    ///
    /// It is skipped when `feed_camera_server` is on, because **`set_ycbcr_images` emits
    /// `frame_changed` itself**, measured on device 2026-07-21: with the flag on, the signal rate
    /// doubled to 56.7/s against 29.4 grabs/s, and it matched the grab rate exactly with the flag off.
    /// Emitting here as well would make every handler run twice. Relying on the engine's emission for
    /// ordering is safe, because `set_ycbcr_images` is deliberately the *last* thing that path does.
    ///
    /// Calling back into this feed from a handler is safe: the handler runs inside `poll_frame`'s
    /// `&mut self`, and a re-entrant `get_y_data()` was verified on device to reborrow rather than
    /// panic.
    fn emit_frame_changed(&mut self) {
        if !self.feed_camera_server {
            self.base_mut().emit_signal("frame_changed", &[]);
        }
    }

    /// Fold one grab's per-stage costs into the running means. `cpu_us` is `(packed, image, feed)`,
    /// all zero on the direct path, which builds none of them, and `upload_us` is `(y, cbcr)`.
    fn accumulate(
        &mut self,
        grab: &crate::native::GrabTimings,
        cpu_us: (u64, u64, u64),
        upload_us: (u64, u64),
        cpu_luma_us: u64,
    ) {
        let acc = &mut self.timing;
        acc.grabs += 1;
        acc.acquire_us += grab.acquire_us as u64;
        acc.planes_us += grab.planes_us as u64;
        acc.interleave_us += grab.interleave_us as u64;
        acc.dispose_us += grab.dispose_us as u64;
        acc.packed_us += cpu_us.0;
        acc.image_us += cpu_us.1;
        acc.feed_us += cpu_us.2;
        acc.upload_y_us += upload_us.0;
        acc.upload_cbcr_us += upload_us.1;
        acc.cpu_luma_us += cpu_luma_us;
    }

    /// Print the frame line, and, when [`TIMING_PROP`] is set, the per-stage means since the last
    /// report. It resets the accumulator either way.
    fn report(&mut self, path: &str, yw: i32, yh: i32, cw: i32, ch: i32, mean: u64) {
        godot_print!(
            "[xreal] camera frame #{} (polls={}) y={yw}x{yh} cbcr={cw}x{ch} mean_luma={mean} path={path}",
            self.frames,
            self.polls
        );
        let acc = std::mem::take(&mut self.timing);
        if !self.timing_report {
            return;
        }
        let n = acc.grabs.max(1) as u64;
        let total = acc.acquire_us
            + acc.planes_us
            + acc.interleave_us
            + acc.dispose_us
            + acc.packed_us
            + acc.image_us
            + acc.feed_us
            + acc.upload_y_us
            + acc.upload_cbcr_us
            + acc.cpu_luma_us;
        godot_print!(
            "[xreal] camera timing/grab (us, n={}): acquire={} planes={} interleave={} dispose={} packed={} image={} feed={} upload_y={} upload_cbcr={} cpu_luma={}(step {}) | total={}",
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
            acc.cpu_luma_us / n,
            self.cpu_luma_step,
            total / n
        );
    }

    /// The luma (Y) plane as an `R8` texture; sample `.r` for Y. It is `null` until the first frame.
    #[func]
    fn get_y_texture(&self) -> Option<Gd<ImageTexture>> {
        self.y_tex.clone()
    }

    /// The retained CPU copy of the luma plane, a dense 8-bit greyscale image ready to wrap as an
    /// OpenCV `CV_8UC1` Mat with no conversion. It is empty unless [`Self::cpu_luma_step`] is
    /// non-zero, and until the first frame after enabling it. Use [`Self::get_y_data_size`] for its
    /// dimensions: they are the camera's divided by `cpu_luma_step`, not the texture's.
    ///
    /// The returned array shares copy-on-write storage with the feed's, so this call is a refcount
    /// bump rather than a copy. Holding it across frames is safe: the next frame's write forks it
    /// instead of mutating the snapshot underneath a reader, which is what makes it safe to hand to a
    /// worker thread.
    #[func]
    fn get_y_data(&self) -> PackedByteArray {
        self.y_cpu.clone()
    }

    /// Dimensions of [`Self::get_y_data`] in pixels, or `(0, 0)` when it is empty.
    #[func]
    fn get_y_data_size(&self) -> Vector2i {
        self.y_cpu_size
    }

    /// The chroma plane as an `RG8` texture, with `.r` as Cb (U) and `.g` as Cr (V). It is `null` until
    /// the first frame.
    #[func]
    fn get_cbcr_texture(&self) -> Option<Gd<ImageTexture>> {
        self.cbcr_tex.clone()
    }
}

/// Copy the luma plane into `out`, taking every `step`-th pixel and row, and return the resulting
/// size. `step = 1` is a straight memcpy, and a larger step decimates nearest-neighbour, with no
/// filtering, which is what CV front-ends want and what keeps this cheap.
fn copy_luma(y: &[u8], width: i32, height: i32, step: i32, out: &mut PackedByteArray) -> Vector2i {
    let step = step.max(1) as usize;
    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
    let (ow, oh) = (w / step, h / step);
    let n = ow * oh;
    if n == 0 || y.len() < w * h {
        out.resize(0);
        return Vector2i::ZERO;
    }
    if out.len() != n {
        out.resize(n);
    }
    let dst = out.as_mut_slice();
    if step == 1 {
        dst.copy_from_slice(&y[..n]);
    } else {
        for j in 0..oh {
            let src = &y[j * step * w..j * step * w + w];
            let row = &mut dst[j * ow..(j + 1) * ow];
            for (i, d) in row.iter_mut().enumerate() {
                *d = src[i * step];
            }
        }
    }
    Vector2i::new(ow as i32, oh as i32)
}

/// Mean luma over a sparse sample of the plane, a cheap "is the image alive?" diagnostic.
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

/// Per-stage costs the direct path measures inside the frame closure, plus the frame facts the
/// periodic report needs, since the planes themselves do not outlive that closure.
#[derive(Default)]
struct DirectStages {
    cpu_luma_us: u64,
    interleave_us: u64,
    upload_y_us: u64,
    upload_cbcr_us: u64,
    /// `(y_width, y_height, chroma_width, chroma_height)`.
    size: (i32, i32, i32, i32),
    mean: u64,
}

/// Run `adb shell setprop debug.xreal.camera_timing 1`, then toggle the camera off and on, to print
/// the per-stage grab breakdown every 120 frames. It is off by default, being a diagnostic rather
/// than telemetry.
const TIMING_PROP: &[u8] = b"debug.xreal.camera_timing\0";

/// Latched on the first GL upload failure; the feed then stays on the Image path for the process's
/// life. The Image path is Godot's own and always works, so this is a safe permanent fallback.
static PBO_FAILED: AtomicBool = AtomicBool::new(false);

/// Create the `ImageTexture` on the first frame, then `update()` it in place, which is cheap at the
/// same size.
fn update_texture(slot: &mut Option<Gd<ImageTexture>>, img: &Gd<Image>) {
    match slot {
        Some(tex) => tex.update(img),
        None => *slot = ImageTexture::create_from_image(img),
    }
}
