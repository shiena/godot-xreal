//! Godot-native XR interface for the XREAL backend.
//!
//! It always exposes the native head pose to standard Godot XR nodes. The XREAL compositor remains
//! on the existing Unity-provider path: GLES uses the established two-`SubViewport` renderer,
//! while the opt-in Vulkan proof of concept renders both views through a Godot XR viewport.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use godot::classes::rendering_device::{DataFormat, DriverResource};
use godot::classes::xr_interface::{Capabilities, PlayAreaMode, TrackingStatus, VrsTextureFormat};
use godot::classes::{IXrInterfaceExtension, RenderingServer, XrInterfaceExtension, XrServer};
use godot::obj::EngineEnum;
use godot::prelude::*;

use crate::node::{EYE_H, EYE_W, FALLBACK_EYE_VERTICAL_FOV, HALF_IPD};

#[derive(Clone, Copy)]
struct ViewState {
    camera_transform: Transform3D,
    fallback_fov: f32,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            camera_transform: Transform3D::IDENTITY,
            fallback_fov: FALLBACK_EYE_VERTICAL_FOV,
        }
    }
}

static VIEW_STATE: OnceLock<Mutex<ViewState>> = OnceLock::new();

/// Render-thread latch: the multiview target is sRGB-typed AND smaller than the eye buffers, so
/// the bridge's linear scale blit would sRGB-decode the texels and shift colors. The main thread
/// consumes it and re-targets the interface at the full eye size, so Godot's own bilinear scaling
/// does the reduction, mirroring update_stereo's multipass guard.
static SRGB_SCALE_BLOCKED: AtomicBool = AtomicBool::new(false);

fn view_state() -> &'static Mutex<ViewState> {
    VIEW_STATE.get_or_init(|| Mutex::new(ViewState::default()))
}

/// Consume the render thread's "sRGB source needs the scale-blit fallback" latch.
pub fn take_srgb_scale_blocked() -> bool {
    SRGB_SCALE_BLOCKED.swap(false, Ordering::Relaxed)
}

/// Publish the app camera transform used by Godot's next XR scene render.
pub fn publish_view_state(camera_transform: Transform3D, fallback_fov: f32) {
    *view_state().lock().expect("XR view-state mutex") = ViewState {
        camera_transform,
        fallback_fov,
    };
}

fn projection_array(projection: Projection) -> PackedFloat64Array {
    let mut values = [0.0_f64; 16];
    for (column_index, column) in projection.cols.iter().enumerate() {
        let offset = column_index * 4;
        values[offset] = f64::from(column.x);
        values[offset + 1] = f64::from(column.y);
        values[offset + 2] = f64::from(column.z);
        values[offset + 3] = f64::from(column.w);
    }
    PackedFloat64Array::from(values)
}

/// The XREAL `XRInterfaceExtension` used by standard Godot XR scene nodes.
///
/// The addon always registers and activates it internally for pose delivery; scenes never
/// instantiate it directly. It claims Godot's primary interface slot, so `XrealXRRuntime` hands
/// that slot back first when a startup OpenXR interface holds it. When the ProjectSetting
/// `xreal/xr_multiview_poc`, or the Android property `debug.xreal.xr_multiview`, enables the
/// Vulkan-only multiview renderer, it also needs `xr/shaders/enabled=true` and an export preset
/// whose XR Mode is `OpenXR`. The established two-SubViewport Multipass path remains the default.
#[derive(GodotClass)]
#[class(base = XrInterfaceExtension, tool)]
pub struct XrealXrInterface {
    base: Base<XrInterfaceExtension>,
    initialized: bool,
    render_size: Vector2,
}

#[godot_api]
impl IXrInterfaceExtension for XrealXrInterface {
    fn init(base: Base<XrInterfaceExtension>) -> Self {
        Self {
            base,
            initialized: false,
            // activate() sets the real target size before initialize(); full-size is the fallback.
            render_size: Vector2::new(EYE_W as f32, EYE_H as f32),
        }
    }

    fn get_name(&self) -> StringName {
        StringName::from("XREAL")
    }

