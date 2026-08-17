class_name XrealShared
extends Object
## Static helpers shared by the godot_xreal feature components (addons/godot_xreal/features/*).
## Never instantiated.
##
## Some native resources are process-global singletons, so the feature scenes must coordinate:
##   - XrealAR polls native change queues that are CONSUMED on poll, so a second XrealAR polling
##     the same stream would steal events. get_ar() shares exactly one node across all features.
##   - XrealHandTracker registers the XRServer hand trackers, and one instance suffices.
##   - The XrealHeadTracker owns the compositor render driver. The standard XRCamera3D is the app's
##     tracking-space view; legacy rigs can still use the driver's mirrored Node3D transform.
##   - XrealSystem, by contrast, is a stateless facade over that global state, so every feature
##     creates its own instance freely (make_system).

const GROUP_AR := &"xreal_shared_ar"
const GROUP_HAND_TRACKER := &"xreal_shared_hand_tracker"
## xr_origin.tscn's XRCamera3D joins this group. Custom common XR rigs should do the same.
const GROUP_XR_CAMERA := &"xreal_shared_xr_camera"
## xreal_rig.tscn's root joins this group; add it to a custom rig's XrealHeadTracker too.
const GROUP_HEAD_TRACKER := &"xreal_head_tracker"
const GROUP_CAMERA := &"xreal_camera_feature"
## The desktop preview window's head node (xreal_desktop_preview.tscn) joins this group.
const GROUP_DESKTOP_PREVIEW := &"xreal_desktop_preview_head"

# Same-frame duplicate-creation guard: the group lookup only sees nodes already INSIDE the tree,
# and auto-created nodes enter it through call_deferred, so two features enabling in the same
# frame would both miss the group. These caches arbitrate between creation and tree entry.
static var _ar: Node = null
static var _hand_tracker: Node = null
# Process-global native subsystems accept one configuration at a time. Feature nodes claim their
# subsystem on first enable and keep it until they leave the tree, so a duplicate cannot overwrite
# or stop the active owner's state.
static var _feature_owners: Dictionary = {}

## Claim one process-global feature for `owner`. Repeated claims by the same owner are idempotent;
## a stale owner is discarded automatically. Returns false while another live node owns it.
static func claim_feature(feature: StringName, owner: Node) -> bool:
	var current: Node = _feature_owners.get(feature)
	if is_instance_valid(current):
		return current == owner
	_feature_owners[feature] = owner
	return true

## Whether `owner` currently holds a process-global feature.
static func is_feature_owner(feature: StringName, owner: Node) -> bool:
	return is_instance_valid(owner) and _feature_owners.get(feature) == owner

## Release one process-global feature. Only its current owner can release it.
static func release_feature(feature: StringName, owner: Node) -> bool:
	if not is_feature_owner(feature, owner):
		return false
	_feature_owners.erase(feature)
	return true

## Capture resolution presets, mirroring the SDK VideoCapture sample's Resolution Level. `High` is
## the RGB camera's own 1280x720, and going above that only upscales, so it tops the range.
## Returns `(width, height, bitrate)`; `CUSTOM` means the caller's own exported values.
enum ResolutionLevel { LOW, MIDDLE, HIGH, CUSTOM }

static func resolution_preset(level: int) -> Vector3i:
	match level:
		ResolutionLevel.LOW:
			return Vector3i(640, 360, 2_000_000)
		ResolutionLevel.MIDDLE:
			return Vector3i(960, 540, 4_000_000)
		_:
			return Vector3i(1280, 720, 8_000_000)

## What a capture viewport clears to behind the holograms. Shared by the stream and recorder
## components, which each expose it as their own `background` export; see either for the trade-offs.
enum CaptureBackground { TRANSPARENT, SCENE, SOLID }

## Point a capture viewport and its camera at one of the CaptureBackground kinds.
##
## TRANSPARENT keeps alpha, so a camera image can show through and the world's own background is
## dropped. The other two clear opaquely: SCENE lets the world draw whatever it puts behind the
## holograms, and SOLID overrides that with `color` through an Environment set on the capture camera
## alone, leaving the wearer's view untouched. The camera Environment is cleared for the other two,
## so a component can switch kinds without rebuilding its viewport.
static func apply_capture_background(
	viewport: SubViewport, camera: Camera3D, kind: CaptureBackground, color: Color
) -> void:
	viewport.transparent_bg = kind == CaptureBackground.TRANSPARENT
	if kind != CaptureBackground.SOLID:
		camera.environment = null
		return
	var env := Environment.new()
	env.background_mode = Environment.BG_COLOR
	env.background_color = color
	camera.environment = env

## Audio sources mixed into a recording or stream, mirroring the SDK VideoCapture sample's Audio
## State. The SDK captures both natively and mixes them in its encoder: `MIC` needs a config flag
## plus RECORD_AUDIO, `APP` a config flag plus an Android MediaProjection (see
## [method request_app_audio_consent]). Godot's own mixer takes part in neither.
enum AudioState { NONE, APP, MIC, APP_AND_MIC }

