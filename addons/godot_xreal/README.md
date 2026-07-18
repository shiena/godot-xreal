# Godot XREAL (addon)

Use XREAL glasses from Godot 4. Provides **6DoF head tracking** (rotation + position;
3DoF/0DoF selectable) through a native GDExtension (Rust / godot-rust). See the repository root for build/RE details.

## Install

1. Copy `addons/godot_xreal/` into your project.
2. Provide the GDExtension binary + the vendored XREAL `.so` files (see the repo's
   `docs/guides/build-and-release.md`). For local dev the repo ships a `godot_xreal.gdextension`
   at the project root pointing at `res://target/...`.
3. Enable **Godot XREAL** in *Project > Project Settings > Plugins* (optional — the
   runtime classes load with the GDExtension regardless).

## Runtime classes

| Class | Base | Purpose |
|---|---|---|
| `XrealHeadTracker` | `Node3D` | Drives its transform (rotation + position) from the native head pose each frame. Parent a `Camera3D` under it. `is_tracking() -> bool`, `recenter()`. Emits hot-plug (`glasses_connected` / `glasses_disconnected`) and hardware-input signals (`key_event`, `key_state_changed`, `wearing_changed`, `brightness_changed`, `volume_changed`, `ec_level_changed`, `glasses_event`) with `KEY_*` / `ACTION_*` / `KEY_STATE_*` constants. |
| `XrealSystem` | `RefCounted` | SDK info + control: `is_available()`, `is_session_started()`, `get_plugin_version() -> String`, `get_device_type() -> int`, `get_tracking_state()/get_tracking_reason()/get_tracking_type() -> int`, `switch_tracking_type(type) -> bool` (`TRACKING_*` constants), `set_display_bypass_psensor(bypass) -> int`, `get_hmd_time_nanos() -> int`, `get_head_rotation() -> Quaternion`, `get_diagnostics() -> String`. |

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
