//! [`XrealHeadTracker`], the head-tracking node.
//!
//! Add it to a scene as the XREAL backend driver. At runtime it publishes the native 6DoF head pose
//! through [`crate::xr_interface::XrealXrInterface`] so a regular Godot `XRCamera3D` can consume it,
//! while the existing compositor bridge presents the world to the glasses. The node still mirrors
//! the pose onto its own transform for backwards compatibility with the original child-Camera3D
//! rig. On desktop the native libraries are absent, so it stays inert.

use godot::classes::sub_viewport::UpdateMode;
use godot::classes::viewport::{Msaa, Scaling3DMode};
use godot::classes::{Camera3D, INode3D, Node3D, ProjectSettings, RenderingServer, SubViewport};
use godot::prelude::*;

use crate::gl;
use crate::session;

/// Per-eye render size (matches the XREAL swapchain buffers created via CreateTexture).
pub(crate) const EYE_W: i32 = 1968;
pub(crate) const EYE_H: i32 = 1134;
/// Last-resort vertical FOV when neither an app Camera3D nor the SDK's calibrated per-eye
/// projection is available. This is an approximation derived from One Pro calibration logs, not an
/// official device default.
pub(crate) const FALLBACK_EYE_VERTICAL_FOV: f32 = 27.4;
const DEFAULT_NEAR: f32 = 0.05;
const DEFAULT_FAR: f32 = 1000.0;
const MIN_RENDER_SCALE: f32 = 0.5;
const RENDER_SCALE_STEP: f32 = 0.05;
const DEFAULT_TARGET_FPS: f64 = 60.0;
const MIN_CALIBRATION_FPS: i32 = 48;
const MAX_CALIBRATION_FPS: i32 = 120;
const SCALE_DOWN_FPS_RATIO: f64 = 0.90;
const SCALE_UP_FPS_RATIO: f64 = 0.97;
const SCALE_DOWN_HOLD_SECONDS: f64 = 0.75;
const SCALE_UP_HOLD_SECONDS: f64 = 4.0;
const SCALE_CHANGE_COOLDOWN_SECONDS: f64 = 1.0;
/// Half the interpupillary distance in metres. Each eye camera is offset by plus or minus this
/// along head-local X.
pub(crate) const HALF_IPD: f32 = 0.0315;

/// Resolve the per-eye 3D render scale from `xreal/render_scale`. Supported backends render the
/// SubViewport at the reduced size and upscale directly into the XREAL eye texture; the fallback
/// keeps a full-size output and uses Godot's bilinear 3D scaling.
///
/// There is no longer an Android-property override, so changing the scale means editing the project
/// setting and re-exporting. On the XR multiview path this value is also read once, at rig creation,
/// and [`DynamicScaleController`] does not run there.
pub(crate) fn eye_render_scale() -> f32 {
    let ps = ProjectSettings::singleton();
    if ps.has_setting("xreal/render_scale") {
        ps.get_setting_with_override("xreal/render_scale")
            .try_to::<f64>()
            .map(|scale| (scale as f32).clamp(0.5, 1.0))
            .unwrap_or(1.0)
    } else {
        1.0
    }
}

/// Carry the project's 3D image-quality settings onto a SubViewport we created in code.
///
/// Godot applies `rendering/anti_aliasing/quality/*` to the root viewport only; a `SubViewport`
/// built at runtime starts from the class defaults, which have both debanding and MSAA off. On a
/// headset that renders through the root viewport (`use_xr`) the project settings simply apply, so
/// the same scene comes out smoother there than it does here — the giveaway is banding in wide, dim
/// gradients such as a night sky, where debanding is the dither that hides the 8-bit steps the
/// tonemap pass leaves behind. Reading the settings rather than hardcoding keeps the eye render
/// looking like the project asked for.
fn apply_project_viewport_quality(viewport: &mut Gd<SubViewport>) {
    let ps = ProjectSettings::singleton();
    let debanding = ps
        .get_setting_with_override("rendering/anti_aliasing/quality/use_debanding")
        .try_to::<bool>()
        .unwrap_or(false);
    viewport.set_use_debanding(debanding);

    let msaa = ps
        .get_setting_with_override("rendering/anti_aliasing/quality/msaa_3d")
        .try_to::<i64>()
        .unwrap_or(0);
    let msaa = match msaa {
        1 => Msaa::MSAA_2X,
        2 => Msaa::MSAA_4X,
        3 => Msaa::MSAA_8X,
        _ => Msaa::DISABLED,
    };
    viewport.set_msaa_3d(msaa);
}

/// Whether this project opts into the Vulkan-only Godot XR multiview proof of concept.
fn xr_multiview_poc_enabled() -> bool {
    let ps = ProjectSettings::singleton();
    ps.has_setting("xreal/xr_multiview_poc")
        && ps
            .get_setting_with_override("xreal/xr_multiview_poc")
            .try_to::<bool>()
            .unwrap_or(false)
}

/// Whether adaptive eye resolution is enabled for this project. The setting is sampled when the
/// stereo rig is created; changing it at runtime does not reconfigure an active session.
fn dynamic_render_scale_enabled() -> bool {
    let ps = ProjectSettings::singleton();
    ps.has_setting("xreal/dynamic_render_scale")
        && ps
            .get_setting_with_override("xreal/dynamic_render_scale")
            .try_to::<bool>()
            .unwrap_or(false)
}

#[derive(Debug)]
struct DynamicScaleController {
    current_scale: f32,
    max_scale: f32,
    target_fps: f64,
    target_calibrated: bool,
    average_frame_seconds: Option<f64>,
    slow_seconds: f64,
    fast_seconds: f64,
    cooldown_seconds: f64,
}

impl DynamicScaleController {
    fn new(max_scale: f32) -> Self {
        let max_scale = max_scale.clamp(MIN_RENDER_SCALE, 1.0);
        Self {
            current_scale: max_scale,
            max_scale,
            target_fps: DEFAULT_TARGET_FPS,
            target_calibrated: false,
            average_frame_seconds: None,
            slow_seconds: 0.0,
            fast_seconds: 0.0,
            cooldown_seconds: 0.0,
        }
    }

