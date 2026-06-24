# godot-xreal

English | [日本語](README_ja.md)

`godot-xreal` is a Godot 4 GDExtension (written in Rust with [godot-rust](https://godot-rust.github.io/))
that drives [XREAL](https://www.xreal.com/) glasses. It is a port of the Unity `com.xreal.xr` SDK
that reuses the SDK's **native** libraries instead of its Unity C# layer.

> **Status: early skeleton.** The current milestone is **3DoF (head rotation) on screen**. The
> stereo display/compositor path is not done yet. See [`docs/port-plan.md`](docs/port-plan.md).

## Why a native port (not a C# translation)

The Unity SDK is a thin C# wrapper over Android `.so` libraries. Those libraries export a flat,
engine-agnostic C ABI (`libXREALNativeSessionManager.so` → `XREALGetHeadPoseAtTime`, …;
`libXREALXRPlugin.so` → 274 exports incl. an OpenXR-style compositor layer API). So instead of
translating C#, this extension `dlopen`s the libraries and feeds Godot directly. The obfuscated
lower NRSDK proc table (`libnr_api.so` / `NRGetProcAddr`) is avoided. Details:
[`docs/reverse-engineering.md`](docs/reverse-engineering.md).

## Platform

XREAL's native libraries ship for **Android arm64 only**, so this targets a **Godot Android app**
running on an XREAL-compatible host (phone / Beam) with glasses on USB-C. On desktop the extension
still loads (for scene editing) but head tracking is inert.

## Usage (MVP)

1. Build the extension and vendor the XREAL libraries — see
   [`docs/build-and-release.md`](docs/build-and-release.md).
2. Instance `addons/godot_xreal/xreal_rig.tscn` (an `XrealHeadTracker` with a `Camera3D`
   child) into your scene, or add an `XrealHeadTracker` and parent a `Camera3D` yourself.
3. On device, the camera looks around with the wearer's head (3DoF).

The bundled `demo/main.tscn` does exactly this with a ring of boxes and an on-screen
status panel.

```
XrealHeadTracker (Node3D)   # rotation driven by native head pose
└── Camera3D                # current = true
```

### Runtime classes (registered by the GDExtension)

| Class | Member | Description |
|---|---|---|
| `XrealHeadTracker` (Node3D) | `is_tracking() -> bool` | A native pose was applied on the last frame. |
| | `recenter()` | Reset the 3DoF forward direction (`RecenterGlasses`). |
| `XrealSystem` (RefCounted) | `is_available() -> bool` | Native libraries loaded (false on desktop). |
| | `is_session_started() -> bool` | A native session is running. |
| | `get_plugin_version() -> String` | XREAL plugin version. |
| | `get_device_type() -> int` | `XREALDeviceType` enum value. |

## Layout

```
addons/godot_xreal/   the installable addon (plugin.cfg, plugin.gd, xreal_rig.tscn)
src/
  lib.rs        ExtensionLibrary entry
  ffi.rs        repr(C) structs / enums / fn-pointer types (the RE'd ABI)
  native.rs     dlopen/dlsym of the XREAL .so files
  session.rs    safe lifecycle + session bootstrap + coordinate conversion
  jni_bridge.rs Android Activity acquisition for the session bootstrap
  node.rs       XrealHeadTracker (Node3D) — the 3DoF MVP node
  system.rs     XrealSystem (RefCounted) — read-only SDK info
demo/           demo scene (main.tscn + main.gd) with a status UI
jniLibs/        vendored XREAL .so (git-ignored) + built libgodot_xreal.so
tools/          vendor_xreal_libs.ps1
docs/           port plan + reverse-engineering notes
```

## License

MIT (see [LICENSE](LICENSE)). The XREAL native libraries are **not** included and remain under
XREAL's own terms.
