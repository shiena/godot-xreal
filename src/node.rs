//! [`XrealHeadTracker`] — the 3DoF MVP node.
//!
//! Add it to a scene and parent a `Camera3D` under it. At runtime on XREAL
//! hardware it drives its own rotation from the native head pose every frame, so
//! the camera looks around with the wearer's head and the result is presented on
//! the (mirrored) device screen. On desktop/editor the native libraries are
//! absent, so the node stays at identity and logs a single warning.

use godot::classes::{INode3D, Node3D, RenderingServer};
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
    /// Last raw/converted pose sample for on-device visual debugging.
    debug_pose: GString,
}

#[godot_api]
impl INode3D for XrealHeadTracker {
    fn init(base: Base<Node3D>) -> Self {
        Self {
            base,
            tracking: false,
            frames: 0,
            debug_pose: GString::new(),
        }
    }

    fn ready(&mut self) {
        // Kick off initialization early; `shared()` logs its own outcome (and retries on
        // later frames if the Android Activity has not been published yet).
        let _ = session::shared();
    }

    fn process(&mut self, _delta: f64) {
        self.frames = self.frames.wrapping_add(1);
        let Some(session) = session::shared() else {
            self.tracking = false;
            return;
        };

        // Keep the glasses display awake by bypassing the proximity (wear) sensor auto-off.
        // The SDK no-ops this until `NativeGlasses` is ready and its return value is ambiguous,
        // so call it every frame for the first ~10s after the session appears.
        if self.frames < 600 {
            let status = session.set_display_bypass_psensor(true);
            if self.frames < 3 || self.frames == 120 || self.frames == 300 {
                godot_print!(
                    "[xreal] set_display_bypass_psensor(true) -> {status:?} (frame {})",
                    self.frames
                );
            }
        }

        // Drive the XREAL swapchain on the rendering thread (EGL context required).
        // First call invokes GfxThreadStart (CreateSwapchainEx → GL textures → SetSwapChainBuffers);
        // subsequent calls drive PopulateNextFrameDesc so the SDK's GLThread has a frame handle.
        let callable = Callable::from_fn("xreal_render_tick", |_| {
            crate::unity_plugin::run_render_thread_tick();
            Variant::nil()
        });
        RenderingServer::singleton().call_on_render_thread(&callable);
        match session.head_pose() {
            Some((pose, rotation)) => {
                self.tracking = true;
                self.base_mut().set_quaternion(rotation);
                let old_rotation =
                    Quaternion::new(-pose.qx, -pose.qy, pose.qz, pose.qw).normalized();
                let old_euler = old_rotation.get_euler() * (180.0 / std::f32::consts::PI);
                let new_euler = rotation.get_euler() * (180.0 / std::f32::consts::PI);
                let debug_pose = format!(
                    "POSE CONV X=-0.5\nNODE X {node_x:.1}\nNODE Y {node_y:.1}\nNODE Z {node_z:.1}\nOLD  X {old_x:.1} Y {old_y:.1} Z {old_z:.1}\nNEW  X {new_x:.1} Y {new_y:.1} Z {new_z:.1}\nQ {qx:.3} {qy:.3} {qz:.3} {qw:.3}",
                    node_x = new_euler.x,
                    node_y = new_euler.y,
                    node_z = new_euler.z,
                    old_x = old_euler.x,
                    old_y = old_euler.y,
                    old_z = old_euler.z,
                    new_x = new_euler.x,
                    new_y = new_euler.y,
                    new_z = new_euler.z,
                    qx = pose.qx,
                    qy = pose.qy,
                    qz = pose.qz,
                    qw = pose.qw,
                );
                self.debug_pose = GString::from(&debug_pose);
            }
            None => {
                self.tracking = false;
                // Throttled (~every 2s at 60fps) so we can diagnose why no pose arrives.
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

    /// Latest raw and converted pose sample for visual on-device debugging.
    #[func]
    fn debug_pose_text(&self) -> GString {
        self.debug_pose.clone()
    }
}