    fn observe(&mut self, delta: f64, present_fps: Option<i32>) -> Option<f32> {
        if let Some(fps) = present_fps.filter(|fps| {
            !self.target_calibrated && (MIN_CALIBRATION_FPS..=MAX_CALIBRATION_FPS).contains(fps)
        }) {
            self.target_fps = f64::from(fps);
            self.target_calibrated = true;
        }

        if !(1.0 / 240.0..=0.5).contains(&delta) {
            return None;
        }
        let alpha = 1.0 - (-delta / 0.5).exp();
        let average = self
            .average_frame_seconds
            .map(|old| old + (delta - old) * alpha)
            .unwrap_or(delta);
        self.average_frame_seconds = Some(average);

        self.cooldown_seconds = (self.cooldown_seconds - delta).max(0.0);
        if self.cooldown_seconds > 0.0 {
            self.slow_seconds = 0.0;
            self.fast_seconds = 0.0;
            return None;
        }

        let fps = 1.0 / average.max(1.0 / 1000.0);
        if fps < self.target_fps * SCALE_DOWN_FPS_RATIO {
            self.slow_seconds += delta;
            self.fast_seconds = 0.0;
        } else if fps >= self.target_fps * SCALE_UP_FPS_RATIO {
            self.fast_seconds += delta;
            self.slow_seconds = 0.0;
        } else {
            self.slow_seconds = 0.0;
            self.fast_seconds = 0.0;
        }

        let next = if self.slow_seconds >= SCALE_DOWN_HOLD_SECONDS {
            (self.current_scale - RENDER_SCALE_STEP).max(MIN_RENDER_SCALE)
        } else if self.fast_seconds >= SCALE_UP_HOLD_SECONDS {
            (self.current_scale + RENDER_SCALE_STEP).min(self.max_scale)
        } else {
            return None;
        };
        self.slow_seconds = 0.0;
        self.fast_seconds = 0.0;
        if (next - self.current_scale).abs() < 0.001 {
            return None;
        }
        self.current_scale = (next * 20.0).round() / 20.0;
        self.cooldown_seconds = SCALE_CHANGE_COOLDOWN_SECONDS;
        Some(self.current_scale)
    }
}

/// Two offscreen SubViewports, left and right, each with a Camera3D, rendering the main world from
/// per-eye viewpoints. Their textures are blitted into the XREAL eye swapchain buffers.
struct StereoRig {
    viewports: [Gd<SubViewport>; 2],
    cameras: [Gd<Camera3D>; 2],
    source_width: i32,
    source_height: i32,
    render_scale: f32,
    scale_blit_supported: bool,
    direct_scale_blit: bool,
}

/// One offscreen XR viewport that renders the shared world into a two-layer multiview target.
struct XrMultiviewRig {
    viewport: Gd<SubViewport>,
    camera: Gd<Camera3D>,
    render_scale: f32,
    /// Whether the bridge's linear blit upscales the reduced target directly. `false` renders
    /// full-size with Godot's bilinear 3D scaling doing the reduction, like the multipass rig.
    direct_scale_blit: bool,
    /// One-shot latch for the post-bridge-init upgrade to the reduced target (see
    /// maybe_upgrade_multiview_scale); also set by the sRGB fallback so a revert sticks.
    scale_upgrade_done: bool,
}

