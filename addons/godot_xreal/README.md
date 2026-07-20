# Godot XREAL (addon)

Use XREAL glasses from Godot 4. Provides **6DoF head tracking** (rotation + position;
3DoF/0DoF selectable) through a native GDExtension (Rust / godot-rust), plus drop-in
**feature sub-scenes** for the rest of the SDK surface (camera, planes, anchors, image
tracking, meshing, hands, capture, streaming). See the repository root for build/RE details.

## Install

1. Copy `addons/godot_xreal/` into your project.
2. Provide the GDExtension binary + the vendored XREAL `.so` files (see the repo's
   `docs/guides/build-and-release.md`). For local dev the repo ships a `godot_xreal.gdextension`
   at the project root pointing at `res://target/...`.
3. Enable **Godot XREAL** in *Project > Project Settings > Plugins* (optional — the
   runtime classes load with the GDExtension regardless; the plugin adds editor docks
   and the Android export hooks).

## Runtime classes (GDExtension)

| Class | Base | Purpose |
|---|---|---|
| `XrealHeadTracker` | `Node3D` | Drives its transform (rotation + position) from the native head pose each frame. Parent a `Camera3D` under it. `is_tracking() -> bool`, `recenter()`. Emits hot-plug (`glasses_connected` / `glasses_disconnected`) and hardware-input signals (`key_event`, `key_state_changed`, `wearing_changed`, `brightness_changed`, `volume_changed`, `ec_level_changed`, `glasses_event`) with `KEY_*` / `ACTION_*` / `KEY_STATE_*` constants. **One per tree** (it owns the stereo eye viewports + render driver). |
| `XrealSystem` | `RefCounted` | SDK info + control: session/tracking state, tracking-type switching, AR-feature availability/config, controller, streaming, metrics, device/camera geometry. Stateless facade over process-global native state — create as many instances as you like. |
| `XrealAR` | `Node` | Per-frame poller for the plane/anchor/image/mesh change streams, re-emitted as signals. The native change queues are **consumed on poll — keep exactly one XrealAR in the tree** (the feature components share one automatically via `XrealShared.get_ar`). |
| `XrealHandTracker` | `Node` | Registers the XRServer hand trackers `/user/hand_tracker/left`/`right` (Air 2 Ultra). One per tree suffices (`XrealShared.get_hand_tracker`). |
| `XrealCameraFeed` | `CameraFeed` | The glasses RGB camera as a CameraServer feed (Y/CbCr ImageTextures). Only one capture can be active; prefer the `xreal_camera.tscn` feature component, which owns the lifecycle. |

## Quick start

Drop `addons/godot_xreal/xreal_rig.tscn` (an `XrealHeadTracker` with a `Camera3D`
child) into your scene, or build it in code:

```gdscript
var rig := preload("res://addons/godot_xreal/xreal_rig.tscn").instantiate()
add_child(rig)            # rig is the XrealHeadTracker; the camera looks around with the head

var sys := XrealSystem.new()
print(sys.is_available(), sys.get_plugin_version(), sys.get_device_type())
```

Then add only the feature sub-scenes you need (below). A complete example wiring every
feature to a phone touch-controller UI is in the repo's `demo/` scene.

## Feature sub-scenes (`features/*.tscn`)

Each feature is a self-contained scene: instance it (editor or code), call `set_enabled(true)`
(or tick `enabled` in the inspector), delete what you don't use. They find their shared
plumbing themselves — no wiring:

- A single shared `XrealAR` poller / `XrealHandTracker` are find-or-created under the tree
  root on first use (groups `xreal_shared_ar` / `xreal_shared_hand_tracker`).
- The head rig is looked up via the `xreal_head_tracker` group (`xreal_rig.tscn` already
  joins it; add the group to a custom rig).
- The live camera feed is discovered via the `xreal_camera_feature` group.

On desktop (editor / PC runs) every component is inert, so scenes stay runnable.

Every feature component emits an **`error(message: String)`** signal when an operation fails or the
feature is unavailable (missing/unbuilt blob, DB init failure, no RGB camera, save failed…), so the
load site can detect it — show UI, flip a toggle, log — instead of the failure being a buried
warning. `set_enabled(on) -> bool` still returns `false` for the unavailable case; `error` adds the
reason and covers runtime failures too. The demo connects them in `demo/main.gd` (`_on_feature_error`).