    fn get_capabilities(&self) -> u32 {
        (Capabilities::STEREO.ord() | Capabilities::AR.ord()) as u32
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn initialize(&mut self) -> bool {
        self.initialized = true;
        godot_print!(
            "[xreal] XRInterfaceExtension initialized: {}x{}, views=2, renderer=Godot multiview",
            self.render_size.x,
            self.render_size.y,
        );
        true
    }

    fn uninitialize(&mut self) {
        self.initialized = false;
        SRGB_SCALE_BLOCKED.store(false, Ordering::Relaxed);
        crate::vk_bridge::set_eye_sources(Default::default(), Default::default());
    }

    fn get_system_info(&self) -> AnyDictionary {
        VarDictionary::new().upcast_any_dictionary()
    }

    fn supports_play_area_mode(&self, _mode: PlayAreaMode) -> bool {
        false
    }

    fn get_play_area_mode(&self) -> PlayAreaMode {
        PlayAreaMode::UNKNOWN
    }

    fn set_play_area_mode(&self, _mode: PlayAreaMode) -> bool {
        false
    }

    fn get_play_area(&self) -> PackedVector3Array {
        PackedVector3Array::new()
    }

    fn get_render_target_size(&mut self) -> Vector2 {
        self.render_size
    }

    fn get_view_count(&mut self) -> u32 {
        2
    }

    fn get_camera_transform(&mut self) -> Transform3D {
        view_state()
            .lock()
            .expect("XR view-state mutex")
            .camera_transform
    }

    fn get_transform_for_view(&mut self, view: u32, _cam_transform: Transform3D) -> Transform3D {
        let state = *view_state().lock().expect("XR view-state mutex");
        let projection = crate::unity_plugin::stereo_projection();
        let eye = projection[(view as usize).min(1)];
        let eye_x = if eye.valid && eye.px != 0.0 {
            eye.px
        } else if view == 0 {
            -HALF_IPD
        } else {
            HALF_IPD
        };
        state.camera_transform * Transform3D::new(Basis::IDENTITY, Vector3::new(eye_x, 0.0, 0.0))
    }

    fn get_projection_for_view(
        &mut self,
        view: u32,
        aspect: f64,
        z_near: f64,
        z_far: f64,
    ) -> PackedFloat64Array {
        let eye = crate::unity_plugin::stereo_projection()[(view as usize).min(1)];
        let near = z_near as f32;
        let far = z_far as f32;
        let projection = if eye.valid && (eye.r - eye.l) > 1e-4 && (eye.t - eye.b) > 1e-4 {
            Projection::create_frustum(
                eye.l * near,
                eye.r * near,
                eye.b * near,
                eye.t * near,
                near,
                far,
            )
        } else {
            let fov = view_state()
                .lock()
                .expect("XR view-state mutex")
                .fallback_fov;
            Projection::create_perspective(fov, aspect as f32, near, far, false)
        };
        projection_array(projection)
    }

    fn get_tracking_status(&self) -> TrackingStatus {
        if self.initialized {
            TrackingStatus::NORMAL_TRACKING
        } else {
            TrackingStatus::NOT_TRACKING
        }
    }

    fn process(&mut self) {}

    fn pre_render(&mut self) {}

    fn end_frame(&mut self) {}

    fn get_suggested_tracker_names(&self) -> PackedStringArray {
        PackedStringArray::new()
    }

    fn get_suggested_pose_names(&self, _tracker_name: StringName) -> PackedStringArray {
        PackedStringArray::new()
    }

    fn trigger_haptic_pulse(
        &mut self,
        _action_name: GString,
        _tracker_name: StringName,
        _frequency: f64,
        _amplitude: f64,
        _duration_sec: f64,
        _delay_sec: f64,
    ) {
    }

    fn get_anchor_detection_is_enabled(&self) -> bool {
        false
    }

    fn set_anchor_detection_is_enabled(&mut self, _enabled: bool) {}

    fn get_camera_feed_id(&self) -> i32 {
        -1
    }

    fn get_vrs_texture(&mut self) -> Rid {
        Rid::Invalid
    }

    fn get_vrs_texture_format(&mut self) -> VrsTextureFormat {
        VrsTextureFormat::UNIFIED
    }

    fn get_color_texture(&mut self) -> Rid {
        Rid::Invalid
    }

    fn get_depth_texture(&mut self) -> Rid {
        Rid::Invalid
    }

    fn get_velocity_texture(&mut self) -> Rid {
        Rid::Invalid
    }

    fn pre_draw_viewport(&mut self, _render_target: Rid) -> bool {
        self.initialized
    }

    fn post_draw_viewport(&mut self, render_target: Rid, _screen_rect: Rect2) {
        let rd_texture = self.base().get_render_target_texture(render_target);
        if !rd_texture.is_valid() {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                godot_warn!(
                    "[xreal] multiview XR render target has no RD texture; Vulkan is required for this PoC"
                );
            }
            return;
        }

        let rs = RenderingServer::singleton();
        let Some(mut rd) = rs.get_rendering_device() else {
            return;
        };
        let vk_image = rd.get_driver_resource(DriverResource::TEXTURE, rd_texture, 0);
        let Some(format) = rd.texture_get_format(rd_texture) else {
            return;
        };
        let data_format = format.get_format();
        let srgb = data_format == DataFormat::R8G8B8A8_SRGB;
        let supported = srgb || data_format == DataFormat::R8G8B8A8_UNORM;
        if vk_image == 0 || !supported || format.get_array_layers() < 2 {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                godot_warn!(
                    "[xreal] unusable multiview target: image={:#x} format={:?} layers={} (RGBA8 and 2 layers required)",
                    vk_image,
                    data_format,
                    format.get_array_layers(),
                );
            }
            return;
        }

