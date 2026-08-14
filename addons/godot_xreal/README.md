# Godot XREAL (addon)

Use XREAL glasses from Godot 4 through Godot's XR workflow: your `XROrigin3D`, `XRCamera3D`,
`XRController3D` and `XRHandTracker` nodes, driven by a native GDExtension (Rust, godot-rust). It
supplies 6DoF head tracking, rotation and position, with 3DoF and 0DoF selectable. Drop-in feature
sub-scenes add the rest: camera, planes, anchors, image tracking, meshing, capture, and streaming.
See the repository root for build and RE details.

## Install

1. Copy `addons/godot_xreal/` into your project.
2. Provide the GDExtension binary and the vendored XREAL `.so` files (see the repo's
   developer docs, indexed at `docs/develop/README.md`). For local dev the repo ships a `godot_xreal.gdextension`
   at the project root pointing at `res://target/...`.
3. Enable **Godot XREAL** in *Project > Project Settings > Plugins*. This step is optional: the
   runtime classes load with the GDExtension either way, and the plugin adds the editor docks and
   the Android export hooks.

## Runtime classes (GDExtension)

| Class | Base | Purpose |
|---|---|---|
| `XrealHeadTracker` | `Node3D` | XREAL backend driver. Publishes the native head pose through the primary `XrealXrInterface` for a standard `XRCamera3D`, while retaining its own transform for legacy child-camera rigs. `is_tracking() -> bool`, `recenter()`. Emits the existing hot-plug and device signals for backwards compatibility. Keep **one per tree**: it owns the compositor render driver. |
| `XrealSystem` | `RefCounted` | SDK info and control: session and tracking state, tracking-type switching, AR-feature availability and config, controller, streaming, metrics, device and camera geometry. A stateless facade over process-global native state, so create as many instances as you like. |
| `XrealAR` | `Node` | Per-frame poller for the plane, anchor, image, and mesh change streams, re-emitted as signals. Polling consumes the native change queues, so **keep exactly one XrealAR in the tree** (the feature components share one automatically through `XrealShared.get_ar`). |
| `XrealHandTracker` | `Node` | Registers the XRServer hand trackers `/user/hand_tracker/left` and `/user/hand_tracker/right` (Air 2 Ultra). One per tree suffices (`XrealShared.get_hand_tracker`). |
| `XrealCameraFeed` | `CameraFeed` | The glasses RGB camera as a CameraServer feed (Y/CbCr ImageTextures). Only one capture can run at a time, so prefer the `xreal_camera.tscn` feature component, which owns the lifecycle. |

## Quick start

Your scene owns the XR hierarchy. Add `addons/godot_xreal/features/xreal_xr_runtime.tscn` under an
`XROrigin3D` and it attaches to the `XRCamera3D` and `XRController3D` nodes it finds there, matching
controllers on their `tracker` rather than on node names, so an application may name and nest its own
however it likes. Set `xr_origin_path` to be explicit. Starting from nothing, instance
`addons/godot_xreal/xr_origin.tscn`: the same hierarchy with the component already in it.

No initialization script is required either way. The component starts the XREAL runtime, applies its
boot settings, installs the standard trackers, and publishes the camera it found to the group the
feature components look the head up in.

Instancing `xr_origin.tscn` makes its controllers part of an instanced scene, so hanging your own
nodes off them needs **Editable Children** on that instance, from its context menu in the scene
tree. Without it the editor accepts the node and then drops it on load, with no error anywhere: the
first sign is something that simply is not there at runtime. Building the hierarchy yourself avoids
the question entirely.

Other Godot XR addons work against these nodes, since they are the standard ones. A stock
godot-xr-tools `function_pointer` was checked on device: it found the controller through
`XRHelpers` and fired on `trigger_click` with nothing modified. Leave that addon's own **plugin**
disabled though, because enabling it writes `xr/openxr/enabled=true` into your project, which is the
one setting the glasses path needs left alone. Its scenes and scripts work without the plugin.

Controller pose and raw XR input come from the standard nodes. The runtime polls and fuses the native phone IMU, publishes its touchpad, maps glasses keys, and accepts app-owned phone
UI controls through `set_controller_button/axis/hand`. `xr_input_router.gd` maps `trigger_click` or
`primary_click`, `grip_click`, and `menu_button` to the `xr_select`, `xr_grab`, and `xr_menu`
InputMap actions. The
raw NRController button bitfield is not mapped because its current-device layout is not yet
verified. The old `xreal_rig.tscn` remains for existing projects.

The current app `Camera3D` remains the source for its transform, near/far clipping, render layers,
environment, camera attributes and offsets. Runtime changes are mirrored to both eye cameras;
XREAL's calibrated asymmetric projection and eye separation remain SDK-controlled.

Then add only the feature sub-scenes you need (below). The repo's `demo/` scene wires every feature
to a phone touch-controller UI as a complete example.

## Feature sub-scenes (`features/*.tscn`)

