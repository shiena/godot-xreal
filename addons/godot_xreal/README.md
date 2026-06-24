# Godot XREAL (addon)

Use XREAL glasses from Godot 4. Provides **3DoF head tracking** through a native
GDExtension (Rust / godot-rust). See the repository root for build/RE details.

## Install

1. Copy `addons/godot_xreal/` into your project.
2. Provide the GDExtension binary + the vendored XREAL `.so` files (see the repo's
   `docs/build-and-release.md`). For local dev the repo ships a `godot_xreal.gdextension`
   at the project root pointing at `res://target/...`.
3. Enable **Godot XREAL** in *Project > Project Settings > Plugins* (optional — the
   runtime classes load with the GDExtension regardless).

## Runtime classes

| Class | Base | Purpose |
|---|---|---|
| `XrealHeadTracker` | `Node3D` | Rotates itself from the native 3DoF head pose each frame. Parent a `Camera3D` under it. `is_tracking() -> bool`, `recenter()`. |
| `XrealSystem` | `RefCounted` | Read-only SDK info: `is_available()`, `is_session_started()`, `get_plugin_version() -> String`, `get_device_type() -> int`. |

## Quick start

Drop `addons/godot_xreal/xreal_rig.tscn` (an `XrealHeadTracker` with a `Camera3D`
child) into your scene, or build it in code:

```gdscript
var rig := preload("res://addons/godot_xreal/xreal_rig.tscn").instantiate()
add_child(rig)            # rig is the XrealHeadTracker; the camera looks around with the head

var sys := XrealSystem.new()
print(sys.is_available(), sys.get_plugin_version(), sys.get_device_type())
```

A complete example is in the repo's `demo/` scene.

## Platform

XREAL natives are Android arm64 only → target a Godot Android app on an XREAL host. On
desktop the classes load but head tracking is inert (so you can edit scenes on PC).
