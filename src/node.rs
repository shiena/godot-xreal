//! [`XrealHeadTracker`] — the 3DoF MVP node.
//!
//! Add it to a scene and parent a `Camera3D` under it. At runtime on XREAL
//! hardware it drives its own rotation from the native head pose every frame, so
//! the camera looks around with the wearer's head and the result is presented on
//! the (mirrored) device screen. On desktop/editor the native libraries are
//! absent, so the node stays at identity and logs a single warning.

use godot::classes::{INode3D, Node3D};
use godot::prelude::*;

use crate::session;

#[derive(GodotClass)]
#[class(base = Node3D)]
pub struct XrealHeadTracker {
    base: Base<Node3D>,
    /// Whether a head pose was applied on the most recent frame.
    tracking: bool,
    /// Frame counter, used to throttle the "no pose" diagnostic log.
    frames: u64,
}

#[godot_api]
impl INode3D for XrealHeadTracker {
    fn init(base: Base<Node3D>) -> Self {
        Self {
            base,
            tracking: false,
            frames: 0,
        }
    }

    fn ready(&mut self) {
        // Kick off initialization early; `shared()` logs its own outcome (and retries on
        // later frames if the Android Activity has not been published yet).
        let _ = session::shared();
    }

    fn process(&mut self, _delta: f64) {
        let Some(session) = session::shared() else {
            self.tracking = false;
            return;
        };
        match session.head_rotation() {
            Some(rotation) => {
                self.tracking = true;
                self.base_mut().set_quaternion(rotation);
            }
            None => {
                self.tracking = false;
                // Throttled (~every 2s at 60fps) so we can diagnose why no pose arrives.
                self.frames = self.frames.wrapping_add(1);
                if self.frames % 120 == 1 {
                    godot_warn!("[xreal] no head pose — {}", session.diagnostics());
                }
            }
        }
    }
}

#[godot_api]
impl XrealHeadTracker {
    /// Whether native head tracking fed a pose on the last frame.
    #[func]
    fn is_tracking(&self) -> bool {
        self.tracking
    }

    /// Re-center the 3DoF view so the current head direction becomes "forward".
    #[func]
    fn recenter(&mut self) {
        if let Some(session) = session::shared() {
            session.recenter();
        }
    }
}