/// Scene node that drives its own transform from the native XREAL head pose each frame. Parent a
/// `Camera3D` under it for a head-tracked view through the glasses.
///
/// It runs 6DoF by default, world-locking both rotation and position, and the tracking mode is
/// selectable. It also emits the glasses hot-plug and hardware-input signals: `key_event`,
/// `key_state_changed`, `wearing_changed`, `brightness_changed` and the rest. `is_tracking()`
/// reports whether a native pose was applied on the last frame, and `recenter()` resets the
/// forward direction. The current app `Camera3D` supplies its transform, clipping distances, FOV
/// fallback, cull mask, environment, attributes and other render settings to both eye cameras at
/// runtime; the SDK's calibrated per-eye projection and IPD still take precedence.
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
    /// Whether the app Camera3D values inherited by the eye cameras were logged once.
    camera_parameters_logged: bool,
    /// Whether the `display_started` signal has been emitted (once, on first tracking).
    display_signaled: bool,
    /// Last-seen glasses hot-plug event counts, from the JNI DisplayManager callbacks. A change
    /// re-emits `glasses_connected` or `glasses_disconnected` on the Godot main thread.
    last_connect_count: u32,
    last_disconnect_count: u32,
    /// App-side recenter reference. The SDK's `RecenterGlasses`, in libXREALXRPlugin.so's display
    /// subsystem, does NOT reset the pose we read through `XREALGetHeadPoseAtTime`, which belongs to
    /// the session-manager subsystem. That is device-confirmed: the pose quaternion is unchanged after
    /// calling it. Recentering therefore happens here: `recenter()` captures the current raw rotation
    /// and `process()` applies `reference.inverse() * raw`, making wherever you look at recenter the
    /// forward direction.
    recenter_reference: Quaternion,
    /// The raw (uncorrected) rotation from the last pose sample; captured by `recenter()`.
    last_raw_rotation: Quaternion,
    /// Keep the glasses display awake when not worn, bypassing the proximity sensor's auto-off. It is
    /// read once in `ready()` from the ProjectSetting `xreal/display_bypass_psensor`, default `true`.
    bypass_psensor: bool,
    /// Whether the root viewport should stop drawing 3D after the stereo eye viewports start.
    disable_host_viewport_3d: bool,
    /// Root viewport state before this tracker changed it, restored when the tracker leaves the tree.
    host_viewport_3d_was_disabled: Option<bool>,
    /// Runtime controller created with the stereo rig when dynamic scaling is enabled.
    dynamic_scale: Option<DynamicScaleController>,
    /// Opt-in Godot-native two-view XR interface. Its renderer produces one layered source image.
    xr_interface: Option<Gd<crate::xr_interface::XrealXrInterface>>,
    /// Offscreen XR viewport. The root viewport remains available for the phone's 2D controls.
    xr_multiview_rig: Option<XrMultiviewRig>,
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
            camera_parameters_logged: false,
            display_signaled: false,
            last_connect_count: 0,
            last_disconnect_count: 0,
            recenter_reference: Quaternion::default(),
            last_raw_rotation: Quaternion::default(),
            bypass_psensor: true,
            disable_host_viewport_3d: true,
            host_viewport_3d_was_disabled: None,
            dynamic_scale: None,
            xr_interface: None,
            xr_multiview_rig: None,
        }
    }

    fn ready(&mut self) {
        // Boot-time addon setting: keep the display awake while not worn, on by default. It falls back to
        // `true` when the project never persisted the setting, whether it is fresh or left at the
        // default.
        let ps = ProjectSettings::singleton();
        self.bypass_psensor = if ps.has_setting("xreal/display_bypass_psensor") {
            ps.get_setting_with_override("xreal/display_bypass_psensor")
                .try_to::<bool>()
                .unwrap_or(true)
        } else {
            true
        };
        self.disable_host_viewport_3d = if ps.has_setting("xreal/disable_host_viewport_3d") {
            ps.get_setting_with_override("xreal/disable_host_viewport_3d")
                .try_to::<bool>()
                .unwrap_or(true)
        } else {
            true
        };
        // Kick off initialization early. `shared()` logs its own outcome, and retries on later frames
        // when the Android Activity has not been published yet.
        let _ = session::shared();
        self.ensure_xr_interface();
        self.try_enable_xr_multiview();
    }

    fn exit_tree(&mut self) {
        if let Some(mut rig) = self.xr_multiview_rig.take() {
            rig.viewport.set_use_xr(false);
            rig.viewport.queue_free();
        }
        if let Some(interface) = self.xr_interface.take() {
            crate::xr_interface::deactivate(interface);
        }
        if let Some(was_disabled) = self.host_viewport_3d_was_disabled.take() {
            if let Some(mut viewport) = self.base().get_viewport() {
                viewport.set_disable_3d(was_disabled);
            }
        }
    }

    fn process(&mut self, delta: f64) {
        self.frames = self.frames.wrapping_add(1);
        // Re-emit glasses hot-plug events before the session check, so connect and disconnect are
        // reported even while no session exists yet, for instance when the app started without the
        // glasses.
        self.poll_glasses_events();
        // Drain the glasses hardware events (keys, wear sensor, brightness and the rest) that the native
        // callback queued on the SDK thread, and re-emit them as signals.
        self.poll_hardware_events();
        // Stage-0 AHB bridge probe, fallback trigger: when no session ever goes live (glasses
        // absent), the primary run_frame_tick trigger never fires, so schedule the one-shot probe
        // on the render thread from here. Double-runs are impossible (run_once is latched).
        // GL renderer only: the probe is a GL-build probe by design (under Vulkan, Godot owns no
        // EGL context, so it could only report "no context"; the stage-2 private-context bridge is
        // where a Vulkan-side equivalent would live).
        let Some(session) = session::shared() else {
            self.tracking = false;
            return;
        };

        // Keep the glasses display awake by bypassing the proximity (wear) sensor auto-off, unless
        // `xreal/display_bypass_psensor` disabled it. The SDK no-ops this until `NativeGlasses` is ready
        // and its return value is ambiguous, so call it every frame for the first 10 s or so after the
        // session appears.
        if self.bypass_psensor && self.frames < 600 {
            let status = session.set_display_bypass_psensor(true);
            if self.frames < 3 || self.frames == 120 || self.frames == 300 {
                godot_print!(
                    "[xreal] set_display_bypass_psensor(true) -> {status:?} (frame {})",
                    self.frames
                );
            }
        }

        // The glasses display path: under the GL (Compatibility) renderer it feeds the SDK
        // compositor client GL texture names out of Godot's own EGL context; under Vulkan the
        // stage-2 bridge does the same through per-slot AHardwareBuffers and a private EGL
        // context (vk_bridge.rs). A bridge failure latches BROKEN, and the glasses submission is
        // then skipped while everything below — head tracking, signals, the phone display — keeps
        // working.
        let xr_multiview = self.xr_multiview_rig.is_some();
        if gl::renderer_is_gl() {
            // Build the per-eye offscreen render rig once we are in the tree and so have a World3D.
            self.ensure_stereo();

            // Drive the XREAL swapchain on the rendering thread, which the EGL context requires. The first
            // call invokes GfxThreadStart, running CreateSwapchainEx, then the GL textures, then
            // SetSwapChainBuffers. Later calls drive PopulateNextFrameDesc so the SDK's GLThread has a frame
            // handle.
            let callable = Callable::from_fn("xreal_render_tick", |_| {
                crate::unity_plugin::run_render_thread_tick();
                Variant::nil()
            });
            RenderingServer::singleton().call_on_render_thread(&callable);
        } else {
            // Vulkan: the tick runs when the glasses kill switch is on (eye rendering) OR the HW
            // encoder has work (stage-4 encoder-only mode, glasses off - streaming/recording the
            // AR view without the eye submission). Either way it needs the bridge machinery up.
            let glasses = crate::vk_bridge::glasses_enabled();
            let want_encoder = crate::video_encoder::is_active();
            if (glasses || want_encoder) && crate::vk_bridge::ensure_init() {
                if glasses && !xr_multiview {
                    self.ensure_stereo();
                }
                if xr_multiview {
                    // The bridge is initialized here, so the scale-blit capability is known.
                    self.maybe_upgrade_multiview_scale();
                }
                // The Vulkan tick MUST run after Godot submitted this frame's rendering: the
                // bridge orders its copies against the SubViewport rendering purely by same-queue
                // submission order. The frame-drawn callback is exactly that point; it is
                // one-shot, so re-request it every frame.
                let callable = Callable::from_fn("xreal_vk_tick", |_| {
                    crate::unity_plugin::run_vulkan_render_thread_tick();
                    Variant::nil()
                });
                RenderingServer::singleton().request_frame_drawn_callback(&callable);
            }
        }
        // Primary path: drive the eye cameras from the **display** InputManager pose, the exact pose the
        // compositor reprojects the glasses layer against. It carries the full orientation, ROLL
        // included, which the compact session-manager NrPose lacks; see
        // docs/develop/archive/roll-tracking-investigation.md. Sharing one pose between our render and the
        // compositor is what makes the peek window correct on every axis.
        // CRASH RULE, device-confirmed: never query BOTH head_pose_display() and head_pose() in the same
        // frame, or the app takes a SIGSEGV at 0x3f800000. The session-manager read is therefore a
        // fallback used ONLY when the display export is entirely absent (None), never merely when a
        // frame's block is unusable.
        match session.head_pose_display() {
            Some(raw) => {
                if let Some(rotation) = Self::display_rotation(&raw) {
                    self.tracking = true;
                    if !self.display_signaled {
                        self.display_signaled = true;
                        self.signals().display_started().emit();
                    }
                    self.last_raw_rotation = rotation;
                    // Bake the compositor's pose straight into the node. There is no app-side recenter here: the
                    // peek window needs the baked camera rotation to equal the compositor's reprojection pose, so
                    // recenter is delegated to the SDK, where session.recenter calls
                    // NativePerception::Recenter and shifts this pose source and the layer together.
                    self.base_mut().set_quaternion(rotation);
                    // 6DoF position: the 4x4 pose's translation row, raw[12..15] as x, y and z, in
                    // metres, 1:1 with Godot and with no axis flipped.
                    //
                    // The Y used to be negated here, described as "the same NRSDK-to-Godot Y-flip as
                    // the rotation". The rotation performs no such flip: (x,y,z,w) -> (-x,-y,z,w)
                    // mirrors Z, which is the left-handed-to-right-handed change, and leaves Y
                    // alone. Negating the position's Y made the head sink as it rose: lifting the
                    // glasses 0.78 m off the desk logged pos.y = -0.776, so putting them on dropped
                    // the viewpoint under the floor.
                    //
                    // The position only world-locks because the per-frame updateType-0 UpdateHMDState
                    // call keeps the SDK's dynamic pose cache at InputManager+0x60 live; without it
                    // the compositor cancels the translation (device-verified 2026-07-18, see
                    // docs/develop/archive/codex-6dof-crash-analysis.md).
                    self.base_mut()
                        .set_position(Vector3::new(raw[12], raw[13], raw[14]));
                    let euler = rotation.get_euler() * (180.0 / std::f32::consts::PI);
                    // Calibration log: the extracted Godot euler plus the raw 4x4 rows. Move the head in a known way,
                    // where a nod is pitch on X, a turn is yaw on Y and a tilt is roll on Z, then check each axis and
                    // its sign. If one reads inverted, flip it in display_rotation().
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
                    // The display export is present but this frame's block is not a valid rotation, for instance a
                    // startup transient. Hold the previous transform, and do NOT fall through to the session-manager
                    // pose, which would query both pipelines this frame and trigger the crash.
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
                    // Session-manager fallback, used when the display export is absent. It recenters app-side, since
                    // the SDK's RecenterGlasses does not affect this pose source.
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
                    // Throttled to roughly every 2 s at 60 fps, so we can diagnose why no pose arrives.
                    if self.frames % 120 == 1 {
                        godot_warn!("[xreal] no head pose: {}", session.diagnostics());
                    }
                }
            },
        }

        // Publish the application camera to the primary XREAL interface every frame. XRCamera3D
        // consumes this transform through Godot's normal XR path; the legacy node transform above
        // remains only for backwards compatibility.
        self.publish_standard_xr_view_state();

        // Point the eye cameras from the now-updated head transform, then publish their offscreen
        // textures for the frame tick to blit into the XREAL eye buffers.
        if xr_multiview {
            self.update_xr_view_state();
        } else {
            self.update_stereo();
            self.update_dynamic_render_scale(delta);
        }
    }
}

