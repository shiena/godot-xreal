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
lower NRSDK proc table (`libnr_api.so` / `NRGetProcAddr`) is avoided. ABI derivation:
[`docs/reverse-engineering.md`](docs/reverse-engineering.md); the RE'd functions and their
GDScript surface: [`docs/native-api-reference.md`](docs/native-api-reference.md).

## Platform

XREAL's native libraries ship for **Android arm64 only**, so this targets a **Godot Android app**
running on an XREAL-compatible host (phone / Beam) with glasses on USB-C. On desktop the extension
still loads (for scene editing) but head tracking is inert.

## Vendoring the XREAL runtime libraries (required)

The XREAL native libraries are **not** included in this repo (they remain under XREAL's terms). You
obtain them from the **XREAL SDK for Unity** — the `com.xreal.xr` package, shipped as a tgz
(`com.xreal.xr.tar.gz`) — and place these **8 `.so` into `jniLibs/arm64-v8a/`** before exporting the
APK (`jniLibs/` is git-ignored):

1. Extract `com.xreal.xr.tar.gz` → a `package/` directory.
2. **3 core libs** from `package/Runtime/Plugins/Android/arm64-v8a/` — copy them, or run
   `pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <…>/package`:
   `libXREALNativeSessionManager.so`, `libXREALXRPlugin.so`, `libVulkanSupport.so`.
3. **5 NR libs** from the package's `.aar` files (an `.aar` is a zip; take `jni/arm64-v8a/<lib>`):
   - `nr_api.aar` → `libnr_api.so`, `libnr_plugin_6dof.so`, `libnr_rgb_camera.so`
   - `nr_loader.aar` → `libnr_loader.so`
   - `nr_common.aar` → `libnr_libusb.so`

`scripts/build.ps1` / `scripts/build.sh` verify these before an export and print this same guide if any
are missing. Details: [`docs/build-and-release.md`](docs/build-and-release.md).

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
| | `debug_pose_text() -> String` | Raw pose readout for on-screen debugging. |
| | signal `display_started()` | Glasses display + head tracking first went live. |
| | signal `glasses_connected()` / `glasses_disconnected()` | USB hot-plug events. |
| | signal `key_event(key, action)` | Physical key click/double/long (`KEY_*`, `ACTION_*` constants). |
| | signal `key_state_changed(key, state)` | Raw key down/up (`KEY_STATE_*` constants). |
| | signal `wearing_changed(wearing)` | Proximity (wear) sensor put-on / take-off. |
| | signal `brightness_changed(level)` / `volume_changed(level)` / `ec_level_changed(level)` | Glasses-side state changes. |
| | signal `glasses_event(action_type, para, para2, para3)` | Catch-all for every raw glasses hardware event. |
| `XrealSystem` (RefCounted) | `is_available() -> bool` | Native libraries loaded (false on desktop). |
| | `is_session_started() -> bool` | A native session is running. |
| | `get_plugin_version() -> String` | XREAL plugin version. |
| | `get_device_type() -> int` | `XREALDeviceType` enum value. |
| | `get_tracking_state() / get_tracking_reason() / get_tracking_type() -> int` | XR-plugin tracking enums (`-1` when unavailable). |
| | `switch_tracking_type(type) -> bool` | Switch tracking mode (`TRACKING_6DOF/3DOF/0DOF/0DOF_STAB` constants). |
| | `set_display_bypass_psensor(bypass) -> int` | Keep the display on while the glasses are not worn (SDK status). |
| | `get_hmd_time_nanos() -> int` | Native HMD clock (ns, `0` when down). |
| | `get_head_rotation() -> Quaternion` | Latest head rotation without a tracker node. |
| | `get_diagnostics() -> String` | One-line perception-pipeline diagnostic. |

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
scripts/        build.ps1 / build.sh (pipeline) + vendor_xreal_libs.ps1 (copy core libs)
docs/           port plan + reverse-engineering notes
```

## License

MIT (see [LICENSE](LICENSE)). The XREAL native libraries are **not** included and remain under
XREAL's own terms.
