# godot-xreal

English | [日本語](README_ja.md)

`godot-xreal` is a Godot 4 GDExtension (written in Rust with [godot-rust](https://godot-rust.github.io/))
that drives [XREAL](https://www.xreal.com/) glasses. It is a port of the Unity `com.xreal.xr` SDK
that reuses the SDK's **native** libraries instead of its Unity C# layer.

> **⚠️ Unofficial & experimental.** This is an independent community project — **not affiliated with,
> endorsed by, or supported by XREAL**. "XREAL" and the SDK are the property of their respective
> owners; the native libraries are **not** bundled — you vendor them yourself as a build prerequisite (see [Prerequisite](#prerequisite-vendor-the-xreal-runtime-libraries)).
> It works by reverse-engineering the vendored SDK's C ABI for interop — use at your own risk.

## Why a native port (not a C# translation)

The Unity SDK is a thin C# wrapper over Android `.so` libraries. Those libraries export a flat,
engine-agnostic C ABI (`libXREALNativeSessionManager.so` → `XREALGetHeadPoseAtTime`, …;
`libXREALXRPlugin.so` → 274 exports incl. an OpenXR-style compositor layer API). So instead of
translating C#, this extension `dlopen`s the libraries and feeds Godot directly. The obfuscated
lower NRSDK proc table (`libnr_api.so` / `NRGetProcAddr`) is avoided. ABI derivation:
[`docs/reference/reverse-engineering.md`](docs/reference/reverse-engineering.md); the RE'd functions and their
GDScript surface: [`docs/reference/native-api-reference.md`](docs/reference/native-api-reference.md).

## Platform

XREAL's native libraries ship for **Android arm64 only**, so this targets a **Godot Android app**
running on an XREAL-compatible host (phone / Beam) with glasses on USB-C. On desktop the extension
still loads (for scene editing) but head tracking is inert.

## Supported features

Verified on XREAL One Pro (and hand tracking on the XREAL Air 2 Ultra) with the **XREAL SDK for Unity
3.1.0** native libraries. Everything below is community-reverse-engineered interop, not an official API.

| Feature | Status | Notes |
|---|---|---|
| **Head tracking** (orientation: pitch / yaw / roll) | ✅ | From the XR-plugin display pose; drives the eye cameras. |
| **Tracking mode** 6DoF / 3DoF / 0DoF | ✅ | Selectable (`xreal/tracking_type` / `XrealSystem.set_tracking_type` / `debug.xreal.tracking_type`). |
| **Stereo glasses display** — head-locked peek window | ✅ | World-locked 3D through the glasses. **Multipass** (both eyes) is the default. |
| **Multiview** stereo (single-pass-instanced) | ✅ works, but **no performance gain** | Renders both eyes correctly (opt-in: `setprop debug.xreal.stereo_mode 2`). **⚠️ It does not reduce rendering load** — in fact slightly heavier than Multipass. Our rig draws two Godot SubViewports (two passes) then copies each into an array layer; the single-pass-instanced win only exists if the *engine* draws both eyes in one multiview pass (which Godot's Compatibility SubViewport rig does not). So **Multipass stays the default.** (The black right eye / colour corruption were two Adreno GLES driver quirks in the layer copy — a `glBlitFramebuffer`-into-layer>0 no-op and a raw-`glCopyImageSubData` format mismatch — now fixed; the old "NR compositor can't sample layer 1" verdict was wrong.) See [`docs/archive/multiview-investigation.md`](docs/archive/multiview-investigation.md). |
| **Recenter** | ✅ | Resets the forward direction (SDK `NativePerception::Recenter`). |
| **Hand tracking** (26-joint, both hands) → Godot `XRHandTracker` | ✅ (Air 2 Ultra) | Live hand joints fed to two `XRServer` hand trackers (`/user/hand_tracker/{left,right}`); the demo draws world-locked joint spheres. **Air 2 Ultra only** — the One Pro lacks the outward cameras (`IsHandTrackingSupported()==false`). Enabled via the internal `SetHandTrackingEnabled` + `input_source=3`. See [`docs/plans/hand-tracking-plan.md`](docs/plans/hand-tracking-plan.md). |
| **RGB camera** as a Godot `CameraFeed` | ✅ | Full-colour, shown in-scene on a head-locked quad. **Requires 3DoF** (it shares the camera with 6DoF SLAM). |
| **Render metrics** — present FPS / dropped / early / latency | ✅ | Live compositor stats via the `NRMetrics*` API (queried directly, not the Unity `UpdateMetrics` sink), on `XrealSystem` (`get_present_fps()`, `get_dropped_frame_count()`, …). See [`docs/plans/render-metrics-gdscript-plan.md`](docs/plans/render-metrics-gdscript-plan.md). |
| **Glasses input** — physical keys (MENU/MULTI: click/double/long) | ✅ | Godot signals (`key_event`, `key_state_changed`). |
| **Wear sensor / brightness / volume / electrochromic / USB hot-plug** | ✅ | Signals (`wearing_changed`, `brightness_changed`, `glasses_connected`, …). |
| **Diagnostics** — session / tracking state, HMD clock, plugin version | ✅ | Via `XrealSystem`. |
| **On-screen touch controller** (phone screen) | ✅ (demo) | App-level Godot UI (`demo/touch_controller.gd`): customizable touchpad + buttons → signals, phone-vibration haptics. The phone shows the controller, the glasses show the 3D scene (separate screens) — no native dependency. Godot analog of the SDK's `XREALVirtualController`. |
| **Phone 3D pointer** (host IMU) | ✅ (demo) | Tilt the phone to aim a 3D ray in the glasses (`demo/phone_pointer.gd`). Orientation is fused in GDScript from the NRController's raw IMU (`accel` → pitch/roll, `gyro` → yaw) exposed by `XrealSystem.poll_controller()` — the NRController *fused pose* and Godot's own `Input.get_gyroscope()` both read empty on this host. The ray raycasts to highlight what it hits and the trigger selects it; an on-screen left/right-hand toggle switches the beam origin; gyro drift is damped by bias-learning + a deadzone. `recenter` sets forward. |
| **Multi-resume** — glasses app keeps running (and rendering) when the phone switches apps | ✅ | **Where the Unity SDK uses a floating return window, this port implements auto-enter Picture-in-Picture instead.** Backgrounding drops the app to a small phone tile (paused-but-visible), so Godot's GL thread + Surface stay alive and the glasses keep showing live frames; tapping the tile returns to fullscreen. `XrealBridge.enableAutoEnterPiP`, driven from `demo/main.gd`; manifest scaffolding `nr_features=multiResume` + `NRFakeActivity`. **Device-verified**: the render submit counter keeps advancing past background (it froze before PiP). Why PiP rather than the floating window / a foreground service / a SurfaceView reparent: `docs/plans/background-render-plan.md`. (A floating return button was implemented first — it returned to the app but couldn't keep the glasses rendering — then removed in favour of PiP: `docs/archive/codex-floatingmanager-analysis.md`.) |
| **Plane detection** → GDScript | ✅ ported (on-device verify pending) | Horizontal/vertical plane detection via `XrealSystem.set_plane_detection_mode()` + `poll_planes()` (added/updated/removed with pose, size, alignment) + `get_plane_boundary()`. Flat C exports in `libXREALXRPlugin.so` (no extra AAR); needs 6DoF. All 4 AR features' C ABI is RE-confirmed — see [`docs/plans/ar-features-plan.md`](docs/plans/ar-features-plan.md). |
| **Spatial anchors** → GDScript | ✅ ported (on-device verify pending) | Create/persist/restore world anchors via `XrealSystem.acquire_anchor()` / `poll_anchors()` / `save_anchor()` / `load_anchor()` / `estimate_anchor_quality()` etc. Flat C exports (`XRTrackedAnchor` layout device-confirmed) + the vendored `nr_spatial_anchor.aar` backend; needs 6DoF. Also adds `is_camera_supported()` / `is_hmd_feature_supported()` — the SDK's per-device gate (the Air 2 Ultra has no RGB camera). |

Not implemented: 6DoF position for the app camera. (Image tracking, marker tracking, depth meshing,
photo / blended capture and FPV streaming are ported — device verification is still pending for some;
see [`docs/plans/ar-features-plan.md`](docs/plans/ar-features-plan.md).)

## Install (prebuilt)

Most users won't build anything — grab the prebuilt addon and vendor the XREAL libraries:

1. Download `godot-xreal-<version>.zip` from the
   [Releases](https://github.com/shiena/godot-xreal/releases) page and extract it into your Godot
   **4.7** project root. It bundles `godot_xreal.gdextension`, the Android arm64 `.so`, the desktop
   editor stubs, and `addons/godot_xreal/` — so no Rust / cargo-ndk / clang is needed.
2. Enable the plugin: Project → Project Settings → Plugins → "Godot XREAL".
3. Vendor the XREAL runtime libraries (the **XREAL Import** dock is the one-click way) — see
   [Prerequisite](#prerequisite-vendor-the-xreal-runtime-libraries) below. These stay under XREAL's
   terms, so they are never bundled.

Building from source is only for modifying the extension — see [Build (from source)](#build-from-source).

## Prerequisite: vendor the XREAL runtime libraries

The XREAL native libraries are **not** included in this repo (they remain under XREAL's terms). Obtain
them from the **XREAL SDK for Unity** — the `com.xreal.xr` package, shipped as a tgz
(`com.xreal.xr.tar.gz`); **3.1.0 is the verified version**. Then stage its libraries one of three ways —
all place the **same** files in the same git-ignored destinations (see the tables below):

1. **Recommended — editor dock.** Enable the addon (Project → Project Settings → Plugins → "Godot
   XREAL"), open the **`XREAL Import`** dock (left panel), click *Select package…*, and pick
   `com.xreal.xr(.tgz|.tar.gz)` (or an already-extracted `package/` folder). It extracts (via the
   system `tar`) and copies everything into place, then rescans — no terminal needed.
2. **Alternative — script.** From a terminal:
   ```powershell
   pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <…>/com.xreal.xr.tar.gz   # or an extracted …/package dir
   ```
   (`./scripts/vendor_xreal_libs.sh <…>` on macOS / Linux.)
3. **Alternative — manual.** Extract the tgz yourself and copy the files in the tables below into their
   destinations under the repo.

Vendoring only handles XREAL's proprietary libs — the addon's own `libgodot_xreal.so` still comes from
the `cargo ndk` build (or a prebuilt release). What gets placed where:

**4 `.so` → `jniLibs/arm64-v8a/`** — packed next to the GDExtension via `godot_xreal.gdextension`
`[dependencies]` and `dlopen`ed at startup. The first three come from `Runtime/Plugins/Android/arm64-v8a/`:

| `.so` | Role |
|---|---|
| `libXREALNativeSessionManager.so` | session / head-pose C ABI |
| `libXREALXRPlugin.so` | XR-plugin compositor / display C ABI |
| `libVulkanSupport.so` | support lib the two above need |
| `libmedia_codec.so` | FPV H.264 encoder (from `Runtime/Scripts/…/Camera Features/…/arm64/`) |

**7 `.aar` → `addons/godot_xreal/android/`** — shipped into the APK by the addon's export plugin
(`export_plugin.gd`): the Java/JNI layer + manifest entries the glasses need. They also carry the
NR native libs (`jni/arm64-v8a/*.so`), which Gradle merges into the APK — so those are **not**
extracted separately. All copied from `Runtime/Plugins/Android/`:

| `.aar` | Role | Native libs delivered into the APK |
|---|---|---|
| `nr_loader.aar` | NR loader Java layer | `libnr_loader.so` |
| `nr_api.aar` | NR API Java layer | `libnr_api.so`, `libnr_plugin_6dof.so`, `libnr_rgb_camera.so` |
| `nr_common.aar` | NR common layer | `libnr_libusb.so` (plus QNN/SNPE libs) |
| `nr_spatial_anchor.aar` | spatial-anchor backend | `libnr_spatial_anchor.so` |
| `nr_image_tracking.aar` | image-tracking backend | `libnr_image_tracking.so` |
| `GlassesDisplayPlugEvent-2.4.2.aar` | glasses-detection `GlassesInitProvider` | — |
| `Log-Control-1.2.aar` | `LogControl` referenced by the above — **required**, or the app crashes before Godot starts | — |

**XrealBridge Java sources** — *not* vendored and *not* pre-compiled: the committed sources
(`addons/godot_xreal/android/src/`) are staged into the gradle build template by the addon's
export plugin and compiled by the export's Gradle run.

**Never copy `nractivitylife*.aar`** — its launcher is Unity-only and breaks a Godot app. (The
QNN/SNPE libs inside `nr_common.aar` are unused by this extension but ride into the APK with the
aar.)

## Build (from source)

Only needed to modify the extension — most users install a prebuilt release (above). The GDExtension
is plain godot-rust; vendor the XREAL libraries first (above), then build. Full
command reference (desktop iteration, manual `cargo ndk` / Gradle steps, signing):
[`docs/guides/build-and-release.md`](docs/guides/build-and-release.md).

To open the project in a **desktop editor** without a missing-library error, build the do-nothing
desktop stubs once after cloning: `pwsh scripts/build_dummy_libs.ps1` (or
`./scripts/build_dummy_libs.sh`) — needs only clang + lld, cross-compiles every desktop target from
any host. The extension is Android-only, but Godot can't express that, so the `.gdextension` points
desktop platforms at these stubs ([`dummy/gdext_dummy.c`](dummy/gdext_dummy.c)); they register
nothing and are not committed.

### Build & install

With the toolchain on `PATH` (Rust `aarch64-linux-android` target, `cargo-ndk`, `ANDROID_NDK_HOME`, a
Godot 4.7-stable binary, `adb`), `scripts/build.sh` (or `scripts/build.ps1`) wraps the four Android
stages — cargo-ndk build → Godot APK export → `adb install` → launch. It re-checks the prerequisite
above first (both the 4 `.so` and the addon's `.aar`/`.jar`) and prints the same guide if anything
is missing.

```bash
./scripts/build.sh --all      # build + export + install + run on the glasses
```

## Usage (MVP)

1. Install the addon — a [prebuilt release](#install-prebuilt) or [built from source](#build-from-source) — and vendor the libraries.
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
| | `get_present_fps() / get_dropped_frame_count() / get_early_frame_count() / …` | Live compositor render metrics (`NRMetrics*`). |
| | `get_diagnostics() -> String` | One-line perception-pipeline diagnostic. |
| `XrealHandTracker` (Node) | (registers trackers) | Publishes XREAL hand tracking to `XRServer` as two `XRHandTracker`s (`/user/hand_tracker/{left,right}`), updated each frame. Add it to the scene; drive a hand skeleton with `XRHandModifier3D` or read the trackers directly. **Air 2 Ultra only.** |

## Layout

```
godot_xreal.gdextension  GDExtension manifest (Android .so + desktop stubs + dlopen deps)
addons/godot_xreal/      the installable addon
  plugin.cfg/.gd         EditorPlugin — also registers the editor docks
  export_plugin.gd       Android export: manifest, permissions, .aar/assets staging
  xreal_rig.tscn         XrealHeadTracker + Camera3D rig
  editor/                docks: vendor_import_dock.gd (SDK import), image_db_dock.gd
  android/               bridge Java source + nr_plugins.json (vendored .aar git-ignored)
src/                     the Rust GDExtension
  lib.rs                 ExtensionLibrary entry
  ffi.rs / native.rs     RE'd ABI (repr(C) structs) + dlopen/dlsym of the XREAL .so
  session.rs/jni_bridge.rs  session lifecycle + Android Activity acquisition
  signal_guard.rs        null-NativeGlasses teardown crash workaround
  node.rs                XrealHeadTracker (Node3D)
  system.rs              XrealSystem (RefCounted) + XrealAR (Node — AR-change signals)
  camera_feed.rs         XrealCameraFeed (CameraFeed) — RGB camera
  hand_tracking.rs       XrealHandTracker (Node) → XRHandTracker
  depth_mesh.rs · metrics.rs · video_encoder.rs · controller_probe.rs
                         AR mesh · render metrics · FPV H.264 streaming · phone-IMU pointer
  gl.rs / unity_plugin.rs   GLES + Unity native-plugin emulation (display path)
  glasses_events.rs / native_error.rs   cached event funnels
demo/                    AR demo (main.tscn + managers: hand/anchor/image/mesh/stream/
                         capture/blend + phone touch controller)
dummy/                   desktop GDExtension stubs (gdext_dummy.c) so the editor loads
jniLibs/                 vendored XREAL .so (git-ignored) + built libgodot_xreal.so
scripts/                 build + vendor_xreal_libs + build_dummy_libs + build_image_db (.ps1/.sh)
.github/workflows/       CI (fmt/clippy/test/build) + Release (prebuilt addon)
docs/                    guides / reference / plans / archive — see docs/README.md for the index
```

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
