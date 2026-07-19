//! godot-xreal — a Godot 4 GDExtension that drives XREAL glasses from Rust.
//!
//! The Unity `com.xreal.xr` SDK is a thin C# layer over native `.so` libraries.
//! Those native libraries export a flat, engine-agnostic C ABI (see
//! `docs/reference/reverse-engineering.md`), so instead of porting the C# we `dlopen` the
//! libraries directly and feed the head pose into Godot.
//!
//! [`node::XrealHeadTracker`] reads the native 6DoF head pose (rotation + position)
//! each frame and applies it to its own transform, so a child `Camera3D` moves and
//! looks around with the wearer's head; the stereo compositor/display path
//! (`InitializeRendering` / `CreateProjectionRigLayer` / `CreateFrame`) renders it to
//! the glasses. On top of head tracking the extension exposes the RGB camera, plane
//! detection, spatial anchors, image tracking, depth meshing, hand tracking, photo /
//! blended capture and FPV streaming — see `node` / `system` / `unity_plugin` and
//! `docs/plans/port-plan.md`.

use godot::prelude::*;

mod camera_feed;
mod controller_probe;
mod depth_mesh;
mod ffi;
mod gl;
mod glasses_events;
mod hand_tracking;
mod jni_bridge;
mod metrics;
mod native;
mod native_error;
mod node;
mod session;
mod signal_guard;
mod system;
mod unity_plugin;
mod video_encoder;

struct GodotXrealExtension;

#[gdextension]
unsafe impl ExtensionLibrary for GodotXrealExtension {}

pub use camera_feed::XrealCameraFeed;
pub use node::XrealHeadTracker;
pub use system::XrealSystem;

#[cfg(test)]
mod doc_gen;