Each feature is a self-contained scene: instance it from the editor or from code, call
`set_enabled(true)` (or tick `enabled` in the inspector), and delete what you don't use. They find
their shared plumbing themselves, with no wiring:

- A single shared `XrealAR` poller and `XrealHandTracker` are find-or-created under the tree
  root on first use (groups `xreal_shared_ar` and `xreal_shared_hand_tracker`).
- The standard head is looked up through the `xreal_shared_xr_camera` group
  (`xr_origin.tscn` already joins it). Legacy `xreal_rig.tscn` projects fall back to the
  `xreal_head_tracker` group.
- The live camera feed is discovered through the `xreal_camera_feature` group.

On desktop (editor and PC runs) every component is inert, so scenes stay runnable.

Every feature component emits an `error(message: String)` signal when an operation fails or the
feature is unavailable: a missing or unbuilt blob, a DB init failure, no RGB camera, a failed save.
The load site can then show UI, flip a toggle, or log, instead of the failure staying buried in a
warning. `set_enabled(on) -> bool` still returns `false` for the unavailable case; `error` adds the
reason and covers runtime failures too. The demo connects them in `demo/main.gd`
(`_on_feature_error`).

| Scene | World-locked¹ | API | Devices |
|---|---|---|---|
| `xreal_xr_runtime.tscn` | under your `XROrigin3D` | autonomous XREAL start-up; attaches to the app's hierarchy (`xr_origin_path` to be explicit); methods `get_xreal_driver()`, `get_xreal_system()`, `recenter()` | all |
| `xreal_camera.tscn` | no | `set_enabled(on) -> bool`, `get_feed()`, `is_feed_live()`, signals `feed_changed(feed)` and `active_changed(active)`; export `enabled` (the feed only, so draw it yourself) | RGB camera = One Series |
| `xreal_planes.tscn` | yes | `set_enabled(on) -> bool`; exports `enabled`, `switch_to_6dof` (plane detection needs 6DoF) | 6DoF devices |
| `xreal_anchors.tscn` | yes | `set_enabled(on) -> bool`, `place_at_fingertip()` (pinch also places); exports `enabled`, `save_file` (Guid persistence) | Air 2 Ultra |
| `xreal_image_tracking.tscn` | yes | `set_enabled(on) -> bool`, `cycle_set()`; exports `enabled`, `manifest_path` (**required**: a reference.json, see `demo/image_tracking/`), `marker_material` (optional overlay override; a ShaderMaterial with a `tracking` bool uniform gets the per-marker state) | Air 2 Ultra |
| `xreal_mesh.tscn` | yes | `set_enabled(on) -> bool`; exports `enabled` | Air 2 Ultra |
| `xreal_hands.tscn` | yes | autonomous (spheres on the 26 joints per hand); hide it through `visible` | Air 2 Ultra |
| `xreal_hand_models.tscn` | under your `XROrigin3D` | autonomous; drives your own skinned hand models through `XRHandModifier3D`. Exports `left_model` and `right_model` (**required**: the addon ships no art, and the bones have to carry Godot's `Left<bone>`/`Right<bone>` names — see [docs/user](../../docs/user/README.md#hands)), plus `material_override` and `bone_update` | Air 2 Ultra |
| `xreal_photo_capture.tscn` | no | `capture_photo() -> String` returns a JPG path; needs the camera component enabled | One Series |
| `xreal_blend_capture.tscn` | no | `capture_blended() -> String` returns a camera+AR composite JPG; needs camera + rig | One Series |
| `xreal_stream.tscn` | no | `set_enabled(on)`, which is async: watch `active_changed(active)`. Streams FPV/MRC to XREAL's StreamingReceiver over LAN; exports `audio_state`, `observer_mode`, and the size, bitrate, and fps settings | any (camera-less devices stream AR-only) |
| `xreal_video_recorder.tscn` | no | `set_enabled(on)`, watching `active_changed(active)`. Records the FPV (the camera+AR blend while the camera is on, AR alone otherwise) to an mp4 in the user data dir; emits `finished(path)` on stop; exports `audio_state` and the size, bitrate, and fps settings. It shares the one HW encoder with `xreal_stream`, so the two refuse to run together | any (camera-less devices record AR-only) |

¹ World-locked components must sit under a world-fixed node, such as the scene root. Under the head
rig their content would appear head-locked, stuck to the screen.

`set_enabled(true)` returns `false` when the feature is unavailable on the device, whether from a
missing ABI or missing hardware; wire that to your UI toggle. Camera start and stream pairing are
async, so their real state comes back through `active_changed`.

### Sharing caveats

- **One `XrealAR` per tree.** If you place your own `XrealAR` node, add it to the
  `xreal_shared_ar` group so the features adopt it instead of creating a second one. Its
  per-stream switches (`planes`, `anchors`, `images`, `mesh`) default to on, and the feature
  components take control of their own stream's switch anyway.
- **One camera.** The glasses have a single RGB camera, so keep one `xreal_camera.tscn`
  instance; a second activation fails cleanly. The component exposes only the feed, and the app
  draws it: the demo renders a head-locked preview in `demo/camera_preview.gd` off the shared feed
  through `XrealShared.find_camera_feed()`.
- Call `XrealAndroidBridge.register()` once at startup to register the Java bridge. It handles the
  companion display and enables auto-enter PiP, so the glasses keep rendering while the app is
  backgrounded (multi-resume).

### Project settings

Start with the renderer. `renderer/rendering_method` has to be `gl_compatibility`, for the mobile
override as well: the glasses path hands its eye-viewport textures to the XREAL compositor as GL
texture names, and only that renderer gives Godot the context those names live in. Nothing errors
out under Forward+ or Mobile. The session starts, head tracking runs, the phone display draws, and
the glasses simply stay black, which makes it an expensive setting to get wrong. Vulkan reaches the
glasses only through the opt-in vk_bridge, behind the Android property
`debug.xreal.vulkan_glasses=1`.

This addon targets XREAL glasses only.

With the plugin enabled, `xreal/tracking_type` appears in *Project > Project Settings*
(SDK default / 6DoF / 3DoF / 0DoF, applied at boot). It is read at runtime with the same
default, so a project without it saved behaves identically.

`xreal/render_scale` sets the fixed per-eye 3D scale. It also becomes the quality ceiling when
`xreal/dynamic_render_scale` is enabled. Both settings are sampled when the stereo rig is created.
Dynamic scaling needs no min/max/target tuning: it uses 0.5 as the internal floor, calibrates the
target from the XREAL compositor when a reliable rate is available, lowers the scale after
sustained frame pressure and restores it only after longer stable headroom. An explicit
`debug.xreal.render_scale` Android property keeps the scale fixed for A/B measurements and disables
the controller.

`xreal/xr_multiview_poc` (default off) is experimental: on the Mobile (Vulkan) renderer it renders
both eyes in one scene pass through Godot's XR multiview instead of two viewports. Enabling it
requires two XR settings. Set `xr/shaders/enabled=true` in *Project Settings* (an advanced
setting), and set **XR Mode** to `OpenXR` in the Android export preset; without them the exporter
strips the XR shaders and 3D stops rendering. Leave `xr/openxr/enabled` at its `false` default for
this path: the preset flag only preserves the shaders, and the OpenXR runtime itself stays off.
The Compatibility renderer ignores the setting, logs a warning and keeps
the regular two-viewport Multipass path, which is fully supported there. The Android property `debug.xreal.xr_multiview` (0/1) overrides the setting for
same-APK A/B comparison. Dynamic render scale does not apply to this path; `xreal/render_scale` is
sampled once at startup.

### Editor tooling

The plugin adds two editor docks: **XREAL Vendor** (imports SDK `.aar`/`.so` packages) and
**XREAL Image DB** (builds image-tracking reference databases). Their default paths point at the
repo's `demo/image_tracking/`; adjust them in the docks for your own project layout.

## Platform

XREAL natives are Android arm64 only, so target a Godot Android app on an XREAL host. On desktop
the classes load as documented stubs (F1 help works) and everything else is inert, so you can edit
and run scenes on PC. Gate device-only code on `OS.get_name() == "Android"`, since class presence
alone is not enough; that is what `XrealShared.is_native_runtime()` does.

### Previewing the glasses view on desktop

On device the phone's root viewport draws your 2D UI and the extension's eye viewports draw the 3D
world. `xreal/disable_host_viewport_3d` defaults to `true`, preventing that shared world from also
being drawn as a hidden third pass on the phone. Turn it off only when the phone intentionally
shows a 3D mirror.

A PC run has no eye viewports, and `xreal_xr_runtime.tscn` marks its `XRCamera3D` current, so the
root viewport would draw the world behind a full-screen phone UI that hides it. The same setting
therefore applies on desktop as well, but only once the preview window below is in the scene, since
the 3D then has somewhere else to go. Without a preview the root viewport is the only view the app
has, so it keeps drawing. Only the drawing is switched off either way, never `current`.

Add `addons/godot_xreal/xreal_desktop_preview.tscn` to your scene to get it back. It opens a second
window onto the same 3D world, and it frees itself on device, so a shipped scene can keep it.
For the XREAL desktop backend, `XrealShared.find_tracking_head(get_tree())` returns this preview's
flycam `head` before the identity runtime camera. On device it prefers the standard `XRCamera3D`,
with the legacy `XrealHeadTracker` as a fallback. Use it for backend-neutral
head-locked content.

| Input | Action |
| --- | --- |
| Right-drag | Look around |
| WASD / QE | Move (Shift sprints) |
| R | Put the flycam back at the origin |
| Tab | Hand the window's mouse and keys to the app, and take them back |

Once the app holds the mouse and keys, the flycam stops reading them and every event goes to the
`app_input` signal instead, so you can drive something else with the same mouse.
`flycam_active_changed` reports each switch, and the window title names who has control. The demo
aims the phone pointer this way, since that pointer has no IMU to follow off device (see
`_setup_desktop_pointer` in `demo/main.gd`).