impl XrealHeadTracker {
    /// Register the XREAL interface even when the renderer uses the established GLES multipass
    /// compositor path. Being primary is what lets a regular XRCamera3D consume the XREAL head
    /// transform; it does not require the root viewport itself to render through the interface.
    fn ensure_xr_interface(&mut self) {
        if self.xr_interface.is_none() {
            self.xr_interface =
                crate::xr_interface::activate(Vector2::new(EYE_W as f32, EYE_H as f32));
        }
    }

    fn try_enable_xr_multiview(&mut self) {
        if !xr_multiview_poc_enabled() {
            return;
        }
        if gl::renderer_is_gl() {
            godot_warn!(
                "[xreal] xreal/xr_multiview_poc requires Vulkan; using the existing GLES Multipass path"
            );
            return;
        }
        let Some(mut host_viewport) = self.base().get_viewport() else {
            godot_warn!("[xreal] XR multiview PoC could not find the root viewport");
            return;
        };
        let Some(world) = self.base().get_world_3d() else {
            godot_warn!("[xreal] XR multiview PoC could not find the shared World3D");
            return;
        };
        // Same scale policy as ensure_stereo: render the target reduced and let the bridge's
        // linear blit upscale it when supported, otherwise render full-size and let Godot's
        // bilinear 3D scaling do the reduction.
        let render_scale = eye_render_scale();
        let scale_blit_supported = crate::vk_bridge::linear_scale_blit_supported();
        let direct_scale_blit = render_scale < 0.999 && scale_blit_supported;
        let target_size = if direct_scale_blit {
            Vector2i::new(
                (EYE_W as f32 * render_scale).round() as i32,
                (EYE_H as f32 * render_scale).round() as i32,
            )
        } else {
            Vector2i::new(EYE_W, EYE_H)
        };
        let Some(interface) = self.xr_interface.as_mut() else {
            godot_warn!("[xreal] XR multiview PoC interface initialization failed");
            return;
        };
        interface
            .bind_mut()
            .set_render_target_size(Vector2::new(target_size.x as f32, target_size.y as f32));

        let mut xr_viewport = SubViewport::new_alloc();
        xr_viewport.set_name("XrealMultiviewViewport");
        xr_viewport.set_world_3d(&world);
        xr_viewport.set_size(target_size);
        if render_scale < 0.999 && !direct_scale_blit {
            xr_viewport.set_scaling_3d_mode(Scaling3DMode::BILINEAR);
            xr_viewport.set_scaling_3d_scale(render_scale);
        }
        xr_viewport.set_update_mode(UpdateMode::ALWAYS);
        xr_viewport.set_use_xr(true);
        apply_project_viewport_quality(&mut xr_viewport);

        let mut xr_camera = Camera3D::new_alloc();
        xr_camera.set_name("XrealMultiviewCamera");
        xr_camera.set_near(DEFAULT_NEAR);
        xr_camera.set_far(DEFAULT_FAR);
        xr_camera.set_current(true);
        xr_viewport.add_child(&xr_camera);
        self.base_mut().add_child(&xr_viewport);

        self.host_viewport_3d_was_disabled = Some(host_viewport.is_3d_disabled());
        if self.disable_host_viewport_3d {
            host_viewport.set_disable_3d(true);
        }
        self.xr_multiview_rig = Some(XrMultiviewRig {
            viewport: xr_viewport,
            camera: xr_camera,
            render_scale,
            direct_scale_blit,
            scale_upgrade_done: false,
        });
        if dynamic_render_scale_enabled() {
            godot_warn!(
                "[xreal] dynamic render scale is not active in the XR multiview PoC; xreal/render_scale is sampled at initialization"
            );
        }
        godot_print!(
            "[xreal] XR multiview PoC active: one offscreen XR SubViewport, root viewport \
             reserved for phone UI (3D scale={render_scale:.2}, scale_path={})",
            if direct_scale_blit {
                "bridge-linear"
            } else if render_scale < 0.999 {
                "godot-bilinear"
            } else {
                "native"
            },
        );
    }

    /// One-shot upgrade of the multiview scale path, run once the Vulkan bridge is initialized.
    /// The PoC activates in ready(), before the bridge exists, so it cannot know then whether the
    /// linear scale blit is supported and conservatively starts full-size with Godot bilinear
    /// scaling. Once the bridge is up and the blit is supported, switch to the reduced target and
    /// let the bridge upscale directly, matching the multipass rig's preferred path. Should the
    /// reduced target then turn out sRGB-typed, the latch in update_xr_view_state reverts it, and
    /// scale_upgrade_done keeps the revert from re-upgrading.
    fn maybe_upgrade_multiview_scale(&mut self) {
        let target = {
            let Some(rig) = self.xr_multiview_rig.as_mut() else {
                return;
            };
            if rig.scale_upgrade_done || rig.direct_scale_blit || rig.render_scale >= 0.999 {
                return;
            }
            rig.scale_upgrade_done = true;
            if !crate::vk_bridge::linear_scale_blit_supported() {
                return;
            }
            let target = Vector2i::new(
                (EYE_W as f32 * rig.render_scale).round() as i32,
                (EYE_H as f32 * rig.render_scale).round() as i32,
            );
            rig.viewport.set_scaling_3d_scale(1.0);
            rig.viewport.set_size(target);
            rig.direct_scale_blit = true;
            target
        };
        if let Some(interface) = self.xr_interface.as_mut() {
            interface
                .bind_mut()
                .set_render_target_size(Vector2::new(target.x as f32, target.y as f32));
        }
        godot_print!(
            "[xreal] multiview scale path upgraded to bridge-linear ({}x{})",
            target.x,
            target.y,
        );
    }

