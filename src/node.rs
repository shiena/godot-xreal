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

        // Point the eye cameras from the (now-updated) head transform, then publish their
        // offscreen textures for the frame tick to blit into the XREAL eye buffers.
        self.update_stereo();
    }
}

impl XrealHeadTracker {
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
        for (i, cam) in rig.cameras.iter_mut().enumerate() {
            let sign = if i == 0 { -1.0 } else { 1.0 };
            let offset = Transform3D::new(Basis::IDENTITY, Vector3::new(sign * HALF_IPD, 0.0, 0.0));
            cam.set_global_transform(head * offset);
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
