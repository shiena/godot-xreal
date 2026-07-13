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
use godot::classes::{CameraFeed, ICameraFeed, Image};
use godot::prelude::*;

use crate::session;

#[derive(GodotClass)]
#[class(base = CameraFeed)]
pub struct XrealCameraFeed {
    base: Base<CameraFeed>,
    /// Capture handle from `StartRGBCameraDataCapture`, while active.
    capture_handle: Option<u64>,
    frames: u64,
}

#[godot_api]
impl ICameraFeed for XrealCameraFeed {
    fn init(base: Base<CameraFeed>) -> Self {
        Self {
            base,
            capture_handle: None,
            frames: 0,
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
    /// Poll the latest RGB-camera frame and push it into the feed (grayscale Y for the spike).
    /// Returns `true` if a frame was pushed this call. Call once per frame from a driver.
    #[func]
    fn poll_frame(&mut self) -> bool {
        let Some(session) = session::shared() else {
            return false;
        };
        let Some((y, w, h)) = session.rgb_camera_grab_y() else {
            return false;
        };
        self.frames += 1;

        // Expand the 8-bit Y plane to grayscale RGB8 (r = g = b = luma).
        let mut rgb = Vec::with_capacity(y.len() * 3);
        for &l in &y {
            rgb.extend_from_slice(&[l, l, l]);
        }
        let data = PackedByteArray::from(rgb.as_slice());
        let Some(image) = Image::create_from_data(w, h, false, Format::RGB8, &data) else {
            godot_warn!("[xreal] camera: Image::create_from_data failed ({w}x{h})");
            return false;
        };
        self.base_mut().set_rgb_image(&image);

        if self.frames <= 5 || self.frames % 60 == 0 {
            // Mean luma over a coarse sample — proves the bytes are real image data, not zeros.
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
                "[xreal] camera frame #{} {w}x{h} y_bytes={} mean_luma={mean}",
                self.frames,
                y.len()
            );
        }
        true
    }
}