    fn update_xr_view_state(&mut self) {
        // The render thread latched "the reduced target is sRGB-typed": re-target the interface
        // at the full eye size and let Godot's bilinear scaling do the reduction, the multiview
        // equivalent of update_stereo's sRGB guard. One-shot: at full size the latch stays clear.
        if crate::xr_interface::take_srgb_scale_blocked() {
            godot_warn!(
                "[xreal] multiview scale blit source is sRGB-typed; falling back to Godot \
                 bilinear upscale to preserve raw display color"
            );
            if let Some(interface) = self.xr_interface.as_mut() {
                interface
                    .bind_mut()
                    .set_render_target_size(Vector2::new(EYE_W as f32, EYE_H as f32));
            }
            if let Some(rig) = self.xr_multiview_rig.as_mut() {
                rig.viewport.set_size(Vector2i::new(EYE_W, EYE_H));
                if rig.render_scale < 0.999 {
                    rig.viewport.set_scaling_3d_mode(Scaling3DMode::BILINEAR);
                    rig.viewport.set_scaling_3d_scale(rig.render_scale);
                }
                rig.direct_scale_blit = false;
                rig.scale_upgrade_done = true;
            }
        }
        let head = self.base().get_transform();
        let source_camera = self
            .base()
            .get_viewport()
            .and_then(|viewport| viewport.get_camera_3d());
        // XRInterface camera transforms are tracking-space poses. XROrigin3D applies its own
        // world transform to XRCamera3D, so publishing the camera's global transform here would
        // feed the previous XR pose back into itself and double the origin transform.
        let transform = head;
        let fov = source_camera
            .as_ref()
            .map(|camera| camera.get_fov())
            .unwrap_or(FALLBACK_EYE_VERTICAL_FOV);
        // Where the application put its rig. This node is parented under XROrigin3D, so the
        // parent's world transform is that origin, which is exactly what a standard XR scene
        // composes the tracking-space pose against.
        let origin_transform = self
            .base()
            .get_parent()
            .and_then(|parent| parent.try_cast::<Node3D>().ok())
            .map(|node| node.get_global_transform())
            .unwrap_or_default();
        if let (Some(source), Some(rig)) = (source_camera.as_ref(), self.xr_multiview_rig.as_mut())
        {
            sync_eye_camera_parameters(source, &mut rig.camera);
            // Godot passes the current camera's world transform to get_transform_for_view as its
            // `cam_transform`, and that camera is this rig's, sitting inside a SubViewport where it
            // inherits nothing. Placing it at the origin's world transform is what carries the
            // application's rig into the view; leaving it at identity rendered every scene from the
            // tracking origin regardless of where XROrigin3D had been moved.
            if rig.camera.get_global_transform() != origin_transform {
                rig.camera.set_global_transform(origin_transform);
            }
            // The XR camera's clipping planes are what Godot hands to get_projection_for_view as
            // z_near/z_far, so follow the app camera the way the multipass eye cameras do.
            let near = source.get_near();
            if rig.camera.get_near() != near {
                rig.camera.set_near(near);
            }
            let far = source.get_far();
            if rig.camera.get_far() != far {
                rig.camera.set_far(far);
            }
        }
        crate::xr_interface::publish_view_state(transform, fov);
    }

    /// Publish the tracking-space head pose for XRCamera3D and the legacy multipass renderer.
    /// XROrigin3D applies its world transform after this value is consumed.
    fn publish_standard_xr_view_state(&self) {
        let head = self.base().get_transform();
        let source_camera = self
            .base()
            .get_viewport()
            .and_then(|viewport| viewport.get_camera_3d());
        let transform = head;
        let fov = source_camera
            .as_ref()
            .map(|camera| camera.get_fov())
            .unwrap_or(FALLBACK_EYE_VERTICAL_FOV);
        crate::xr_interface::publish_view_state(transform, fov);
    }

    /// Interpret libXREALXRPlugin.so's 16-float display head-pose block as a Godot rotation.
    ///
    /// DEVICE-CONFIRMED layout, from an on-device raw log: the 16 floats are a **4x4 row-major
    /// transform**. The upper-left 3x3 is the head rotation, each row a unit vector; the last row,
    /// 12, 13 and 14, is the tiny position; the last column, 3, 7 and 11, is 0; and raw[15] is 1. It
    /// copies `NativePerception::GetHeadPose`'s struct return verbatim. We validate that structure,
    /// extract the quaternion from the 3x3 with Shepperd's method, then apply the same NRSDK-to-Godot
    /// handedness flip as `NrPose::to_godot_quaternion`. It returns `None` when the block is not a
    /// valid rotation transform, for instance before the session is live, so the caller can hold the
    /// previous pose.
    fn display_rotation(raw: &[f32; 16]) -> Option<Quaternion> {
        // Row-major 3×3 rotation.
        let (m00, m01, m02) = (raw[0], raw[1], raw[2]);
        let (m10, m11, m12) = (raw[4], raw[5], raw[6]);
        let (m20, m21, m22) = (raw[8], raw[9], raw[10]);
        // Validate the homogeneous 4x4 structure so we never extract from a zero or garbage block.
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
        // The standard rotation-matrix to quaternion conversion, Shepperd's, in the source frame, which
        // is NRSDK and Unity left-handed.
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
        // NRSDK to Godot handedness, device-calibrated with a wearer against the DISP calibration
        // log: nod tracks pitch/X, turn tracks yaw/Y and tilt tracks roll/Z, all world-locked in
        // the right direction.
        //
        // Every submission path mirrors the eye image vertically - the Vulkan bridge in its blit,
        // GL in `blit_texture` and `blit_default_framebuffer` - because the compositor reads the eye
        // texture with the opposite vertical origin from the one Godot renders into. So the pose is
        // always the mirrored pairing, `(-x,-y,-z,w)`.
        //
        // The image and the pose are one setting. The compositor reprojects each submitted frame
        // onto the latest head pose, so mirroring the image also mirrors the direction that
        // reprojection pulls, on exactly the axes a vertical mirror reverses: pitch and roll,
        // leaving yaw alone. Mirror the image without the pose and the view swings about twice as
        // far as the head on those two axes and in the wrong direction; do both and they cancel.
        //
        // This read `(x,-y,z,w)` on the GL path between ddf2823 and now, on the reasoning that GL
        // submits its image as rendered. It does not: device-confirmed 2026-08-13 on
        // Compatibility/Multipass, a straight copy arrives upside-down, and no rotation undoes a
        // mirror.
        Some(Quaternion::new(-x, -y, -z, w).normalized())
    }

