//! godot-xreal — a Godot 4 GDExtension that drives XREAL glasses from Rust.
//!
//! The Unity `com.xreal.xr` SDK is a thin C# layer over native `.so` libraries.
//! Those native libraries export a flat, engine-agnostic C ABI (see
//! `docs/reverse-engineering.md`), so instead of porting the C# we `dlopen` the
//! libraries directly and feed the head pose into Godot.
//!
//! The MVP milestone is **3DoF on screen**: [`node::XrealHeadTracker`] reads the
//! native head rotation each frame and applies it to its own transform, so a
//! child `Camera3D` looks around with the wearer's head. The stereo
//! compositor/display path (`InitializeRendering` / `CreateProjectionRigLayer` /
//! `CreateFrame`) is the next milestone — see `docs/port-plan.md`.

use godot::prelude::*;

mod ffi;
mod jni_bridge;
mod native;
mod node;
mod session;
mod system;
mod unity_plugin;

struct GodotXrealExtension;

#[gdextension]
unsafe impl ExtensionLibrary for GodotXrealExtension {}

pub use node::XrealHeadTracker;
pub use system::XrealSystem;