        // A reduced sRGB-typed target would go through the bridge's linear-filtered blit, which
        // sRGB-decodes on read and shifts colors, so flag the main thread to fall back to a
        // full-size target (raw copies are typed-twin compatible) and publish nothing this frame.
        let scaled = format.get_width() as i32 != EYE_W || format.get_height() as i32 != EYE_H;
        if srgb && scaled {
            SRGB_SCALE_BLOCKED.store(true, Ordering::Relaxed);
            crate::vk_bridge::set_eye_sources(Default::default(), Default::default());
            return;
        }

        let source = |array_layer| crate::vk_bridge::EyeSource {
            vk_image,
            width: format.get_width() as i32,
            height: format.get_height() as i32,
            array_layer,
            srgb,
            valid: true,
        };
        crate::vk_bridge::set_eye_sources(source(0), source(1));
    }
}

impl XrealXrInterface {
    /// Set the size Godot renders the XR viewport at. It is read back every frame through
    /// get_render_target_size(), so a change re-targets the render on the next draw.
    pub fn set_render_target_size(&mut self, size: Vector2) {
        self.render_size = size;
    }
}

/// Register, initialize and select the XREAL interface as Godot's primary XR interface.
/// `render_size` is the target the XR viewport renders at: the reduced size when the bridge
/// upscales directly, the full eye size otherwise.
pub fn activate(render_size: Vector2) -> Option<Gd<XrealXrInterface>> {
    let mut server = XrServer::singleton();
    if let Some(primary) = server.get_primary_interface() {
        godot_warn!(
            "[xreal] XREAL XR interface did not replace existing primary XR interface '{}'",
            primary.get_name(),
        );
        return None;
    }
    let mut interface = XrealXrInterface::new_gd();
    interface.bind_mut().render_size = render_size;
    let mut base_interface = interface.clone().upcast::<godot::classes::XrInterface>();
    server.add_interface(&base_interface);
    if !base_interface.initialize() {
        server.remove_interface(&base_interface);
        return None;
    }
    server.set_primary_interface(&base_interface);
    Some(interface)
}

/// Remove the XREAL interface without disturbing a different primary interface.
pub fn deactivate(interface: Gd<XrealXrInterface>) {
    let mut base_interface = interface.upcast::<godot::classes::XrInterface>();
    let mut server = XrServer::singleton();
    if server
        .get_primary_interface()
        .is_some_and(|primary| primary == base_interface)
    {
        server.set_primary_interface(None::<&Gd<godot::classes::XrInterface>>);
    }
    base_interface.uninitialize();
    server.remove_interface(&base_interface);
}
