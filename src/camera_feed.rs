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

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use godot::classes::image::Format;
use godot::classes::{CameraFeed, ICameraFeed, Image, ImageTexture, RenderingServer};
use godot::prelude::*;

use crate::native::CameraAhbPlane;

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
    frames: u64,
    /// Plain textures the shader samples directly (Y = R8, CbCr = RG8). Kept in sync with the frame.
    y_tex: Option<Gd<ImageTexture>>,
    cbcr_tex: Option<Gd<ImageTexture>>,
    /// Also push each frame into the base `CameraFeed` via `set_ycbcr_images` — the route standard
    /// `CameraServer` consumers read. **Off by default**: nothing in the addon reads it (a
    /// `CameraTexture` bound to this script-fed feed shows only the placeholder on this build) and
    /// it duplicates every frame's GPU upload. Turn on only for external code that consumes
    /// `CameraServer` feeds through the standard API. (Ignored while the `camera_ahb` PoC path is
    /// active — that mode never materialises a CPU-side Y `Image` to push.)
    #[export]
    feed_camera_server: bool,
    /// Zero-upload-Y PoC state (`debug.xreal.camera_ahb 1`), shared with the render-thread init.
    ahb: Arc<AhbShared>,
    /// Direct-upload PoC state (`debug.xreal.camera_direct 1`), shared with the render thread.
    direct: Arc<DirectShared>,
    /// Rolling `poll_frame` cost instrumentation: accumulated ns / samples since the last log.
    poll_ns: u64,
    poll_samples: u32,
}

const AHB_IDLE: u32 = 0;
const AHB_PENDING: u32 = 1;
const AHB_READY: u32 = 2;
const AHB_FAILED: u32 = 3;

/// Shared between `poll_frame` (main thread) and the one-shot render-thread AHB init.
struct AhbShared {
    /// [`AHB_IDLE`] → [`AHB_PENDING`] (init queued on the render thread) → [`AHB_READY`] /
    /// [`AHB_FAILED`] (permanent fallback to the copy path).
    state: AtomicU32,
    plane: Mutex<Option<CameraAhbPlane>>,
}

/// Shared between `poll_frame` (main thread, dispatcher) and the render-thread direct-upload
/// closures. Same state machine constants as [`AhbShared`].
struct DirectShared {
    state: AtomicU32,
    /// GL texture ids of `y_tex` / `cbcr_tex` (`texture_get_native_handle`, resolved once on the
    /// render thread).
    y_gl_id: AtomicU32,
    cbcr_gl_id: AtomicU32,
    /// Reused CbCr interleave staging buffer (render thread only).
    cbcr_buf: Mutex<Vec<u8>>,
    /// Render-thread grab+upload cost: accumulated ns / samples since the last log.
    ns: std::sync::atomic::AtomicU64,
    samples: AtomicU32,
}

