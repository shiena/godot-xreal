class_name XrealShared
extends Object
## Static helpers shared by the godot_xreal feature components (addons/godot_xreal/features/*).
## Never instantiated.
##
## Some native resources are process-global singletons, so the feature scenes must coordinate:
##   - XrealAR polls native change queues that are CONSUMED on poll — a second XrealAR polling the
##     same stream would steal events. get_ar() shares exactly one node across all features.
##   - XrealHandTracker registers the XRServer hand trackers; one instance suffices.
##   - The XrealHeadTracker owns the stereo eye viewports + render driver: the app owns its
##     lifecycle (usually via addons/godot_xreal/xreal_rig.tscn), so it is only ever looked up.
##   - XrealSystem, by contrast, is a stateless facade over that global state — every feature
##     creates its own instance freely (make_system).

const GROUP_AR := &"xreal_shared_ar"
const GROUP_HAND_TRACKER := &"xreal_shared_hand_tracker"
## xreal_rig.tscn's root joins this group; add it to a custom rig's XrealHeadTracker too.
const GROUP_HEAD_TRACKER := &"xreal_head_tracker"
const GROUP_CAMERA := &"xreal_camera_feature"

# Same-frame duplicate-creation guard: the group lookup only sees nodes already INSIDE the tree,
# and auto-created nodes enter it via call_deferred — so two features enabling in the same frame
# would both miss the group. These caches are the arbiter between creation and tree entry.
static var _ar: Node = null
static var _hand_tracker: Node = null

## True only when the REAL native extension is live. The desktop editor loads a dummy stub that
## registers all the Xreal* classes (for the F1 docs), so class presence alone is not enough —
## gate on the platform too.
static func is_native_runtime() -> bool:
	return OS.get_name() == "Android" and ClassDB.class_exists(&"XrealSystem")

## A fresh XrealSystem — a stateless facade over process-global native state, so each feature may
## own one. Returns null off-device (keeps the features inert on desktop).
static func make_system() -> Object:
	return ClassDB.instantiate(&"XrealSystem") if is_native_runtime() else null

## Find-or-create the ONE shared XrealAR poller. An auto-created node has all four AR stream
## switches (planes/anchors/images/mesh) off — each feature turns its own stream on/off — and is
## parented under the tree root (it survives scene changes; add_child must be deferred because the
## root is busy while the initial scene is still entering the tree). A user-placed XrealAR is
## honoured instead when it joined GROUP_AR (note its stream switches default to ON).
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

## Find-or-create the ONE shared XrealHandTracker (registers the XRServer hand trackers
## /user/hand_tracker/left|right). Same sharing pattern as get_ar.
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

## The XrealHeadTracker (head rig), or null while it doesn't exist yet. Never auto-created — the
## app owns the rig's lifecycle and may spawn it late, so callers re-poll per frame / per use.
static func find_head_tracker(tree: SceneTree) -> Node3D:
	return tree.get_first_node_in_group(GROUP_HEAD_TRACKER) as Node3D

## The XrealCamera feature component in the tree, or null. O(1) group lookup — safe per frame.
static func find_camera_feature(tree: SceneTree) -> Node:
	return tree.get_first_node_in_group(GROUP_CAMERA)

## The live XrealCameraFeed, or null while the camera is off/absent. Consumers poll this at point
## of use (per frame / per capture), which makes feed sharing independent of scene-tree insertion
## order and of when the camera toggles.
static func find_camera_feed(tree: SceneTree) -> Object:
	var cam := find_camera_feature(tree)
	return cam.get_feed() if cam != null and cam.has_method(&"get_feed") else null

## Drive a Camera3D from the glasses RGB camera's real geometry so rendered holograms line up with
## the camera image: intrinsics -> vertical FOV (from fy, KEEP_HEIGHT), pose-from-head -> the small
## forward offset, returned in Godot space (this port's Unity->Godot map (x,-y,-z)). Static per
## device — callers apply once and cache the returned offset. Vector3.ZERO when unavailable.
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