static func audio_wants_app(state: int) -> bool:
	return state == AudioState.APP or state == AudioState.APP_AND_MIC

## RECORD_AUDIO, which the SDK's native mic capture needs. True off-Android so editor runs behave.
static func is_mic_granted() -> bool:
	if not OS.has_feature("android"):
		return true
	return "android.permission.RECORD_AUDIO" in OS.get_granted_permissions()

static func audio_wants_mic(state: int) -> bool:
	return state == AudioState.MIC or state == AudioState.APP_AND_MIC

## The Java side of app-audio consent, or null off-device.
static func _projection_class() -> Object:
	if not is_native_runtime():
		return null
	return JavaClassWrapper.wrap("com.godot.game.XrealProjection")

## Ask the user for screen-capture consent, which is what app ("internal") audio needs.
##
## Android will only let an app capture playback audio through a MediaProjection, and the XREAL
## encoder builds its own AudioPlaybackCaptureConfiguration from the one we hand it, so without
## consent a recording carries the microphone alone. This returns immediately, and the system
## dialog is asynchronous, so poll [method is_app_audio_ready]. Calling it again while a dialog is
## up, or once consent is held, does nothing.
static func request_app_audio_consent() -> void:
	var projection := _projection_class()
	if projection == null:
		return
	var activity := XrealAndroidBridge.get_activity()
	if activity != null:
		projection.request(activity)

## Whether app audio can be captured right now. False until consent is granted, and again if the
## user revokes it from the status bar.
static func is_app_audio_ready() -> bool:
	var projection := _projection_class()
	return projection.isReady() if projection != null else false

## True only when the REAL native extension is live. The desktop editor loads a dummy stub that
## registers all the Xreal* classes for the F1 docs, so class presence alone proves nothing; gate
## on the platform too.
static func is_native_runtime() -> bool:
	return OS.get_name() == "Android" \
		and ClassDB.class_exists(&"XrealSystem")

## A fresh XrealSystem, a stateless facade over process-global native state, so each feature may
## own one. It returns null off device, which keeps the features inert on desktop.
static func make_system() -> Object:
	return ClassDB.instantiate(&"XrealSystem") if is_native_runtime() else null

## Find-or-create the ONE shared XrealAR poller. An auto-created node starts with all four AR
## stream switches (planes, anchors, images, mesh) off, since each feature turns its own stream on
## and off, and it is parented under the tree root so it survives scene changes. The add_child has
## to be deferred, because the root is busy while the initial scene is still entering the tree. A
## user-placed XrealAR is honoured instead once it joined GROUP_AR; note that its stream switches
## default to ON.
static func get_ar(tree: SceneTree) -> Node:
	var found := tree.get_first_node_in_group(GROUP_AR)
	if found:
		return found
	if is_instance_valid(_ar):
		return _ar  # created this frame, still on its way into the tree
	if not is_native_runtime() or not ClassDB.class_exists(&"XrealAR"):
		return null
	var ar: Node = ClassDB.instantiate(&"XrealAR")
	ar.name = "XrealARShared"
	for stream in [&"planes", &"anchors", &"images", &"mesh"]:
		ar.set(stream, false)
	ar.add_to_group(GROUP_AR)
	_ar = ar
	tree.root.add_child.call_deferred(ar)
	return ar

## Find-or-create the ONE shared XrealHandTracker, which registers the XRServer hand trackers
## /user/hand_tracker/left and /user/hand_tracker/right. Same sharing pattern as get_ar.
static func get_hand_tracker(tree: SceneTree) -> Node:
	var found := tree.get_first_node_in_group(GROUP_HAND_TRACKER)
	if found:
		return found
	if is_instance_valid(_hand_tracker):
		return _hand_tracker
	if not is_native_runtime() or not ClassDB.class_exists(&"XrealHandTracker"):
		return null
	var ht: Node = ClassDB.instantiate(&"XrealHandTracker")
	ht.name = "XrealHandTrackerShared"
	ht.add_to_group(GROUP_HAND_TRACKER)
	_hand_tracker = ht
	tree.root.add_child.call_deferred(ht)
	return ht

## The XrealHeadTracker (head rig), or null while it does not exist yet. It is never auto-created,
## because the app owns the rig's lifecycle and may spawn it late, so callers re-poll each frame or
## at each use.
static func find_head_tracker(tree: SceneTree) -> Node3D:
	return tree.get_first_node_in_group(GROUP_HEAD_TRACKER) as Node3D

## Read a project setting with feature overrides resolved. ProjectSettings.get_setting() skips
## `name.feature` entries and hands back the base value, which would quietly ignore a project that
## scopes a setting per build, which projects shared with another XR target commonly do.
## Every runtime read goes through here.
static func read_setting(name: String, default: Variant) -> Variant:
	if not ProjectSettings.has_setting(name):
		return default
	var value: Variant = ProjectSettings.get_setting_with_override(name)
	return default if value == null else value

