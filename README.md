# godot-xreal

English | [日本語](README_ja.md)

`godot-xreal` is a Godot 4 GDExtension (written in Rust with [godot-rust](https://godot-rust.github.io/))
that drives [XREAL](https://www.xreal.com/) glasses. It is a port of the Unity `com.xreal.xr` SDK
that reuses the SDK's **native** libraries instead of its Unity C# layer.

> **⚠️ Unofficial & experimental.** This is an independent community project — **not affiliated with,
> endorsed by, or supported by XREAL**. "XREAL" and the SDK are the property of their respective
> owners; the native libraries are **not** bundled — you vendor them yourself as a build prerequisite (see [Build](#build)).
> It works by reverse-engineering the vendored SDK's C ABI for interop — use at your own risk.

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

## Supported features

Verified on XREAL One Pro with the **XREAL SDK for Unity 3.1.0** native libraries. Everything below is
community-reverse-engineered interop, not an official API.

| Feature | Status | Notes |
|---|---|---|
| **Head tracking** (orientation: pitch / yaw / roll) | ✅ | From the XR-plugin display pose; drives the eye cameras. |
| **Tracking mode** 6DoF / 3DoF / 0DoF | ✅ | Selectable (`xreal/tracking_type` / `XrealSystem.set_tracking_type` / `debug.xreal.tracking_type`). |
| **Stereo glasses display** — head-locked peek window | ✅ | World-locked 3D through the glasses. **Multipass** (both eyes); it is the only stereo mode (no selector). |
| **Multiview** stereo | ❌ Shelved | Right eye is black — the NR compositor (`libnr_api`) can't import our client `GL_TEXTURE_2D_ARRAY`, and it gives no benefit on this two-SubViewport rig anyway. The code is kept but disabled; dev-only escape `setprop debug.xreal.force_multiview 1`. See `docs/codex-righteye-analysis.md`. |
| **Recenter** | ✅ | Resets the forward direction (SDK `NativePerception::Recenter`). |
| **RGB camera** as a Godot `CameraFeed` | ✅ | Full-colour, shown in-scene on a head-locked quad. **Requires 3DoF** (it shares the camera with 6DoF SLAM). |
| **Glasses input** — physical keys (MENU/MULTI: click/double/long) | ✅ | Godot signals (`key_event`, `key_state_changed`). |
| **Wear sensor / brightness / volume / electrochromic / USB hot-plug** | ✅ | Signals (`wearing_changed`, `brightness_changed`, `glasses_connected`, …). |
| **Diagnostics** — session / tracking state, HMD clock, plugin version | ✅ | Via `XrealSystem`. |

Not implemented: 6DoF position for the app camera, hand/image/plane tracking, spatial anchors, meshing,
audio/photo capture, the NRSDK's higher-level perception features.

## Build

The GDExtension is plain godot-rust; the one project-specific step is a **prerequisite** you do once —
vendoring the XREAL native libraries — before the Android export. Full command reference (desktop
iteration, manual `cargo ndk` / Gradle steps, signing): [`docs/build-and-release.md`](docs/build-and-release.md).

### Prerequisite: vendor the XREAL runtime libraries

The XREAL native libraries are **not** included in this repo (they remain under XREAL's terms). Obtain
them from the **XREAL SDK for Unity** — the `com.xreal.xr` package, shipped as a tgz
(`com.xreal.xr.tar.gz`); **3.1.0 is the verified version** — and place these **8 `.so` into
`jniLibs/arm64-v8a/`** (git-ignored) before exporting the APK:

1. Extract `com.xreal.xr.tar.gz` → a `package/` directory.
2. **3 core libs** from `package/Runtime/Plugins/Android/arm64-v8a/` — copy them, or run
   `pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <…>/package`:
   `libXREALNativeSessionManager.so`, `libXREALXRPlugin.so`, `libVulkanSupport.so`.
3. **5 NR libs** from the package's `.aar` files (an `.aar` is a zip; take `jni/arm64-v8a/<lib>`):
   - `nr_api.aar` → `libnr_api.so`, `libnr_plugin_6dof.so`, `libnr_rgb_camera.so`
   - `nr_loader.aar` → `libnr_loader.so`
   - `nr_common.aar` → `libnr_libusb.so`

### Build & install

With the toolchain on `PATH` (Rust `aarch64-linux-android` target, `cargo-ndk`, `ANDROID_NDK_HOME`, a
Godot 4.7-stable binary, `adb`), `scripts/build.sh` (or `scripts/build.ps1`) wraps the four Android
stages — cargo-ndk build → Godot APK export → `adb install` → launch. It re-checks the prerequisite
above first and prints the same guide if any `.so` is missing.

```bash
./scripts/build.sh --all      # build + export + install + run on the glasses
```

## Usage (MVP)

1. Vendor the libraries and build/deploy — see [Build](#build) above.
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
| | `get_tracking_state() / get_tracking_reason() / get_tracking_type() -> int` | XR-plugin tracking enums (`-1` when unavailable). Also the SLAM-state notification source. |
| | `get_glasses_temperature_level() -> int` | Over-temperature poll: `0` normal / `1` warm / `2` hot (`-1` until first reported). |
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
