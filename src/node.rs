//! [`XrealHeadTracker`] — the 3DoF MVP node.
//!
//! Add it to a scene and parent a `Camera3D` under it. At runtime on XREAL
//! hardware it drives its own rotation from the native head pose every frame, so
//! the camera looks around with the wearer's head and the result is presented on
//! the (mirrored) device screen. On desktop/editor the native libraries are
//! absent, so the node stays at identity and logs a single warning.

use godot::classes::sub_viewport::UpdateMode;
use godot::classes::{Camera3D, INode3D, Node3D, RenderingServer, SubViewport};
use godot::prelude::*;

use crate::session;

/// Per-eye render size (matches the XREAL swapchain buffers created via CreateTexture).
const EYE_W: i32 = 1968;
const EYE_H: i32 = 1134;
/// Vertical FOV (deg) ≈ XREAL One Pro per-eye (~46° horizontal at 1968×1134 aspect).
const EYE_FOV: f32 = 27.4;
/// Half the interpupillary distance (m) — each eye camera is offset ±this along head-local X.
const HALF_IPD: f32 = 0.0315;

/// Two offscreen SubViewports (left/right), each with a Camera3D, rendering the main world from
/// per-eye viewpoints. Their textures are blitted into the XREAL eye swapchain buffers.
struct StereoRig {
    viewports: [Gd<SubViewport>; 2],
    cameras: [Gd<Camera3D>; 2],
}

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
    /// Lazily-created per-eye offscreen render rig (stereo).
    stereo: Option<StereoRig>,
    /// Whether the `display_started` signal has been emitted (once, on first tracking).
    display_signaled: bool,
    /// Last-seen glasses hot-plug event counts (from the JNI DisplayManager callbacks); a change
    /// re-emits `glasses_connected` / `glasses_disconnected` on the Godot main thread.
    last_connect_count: u32,
    last_disconnect_count: u32,
}

#[godot_api]
impl INode3D for XrealHeadTracker {
    fn init(base: Base<Node3D>) -> Self {
        Self {
            base,
            tracking: false,
            frames: 0,
            debug_pose: GString::new(),
            stereo: None,
            display_signaled: false,
            last_connect_count: 0,
            last_disconnect_count: 0,
        }
    }

    fn ready(&mut self) {
        // Kick off initialization early; `shared()` logs its own outcome (and retries on
        // later frames if the Android Activity has not been published yet).
        let _ = session::shared();
    }

    fn process(&mut self, _delta: f64) {
        self.frames = self.frames.wrapping_add(1);
        // Re-emit glasses hot-plug events before the session check so connect/disconnect are
        // reported even while no session exists yet (e.g. started without the glasses).
        self.poll_glasses_events();
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

        // Build the per-eye offscreen render rig once we're in the tree (has a World3D).
        self.ensure_stereo();

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
                if !self.display_signaled {
                    self.display_signaled = true;
                    self.signals().display_started().emit();
                }
                self.base_mut().set_quaternion(rotation);
                let euler = rotation.get_euler() * (180.0 / std::f32::consts::PI);
                // Rotation calibration log: raw NRSDK quaternion + resulting Godot Euler (deg).
                // Move the head in a known way and match: pitch=X (nod up/down), yaw=Y (turn
                // left/right), roll=Z (tilt ear-to-shoulder). Wrong sign/axis → adjust the flip in
                // NrPose::to_godot_quaternion.
                if self.frames % 30 == 0 {
                    godot_print!(
                        "[xreal] pose q(wxyz)=({:.3},{:.3},{:.3},{:.3}) euler_deg pitch/x={:.1} yaw/y={:.1} roll/z={:.1}",
                        pose.qx, pose.qy, pose.qz, pose.qw, euler.x, euler.y, euler.z
                    );
                }
                self.debug_pose = GString::from(&format!(
                    "pitch {:.0}\nyaw {:.0}\nroll {:.0}\nq {:.2} {:.2} {:.2} {:.2}",
                    euler.x, euler.y, euler.z, pose.qx, pose.qy, pose.qz, pose.qw
                ));
            }
            None => {
                self.tracking = false;
                // Throttled (~every 2s at 60fps) so we can diagnose why no pose arrives.
                if self.frames % 120 == 1 {
                    godot_warn!("[xreal] no head pose — {}", session.diagnostics());
                }
            }
        }

        // Point the eye cameras from the (now-updated) head transform, then publish their
        // offscreen textures for the frame tick to blit into the XREAL eye buffers.
        self.update_stereo();
    }
}

impl XrealHeadTracker {
    /// Poll the JNI glasses hot-plug counters and re-emit any new events as signals (called on the
    /// Godot main thread, where signal emission is safe — the JNI callbacks run on the UI thread).
    fn poll_glasses_events(&mut self) {
        let connect = crate::jni_bridge::glasses_connect_count();
        if connect != self.last_connect_count {
            self.last_connect_count = connect;
            self.signals().glasses_connected().emit();
        }
        let disconnect = crate::jni_bridge::glasses_disconnect_count();
        if disconnect != self.last_disconnect_count {
            self.last_disconnect_count = disconnect;
            self.signals().glasses_disconnected().emit();
        }
    }

