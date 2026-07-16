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
    /// App-side recenter reference. The SDK's `RecenterGlasses` (libXREALXRPlugin.so, display
    /// subsystem) does NOT reset the pose we read via `XREALGetHeadPoseAtTime` (session-manager
    /// subsystem) — device-confirmed: the pose quaternion is unchanged after calling it. So
    /// recentering is done here: `recenter()` captures the current raw rotation and `process()`
    /// applies `reference.inverse() * raw`, making "wherever you look at recenter" the forward.
    recenter_reference: Quaternion,
    /// The raw (uncorrected) rotation from the last pose sample; captured by `recenter()`.
    last_raw_rotation: Quaternion,
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
            recenter_reference: Quaternion::default(),
            last_raw_rotation: Quaternion::default(),
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
        // Drain glasses hardware events (keys, wear sensor, brightness…) queued by the
        // native callback on the SDK thread, and re-emit them as signals.
        self.poll_hardware_events();
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
        // Primary: drive the eye cameras from the **display** InputManager pose — the exact pose the
        // compositor reprojects the glasses layer against (full orientation incl. ROLL, which the
        // compact session-manager NrPose lacks; see docs/archive/roll-tracking-investigation.md). Sharing one
        // pose between our render and the compositor is what makes a correct peek window on all axes.
        // CRASH RULE (device-confirmed): never query BOTH head_pose_display() and head_pose() in the
        // same frame → SIGSEGV 0x3f800000. So the session-manager read is a fallback used ONLY when
        // the display export is entirely absent (None), never merely when a frame's block is unusable.
        match session.head_pose_display() {
            Some(raw) => {
                if let Some(rotation) = Self::display_rotation(&raw) {
                    self.tracking = true;
                    if !self.display_signaled {
                        self.display_signaled = true;
                        self.signals().display_started().emit();
                    }
                    self.last_raw_rotation = rotation;
                    // Bake the compositor's pose straight into the node. No app-side recenter — the
                    // peek window needs baked camera rotation == the compositor's reprojection pose;
                    // recenter is delegated to the SDK (session.recenter → NativePerception::Recenter),
                    // which shifts this pose source and the layer together.
                    self.base_mut().set_quaternion(rotation);
                    let euler = rotation.get_euler() * (180.0 / std::f32::consts::PI);
                    // Calibration log: extracted Godot euler + the raw 4×4 rows. Move head in a known
                    // way — nod=pitch/X, turn=yaw/Y, tilt=roll/Z — and check each axis + sign; if one
                    // reads inverted, flip it in display_rotation().
                    if self.frames < 16 || self.frames.is_multiple_of(30) {
                        godot_print!(
                            "[xreal] DISP euler pitch/x={:.1} yaw/y={:.1} roll/z={:.1} | \
                             r0=[{:.3},{:.3},{:.3}] r1=[{:.3},{:.3},{:.3}] r2=[{:.3},{:.3},{:.3}] pos=[{:.3},{:.3},{:.3}]",
                            euler.x, euler.y, euler.z,
                            raw[0], raw[1], raw[2], raw[4], raw[5], raw[6], raw[8], raw[9], raw[10],
                            raw[12], raw[13], raw[14]
                        );
                    }
                    self.debug_pose = GString::from(&format!(
                        "DISP\npitch {:.0}\nyaw {:.0}\nroll {:.0}",
                        euler.x, euler.y, euler.z
                    ));
                } else {
                    // Display export present but this frame's block isn't a valid rotation (e.g. a
                    // startup transient). Hold the previous transform; do NOT fall through to the
                    // session-manager pose (that would query both pipelines this frame — the crash).
                    if self.frames % 120 == 1 {
                        godot_warn!(
                            "[xreal] DISP pose: 16-float block not a valid rotation transform (raw={raw:?})"
                        );
                    }
                }
            }
            None => match session.head_pose() {
                Some((pose, rotation)) => {
                    self.tracking = true;
                    if !self.display_signaled {
                        self.display_signaled = true;
                        self.signals().display_started().emit();
                    }
                    // Session-manager fallback (display export absent): app-side recenter, since the
                    // SDK's RecenterGlasses does not affect this pose source.
                    self.last_raw_rotation = rotation;
                    let corrected = (self.recenter_reference.inverse() * rotation).normalized();
                    self.base_mut().set_quaternion(corrected);
                    let euler = corrected.get_euler() * (180.0 / std::f32::consts::PI);
                    if self.frames.is_multiple_of(30) {
                        godot_print!(
                            "[xreal] SM pose q(wxyz)=({:.3},{:.3},{:.3},{:.3}) euler_deg pitch/x={:.1} yaw/y={:.1} roll/z={:.1}",
                            pose.qx, pose.qy, pose.qz, pose.qw, euler.x, euler.y, euler.z
                        );
                    }
                    self.debug_pose = GString::from(&format!(
                        "SM\npitch {:.0}\nyaw {:.0}\nroll {:.0}",
                        euler.x, euler.y, euler.z
                    ));
                }
                None => {
                    self.tracking = false;
                    // Throttled (~every 2s at 60fps) so we can diagnose why no pose arrives.
                    if self.frames % 120 == 1 {
                        godot_warn!("[xreal] no head pose — {}", session.diagnostics());
                    }
                }
            },
        }

        // Point the eye cameras from the (now-updated) head transform, then publish their
        // offscreen textures for the frame tick to blit into the XREAL eye buffers.
        self.update_stereo();
    }
}

