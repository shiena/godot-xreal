# Godot XREAL (addon)

Use XREAL glasses from Godot 4. A native GDExtension (Rust, godot-rust) drives 6DoF head tracking,
rotation and position, with 3DoF and 0DoF selectable, and drop-in feature sub-scenes cover the rest
of the SDK surface: camera, planes, anchors, image tracking, meshing, hands, capture, and streaming.
See the repository root for build and RE details.

## Install

1. Copy `addons/godot_xreal/` into your project.
2. Provide the GDExtension binary and the vendored XREAL `.so` files (see the repo's
   `docs/guides/build-and-release.md`). For local dev the repo ships a `godot_xreal.gdextension`
   at the project root pointing at `res://target/...`.
3. Enable **Godot XREAL** in *Project > Project Settings > Plugins*. This step is optional: the
   runtime classes load with the GDExtension either way, and the plugin adds the editor docks and
   the Android export hooks.

## Runtime classes (GDExtension)

| Class | Base | Purpose |
|---|---|---|
| `XrealHeadTracker` | `Node3D` | Drives its transform, rotation and position, from the native head pose each frame. Parent a `Camera3D` under it. `is_tracking() -> bool`, `recenter()`. Emits hot-plug signals (`glasses_connected`, `glasses_disconnected`) and hardware-input signals (`key_event`, `key_state_changed`, `wearing_changed`, `brightness_changed`, `volume_changed`, `ec_level_changed`, `glasses_event`) with `KEY_*`, `ACTION_*`, and `KEY_STATE_*` constants. Keep **one per tree**: it owns the stereo eye viewports and the render driver. |
| `XrealSystem` | `RefCounted` | SDK info and control: session and tracking state, tracking-type switching, AR-feature availability and config, controller, streaming, metrics, device and camera geometry. A stateless facade over process-global native state, so create as many instances as you like. |
| `XrealAR` | `Node` | Per-frame poller for the plane, anchor, image, and mesh change streams, re-emitted as signals. Polling consumes the native change queues, so **keep exactly one XrealAR in the tree** (the feature components share one automatically through `XrealShared.get_ar`). |
| `XrealHandTracker` | `Node` | Registers the XRServer hand trackers `/user/hand_tracker/left` and `/user/hand_tracker/right` (Air 2 Ultra). One per tree suffices (`XrealShared.get_hand_tracker`). |
| `XrealCameraFeed` | `CameraFeed` | The glasses RGB camera as a CameraServer feed (Y/CbCr ImageTextures). Only one capture can run at a time, so prefer the `xreal_camera.tscn` feature component, which owns the lifecycle. |

## Quick start

Drop `addons/godot_xreal/xreal_rig.tscn` (an `XrealHeadTracker` with a `Camera3D`
child) into your scene, or build it in code:

```gdscript
var rig := preload("res://addons/godot_xreal/xreal_rig.tscn").instantiate()
add_child(rig)            # rig is the XrealHeadTracker; the camera looks around with the head

var sys := XrealSystem.new()
print(sys.is_available(), sys.get_plugin_version(), sys.get_device_type())
```

Then add only the feature sub-scenes you need (below). The repo's `demo/` scene wires every feature
to a phone touch-controller UI as a complete example.

## Feature sub-scenes (`features/*.tscn`)

Each feature is a self-contained scene: instance it from the editor or from code, call
`set_enabled(true)` (or tick `enabled` in the inspector), and delete what you don't use. They find
their shared plumbing themselves, with no wiring:

- A single shared `XrealAR` poller and `XrealHandTracker` are find-or-created under the tree
  root on first use (groups `xreal_shared_ar` and `xreal_shared_hand_tracker`).
- The head rig is looked up through the `xreal_head_tracker` group (`xreal_rig.tscn` already
  joins it; add the group to a custom rig).
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
| `xreal_camera.tscn` | no | `set_enabled(on) -> bool`, `get_feed()`, `is_feed_live()`, signals `feed_changed(feed)` and `active_changed(active)`; export `enabled` (the feed only, so draw it yourself) | RGB camera = One Series |
| `xreal_planes.tscn` | yes | `set_enabled(on) -> bool`; exports `enabled`, `switch_to_6dof` (plane detection needs 6DoF) | 6DoF devices |
| `xreal_anchors.tscn` | yes | `set_enabled(on) -> bool`, `place_at_fingertip()` (pinch also places); exports `enabled`, `save_file` (Guid persistence) | Air 2 Ultra |
| `xreal_image_tracking.tscn` | yes | `set_enabled(on) -> bool`, `cycle_set()`; exports `enabled`, `manifest_path` (**required**: a reference.json, see `demo/image_tracking/`), `marker_material` (optional overlay override; a ShaderMaterial with a `tracking` bool uniform gets the per-marker state) | Air 2 Ultra |
| `xreal_mesh.tscn` | yes | `set_enabled(on) -> bool`; exports `enabled` | Air 2 Ultra |
| `xreal_hands.tscn` | yes | autonomous (spheres on the 26 joints per hand); hide it through `visible` | Air 2 Ultra |
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

With the plugin enabled, `xreal/tracking_type` appears in *Project > Project Settings*
(SDK default / 6DoF / 3DoF / 0DoF, applied at boot). It is read at runtime with the same
default, so a project without it saved behaves identically.

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
shows a 3D mirror. A PC run has no eye viewports, so the setting is not applied there; the 3D half
otherwise has nowhere to go, and a full-screen phone UI hides whatever the root viewport drew.

Add `addons/godot_xreal/xreal_desktop_preview.tscn` to your scene to get it back. It opens a second
window onto the same 3D world, and it frees itself on device, so a shipped scene can keep it. Parent
head-locked content to its `head` node, the desktop stand-in for the `XrealHeadTracker`;
`XrealShared.find_preview_head(get_tree())` returns it, as `find_head_tracker` returns the real one.

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