#[godot_api]
impl ICameraFeed for XrealCameraFeed {
    fn init(base: Base<CameraFeed>) -> Self {
        Self {
            base,
            capture_handle: None,
            frames: 0,
            y_tex: None,
            cbcr_tex: None,
            feed_camera_server: false,
            ahb: Arc::new(AhbShared {
                state: AtomicU32::new(AHB_IDLE),
                plane: Mutex::new(None),
            }),
            direct: Arc::new(DirectShared {
                state: AtomicU32::new(AHB_IDLE),
                y_gl_id: AtomicU32::new(0),
                cbcr_gl_id: AtomicU32::new(0),
                cbcr_buf: Mutex::new(Vec::new()),
                ns: std::sync::atomic::AtomicU64::new(0),
                samples: AtomicU32::new(0),
            }),
            poll_ns: 0,
            poll_samples: 0,
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
                // Frame, -99"). Re-plug the glasses to reset it. (Or CAMERA permission was denied.)
                godot_warn!(
                    "[xreal] camera: RGB capture did not start — glasses camera wedged (re-plug to reset) or CAMERA permission denied"
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
    /// PoC (`setprop debug.xreal.camera_ahb 1`): the Y plane is written straight into an
    /// `AHardwareBuffer` whose `EGLImage` is bound over `get_y_texture()`'s GL storage — one CPU
    /// row-copy, no `Image`, no texture upload (the GPU samples the same memory). Chroma stays on
    /// the copy path. Falls back to the copy path automatically if any link fails (see the
    /// step-tagged `[xreal] camera ahb:` log). Poll cost is logged every 120 frames either way.
    #[func]
    fn poll_frame(&mut self) -> bool {
        let Some(session) = session::shared() else {
            return false;
        };
        let t0 = Instant::now();
        let ahb_want = session::camera_ahb_requested();
        let direct_want = session::camera_direct_requested();

        // Direct-upload fast path: the whole grab + glTexSubImage2D runs on the render thread —
        // this poll only dispatches. Cost is logged from the render thread as mode=direct.
        if direct_want && self.direct.state.load(Ordering::Acquire) == AHB_READY {
            self.frames += 1;
            Self::dispatch_direct_grab(&self.direct);
            if self.frames.is_multiple_of(120) {
                godot_print!("[xreal] camera frame #{} (direct mode)", self.frames);
            }
            return true;
        }

        // Zero-upload-Y fast path: row-copy Y into the AHB mapping; only chroma continues below.
        let mut ahb_mode = false;
        let grabbed = if ahb_want && self.ahb.state.load(Ordering::Acquire) == AHB_READY {
            let plane_guard = self.ahb.plane.lock().expect("ahb plane mutex");
            plane_guard.as_ref().and_then(|plane| {
                session
                    .rgb_camera_grab_yuv_into_ahb(plane)
                    .map(|(yw, yh, cbcr, cw, ch)| {
                        ahb_mode = true;
                        (Vec::new(), yw, yh, cbcr, cw, ch)
                    })
            })
        } else {
            session.rgb_camera_grab_yuv()
        };
        let Some((y, yw, yh, cbcr, cw, ch)) = grabbed else {
            return false;
        };
        self.frames += 1;

        // CbCr = two-channel RG8 (Cb in R, Cr in G) — the copy path in both modes.
        let cbcr_data = PackedByteArray::from(cbcr.as_slice());
        let Some(cbcr_img) = Image::create_from_data(cw, ch, false, Format::RG8, &cbcr_data) else {
            return false;
        };
        update_texture(&mut self.cbcr_tex, &cbcr_img);

        if !ahb_mode {
            // Y = single-channel R8 (luma), through the Image/upload path.
            let y_data = PackedByteArray::from(y.as_slice());
            let Some(y_img) = Image::create_from_data(yw, yh, false, Format::R8, &y_data) else {
                return false;
            };
            // Keep the plain ImageTextures the shaders sample directly (the display route); feed the
            // base CameraFeed only on request (see the struct docs for why that's off by default).
            if self.feed_camera_server {
                self.base_mut().set_ycbcr_images(&y_img, &cbcr_img);
            }
            update_texture(&mut self.y_tex, &y_img);
            // Once the Y texture exists at its final size, queue the one-shot AHB init (render
            // thread — EGL context needed). Copy frames continue until it reports ready.
            if ahb_want
                && self
                    .ahb
                    .state
                    .compare_exchange(AHB_IDLE, AHB_PENDING, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                self.queue_ahb_init(yw, yh);
            }
            // Ditto for the direct-upload mode (resolves the two GL texture ids once).
            if direct_want
                && self
                    .direct
                    .state
                    .compare_exchange(AHB_IDLE, AHB_PENDING, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                self.queue_direct_init();
            }
        }

        self.poll_ns += t0.elapsed().as_nanos() as u64;
        self.poll_samples += 1;
        if self.poll_samples >= 120 {
            let avg_us = self.poll_ns / u64::from(self.poll_samples) / 1000;
            godot_print!(
                "[xreal] camera poll avg={avg_us}us over {} frames mode={}",
                self.poll_samples,
                if ahb_mode { "ahb" } else { "copy" }
            );
            self.poll_ns = 0;
            self.poll_samples = 0;
        }

        if self.frames <= 3 || self.frames.is_multiple_of(120) {
            let step = (y.len() / 4096).max(1);
            let (mut sum, mut n) = (0u64, 0u64);
            let mut i = 0;
            while i < y.len() {
                sum += y[i] as u64;
                n += 1;
                i += step;
            }
            let mean = sum.checked_div(n).unwrap_or(0);
            godot_print!(
                "[xreal] camera frame #{} y={yw}x{yh} cbcr={cw}x{ch} mean_luma={mean}{}",
                self.frames,
                if ahb_mode { " (ahb: luma n/a)" } else { "" }
            );
        }
        true
    }

    /// Queue the one-shot direct-upload setup on the render thread: resolve the GL ids of both
    /// textures (`texture_get_native_handle`). Any failure pins [`AHB_FAILED`] → copy path.
    fn queue_direct_init(&self) {
        let (Some(y_tex), Some(cbcr_tex)) = (self.y_tex.as_ref(), self.cbcr_tex.as_ref()) else {
            self.direct.state.store(AHB_FAILED, Ordering::Release);
            return;
        };
        let y_rid = y_tex.get_rid();
        let cbcr_rid = cbcr_tex.get_rid();
        let shared = self.direct.clone();
        let callable = Callable::from_fn("xreal_camera_direct_init", move |_| {
            let mut rs = RenderingServer::singleton();
            let y_id = rs.texture_get_native_handle(y_rid) as u32;
            let cbcr_id = rs.texture_get_native_handle(cbcr_rid) as u32;
            if y_id == 0 || cbcr_id == 0 {
                godot_warn!(
                    "[xreal] camera direct: texture_get_native_handle y={y_id} cbcr={cbcr_id}; \
                     staying on the copy path"
                );
                shared.state.store(AHB_FAILED, Ordering::Release);
            } else {
                godot_print!(
                    "[xreal] camera direct: gl ids y={y_id} cbcr={cbcr_id} (render-thread upload active)"
                );
                shared.y_gl_id.store(y_id, Ordering::Release);
                shared.cbcr_gl_id.store(cbcr_id, Ordering::Release);
                shared.state.store(AHB_READY, Ordering::Release);
            }
            Variant::nil()
        });
        RenderingServer::singleton().call_on_render_thread(&callable);
    }

    /// Dispatch one render-thread grab + upload: acquire the latest frame there, `glTexSubImage2D`
    /// the Y plane straight from SDK memory, interleave CbCr into the reused staging buffer and
    /// upload it too. Falls back permanently (state → [`AHB_FAILED`]) on a GL error.
    fn dispatch_direct_grab(shared: &Arc<DirectShared>) {
        let shared = shared.clone();
        let callable = Callable::from_fn("xreal_camera_direct_grab", move |_| {
            let Some(session) = session::shared() else {
                return Variant::nil();
            };
            let t0 = Instant::now();
            let y_id = shared.y_gl_id.load(Ordering::Acquire);
            let cbcr_id = shared.cbcr_gl_id.load(Ordering::Acquire);
            let mut gl_err = 0u32;
            let uploaded = session.rgb_camera_with_planes(&mut |planes| {
                let (y_ptr, yw, yh) = planes[0]; // Y straight from SDK memory — no staging at all.
                gl_err = crate::gl::upload_texture_2d(y_id, yw, yh, 1, y_ptr);
                if gl_err != 0 {
                    return false;
                }
                let (v_ptr, vw, vh) = planes[1];
                let (u_ptr, uw, uh) = planes[2];
                let n = ((uw as usize) * (uh as usize)).min((vw as usize) * (vh as usize));
                let mut buf = shared.cbcr_buf.lock().expect("cbcr staging mutex");
                buf.resize(n * 2, 0);
                unsafe {
                    for i in 0..n {
                        *buf.get_unchecked_mut(i * 2) = *u_ptr.add(i); // Cb = U
                        *buf.get_unchecked_mut(i * 2 + 1) = *v_ptr.add(i); // Cr = V
                    }
                }
                gl_err = crate::gl::upload_texture_2d(cbcr_id, uw, uh, 2, buf.as_ptr());
                gl_err == 0
            });
            match uploaded {
                Some(true) => {
                    shared
                        .ns
                        .fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    let n = shared.samples.fetch_add(1, Ordering::Relaxed) + 1;
                    if n >= 120 {
                        let total = shared.ns.swap(0, Ordering::Relaxed);
                        shared.samples.store(0, Ordering::Relaxed);
                        godot_print!(
                            "[xreal] camera poll avg={}us over {n} frames mode=direct",
                            total / u64::from(n) / 1000
                        );
                    }
                }
                Some(false) => {
                    godot_warn!(
                        "[xreal] camera direct: upload failed gl_err={gl_err}; back to the copy path"
                    );
                    shared.state.store(AHB_FAILED, Ordering::Release);
                }
                None => {} // no fresh frame this tick
            }
            Variant::nil()
        });
        RenderingServer::singleton().call_on_render_thread(&callable);
    }

    /// Queue the one-shot AHB/EGLImage setup on the render thread: resolve `y_tex`'s GL id, then
    /// allocate + bind the R8 hardware buffer over it (`camera_ahb_create_r8_bound`). Publishes
    /// the plane into [`AhbShared`] on success; any failure logs the failing step and pins the
    /// state to [`AHB_FAILED`] so `poll_frame` stays on the copy path.
    fn queue_ahb_init(&self, w: i32, h: i32) {
        let Some(y_tex) = self.y_tex.as_ref() else {
            self.ahb.state.store(AHB_FAILED, Ordering::Release);
            return;
        };
        let rid = y_tex.get_rid();
        let shared = self.ahb.clone();
        let callable = Callable::from_fn("xreal_camera_ahb_init", move |_| {
            let result = (|| -> Result<CameraAhbPlane, String> {
                let session = session::shared().ok_or_else(|| "no session".to_string())?;
                let gl_id = RenderingServer::singleton().texture_get_native_handle(rid) as u32;
                if gl_id == 0 {
                    return Err("texture_get_native_handle -> 0".into());
                }
                session.camera_ahb_create_r8_bound(w, h, gl_id)
            })();
            match result {
                Ok(plane) => {
                    godot_print!(
                        "[xreal] camera ahb: R8 {w}x{h} bound over the Y texture (zero-upload Y active)"
                    );
                    *shared.plane.lock().expect("ahb plane mutex") = Some(plane);
                    shared.state.store(AHB_READY, Ordering::Release);
                }
                Err(e) => {
                    godot_warn!("[xreal] camera ahb: init failed — {e}; staying on the copy path");
                    shared.state.store(AHB_FAILED, Ordering::Release);
                }
            }
            Variant::nil()
        });
        RenderingServer::singleton().call_on_render_thread(&callable);
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

/// Create the `ImageTexture` on the first frame, then `update()` it in place (cheap, same size).
fn update_texture(slot: &mut Option<Gd<ImageTexture>>, img: &Gd<Image>) {
    match slot {
        Some(tex) => tex.update(img),
        None => *slot = ImageTexture::create_from_image(img),
    }
}