impl XrealHeadTracker {
    /// Interpret libXREALXRPlugin.so's 16-float display head-pose block as a Godot rotation.
    ///
    /// DEVICE-CONFIRMED layout (on-device raw log): the 16 floats are a **4×4 row-major transform**
    /// — the upper-left 3×3 is the head rotation (each row a unit vector), the last row (12,13,14) is
    /// the tiny position, the last column (3,7,11) is 0 and raw[15] is 1 (it copies
    /// `NativePerception::GetHeadPose`'s struct return verbatim). We validate that structure, extract
    /// the quaternion from the 3×3 (Shepperd), then apply the same NRSDK→Godot handedness flip as
    /// `NrPose::to_godot_quaternion`. Returns `None` if the block isn't a valid rotation transform
    /// (e.g. before the session is live), so the caller can hold the previous pose.
    fn display_rotation(raw: &[f32; 16]) -> Option<Quaternion> {
        // Row-major 3×3 rotation.
        let (m00, m01, m02) = (raw[0], raw[1], raw[2]);
        let (m10, m11, m12) = (raw[4], raw[5], raw[6]);
        let (m20, m21, m22) = (raw[8], raw[9], raw[10]);
        // Validate the homogeneous 4×4 structure so we don't extract from a zero/garbage block.
        let unit = |a: f32, b: f32, c: f32| ((a * a + b * b + c * c).sqrt() - 1.0).abs() < 0.05;
        let structured = unit(m00, m01, m02)
            && unit(m10, m11, m12)
            && unit(m20, m21, m22)
            && raw[3].abs() < 0.01
            && raw[7].abs() < 0.01
            && raw[11].abs() < 0.01
            && (raw[15] - 1.0).abs() < 0.05;
        if !structured {
            return None;
        }
        // Standard rotation-matrix → quaternion (Shepperd), in the source (NRSDK/Unity LH) frame.
        let trace = m00 + m11 + m22;
        let (x, y, z, w) = if trace > 0.0 {
            let s = (trace + 1.0).sqrt() * 2.0; // s = 4w
            ((m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s, 0.25 * s)
        } else if m00 > m11 && m00 > m22 {
            let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0; // s = 4x
            (0.25 * s, (m01 + m10) / s, (m02 + m20) / s, (m21 - m12) / s)
        } else if m11 > m22 {
            let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0; // s = 4y
            ((m01 + m10) / s, 0.25 * s, (m12 + m21) / s, (m02 - m20) / s)
        } else {
            let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0; // s = 4z
            ((m02 + m20) / s, (m12 + m21) / s, 0.25 * s, (m10 - m01) / s)
        };
        // NRSDK → Godot handedness. Device-calibrated with a wearer (DISP calibration log): with
        // (-x,-y,z,w) roll (Z) and yaw (Y) were correct but PITCH (X) came out inverted — nodding
        // down clipped the box's bottom instead of its top. Keep pitch un-negated: (x,-y,z,w) makes
        // nod=pitch/X, turn=yaw/Y, tilt=roll/Z all track world-locked in the correct direction.
        Some(Quaternion::new(x, -y, z, w).normalized())
    }

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

    /// Dispatch queued `GlassesEventData` into typed signals (main thread). Unknown action
    /// types still reach GDScript through the catch-all `glasses_event` signal.
    fn poll_hardware_events(&mut self) {
        use crate::ffi as f;
        for ev in crate::glasses_events::drain() {
            // Raw event log — this is the Phase A device-verification instrument (one line
            // per physical key press / wear change / brightness step; low volume).
            godot_print!(
                "[xreal] glasses event: type={} para={} para2={} para3={}",
                ev.action_type,
                ev.para,
                ev.para2,
                ev.para3
            );
            match ev.action_type {
                f::ACTION_TYPE_CLICK | f::ACTION_TYPE_DOUBLE_CLICK | f::ACTION_TYPE_LONG_PRESS => {
                    // para = XREALKeyType, action_type = XREALClickType (same numbering as
                    // the ACTION_CLICK/DOUBLE_CLICK/LONG_PRESS constants).
                    self.signals()
                        .key_event()
                        .emit(ev.para as i64, ev.action_type as i64);
                }
                f::ACTION_TYPE_KEY_STATE => {
                    self.signals()
                        .key_state_changed()
                        .emit(ev.para as i64, ev.para2 as i64);
                }
                f::ACTION_TYPE_PROXIMITY_WEARING_STATE => {
                    // Mirror the Unity handler: only PUT_ON / TAKE_OFF are forwarded.
                    if ev.para == f::WEARING_STATUS_PUT_ON || ev.para == f::WEARING_STATUS_TAKE_OFF
                    {
                        self.signals()
                            .wearing_changed()
                            .emit(ev.para == f::WEARING_STATUS_PUT_ON);
                    }
                }
                f::ACTION_TYPE_INCREASE_BRIGHTNESS | f::ACTION_TYPE_DECREASE_BRIGHTNESS => {
                    self.signals().brightness_changed().emit(ev.para as i64);
                }
                f::ACTION_TYPE_INCREASE_VOLUME | f::ACTION_TYPE_DECREASE_VOLUME => {
                    self.signals().volume_changed().emit(ev.para as i64);
                }
                f::ACTION_TYPE_NEXT_EC_LEVEL => {
                    self.signals().ec_level_changed().emit(ev.para as i64);
                }
                _ => {}
            }
            self.signals().glasses_event().emit(
                ev.action_type as i64,
                ev.para as i64,
                ev.para2 as i64,
                ev.para3 as f64,
            );
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
        let make_eye = || {
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
            cam.set_global_transform(
                head * Transform3D::new(Basis::IDENTITY, Vector3::new(eye_x, 0.0, 0.0)),
            );

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
        let rs = RenderingServer::singleton();
        // Use the actual render-target texture RID (viewport_get_texture on the viewport RID), not
        // the ViewportTexture *resource* RID, whose native handle is 0.
        let handle = |sv: &Gd<SubViewport>| -> u32 {
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

    /// A physical key on the glasses was clicked. `key` is one of the `KEY_*` constants
    /// (MULTI / INCREASE / DECREASE / MENU), `action` one of `ACTION_CLICK` /
    /// `ACTION_DOUBLE_CLICK` / `ACTION_LONG_PRESS`.
    #[signal]
    fn key_event(key: i64, action: i64);

    /// Raw down/up transition of a physical key: `state` is `KEY_STATE_DOWN` / `KEY_STATE_UP`.
    #[signal]
    fn key_state_changed(key: i64, state: i64);

    /// The proximity (wear) sensor reported the glasses were put on (`true`) or taken off.
    #[signal]
    fn wearing_changed(wearing: bool);

    /// The glasses brightness level changed (brightness rocker or system UI).
    #[signal]
    fn brightness_changed(level: i64);

    /// The glasses volume level changed.
    #[signal]
    fn volume_changed(level: i64);

    /// The electrochromic dimming level changed (One Pro).
    #[signal]
    fn ec_level_changed(level: i64);

    /// Catch-all for every native glasses event, including types without a dedicated
    /// signal (temperature, screen on/off, disconnect reason…). Values are the raw
    /// `GlassesEventData` fields; see `XREALActionType` in `docs/plans/input-plan.md`.
    #[signal]
    fn glasses_event(action_type: i64, para: i64, para2: i64, para3: f64);

    // `XREALKeyType` (which physical key).
    #[constant]
    const KEY_MULTI: i64 = 1;
    #[constant]
    const KEY_INCREASE: i64 = 2;
    #[constant]
    const KEY_DECREASE: i64 = 3;
    #[constant]
    const KEY_MENU: i64 = 4;

    // `XREALClickType` (how it was pressed).
    #[constant]
    const ACTION_CLICK: i64 = 1;
    #[constant]
    const ACTION_DOUBLE_CLICK: i64 = 2;
    #[constant]
    const ACTION_LONG_PRESS: i64 = 3;

    // `XREALKeyState` (raw transitions, `key_state_changed`).
    #[constant]
    const KEY_STATE_DOWN: i64 = 1;
    #[constant]
    const KEY_STATE_UP: i64 = 2;

    /// Whether native head tracking fed a pose on the last frame.
    #[func]
    fn is_tracking(&self) -> bool {
        self.tracking
    }

    /// Re-center the 3DoF view so the current head direction becomes "forward".
    #[func]
    fn recenter(&mut self) {
        // App-side recenter: the current head direction becomes "forward" (identity). The
        // reference is the raw rotation of the latest pose sample, so this also cancels any
        // pitch offset picked up while the glasses sat on a desk during session start.
        self.recenter_reference = self.last_raw_rotation;
        let e = self.last_raw_rotation.get_euler() * (180.0 / std::f32::consts::PI);
        godot_print!(
            "[xreal] recenter: reference euler=({:.1},{:.1},{:.1})",
            e.x,
            e.y,
            e.z
        );
        // Still forward to the SDK's display-side recenter — harmless, and it may matter for
        // the compositor path even though it does not reset our pose source.
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
        assert!(
            offset.x.abs() < 1e-6 && offset.y.abs() < 1e-6,
            "offset {offset:?}"
        );
    }

    #[test]
    fn asymmetric_tangents_shift_the_frustum() {
        // l=-0.6,r=0.4 → horizontal center at (r+l)/2=-0.1; t=0.5,b=-0.3 → vertical center at 0.1.
        let (size, offset) = frustum_size_offset(-0.6, 0.4, 0.5, -0.3, 0.05);
        assert!((size - 0.8 * 0.05).abs() < 1e-6, "size {size}");
        assert!(
            (offset.x - (-0.1 * 0.05)).abs() < 1e-6,
            "offset.x {}",
            offset.x
        );
        assert!(
            (offset.y - (0.1 * 0.05)).abs() < 1e-6,
            "offset.y {}",
            offset.y
        );
    }
}
