# godot-xreal

English | [日本語](README_ja.md)

`godot-xreal` is a Godot 4 GDExtension, written in Rust with [godot-rust](https://godot-rust.github.io/),
that drives [XREAL](https://www.xreal.com/) glasses. It ports the Unity `com.xreal.xr` SDK by reusing
the SDK's native libraries instead of its Unity C# layer.

You build with Godot's XR workflow: an `XROrigin3D` holding an `XRCamera3D` and `XRController3D`
nodes, hand joints on `XRHandTracker`, buttons on InputMap actions. The addon attaches to that
hierarchy rather than replacing it.

> **⚠️ Unofficial and experimental.** This independent community project is not affiliated with,
> endorsed by, or supported by XREAL. "XREAL" and the SDK belong to their respective owners. The
> native libraries are not bundled; you vendor them yourself as a build prerequisite (see
> [Prerequisite](#prerequisite-vendor-the-xreal-runtime-libraries)). Interop rests on
> reverse-engineering the vendored SDK's C ABI, so use it at your own risk.

## Why a native port (not a C# translation)

The Unity SDK is a thin C# wrapper over Android `.so` libraries. Those libraries export a flat,
engine-agnostic C ABI (`libXREALNativeSessionManager.so` → `XREALGetHeadPoseAtTime`, …;
`libXREALXRPlugin.so` → 274 exports including an OpenXR-style compositor layer API). So instead of
translating C#, this extension `dlopen`s the libraries and feeds Godot directly. It avoids the
obfuscated lower NRSDK proc table (`libnr_api.so` / `NRGetProcAddr`). How the ABI was derived, and
the full list of RE'd functions, are in the developer docs, indexed at
[`docs/develop/README.md`](docs/develop/README.md).

## Platform

XREAL's native libraries ship for Android arm64 only, so this targets a Godot Android app running on
an XREAL-compatible host (phone or Beam) with glasses on USB-C. On desktop the extension still loads,
for scene editing, but head tracking stays inert.

## Supported features

Verified on the XREAL One Pro (rows marked "Air 2 Ultra" on the XREAL Air 2 Ultra) with the
XREAL SDK for Unity 3.1.0 native libraries. Everything below is community-reverse-engineered interop, not an official API.
The design notes and measurements behind each row live in the developer docs, indexed at
[`docs/develop/README.md`](docs/develop/README.md).

| Feature | Status | Notes |
|---|---|---|
| **Godot XR workflow** (`XROrigin3D`, `XRCamera3D`, `XRController3D`, `XRHandTracker`, InputMap) | ✅ | Your scene owns the hierarchy; the addon attaches to it. Controller inputs use Godot's standard OpenXR action names (`trigger_click`, `grip_click`, `menu_button`, `primary_click`, `trigger`, `grip`, `primary`). The XREAL-only capabilities below stay in this addon's own components. |
| **Head tracking** (6DoF: rotation and position world-lock) | ✅ | The XR-plugin display pose, full orientation and translation, drives the eye cameras. |
| **Tracking mode** 6DoF / 3DoF / 0DoF | ✅ | Select it with `xreal/tracking_type`, `XrealSystem.set_tracking_type`, or `debug.xreal.tracking_type`. |
| **Stereo glasses display** (head-locked peek window) | ✅ | World-locked 3D through the glasses. Multipass, both eyes, is the default. |
| **Multiview** stereo (single-pass-instanced) | ✅ Vulkan Mobile only; gains depend on content | Multipass works on both renderers and stays the default. Only the Vulkan Mobile renderer has true single-pass multiview: enable the `xreal/xr_multiview_poc` project setting (or `setprop debug.xreal.xr_multiview 1`) and a custom `XRInterfaceExtension` has Godot render both eyes in one scene pass into a two-layer target. On device it halves draw calls: a draw-call-bound scene ran 5.9% faster and a 100k-splat 3DGS scene slightly faster, while a GPU-bound scene ran 13% slower on the Adreno 710. It needs `xr/shaders/enabled=true` and XR Mode `OpenXR` on the export preset; the [addon README](addons/godot_xreal/README.md#project-settings) covers the setup. On the Compatibility (GL) renderer the setting is ignored with a warning and Multipass runs instead. The SDK's own Multiview was reachable there through `xreal/stereo_mode` until 2026-08-13, but it **cost exactly what Multipass cost** (the rig still drew two SubViewports and copied each into an array layer) and its layer copy could not mirror the eye image, so it was removed. |
| **Vulkan Mobile renderer** (experimental) | ✅ device-verified, opt-in | A second export preset, "Android Vulkan", runs the whole port on Godot's Forward Mobile Vulkan renderer and installs alongside the shipping Compatibility build. Glasses rendering, the RGB camera, and FPV streaming and recording all work on device, matching the Compatibility build's colors. Glasses default to a tear-free `vkQueueWaitIdle` sync at about 52 FPS; the faster pipelined mode reaches 58-60 FPS but shears the lower part of the view under fast head motion, because the SDK compositor reprojects the eye image with no GPU fence against our Vulkan-to-GL copy. `debug.xreal.vk_sync 1` opts into 58-60 FPS with that tear, and the tear-free-at-60 fix needs a Vulkan device extension Godot does not enable. Each eye and each encoder frame crosses from Vulkan to the SDK's GL compositor through a self-allocated `VkImage` shared as an opaque-fd GL texture, so the compositor still receives plain GL texture names. It unlocks `RenderingDevice` and GPU compute, which the Compatibility renderer lacks, and unifies the render path with Android XR and Project Aura. It stays behind `debug.xreal.vulkan_glasses` while a thermal soak finishes, so Compatibility remains the default. |
| **Recenter** | ✅ | Resets the forward direction (SDK `NativePerception::Recenter`). |
| **Render metrics** (present FPS, dropped, early, latency) | ✅ | Live compositor stats from the `NRMetrics*` API, queried directly rather than through the Unity `UpdateMetrics` sink. Read them on `XrealSystem` with `get_present_fps()`, `get_dropped_frame_count()`, and friends. |
| **Focus plane** (compositor reprojection) | ✅ device verification pending | Before every VSync the compositor warps the last frame onto the newest head pose, against a single plane the SDK pins at 1.4 m. Content far from that plane smears and doubles. `XrealSystem.set_focus_plane()` moves it per frame in head-local space, and the `XrealFocusPlane` component drives it from a forward raycast, as the SDK's `FocusManager` does. The `SetFocusPlane` export takes two by-value `UnityXRVector3`s, point and normal, where Unity's own wrapper takes a third velocity it drops before this point. |
| **Glasses input** (physical keys MENU/MULTI: click, double, long) | ✅ | Godot signals `key_event` and `key_state_changed`. |
| **Wear sensor, brightness, volume, electrochromic, USB hot-plug** | ✅ | Signals `wearing_changed`, `brightness_changed`, `glasses_connected`, and the rest. |
| **Diagnostics** (session and tracking state, HMD clock, plugin version) | ✅ | Read from `XrealSystem`. |
| **Multi-resume** (the glasses app keeps running and rendering when the phone switches apps) | ✅ | Where the Unity SDK uses a floating return window, this port enters Picture-in-Picture automatically. Backgrounding drops the app to a small phone tile, paused but visible, so Godot's GL thread and Surface stay alive and the glasses keep showing live frames; tapping the tile returns to fullscreen. `XrealBridge.enableAutoEnterPiP` drives it from `demo/main.gd`, with manifest scaffolding `nr_features=multiResume` and `NRFakeActivity`. On device, the render submit counter keeps advancing past background. PiP won the design comparison against a floating window, a foreground service, and a SurfaceView reparent. |
| **Capture audio** (microphone and app audio) | ✅ | Recordings and FPV streams can carry both, and the SDK's encoder captures and mixes them natively; Godot's own mixer stays out of the path. Set the capture component's `audio_state`. The mic needs `RECORD_AUDIO`. App ("internal") audio needs an Android MediaProjection, because `addInternalAudio` makes the encoder open its own `AudioPlaybackCapture`: a screen-capture consent dialog appears on the first capture that asks for it, and that capture records mic-only while the next has both. For the DSP the mic goes through, see the [audio note](#note-what-the-microphone-does-and-does-not-pick-up) below. |
| **RGB camera** as a Godot `CameraFeed` | ✅ (One series) | Full colour, shown in-scene on a head-locked quad. Runs alongside 6DoF, since SLAM uses the separate grayscale cameras. |
| **Hand tracking** (26 joints, both hands) → Godot `XRHandTracker` | ✅ (Air 2 Ultra) | Live hand joints feed two `XRServer` hand trackers (`/user/hand_tracker/{left,right}`), and the demo draws world-locked joint spheres. The One Pro lacks the outward cameras and answers `IsHandTrackingSupported()==false`, so this needs an Air 2 Ultra. The internal `SetHandTrackingEnabled` plus `input_source=3` turns it on. |
| **Plane detection** → GDScript | ✅ (Air 2 Ultra) | Horizontal and vertical plane detection through `XrealSystem.set_plane_detection_mode()` and `poll_planes()`, which reports added, updated, and removed planes with pose, size, and alignment, plus `get_plane_boundary()`. Flat C exports in `libXREALXRPlugin.so` carry it, so it needs no extra AAR, but it does need 6DoF. All four AR features' C ABI is RE-confirmed. |
| **Spatial anchors** → GDScript | ✅ (Air 2 Ultra) | Create, persist, and restore world anchors with `XrealSystem.acquire_anchor()`, `poll_anchors()`, `save_anchor()`, `load_anchor()`, `estimate_anchor_quality()`, and the rest. Flat C exports (the `XRTrackedAnchor` layout is device-confirmed) sit on the vendored `nr_spatial_anchor.aar` backend, and 6DoF is required. This also adds the SDK's per-device gate, `is_camera_supported()` and `is_hmd_feature_supported()`, since the Air 2 Ultra has no RGB camera. |
| **On-screen touch controller** (phone screen) | ✅ (demo) | App-level Godot UI (`demo/touch_controller.gd`): a customizable touchpad and buttons emit signals, and the phone vibrates for haptics. The phone shows the controller while the glasses show the 3D scene, on separate screens, with no native dependency. It is the Godot analog of the SDK's `XREALVirtualController`. |
| **Phone controller → Godot XR/Input** | ✅ | `XrealXRRuntime` polls the NRController raw IMU/touchpad and `XrealXRInputRouter` fuses a standard `XRControllerTracker` pose. Glasses keys and app-owned phone UI buttons flow through the same addon bridge to `XRController3D` and `xr_select`/`xr_grab`/`xr_menu`. The demo only draws the standard pose as a ray. Raw native button bits remain unmapped until device-verified. |

Also ported: image tracking, marker tracking, depth meshing, photo and blended capture, and FPV
streaming. Depth meshing carries the SDK's per-vertex semantic classification, and a scan saved on
the glasses becomes an `ArrayMesh` or a `.glb` through an editor dock. Device verification is still
pending for some.

## Install (prebuilt)

Most users build nothing: grab the prebuilt addon and vendor the XREAL libraries.

1. Download `godot-xreal-<version>.zip` from the
   [Releases](https://github.com/shiena/godot-xreal/releases) page and extract it into your Godot
   4.7 project root. It bundles `godot_xreal.gdextension`, the Android arm64 `.so`, the desktop
   editor stubs, and `addons/godot_xreal/`, so you need no Rust, cargo-ndk, or clang.
2. Enable the plugin: Project → Project Settings → Plugins → "Godot XREAL".
3. Vendor the XREAL runtime libraries; the `XREAL Import` dock does it in one click. See
   [Prerequisite](#prerequisite-vendor-the-xreal-runtime-libraries) below. They stay under XREAL's
   terms, so this repo never bundles them.

Build from source only to modify the extension: see [Build (from source)](#build-from-source).

## Prerequisite: vendor the XREAL runtime libraries

This repo excludes the XREAL native libraries, which remain under XREAL's terms. Obtain them from the
XREAL SDK for Unity: the `com.xreal.xr` package, shipped as a tgz (`com.xreal.xr.tar.gz`). Version
3.1.0 is the verified one. Then stage its libraries in one of three ways, all of which place the same
files in the same git-ignored destinations (see the tables below):

1. **Editor dock (recommended).** Enable the addon (Project → Project Settings → Plugins → "Godot
   XREAL"), open the `XREAL Import` dock (left panel), click *Select package…*, and pick
   `com.xreal.xr(.tgz|.tar.gz)` (or an already-extracted `package/` folder). It extracts through the
   system `tar`, copies everything into place, and rescans, so you need no terminal.
2. **Script.** From a terminal:
   ```powershell
   pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <…>/com.xreal.xr.tar.gz   # or an extracted …/package dir
   ```
   (`./scripts/vendor_xreal_libs.sh <…>` on macOS / Linux.)
3. **Manual copy.** Extract the tgz yourself and copy the files in the tables below into their
   destinations under the repo.

Vendoring handles only XREAL's proprietary libs; the addon's own `libgodot_xreal.so` still comes from
the `cargo ndk` build or a prebuilt release. What lands where:

The four `.so` files go to `addons/godot_xreal/jniLibs/arm64-v8a/`. `godot_xreal.gdextension` packs them next to the
GDExtension through its `[dependencies]` block, and the app `dlopen`s them at startup. The first three
come from `Runtime/Plugins/Android/arm64-v8a/`:

| `.so` | Role |
|---|---|
| `libXREALNativeSessionManager.so` | session and head-pose C ABI |
| `libXREALXRPlugin.so` | XR-plugin compositor and display C ABI |
| `libVulkanSupport.so` | support lib the two above need |
| `libmedia_codec.so` | FPV H.264 encoder (from `Runtime/Scripts/…/Camera Features/…/arm64/`) |

The seven `.aar` files go to `addons/godot_xreal/android/`. The addon's export plugin
(`export_plugin.gd`) ships them into the APK, carrying the Java/JNI layer and the manifest entries the
glasses need. They also hold the NR native libs (`jni/arm64-v8a/*.so`), which Gradle merges into the
APK, so vendoring leaves those inside the aar. All copied from `Runtime/Plugins/Android/`:

| `.aar` | Role | Native libs delivered into the APK |
|---|---|---|
| `nr_loader.aar` | NR loader Java layer | `libnr_loader.so` |
| `nr_api.aar` | NR API Java layer | `libnr_api.so`, `libnr_plugin_6dof.so`, `libnr_rgb_camera.so` |
| `nr_common.aar` | NR common layer | `libnr_libusb.so` (plus QNN/SNPE libs) |
| `nr_spatial_anchor.aar` | spatial-anchor backend | `libnr_spatial_anchor.so` |
| `nr_image_tracking.aar` | image-tracking backend | `libnr_image_tracking.so` |
| `GlassesDisplayPlugEvent-2.4.2.aar` | glasses-detection `GlassesInitProvider` | none |
| `Log-Control-1.2.aar` | `LogControl`, referenced by the entry above. Required: without it the app crashes before Godot starts | none |

The XrealBridge Java sources need neither vendoring nor pre-compilation. The addon's export plugin
stages the committed sources (`addons/godot_xreal/android/src/`) into the gradle build template, and
the export's Gradle run compiles them.

**Never copy `nractivitylife*.aar`**: its launcher is Unity-only and breaks a Godot app. (This
extension ignores the QNN/SNPE libs inside `nr_common.aar`, but they ride into the APK with the aar.)

## Build (from source)

You need this only to modify the extension; most users install a prebuilt release (above). The
GDExtension is plain godot-rust: vendor the XREAL libraries first (above), then build. The full
command reference (desktop iteration, manual `cargo ndk` and Gradle steps, signing) is in the
developer docs, indexed at [`docs/develop/README.md`](docs/develop/README.md).

To open the project in a desktop editor without a missing-library error, build the do-nothing desktop
stubs once after cloning: `pwsh scripts/build_dummy_libs.ps1` (or `./scripts/build_dummy_libs.sh`). It
needs only clang and lld, and it cross-compiles every desktop target from any host. The extension runs
on Android alone, but Godot has no way to say so, and the `.gdextension` therefore points desktop
platforms at these stubs ([`dummy/gdext_dummy.c`](dummy/gdext_dummy.c)). They register nothing, and
they stay out of the repo.

### Build & install

With the toolchain on `PATH` (Rust `aarch64-linux-android` target, `cargo-ndk`, `ANDROID_NDK_HOME`, a
Godot 4.7-stable binary, `adb`), `scripts/build.sh` (or `scripts/build.ps1`) wraps the four Android
stages: cargo-ndk build, Godot APK export, `adb install`, launch. It re-checks the prerequisite above
first, both the four `.so` and the addon's `.aar`/`.jar`, and prints the same guide if anything is
missing.

```bash
./scripts/build.sh --all      # build + export + install + run on the glasses
```

## Usage

1. Install the addon, either a [prebuilt release](#install-prebuilt) or one
   [built from source](#build-from-source), and vendor the libraries.
2. Set the renderer to **Compatibility**. The glasses path hands its eye textures to the XREAL
   compositor as GL texture names, which only that renderer's context can supply. Under Forward+
   or Mobile the session, head tracking and the phone display still come up, so the only symptom
   is glasses that stay black.
3. Build the scene the way you would for any Godot XR target: an `XROrigin3D` with an
   `XRCamera3D` and controllers, named however you like. Your scene owns it. Add
   `addons/godot_xreal/features/xreal_xr_runtime.tscn` under it and it attaches to what it finds,
   with no application code. Starting from nothing, instance `addons/godot_xreal/xr_origin.tscn`
   instead: the same hierarchy with that component already in it.
4. On a PC, add `addons/godot_xreal/xreal_desktop_preview.tscn` too. A desktop run has no eye
   viewports, so it draws the 3D world into a second window you fly with right-drag and WASD, and
   it frees itself on device. The
   [addon README](addons/godot_xreal/README.md#previewing-the-glasses-view-on-desktop) lists the
   rest of the controls.

The bundled `demo/main.tscn` does exactly this with a ring of boxes and an on-screen controller.
Its shared input router exposes XR inputs through the controller nodes and maps the canonical
buttons to `xr_select`, `xr_grab`, and `xr_menu` InputMap actions.

```
XROrigin3D                     # yours
├── XRCamera3D
├── LeftController  (XRController3D, tracker = left_hand)
├── RightController (XRController3D, tracker = right_hand)
└── XrealXRRuntime             # the XREAL bootstrap, attached to the above
```

### Runtime classes (registered by the GDExtension)

The highlights are below. The [full class reference](docs/user/api/README.md) covers every class, method,
signal, property, and constant, including the GDScript feature components; it is generated from the
doc comments and lives in [`docs/user/api/`](docs/user/api/README.md).

| Class | Member | Description |
|---|---|---|
| `XrealHeadTracker` (Node3D) | `is_tracking() -> bool` | A native pose was applied on the last frame. |
| | `recenter()` | Reset the forward direction (`RecenterGlasses`). |
| | `debug_pose_text() -> String` | Raw pose readout for on-screen debugging. |
| | signal `display_started()` | Glasses display and head tracking first went live. |
| | signal `glasses_connected()` / `glasses_disconnected()` | USB hot-plug events. |
| | signal `key_event(key, action)` | Physical key click, double, or long press (`KEY_*`, `ACTION_*` constants). |
| | signal `key_state_changed(key, state)` | Raw key down and up (`KEY_STATE_*` constants). |
| | signal `wearing_changed(wearing)` | Proximity (wear) sensor put-on and take-off. |
| | signal `brightness_changed(level)` / `volume_changed(level)` / `ec_level_changed(level)` | Glasses-side state changes. |
| | signal `glasses_event(action_type, para, para2, para3)` | Catch-all for every raw glasses hardware event. |
| `XrealSystem` (RefCounted) | `is_available() -> bool` | Native libraries loaded (false on desktop). |
| | `is_session_started() -> bool` | A native session is running. |
| | `get_plugin_version() -> String` | XREAL plugin version. |
| | `get_device_type() -> int` | `XREALDeviceType` enum value. |
| | `get_tracking_state() / get_tracking_reason() / get_tracking_type() -> int` | XR-plugin tracking enums (`-1` when unavailable). Also the SLAM-state notification source. |
| | `get_glasses_temperature_level() -> int` | Over-temperature poll: `0` normal, `1` warm, `2` hot (`-1` until first reported). |
| | `get_last_native_error_code() -> int` / `get_last_native_error_message() -> String` | Latest native error (`XREALErrorCode`; `-1` and `""` until one fires). |
| | `switch_tracking_type(type) -> bool` | Switch tracking mode (`TRACKING_6DOF/3DOF/0DOF/0DOF_STAB` constants). |
| | `set_display_bypass_psensor(bypass) -> int` | Keep the display on while the glasses are off the head (SDK status). |
| | `get_hmd_time_nanos() -> int` | Native HMD clock (ns, `0` when down). |
| | `get_present_fps() / get_dropped_frame_count() / get_early_frame_count() / …` | Live compositor render metrics (`NRMetrics*`). |
| | `get_diagnostics() -> String` | One-line perception-pipeline diagnostic. |
| `XrealHandTracker` (Node) | (registers trackers) | Publishes XREAL hand tracking to `XRServer` as two `XRHandTracker`s (`/user/hand_tracker/{left,right}`), updated each frame. Add it to the scene; drive a hand skeleton with `XRHandModifier3D` or read the trackers directly. **Air 2 Ultra only.** |

## Feature-specific setup

Most features work as soon as you drop their sub-scene in (see `addons/godot_xreal/features/`). Two
need an extra step, and both use XREAL's own tools; for background, see XREAL's
[developer documentation](https://docs.xreal.com/).

### Image tracking: build the reference database

Image tracking loads a compiled reference-image **database blob** (`.bin`) at runtime. Build it from
your own images with the vendored `trackableImageTools` CLI, the Godot analog of Unity's
`XREALImageLibraryBuildProcessor`. The CLI comes from the SDK package's `Tools~/`, runs on a Windows or
macOS host only, and the [vendoring step](#prerequisite-vendor-the-xreal-runtime-libraries) places it
in `addons/godot_xreal/tools/`.

The "XREAL Image DB" editor dock is the recommended route (left panel, present once the addon is
enabled):

1. Keep or pick a manifest (default `res://demo/image_tracking/reference.json`). Each manifest holds one
   or more **sets**; each set compiles to one tracking database the runtime activates and cycles through.
2. Press **Add image** for every reference image: choose the file and enter its physical printed width
   in metres (the dock generates a GUID). The SDK caps a set at 5 images. The dock rejects images with
   too few features or high self-similarity rather than warning about them, which keeps a crash-prone
   database from ever being built.
3. **Build blob** runs the tool and writes the `.bin` next to the manifest.

Or from a terminal: `pwsh scripts/build_image_db.ps1` (defaults to `demo/image_tracking/reference.json`).

Nothing under `demo/image_tracking/` is committed. The manifest, the reference images and the built
`.bin` are all git-ignored, because each project supplies its own; the dock writes the manifest there
on the first **Add image**. At runtime, point the `xreal_image_tracking` feature's `manifest_path` at
the manifest, and it registers every set through `XrealSystem.init_image_database`.

### FPV streaming: the receiver app

The demo's **Stream** button streams the first-person view as H.264 over RTP: the AR scene, or the
camera and AR blend when the RGB camera is on. Microphone audio, app audio, or both ride along,
depending on the component's `audio_state` (see the capture-audio row in
[Supported features](#supported-features)).

Two receivers exist: XREAL's own desktop app, and the scripts in this repo.

Either way, start the receiver on a PC on the same LAN *before* pressing Stream. You never type an
address: the app broadcasts, whichever receiver is listening answers, and streaming begins. The order
matters, because a receiver started afterwards has already missed the handshake.

#### 1. XREAL's official StreamingReceiver

The desktop app from XREAL's [First Person View](https://docs.xreal.com/Tools/First%20Person%20View)
page. Run it, press Stream, done: this port pairs with it exactly as the Unity SDK does.

#### 2. This repo's own receivers (`scripts/stream_server/`)

Open source, no vendor software. Both wire formats turn out to be ordinary standards (video RFC 6184
H.264, audio RFC 3016 LATM carrying AAC-LC 16 kHz mono), so decoding them takes nothing proprietary.
Two front ends:

**Watch it in a browser** with [`fpv_server.py`](scripts/stream_server/fpv_server.py):

```bash
python scripts/stream_server/fpv_server.py       # then open http://localhost:8080
```

Python 3 and nothing else: no `pip install`, no ffmpeg. The server never decodes. It turns RTP into
FLV over a WebSocket, and the browser's own H.264/AAC decoders play it. Viewers may join and leave at
any time.

**Watch or record with ffplay/ffmpeg**, which additionally needs ffmpeg on `PATH`:

```powershell
pwsh scripts/stream_server/receive.ps1           # live ffplay window
pwsh scripts/stream_server/receive.ps1 -Record   # record to an .mkv in that folder
```

(`scripts/stream_server/receive.sh [--record]` on macOS / Linux.)

Options, the discovery protocol, and why a silent audio track in a quiet room is expected rather than a
fault: [`scripts/stream_server/README.md`](scripts/stream_server/README.md).

Streaming uses our own render target rather than the camera, so it needs no RGB camera and works on the
camera-less Air 2 Ultra.

#### Note: what the microphone does and does not pick up

The encoder opens the mic as `AUDIO_SOURCE_VOICE_COMMUNICATION` with an Acoustic Echo Canceler and
Noise Suppression attached, visible under `RecordActivityMonitor` in `adb shell dumpsys audio`.
That is a telephony front-end, not a plain recorder, and it changes what you get in ways worth knowing
before you conclude something is broken. Every measurement below came from the device:

- A quiet room records as exact digital silence rather than a noise floor: every sample identical,
  `mean == max == -91 dB`. Make some noise before deciding the audio path is dead.
- Steady tones get suppressed. A continuous 1 kHz tone spiked the level 60 dB at onset, then sank back
  down, which is the noise suppressor doing its job on a stationary signal. Test with speech or music:
  a test tone is close to the worst possible probe for this path.
- It will not howl. Set the glasses next to a speaker playing the stream and nothing feeds back: the
  echo canceler treats the returning app audio as echo, and the suppressor flattens whatever steady
  tone a loop would build.
- With both sources on, app audio dominates: BGM measured around -22 dBFS against a mic contribution
  below -38 dBFS, so the mic is easy to mistake for absent.

Historical note: earlier versions fed app audio in from Godot's mixer through `AudioEffectCapture` and
`HWEncoderNotifyAudioData`, and this README claimed app audio was impossible because of an engine
limitation. That was wrong on both counts. `HWEncoderNotifyAudioData` feeds the *microphone* pipeline
rather than an app-audio one, so enabling it alongside the native mic produced two rival producers on
one track: an audio track 1.79× the video's length, 35 % of it silence. The SDK intends the
MediaProjection path above.

## Layout

```
godot_xreal.gdextension  GDExtension manifest (Android .so + desktop stubs + dlopen deps)
addons/godot_xreal/      the installable addon
  plugin.cfg/.gd         EditorPlugin — project settings + editor docks
  export_plugin.gd       Android export: manifest, permissions, .aar/assets staging
  xr_origin.tscn         shared standard Godot XR node hierarchy
  xreal_rig.tscn         legacy XrealHeadTracker + Camera3D rig
  xreal_desktop_preview.tscn/.gd   desktop preview window (frees itself on device)
  xreal_android_bridge.gd   bootstrap for the XrealBridge Java helper (PiP, activity access)
  features/              drop-in components: XR runtime bootstrap, camera, planes, anchors,
                         image tracking, depth mesh, hands, focus plane, captures, stream, recorder
  shaders/               YCbCr and camera+AR blend shaders the components share
  editor/                docks: vendor_import_dock.gd (SDK import), image_db_dock.gd,
                         mesh_snapshot_dock.gd (depth-mesh scan -> ArrayMesh/.glb)
  android/               bridge Java source (nr_plugins.json + .aar vendored, git-ignored)
  tools/                 vendored trackableImageTools CLI (git-ignored)
  bin/                   built libs (git-ignored): android/libgodot_xreal.so + desktop dummy stubs
src/                     the Rust GDExtension
  lib.rs                 ExtensionLibrary entry
  ffi.rs / native.rs     RE'd ABI (repr(C) structs) + dlopen/dlsym of the XREAL .so
  session.rs/jni_bridge.rs  session lifecycle + Android Activity acquisition
  signal_guard.rs        null-NativeGlasses teardown crash workaround
  node.rs                XrealHeadTracker (Node3D)
  system.rs              XrealSystem (RefCounted) + XrealAR (Node — AR-change signals)
  camera_feed.rs         XrealCameraFeed (CameraFeed) — RGB camera
  hand_tracking.rs       XrealHandTracker (Node) → XRHandTracker
  xr_interface.rs        XrealXrInterface — standard XR pose path + opt-in Vulkan multiview
  depth_mesh.rs · metrics.rs · video_encoder.rs · controller_probe.rs
                         AR mesh · render metrics · FPV H.264 streaming · phone-IMU pointer
  gl.rs / unity_plugin.rs   GLES + Unity native-plugin emulation (display path)
  vk_bridge.rs / egl_context.rs   Vulkan glasses bridge (opaque-fd VkImage → GL texture)
                         + its private EGL context; ahb_probe.rs is the stage-0 share probe
  glasses_events.rs / native_error.rs   cached event funnels
  doc_gen.rs / api_docs.rs   doc generators (F1 stubs + docs/user/api), run as host cargo tests
demo/                    AR demo (main.tscn + managers: hand/anchor/image/mesh/stream/
                         capture/blend + phone touch controller)
dummy/                   desktop GDExtension stub source (gdext_dummy.c + generated
                         stub_*.inc: class list, members, F1 docs) — built into addons/godot_xreal/bin/
addons/godot_xreal/jniLibs/  vendored XREAL core .so (git-ignored)
scripts/                 build + vendor_xreal_libs + build_dummy_libs + build_image_db
                         + gen_stub_classes / gen_docs / gen_api_docs (.ps1/.sh twins)
  stream_server/         FPV receivers: fpv_server.py (browser) + receive.ps1/.sh (ffplay/record)
.github/workflows/       CI (fmt/clippy/test/build) + Release (prebuilt addon)
docs/                    user/ (the generated class reference api/ and its index) and develop/
                         (developer docs: guides / reference / plans / archive; each has a README)
```

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