    /// Poll the JNI glasses hot-plug counters and re-emit any new events as signals. It is called on
    /// the Godot main thread, where signal emission is safe, since the JNI callbacks run on the UI
    /// thread.
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
            // Raw event log, the Phase A device-verification instrument. It prints one line per physical key
            // press, wear change or brightness step, so the volume stays low.
            godot_print!(
                "[xreal] glasses event: type={} para={} para2={} para3={}",
                ev.action_type,
                ev.para,
                ev.para2,
                ev.para3
            );
            match ev.action_type {
                f::ACTION_TYPE_CLICK | f::ACTION_TYPE_DOUBLE_CLICK | f::ACTION_TYPE_LONG_PRESS => {
                    // para is an XREALKeyType and action_type an XREALClickType, numbered the same as the
                    // ACTION_CLICK, ACTION_DOUBLE_CLICK and ACTION_LONG_PRESS constants.
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
                    // Mirror the Unity handler, which forwards PUT_ON and TAKE_OFF alone.
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

    /// Create the two per-eye SubViewports and their cameras once, sharing the main World3D so they
    /// render the same scene. It does nothing until the node is in the tree and has a World3D.
    fn ensure_stereo(&mut self) {
        if self.stereo.is_some() {
            return;
        }
        let Some(world) = self.base().get_world_3d() else {
            return;
        };
        let render_scale = eye_render_scale();
        let scale_blit_supported =
            gl::renderer_is_gl() || crate::vk_bridge::linear_scale_blit_supported();
        let direct_scale_blit = render_scale < 0.999 && scale_blit_supported;
        let source_width = if direct_scale_blit {
            (EYE_W as f32 * render_scale).round() as i32
        } else {
            EYE_W
        };
        let source_height = if direct_scale_blit {
            (EYE_H as f32 * render_scale).round() as i32
        } else {
            EYE_H
        };
        let make_eye = || {
            let mut sv = SubViewport::new_alloc();
            sv.set_size(Vector2i::new(source_width, source_height));
            if render_scale < 0.999 && !direct_scale_blit {
                sv.set_scaling_3d_mode(Scaling3DMode::BILINEAR);
                sv.set_scaling_3d_scale(render_scale);
            }
            sv.set_update_mode(UpdateMode::ALWAYS);
            sv.set_world_3d(&world);
            apply_project_viewport_quality(&mut sv);
            let mut cam = Camera3D::new_alloc();
            cam.set_fov(FALLBACK_EYE_VERTICAL_FOV);
            cam.set_near(DEFAULT_NEAR);
            cam.set_far(DEFAULT_FAR);
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
            source_width,
            source_height,
            render_scale,
            scale_blit_supported,
            direct_scale_blit,
        });
        self.dynamic_scale =
            dynamic_render_scale_enabled().then(|| DynamicScaleController::new(render_scale));
        // The two eye viewports already draw the shared World3D. Most XREAL apps use the host
        // display for 2D controls, so drawing that world on the root viewport adds a hidden third
        // scene pass. Preserve an opt-out for apps that intentionally show a 3D phone mirror.
        if self.disable_host_viewport_3d && self.host_viewport_3d_was_disabled.is_none() {
            if let Some(mut viewport) = self.base().get_viewport() {
                self.host_viewport_3d_was_disabled = Some(viewport.is_3d_disabled());
                viewport.set_disable_3d(true);
            }
        }
        godot_print!(
            "[xreal] stereo rig created ({EYE_W}x{EYE_H} per eye, 3D scale={render_scale:.2}, \
             internal={}x{}, scale_path={}, dynamic={})",
            (EYE_W as f32 * render_scale).round() as i32,
            (EYE_H as f32 * render_scale).round() as i32,
            if direct_scale_blit {
                "bridge-linear"
            } else if render_scale < 0.999 {
                "godot-bilinear"
            } else {
                "native"
            },
            self.dynamic_scale.is_some(),
        );
    }

    fn update_dynamic_render_scale(&mut self, delta: f64) {
        if self.dynamic_scale.is_none() || self.stereo.is_none() {
            return;
        }
        let present_fps = self
            .frames
            .is_multiple_of(30)
            .then(crate::metrics::present_fps)
            .flatten();
        let Some(next_scale) = self
            .dynamic_scale
            .as_mut()
            .and_then(|controller| controller.observe(delta, present_fps))
        else {
            return;
        };
        let Some(rig) = self.stereo.as_mut() else {
            return;
        };
        configure_stereo_scale(rig, next_scale);
        let target_fps = self
            .dynamic_scale
            .as_ref()
            .map(|controller| controller.target_fps)
            .unwrap_or(DEFAULT_TARGET_FPS);
        godot_print!(
            "[xreal] dynamic render scale -> {:.2} (target {:.0} FPS, internal {}x{}, path={})",
            rig.render_scale,
            target_fps,
            (EYE_W as f32 * rig.render_scale).round() as i32,
            (EYE_H as f32 * rig.render_scale).round() as i32,
            if rig.direct_scale_blit {
                "bridge-linear"
            } else if rig.render_scale < 0.999 {
                "godot-bilinear"
            } else {
                "native"
            },
        );
    }

    /// Aim the eye cameras from the head transform, offset by plus or minus the IPD, and publish their
    /// GL textures.
    fn update_stereo(&mut self) {
        let head = self.base().get_global_transform();
        // The app-facing Camera3D remains the source of scene-specific camera state even when the
        // host viewport's 3D pass is disabled. Eye projection and IPD remain XREAL-owned, while
        // clipping, render layers, post-processing resources and intentional camera offsets belong
        // to the app and must follow runtime camera changes.
        let source_camera = self
            .base()
            .get_viewport()
            .and_then(|viewport| viewport.get_camera_3d());
        // The eye pose comes from the head, never from the app camera. The shared XR scene leaves an
        // XRCamera3D `current` on the root viewport, and that node does not follow the headset on
        // this path, so reading its transform pinned both eyes at the origin and the world rode
        // along with the head. Everything else below still follows the app camera.
        let source_transform = head;
        let source_near = source_camera
            .as_ref()
            .map(|camera| camera.get_near())
            .unwrap_or(DEFAULT_NEAR);
        let source_far = source_camera
            .as_ref()
            .map(|camera| camera.get_far())
            .unwrap_or(DEFAULT_FAR);
        let source_fov = source_camera
            .as_ref()
            .map(|camera| camera.get_fov())
            .unwrap_or(FALLBACK_EYE_VERTICAL_FOV);
        if !self.camera_parameters_logged {
            if let Some(camera) = source_camera.as_ref() {
                godot_print!(
                    "[xreal] eye cameras inherit app camera: near={source_near:.3} \
                     far={source_far:.1} fov={source_fov:.1} cull_mask={:#x} \
                     environment={} attributes={}",
                    camera.get_cull_mask(),
                    camera.get_environment().is_some(),
                    camera.get_attributes().is_some(),
                );
                self.camera_parameters_logged = true;
            }
        }
        let Some(rig) = self.stereo.as_mut() else {
            // Mono fallback: publish the window size so the frame tick blits the default framebuffer.
            if let Some(viewport) = self.base().get_viewport() {
                let size = viewport.get_visible_rect().size;
                crate::unity_plugin::set_godot_source_size(size.x as i32, size.y as i32);
            }
            return;
        };
        // Apply the SDK's exact per-eye projection and eye offset when available, which makes the AR
        // pixel-accurate, and otherwise fall back to the symmetric IPD and the app camera's FOV.
        let proj = crate::unity_plugin::stereo_projection();
        for (i, cam) in rig.cameras.iter_mut().enumerate() {
            if let Some(source) = source_camera.as_ref() {
                sync_eye_camera_parameters(source, cam);
            }
            let p = proj[i];
            let eye_x = if p.valid && p.px != 0.0 {
                p.px
            } else if i == 0 {
                -HALF_IPD
            } else {
                HALF_IPD
            };
            cam.set_global_transform(
                source_transform * Transform3D::new(Basis::IDENTITY, Vector3::new(eye_x, 0.0, 0.0)),
            );

            if p.valid && (p.r - p.l) > 1e-4 && (p.t - p.b) > 1e-4 {
                // Half-angle tangents give an asymmetric frustum. Godot's Camera3D.set_frustum(size, offset,
                // near, far) maps to near-plane extents of plus or minus size/2 vertically and plus or minus
                // size*aspect/2 horizontally, shifted by offset, and a near-plane coordinate equals
                // tangent*near.
                let (size, offset) = frustum_size_offset(p.l, p.r, p.t, p.b, source_near);
                cam.set_frustum(size, offset, source_near, source_far);
            } else {
                cam.set_fov(source_fov);
                cam.set_near(source_near);
                cam.set_far(source_far);
            }
        }
        let rs = RenderingServer::singleton();
        if gl::renderer_is_gl() {
            // Use the actual render-target texture RID, from viewport_get_texture on the viewport RID, and
            // not the ViewportTexture *resource* RID, whose native handle is 0.
            let handle = |sv: &Gd<SubViewport>| -> u32 {
                let tex_rid = rs.viewport_get_texture(sv.get_viewport_rid());
                rs.texture_get_native_handle(tex_rid) as u32
            };
            let left = handle(&rig.viewports[0]);
            let right = handle(&rig.viewports[1]);
            crate::unity_plugin::set_godot_eye_sources(
                left,
                right,
                rig.source_width,
                rig.source_height,
            );
        } else {
            // Vulkan bridge: publish each eye SubViewport's VkImage (and its RD format) for the
            // per-frame copy or direct scale blit. Resolved every frame, because Godot may
            // reallocate the render target, invalidating both the RID chain and driver handle.
            let left = Self::vk_eye_source(&rig.viewports[0]);
            let right = Self::vk_eye_source(&rig.viewports[1]);
            if rig.direct_scale_blit && (left.valid && left.srgb || right.valid && right.srgb) {
                godot_warn!(
                    "[xreal] direct scale blit source is sRGB-typed; falling back to Godot \
                     bilinear upscale to preserve raw display color"
                );
                rig.scale_blit_supported = false;
                let render_scale = rig.render_scale;
                configure_stereo_scale(rig, render_scale);
                crate::vk_bridge::set_eye_sources(
                    crate::vk_bridge::EyeSource::default(),
                    crate::vk_bridge::EyeSource::default(),
                );
                return;
            }
            crate::vk_bridge::set_eye_sources(left, right);
        }
    }

    /// Resolve one eye SubViewport's render-target texture down to its Vulkan identity for the
    /// bridge: viewport RID -> render-target texture RID -> RD texture RID -> `VkImage` +
    /// format. An unresolvable or non-RGBA8-class texture comes back `valid: false`, which the
    /// fill turns into a black clear rather than a bad copy (RGBA8 class is required for the
    /// raw-texel `vkCmdCopyImage`; a 16F/10-bit source would need the fill-v2 sampled pass).
    fn vk_eye_source(sv: &Gd<SubViewport>) -> crate::vk_bridge::EyeSource {
        use godot::classes::rendering_device::{DataFormat, DriverResource};
        let rs = RenderingServer::singleton();
        let zero = crate::vk_bridge::EyeSource::default();
        let Some(mut rd) = rs.get_rendering_device() else {
            return zero;
        };
        let tex_rid = rs.viewport_get_texture(sv.get_viewport_rid());
        let rd_rid = rs.texture_get_rd_texture(tex_rid);
        if !rd_rid.is_valid() {
            return zero;
        }
        let vk_image = rd.get_driver_resource(DriverResource::TEXTURE, rd_rid, 0);
        let Some(fmt) = rd.texture_get_format(rd_rid) else {
            return zero;
        };
        let format = fmt.get_format();
        let srgb = format == DataFormat::R8G8B8A8_SRGB;
        if !srgb && format != DataFormat::R8G8B8A8_UNORM {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                godot_warn!(
                    "[xreal] vk eye source format {format:?} is not RGBA8-class; the raw copy \
                     cannot convert it (fill v2 needed) - eyes stay black"
                );
            }
            return zero;
        }
        crate::vk_bridge::EyeSource {
            vk_image,
            width: fmt.get_width() as i32,
            height: fmt.get_height() as i32,
            array_layer: 0,
            srgb,
            valid: vk_image != 0,
        }
    }
}

