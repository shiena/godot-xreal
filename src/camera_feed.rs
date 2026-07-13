//! [`XrealCameraFeed`] — the XREAL glasses' RGB camera exposed as a Godot `CameraFeed`.
//!
//! Subclasses `CameraFeed` (the idiomatic custom-camera-source pattern): `activate_feed` starts the
//! XREAL RGB capture, `deactivate_feed` stops it, and `poll_frame()` — called each frame by a driver
//! (e.g. the demo's `_process`) — pushes the latest frame into the feed. Spike scope: pushes the
//! **Y plane** as grayscale `RGB8` via `set_rgb_image`. See `docs/camera-feed-plan.md`.
//!
//! Usage (GDScript):
//! ```gdscript
//! var feed = ClassDB.instantiate(&"XrealCameraFeed")
//! CameraServer.add_feed(feed)
//! feed.set_active(true)          # -> activate_feed() starts the camera
//! # each frame:
//! feed.poll_frame()
//! ```

use godot::classes::image::Format;
use godot::classes::{CameraFeed, ICameraFeed, Image, ImageTexture};
use godot::prelude::*;

/// Feed-image plumbing:
/// - `poll_frame` grabs the XREAL frame as Y + interleaved CbCr, calls `set_ycbcr_images` (so the
///   Godot CameraFeed carries data), AND keeps two plain `ImageTexture`s (`get_y_texture` /
///   `get_cbcr_texture`) updated. The 3D panel shader samples those ImageTextures directly — a
///   `CameraTexture` bound to a *script-fed* feed shows only the placeholder on this build, so the
///   direct textures are what actually display (matching the XREAL SDK's YUVTransRGB sample).

use crate::session;

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
                godot_warn!(
                    "[xreal] camera: StartRGBCameraDataCapture failed (CAMERA permission? plugin not ready?)"
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
    /// Poll the latest RGB-camera frame and push it into the feed as **separate Y + CbCr** images
    /// (`FEED_YCBCR_SEP`), for full-colour display via the YCbCr→RGB shader. Returns `true` if a
    /// frame was pushed this call. Call once per frame from a driver.
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
        // Feed the Godot CameraFeed (for CameraServer integration), and keep the plain ImageTextures
        // the 3D panel shader samples directly — a CameraTexture on a script feed only shows the
        // placeholder, so these are what actually display.
        self.base_mut().set_ycbcr_images(&y_img, &cbcr_img);
        update_texture(&mut self.y_tex, &y_img);
        update_texture(&mut self.cbcr_tex, &cbcr_img);

        if self.frames <= 3 || self.frames % 120 == 0 {
            let step = (y.len() / 4096).max(1);
            let (mut sum, mut n) = (0u64, 0u64);
            let mut i = 0;
            while i < y.len() {
                sum += y[i] as u64;
                n += 1;
                i += step;
            }
            let mean = if n > 0 { sum / n } else { 0 };
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