## Whether the desktop preview window stands in for the glasses, which is any run off device. On
## device the glasses present the view and a second window would be redundant.
static func uses_desktop_preview() -> bool:
	return OS.get_name() != "Android"

## The effective XR head node used by shared scenes. XREAL desktop preview takes priority off-device
## so head-locked content follows its flycam. Otherwise the standard XRCamera3D is preferred because
## its global transform includes XROrigin3D movement, with legacy rigs as a final fallback.
static func find_tracking_head(tree: SceneTree) -> Node3D:
	# The XREAL desktop preview is the actual simulated head. The runtime XRCamera3D remains at
	# identity off-device, so choosing it first would make head-locked content ignore the flycam.
	if uses_desktop_preview():
		var preview := find_preview_head(tree)
		if preview != null:
			return preview
	var camera := tree.get_first_node_in_group(GROUP_XR_CAMERA) as XRCamera3D
	if camera != null:
		return camera
	var driver := find_head_tracker(tree)
	return driver if driver != null else find_preview_head(tree)

## The desktop preview window's head node, or null. It is the off-device stand-in for the head
## tracker, so head-locked content parents to whichever of the two exists: the tracker on device,
## this in the editor. Null on device, and in a scene with no xreal_desktop_preview.tscn.
static func find_preview_head(tree: SceneTree) -> Node3D:
	return tree.get_first_node_in_group(GROUP_DESKTOP_PREVIEW) as Node3D

## The XrealCamera feature component in the tree, or null. The group lookup is O(1), so calling it
## every frame is safe.
static func find_camera_feature(tree: SceneTree) -> Node:
	return tree.get_first_node_in_group(GROUP_CAMERA)

## The live XrealCameraFeed, or null while the camera is off or absent. Consumers poll this at the
## point of use, each frame or each capture, which makes feed sharing independent of scene-tree
## insertion order and of when the camera toggles.
static func find_camera_feed(tree: SceneTree) -> Object:
	var cam := find_camera_feature(tree)
	return cam.get_feed() if cam != null and cam.has_method(&"get_feed") else null

## The two displays' offsets from the head as [left, right], in Godot space. This is the device's
## own eye separation, read from the same geometry API as the RGB camera's pose, rather than a
## guessed IPD. It is what a stereo capture needs to give each eye its parallax.
##
## Static per device, so callers cache it. Both entries come back Vector3.ZERO when the geometry is
## unavailable, which collapses a stereo capture into two identical views instead of failing it.
static func eye_offsets(system: Object) -> Array:
	var out := [Vector3.ZERO, Vector3.ZERO]
	if system == null or not system.has_method(&"get_device_pose_from_head"):
		return out
	for eye in 2:  # XREALComponent 0 = left display, 1 = right display
		var pose: PackedFloat32Array = system.get_device_pose_from_head(eye)
		if pose.size() == 7:
			out[eye] = Vector3(pose[0], -pose[1], -pose[2])
	# Logged like the RGB camera geometry beside it: an all-zero pair means the geometry API answered
	# nothing, which is the difference between a real stereo capture and two identical views.
	print("[xreal] eye offsets L=%s R=%s (separation %.1f mm)"
		% [out[0], out[1], ((out[1] as Vector3) - (out[0] as Vector3)).length() * 1000.0])
	return out

## Drive a Camera3D from the glasses RGB camera's real geometry, so rendered holograms line up with
## the camera image. The intrinsics give the vertical FOV (from fy, KEEP_HEIGHT) and the
## pose-from-head gives the small forward offset, returned in Godot space through this port's
## Unity-to-Godot map (x,-y,-z). It is static per device, so callers apply it once and cache the
## returned offset. Returns Vector3.ZERO when unavailable.
static func apply_rgb_camera_geometry(system: Object, cam: Camera3D) -> Vector3:
	var offset := Vector3.ZERO
	if system == null or cam == null or not system.has_method(&"get_camera_intrinsics"):
		return offset
	var comp := 2  # XREALComponent.RGB_CAMERA
	var res: Vector2i = system.get_device_resolution(comp)
	var intr: PackedFloat32Array = system.get_camera_intrinsics(comp)  # [fx, fy, cx, cy]
	if res.y > 0 and intr.size() == 4 and intr[1] > 0.0:
		cam.fov = rad_to_deg(2.0 * atan((res.y * 0.5) / intr[1]))
		print("[xreal] RGB-matched AR FOV=%.1f deg" % cam.fov)
	var pose: PackedFloat32Array = system.get_device_pose_from_head(comp)  # [px,py,pz, qx,qy,qz,qw] Unity
	if pose.size() == 7:
		offset = Vector3(pose[0], -pose[1], -pose[2])  # ~cm parallax
	return offset