#[godot_api]
impl XrealHeadTracker {
    /// Emitted once when the glasses display and head tracking first go live, on the first frame a
    /// head pose arrives. Connect it in GDScript and call `recenter()` to make the current head
    /// direction "forward" at startup.
    #[signal]
    fn display_started();

    /// Emitted when the XREAL glasses display is plugged in at runtime, on `onDisplayAdded`. It fires
    /// even when the app started with the glasses disconnected: the native session bootstrap then
    /// retries `CreateSession`, and `display_started` follows once tracking comes up.
    #[signal]
    fn glasses_connected();

    /// Emitted when the XREAL glasses display is unplugged at runtime (`onDisplayRemoved`).
    #[signal]
    fn glasses_disconnected();

    /// A physical key on the glasses was clicked. `key` is one of the `KEY_*` constants, MULTI,
    /// INCREASE, DECREASE or MENU, and `action` is `ACTION_CLICK`, `ACTION_DOUBLE_CLICK` or
    /// `ACTION_LONG_PRESS`.
    #[signal]
    fn key_event(key: i64, action: i64);

    /// Raw down or up transition of a physical key: `state` is `KEY_STATE_DOWN` or `KEY_STATE_UP`.
    #[signal]
    fn key_state_changed(key: i64, state: i64);

    /// The proximity (wear) sensor reported the glasses were put on (`true`) or taken off.
    #[signal]
    fn wearing_changed(wearing: bool);

    /// The glasses brightness level changed, from the brightness rocker or the system UI.
    #[signal]
    fn brightness_changed(level: i64);

    /// The glasses volume level changed.
    #[signal]
    fn volume_changed(level: i64);

    /// The electrochromic dimming level changed. One Pro only.
    #[signal]
    fn ec_level_changed(level: i64);

    /// Catch-all for every native glasses event, including the types without a dedicated signal, such
    /// as temperature, screen on and off, and the disconnect reason. The values are the raw
    /// `GlassesEventData` fields; see `XREALActionType` in `docs/develop/plans/input-plan.md`.
    #[signal]
    fn glasses_event(action_type: i64, para: i64, para2: i64, para3: f64);

    /// `key` value in `key_event` and `key_state_changed`, an `XREALKeyType`: the MULTI, or function,
    /// key.
    #[constant]
    const KEY_MULTI: i64 = 1;
    /// `key` value: the brightness and volume INCREASE key.
    #[constant]
    const KEY_INCREASE: i64 = 2;
    /// `key` value: the brightness and volume DECREASE key.
    #[constant]
    const KEY_DECREASE: i64 = 3;
    /// `key` value: the MENU key.
    #[constant]
    const KEY_MENU: i64 = 4;

