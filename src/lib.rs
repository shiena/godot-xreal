//! godot-xreal — a Godot 4 GDExtension that drives XREAL glasses from Rust.
//!
//! The Unity `com.xreal.xr` SDK is a thin C# layer over native `.so` libraries.
//! Those native libraries export a flat, engine-agnostic C ABI (see
//! `docs/reference/reverse-engineering.md`), so instead of porting the C# we `dlopen` the
//! libraries directly and feed the head pose into Godot.
//!
//! The MVP milestone is **3DoF on screen**: [`node::XrealHeadTracker`] reads the
//! native head rotation each frame and applies it to its own transform, so a
//! child `Camera3D` looks around with the wearer's head. The stereo
//! compositor/display path (`InitializeRendering` / `CreateProjectionRigLayer` /
//! `CreateFrame`) is the next milestone — see `docs/plans/port-plan.md`.

use godot::prelude::*;

mod camera_feed;
mod controller_probe;
mod ffi;
mod gl;
mod glasses_events;
mod jni_bridge;
mod metrics;
mod native;
mod native_error;
mod node;
mod session;
mod signal_guard;
mod system;
mod unity_plugin;

struct GodotXrealExtension;

#[gdextension]
unsafe impl ExtensionLibrary for GodotXrealExtension {}

pub use camera_feed::XrealCameraFeed;
pub use node::XrealHeadTracker;
pub use system::XrealSystem;