    /// Create the two per-eye SubViewports + cameras once, sharing the main World3D so they render
    /// the same scene. No-op until the node is in the tree (needs a World3D).
    fn ensure_stereo(&mut self) {
        if self.stereo.is_some() {
            return;
        }
        let Some(world) = self.base().get_world_3d() else {
            return;
        };
        let mut make_eye = || {
            let mut sv = SubViewport::new_alloc();
            sv.set_size(Vector2i::new(EYE_W, EYE_H));
            sv.set_update_mode(UpdateMode::ALWAYS);
            sv.set_world_3d(&world);
            let mut cam = Camera3D::new_alloc();
            cam.set_fov(EYE_FOV);
            cam.set_near(0.05);
            cam.set_far(1000.0);
            cam.set_current(true);
            sv.add_child(&cam);
            (sv, cam)
        };
        let (svl, caml) = make_eye();
        let (svr, camr) = make_eye();
        self.base_mut().add_child(&svl);
        self.base_mut().add_child(&svr);
        self.stereo = Some(StereoRig {
            viewports: [svl, svr],
            cameras: [caml, camr],
        });
        godot_print!("[xreal] stereo rig created ({EYE_W}x{EYE_H} per eye)");
    }

    /// Aim the eye cameras from the head transform (±IPD) and publish their GL textures.
    fn update_stereo(&mut self) {
        let head = self.base().get_global_transform();
        let Some(rig) = self.stereo.as_mut() else {
            // Mono fallback: publish the window size so the frame tick blits the default framebuffer.
            if let Some(viewport) = self.base().get_viewport() {
                let size = viewport.get_visible_rect().size;
                crate::unity_plugin::set_godot_source_size(size.x as i32, size.y as i32);
            }
            return;
        };
        // Apply the SDK's exact per-eye projection + eye offset when available (pixel-accurate AR),
        // else fall back to the symmetric IPD + hardcoded FOV.
        let proj = crate::unity_plugin::stereo_projection();
        const NEAR: f32 = 0.05;
        const FAR: f32 = 1000.0;
        for (i, cam) in rig.cameras.iter_mut().enumerate() {
            let p = proj[i];
            let eye_x = if p.valid && p.px != 0.0 {
                p.px
            } else if i == 0 {
                -HALF_IPD
            } else {
                HALF_IPD
            };
            cam.set_global_transform(head * Transform3D::new(Basis::IDENTITY, Vector3::new(eye_x, 0.0, 0.0)));

            if p.valid && (p.r - p.l) > 1e-4 && (p.t - p.b) > 1e-4 {
                // Half-angle tangents → asymmetric frustum. Godot's Camera3D.set_frustum(size,
                // offset, near, far) maps to near-plane extents ±size/2 (vert) / ±size*aspect/2
                // (horiz) shifted by offset; near-plane coord = tangent*near.
                let (size, offset) = frustum_size_offset(p.l, p.r, p.t, p.b, NEAR);
                cam.set_frustum(size, offset, NEAR, FAR);
            } else {
                cam.set_fov(EYE_FOV);
                cam.set_near(NEAR);
                cam.set_far(FAR);
            }
        }
        let mut rs = RenderingServer::singleton();
        // Use the actual render-target texture RID (viewport_get_texture on the viewport RID), not
        // the ViewportTexture *resource* RID, whose native handle is 0.
        let mut handle = |sv: &Gd<SubViewport>| -> u32 {
            let tex_rid = rs.viewport_get_texture(sv.get_viewport_rid());
            rs.texture_get_native_handle(tex_rid) as u32
        };
        let left = handle(&rig.viewports[0]);
        let right = handle(&rig.viewports[1]);
        crate::unity_plugin::set_godot_eye_sources(left, right, EYE_W, EYE_H);
    }
}

#[godot_api]
impl XrealHeadTracker {
    /// Emitted once when the glasses display + head tracking first go live (the first frame a head
    /// pose arrives). Connect it in GDScript and call `recenter()` to make the current head
    /// direction "forward" at startup.
    #[signal]
    fn display_started();

    /// Emitted when the XREAL glasses display is plugged in at runtime (`onDisplayAdded`). Fires
    /// even if the app started with the glasses disconnected — the native session bootstrap then
    /// retries `CreateSession` and `display_started` follows once tracking comes up.
    #[signal]
    fn glasses_connected();

    /// Emitted when the XREAL glasses display is unplugged at runtime (`onDisplayRemoved`).
    #[signal]
    fn glasses_disconnected();

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

/// Asymmetric projection from the SDK's per-eye half-angle tangents (l, r, t, b) into Godot's
/// `Camera3D::set_frustum(size, offset, near, far)` parameters. `size` is the vertical near-plane
/// extent and `offset` shifts the (otherwise centered) near-plane rectangle; a near-plane
/// coordinate equals `tangent * near`. Kept as a free function so the calibrated mapping is unit
/// tested (see the tests module) without needing a live Camera3D.
fn frustum_size_offset(l: f32, r: f32, t: f32, b: f32, near: f32) -> (f32, Vector2) {
    let size = (t - b) * near;
    let offset = Vector2::new((r + l) * 0.5 * near, (t + b) * 0.5 * near);
    (size, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_tangents_center_the_frustum() {
        // Symmetric l/r and t/b → no offset; size is the full vertical extent at the near plane.
        let (size, offset) = frustum_size_offset(-0.5, 0.5, 0.4, -0.4, 0.05);
        assert!((size - 0.8 * 0.05).abs() < 1e-6, "size {size}");
        assert!(offset.x.abs() < 1e-6 && offset.y.abs() < 1e-6, "offset {offset:?}");
    }

    #[test]
    fn asymmetric_tangents_shift_the_frustum() {
        // l=-0.6,r=0.4 → horizontal center at (r+l)/2=-0.1; t=0.5,b=-0.3 → vertical center at 0.1.
        let (size, offset) = frustum_size_offset(-0.6, 0.4, 0.5, -0.3, 0.05);
        assert!((size - 0.8 * 0.05).abs() < 1e-6, "size {size}");
        assert!((offset.x - (-0.1 * 0.05)).abs() < 1e-6, "offset.x {}", offset.x);
        assert!((offset.y - (0.1 * 0.05)).abs() < 1e-6, "offset.y {}", offset.y);
    }
}