| Scene | World-locked¹ | API | Devices |
|---|---|---|---|
| `xreal_camera.tscn` | — | `set_enabled(on) -> bool`, `get_feed()`, `is_feed_live()`, signals `feed_changed(feed)` / `active_changed(active)`; export `enabled` (feed only — draw it yourself) | RGB camera = One Series |
| `xreal_planes.tscn` | ✔ | `set_enabled(on) -> bool`; exports `enabled`, `switch_to_6dof` (plane detection needs 6DoF) | 6DoF devices |
| `xreal_anchors.tscn` | ✔ | `set_enabled(on) -> bool`, `place_at_fingertip()` (pinch also places); exports `enabled`, `save_file` (Guid persistence) | Air 2 Ultra |
| `xreal_image_tracking.tscn` | ✔ | `set_enabled(on) -> bool`, `cycle_set()`; exports `enabled`, `manifest_path` (**required** — a reference.json, see `demo/image_tracking/`), `marker_material` (optional overlay override; a ShaderMaterial with a `tracking` bool uniform gets the per-marker state) | Air 2 Ultra |
| `xreal_mesh.tscn` | ✔ | `set_enabled(on) -> bool`; exports `enabled` | Air 2 Ultra |
| `xreal_hands.tscn` | ✔ | autonomous (spheres on the 26 joints/hand); hide via `visible` | Air 2 Ultra |
| `xreal_photo_capture.tscn` | — | `capture_photo() -> String` (JPG path) — needs the camera component enabled | One Series |
| `xreal_blend_capture.tscn` | — | `capture_blended() -> String` (camera+AR composite JPG) — needs camera + rig | One Series |
| `xreal_stream.tscn` | — | `set_enabled(on)` (async — watch `active_changed(active)`), streams FPV/MRC to XREAL's StreamingReceiver over LAN; exports `with_mic`, `observer_mode`, size/bitrate/fps | any (camera-less devices stream AR-only) |
| `xreal_video_recorder.tscn` | — | `set_enabled(on)` (watch `active_changed(active)`), records the FPV (camera+AR blend while the camera is on, AR alone otherwise) to an mp4 in the user data dir; `finished(path)` on stop; exports size/bitrate/fps. Shares the one HW encoder with `xreal_stream` — they refuse to run together | any (camera-less devices record AR-only) |

¹ World-locked components must sit under a **world-fixed** node (e.g. the scene root) — under
the head rig their content would appear head-locked (stuck to the screen).

`set_enabled(true)` returns `false` when the feature is unavailable on the device (missing
ABI / hardware) — wire that to your UI toggle. Camera start and stream pairing are async:
their real state comes back through `active_changed`.

### Sharing caveats

- **One `XrealAR` per tree.** If you place your own `XrealAR` node, add it to the
  `xreal_shared_ar` group so the features adopt it instead of creating a second one — and
  note its per-stream switches (`planes`/`anchors`/`images`/`mesh`) default to **on**; the
  feature components will take control of their own stream's switch anyway.
- **One camera.** The glasses have a single RGB camera; keep one `xreal_camera.tscn`
  instance (a second activation fails cleanly). The component only exposes the feed — drawing it
  is up to the app (the demo renders a head-locked preview in `demo/camera_preview.gd`, off the
  shared feed via `XrealShared.find_camera_feed()`).
- `XrealAndroidBridge.register()` (call once at startup) registers the Java bridge:
  companion-display handling + auto-enter PiP so the glasses keep rendering while the app
  is backgrounded (multi-resume).

### Project settings

With the plugin enabled, `xreal/tracking_type` appears in *Project > Project Settings*
(SDK default / 6DoF / 3DoF / 0DoF, applied at boot). It is read at runtime with the same
default, so a project without it saved behaves identically.

### Editor tooling

The plugin adds two editor docks: **XREAL Vendor** (imports SDK `.aar`/`.so` packages) and
**XREAL Image DB** (builds image-tracking reference databases). Their default paths point at
the repo's `demo/image_tracking/` — adjust in the docks for your own project layout.

## Platform

XREAL natives are Android arm64 only → target a Godot Android app on an XREAL host. On
desktop the classes load as documented stubs (F1 help works) but everything is inert, so
you can edit and run scenes on PC. Gate device-only code on `OS.get_name() == "Android"`
(class presence alone is not enough) — that's what `XrealShared.is_native_runtime()` does.
