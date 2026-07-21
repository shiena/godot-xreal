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
use godot::classes::{CameraFeed, ICameraFeed, Image, ImageTexture};
use godot::prelude::*;

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
    #[func]
    fn poll_frame(&mut self) -> bool {
        let Some(session) = session::shared() else {
            return false;
        };
        let Some((y, yw, yh, cbcr, cw, ch)) = session.rgb_camera_grab_yuv() else {
            return false;
        };
        self.frames += 1;

        let y_data = PackedByteArray::from(y.as_slice());
        let cbcr_data = PackedByteArray::from(cbcr.as_slice());
        // Y = single-channel R8 (luma); CbCr = two-channel RG8 (Cb in R, Cr in G).
        let Some(y_img) = Image::create_from_data(yw, yh, false, Format::R8, &y_data) else {
            return false;
        };
        let Some(cbcr_img) = Image::create_from_data(cw, ch, false, Format::RG8, &cbcr_data) else {
            return false;
        };
        // Keep the plain ImageTextures the shaders sample directly (the display route); feed the
        // base CameraFeed only on request (see the struct docs for why that route is off by default).
        if self.feed_camera_server {
            self.base_mut().set_ycbcr_images(&y_img, &cbcr_img);
        }
        update_texture(&mut self.y_tex, &y_img);
        update_texture(&mut self.cbcr_tex, &cbcr_img);

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
                "[xreal] camera frame #{} y={yw}x{yh} cbcr={cw}x{ch} mean_luma={mean}",
                self.frames
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

/// Create the `ImageTexture` on the first frame, then `update()` it in place (cheap, same size).
fn update_texture(slot: &mut Option<Gd<ImageTexture>>, img: &Gd<Image>) {
    match slot {
        Some(tex) => tex.update(img),
        None => *slot = ImageTexture::create_from_image(img),
    }
}