    /// `action` value in `key_event`, an `XREALClickType`: a single click.
    #[constant]
    const ACTION_CLICK: i64 = 1;
    /// `action` value in `key_event`: a double click.
    #[constant]
    const ACTION_DOUBLE_CLICK: i64 = 2;
    /// `action` value in `key_event`: a long press.
    #[constant]
    const ACTION_LONG_PRESS: i64 = 3;

    /// `state` value in `key_state_changed`, an `XREALKeyState`: the key went down.
    #[constant]
    const KEY_STATE_DOWN: i64 = 1;
    /// `state` value in `key_state_changed`: the key was released.
    #[constant]
    const KEY_STATE_UP: i64 = 2;

    /// Whether native head tracking fed a pose on the last frame.
    #[func]
    fn is_tracking(&self) -> bool {
        self.tracking
    }

    /// Re-center the view so the current head direction becomes "forward".
    #[func]
    fn recenter(&mut self) {
        // App-side recenter: the current head direction becomes "forward", meaning identity. The
        // reference is the raw rotation of the latest pose sample, so this also cancels any pitch offset
        // picked up while the glasses sat on a desk during session start.
        self.recenter_reference = self.last_raw_rotation;
        let e = self.last_raw_rotation.get_euler() * (180.0 / std::f32::consts::PI);
        godot_print!(
            "[xreal] recenter: reference euler=({:.1},{:.1},{:.1})",
            e.x,
            e.y,
            e.z
        );
        // Still forward to the SDK's display-side recenter. It is harmless, and it may matter for the
        // compositor path even though it does not reset our pose source.
        if let Some(session) = session::shared() {
            session.recenter();
        }
    }

    /// Latest raw and converted pose sample for visual on-device debugging.
    #[func]
    fn debug_pose_text(&self) -> GString {
        self.debug_pose.clone()
    }

    /// Current per-eye render scale after any dynamic adjustment. Returns the configured ceiling
    /// before the stereo rig is created.
    #[func]
    fn get_current_render_scale(&self) -> f64 {
        self.stereo
            .as_ref()
            .map(|rig| f64::from(rig.render_scale))
            .unwrap_or_else(|| f64::from(eye_render_scale()))
    }
}

fn configure_stereo_scale(rig: &mut StereoRig, render_scale: f32) {
    let render_scale = render_scale.clamp(MIN_RENDER_SCALE, 1.0);
    let direct_scale_blit = render_scale < 0.999 && rig.scale_blit_supported;
    let source_width = if direct_scale_blit {
        (EYE_W as f32 * render_scale).round() as i32
    } else {
        EYE_W
    };
    let source_height = if direct_scale_blit {
        (EYE_H as f32 * render_scale).round() as i32
    } else {
        EYE_H
    };
    for viewport in &mut rig.viewports {
        let size = Vector2i::new(source_width, source_height);
        if viewport.get_size() != size {
            viewport.set_size(size);
        }
        viewport.set_scaling_3d_mode(Scaling3DMode::BILINEAR);
        viewport.set_scaling_3d_scale(if direct_scale_blit { 1.0 } else { render_scale });
    }
    rig.source_width = source_width;
    rig.source_height = source_height;
    rig.render_scale = render_scale;
    rig.direct_scale_blit = direct_scale_blit;
}

/// Copy the app-owned Camera3D state to an offscreen eye camera. Projection shape, current state and
/// eye transform are deliberately excluded: the XREAL frame descriptor owns those values. Each
/// setter is guarded because unchanged Camera3D setters enqueue RenderingServer work.
fn sync_eye_camera_parameters(source: &Gd<Camera3D>, eye: &mut Gd<Camera3D>) {
    let cull_mask = source.get_cull_mask();
    if eye.get_cull_mask() != cull_mask {
        eye.set_cull_mask(cull_mask);
    }

    let environment = source.get_environment();
    if eye.get_environment() != environment {
        eye.set_environment(environment.as_ref());
    }

    let attributes = source.get_attributes();
    if eye.get_attributes() != attributes {
        eye.set_attributes(attributes.as_ref());
    }

    let keep_aspect = source.get_keep_aspect_mode();
    if eye.get_keep_aspect_mode() != keep_aspect {
        eye.set_keep_aspect_mode(keep_aspect);
    }

    let h_offset = source.get_h_offset();
    if eye.get_h_offset() != h_offset {
        eye.set_h_offset(h_offset);
    }

    let v_offset = source.get_v_offset();
    if eye.get_v_offset() != v_offset {
        eye.set_v_offset(v_offset);
    }

    let doppler = source.get_doppler_tracking();
    if eye.get_doppler_tracking() != doppler {
        eye.set_doppler_tracking(doppler);
    }
}

/// Asymmetric projection from the SDK's per-eye half-angle tangents, l, r, t and b, into Godot's
/// `Camera3D::set_frustum(size, offset, near, far)` parameters. `size` is the vertical near-plane
/// extent and `offset` shifts the otherwise centered near-plane rectangle, and a near-plane
/// coordinate equals `tangent * near`. It stays a free function so the calibrated mapping is unit
/// tested, in the tests module, without needing a live Camera3D.
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
        // Symmetric l and r, and t and b, give no offset, and size is the full vertical extent at the
        // near plane.
        let (size, offset) = frustum_size_offset(-0.5, 0.5, 0.4, -0.4, 0.05);
        assert!((size - 0.8 * 0.05).abs() < 1e-6, "size {size}");
        assert!(
            offset.x.abs() < 1e-6 && offset.y.abs() < 1e-6,
            "offset {offset:?}"
        );
    }

    #[test]
    fn asymmetric_tangents_shift_the_frustum() {
        // l=-0.6 and r=0.4 put the horizontal center at (r+l)/2=-0.1; t=0.5 and b=-0.3 put the vertical
        // center at 0.1.
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

    #[test]
    fn dynamic_scale_drops_quickly_and_recovers_slowly() {
        let mut controller = DynamicScaleController::new(1.0);
        let mut changed = None;
        for _ in 0..60 {
            changed = changed.or_else(|| controller.observe(1.0 / 30.0, Some(60)));
        }
        assert_eq!(changed, Some(0.95));

        let mut recovered = None;
        for _ in 0..480 {
            recovered = recovered.or_else(|| controller.observe(1.0 / 60.0, Some(60)));
        }
        assert_eq!(recovered, Some(1.0));
    }

    #[test]
    fn dynamic_scale_uses_a_valid_compositor_rate_as_its_target() {
        let mut controller = DynamicScaleController::new(0.75);
        controller.observe(1.0 / 52.0, Some(52));
        assert!(controller.target_calibrated);
        assert_eq!(controller.target_fps, 52.0);
        assert_eq!(controller.max_scale, 0.75);
    }

    #[test]
    fn dynamic_scale_never_goes_below_the_internal_floor() {
        let mut controller = DynamicScaleController::new(0.5);
        for _ in 0..300 {
            assert_eq!(controller.observe(1.0 / 15.0, Some(60)), None);
        }
        assert_eq!(controller.current_scale, MIN_RENDER_SCALE);
    }
}
