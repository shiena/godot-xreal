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
| **On-screen touch controller** (phone screen) | ✅ (demo) | App-level Godot UI (`demo/touch_controller.gd`): customizable touchpad + buttons → signals, phone-vibration haptics. The phone shows the controller, the glasses show the 3D scene (separate screens) — no native dependency. Godot analog of the SDK's `XREALVirtualController`. |
| **Phone 3D pointer** (host IMU) | ✅ (demo) | Tilt the phone to aim a 3D ray in the glasses (`demo/phone_pointer.gd`). Orientation is fused in GDScript from the NRController's raw IMU (`accel` → pitch/roll, `gyro` → yaw) exposed by `XrealSystem.poll_controller()` — the NRController *fused pose* and Godot's own `Input.get_gyroscope()` both read empty on this host. The ray raycasts to highlight what it hits and the trigger selects it; an on-screen left/right-hand toggle switches the beam origin; gyro drift is damped by bias-learning + a deadzone. `recenter` sets forward. |
| **Multi-resume** — glasses app keeps running when the phone switches apps | ✅ | Verified: after Home / another app, head tracking + camera keep updating on the glasses. From the manifest scaffolding (`nr_features=multiResume` + `NRFakeActivity`). A floating "return" button is **not** feasible (a self-overlay disturbs Godot's GL surface; the NR `FloatingManager` isn't accessible to a non-Unity app). |

Not implemented: 6DoF position for the app camera, hand/image/plane tracking, spatial anchors, meshing,
audio/photo capture, the NRSDK's higher-level perception features. (Plane / image / anchor / mesh are
portable without ARCore or AR Foundation — feasibility survey: [`docs/ar-features-plan.md`](docs/ar-features-plan.md).)

## Build

The GDExtension is plain godot-rust; the one project-specific step is a **prerequisite** you do once —
vendoring the XREAL native libraries — before the Android export. Full command reference (desktop
iteration, manual `cargo ndk` / Gradle steps, signing): [`docs/build-and-release.md`](docs/build-and-release.md).

To open the project in a **desktop editor** without a missing-library error, build the do-nothing
desktop stubs once after cloning: `pwsh scripts/build_dummy_libs.ps1` (or
`./scripts/build_dummy_libs.sh`) — needs only clang + lld, cross-compiles every desktop target from
any host. The extension is Android-only, but Godot can't express that, so the `.gdextension` points
desktop platforms at these stubs ([`dummy/gdext_dummy.c`](dummy/gdext_dummy.c)); they register
nothing and are not committed.

### Prerequisite: vendor the XREAL runtime libraries

The XREAL native libraries are **not** included in this repo (they remain under XREAL's terms). Obtain
them from the **XREAL SDK for Unity** — the `com.xreal.xr` package, shipped as a tgz
(`com.xreal.xr.tar.gz`); **3.1.0 is the verified version**. Extract it (→ a `package/` directory)
and run:

```powershell
pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <…>/package
```

The script stages everything the Android export needs (all destinations are git-ignored; nothing is
downloaded — you supply the package):

**3 core `.so` → `jniLibs/arm64-v8a/`** — copied from `Runtime/Plugins/Android/arm64-v8a/`; packed
next to the GDExtension via `godot_xreal.gdextension` `[dependencies]` and `dlopen`ed at startup:

| `.so` | Role |
|---|---|
| `libXREALNativeSessionManager.so` | session / head-pose C ABI |
| `libXREALXRPlugin.so` | XR-plugin compositor / display C ABI |
| `libVulkanSupport.so` | support lib the two above need |

**5 `.aar` → `addons/godot_xreal/android/`** — shipped into the APK by the addon's export plugin
(`export_plugin.gd`): the Java/JNI layer + manifest entries the glasses need. They also carry the
NR native libs (`jni/arm64-v8a/*.so`), which Gradle merges into the APK — so those are **not**
extracted separately. All copied from `Runtime/Plugins/Android/`:

| `.aar` | Role | Native libs delivered into the APK |
|---|---|---|
| `nr_loader.aar` | NR loader Java layer | `libnr_loader.so` |
| `nr_api.aar` | NR API Java layer | `libnr_api.so`, `libnr_plugin_6dof.so`, `libnr_rgb_camera.so` |
| `nr_common.aar` | NR common layer | `libnr_libusb.so` (plus QNN/SNPE libs) |
| `GlassesDisplayPlugEvent-2.4.2.aar` | glasses-detection `GlassesInitProvider` | — |
| `Log-Control-1.2.aar` | `LogControl` referenced by the above — **required**, or the app crashes before Godot starts | — |

**XrealBridge Java sources** — *not* vendored and *not* pre-compiled: the committed sources
(`addons/godot_xreal/android/src/`) are staged into the gradle build template by the addon's
export plugin and compiled by the export's Gradle run.

**Never copy `nractivitylife*.aar`** — its launcher is Unity-only and breaks a Godot app. (The
QNN/SNPE libs inside `nr_common.aar` are unused by this extension but ride into the APK with the
aar.)

### Build & install

With the toolchain on `PATH` (Rust `aarch64-linux-android` target, `cargo-ndk`, `ANDROID_NDK_HOME`, a
Godot 4.7-stable binary, `adb`), `scripts/build.sh` (or `scripts/build.ps1`) wraps the four Android
stages — cargo-ndk build → Godot APK export → `adb install` → launch. It re-checks the prerequisite
above first (both the 3 core `.so` and the addon's `.aar`/`.jar`) and prints the same guide if anything
is missing.

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
| | `get_last_native_error_code() -> int` / `get_last_native_error_message() -> String` | Latest native error (`XREALErrorCode`; `-1` / `""` until one fires). |
| | `switch_tracking_type(type) -> bool` | Switch tracking mode (`TRACKING_6DOF/3DOF/0DOF/0DOF_STAB` constants). |
| | `set_display_bypass_psensor(bypass) -> int` | Keep the display on while the glasses are not worn (SDK status). |
| | `get_hmd_time_nanos() -> int` | Native HMD clock (ns, `0` when down). |
| | `get_head_rotation() -> Quaternion` | Latest head rotation without a tracker node. |
| | `get_diagnostics() -> String` | One-line perception-pipeline diagnostic. |

## Layout

```
addons/godot_xreal/   the installable addon (plugin.cfg, plugin.gd, xreal_rig.tscn,
                      export_plugin.gd + android/: bridge Java source, vendored .aar/.jar git-ignored)
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
scripts/        build.ps1 / build.sh (pipeline) + vendor_xreal_libs.ps1 (stage all runtime pieces)
docs/           port plan + reverse-engineering notes
```

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
