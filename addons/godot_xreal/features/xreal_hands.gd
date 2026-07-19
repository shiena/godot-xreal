extends Node3D
## Hand-tracking visualization as a drop-in feature component: small spheres at each of the 26
## OpenXR joints per hand. Ensures the shared XrealHandTracker node exists (it registers the
## XRServer hand trackers), so dropping this scene in is all that's needed.
##
## World-locked: add this component under a world-fixed node (e.g. the scene root) — NOT under the
## head rig. The joint poses are in world/tracking space (fixed as the head moves): under the
## rotating rig the head rotation would cancel against the eye cameras and the hands would appear
## head-locked (stuck to the screen); under a fixed node they stay on the real hands.
##
## Hardware-gated to the Air 2 Ultra — on unsupported glasses the trackers simply report
## has_tracking_data = false and the spheres stay hidden.

const JOINT_COUNT := 26
# XRHandTracker.HandJoint fingertip ordinals — drawn a touch larger so the hand shape reads clearly.
const TIPS := [5, 10, 15, 20, 25]  # thumb/index/middle/ring/pinky tips

var _joints := {}  # tracker_name(String) -> Array[MeshInstance3D]

func _make_hand(color: Color) -> Array:
	var arr: Array = []
	for i in JOINT_COUNT:
		var mi := MeshInstance3D.new()
		var sphere := SphereMesh.new()
		var r := 0.011 if i in TIPS else 0.007
		sphere.radius = r
		sphere.height = r * 2.0
		sphere.radial_segments = 8
		sphere.rings = 4
		mi.mesh = sphere
		var mat := StandardMaterial3D.new()
		mat.albedo_color = color
		mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
		mi.material_override = mat
		mi.visible = false
		add_child(mi)
		arr.append(mi)
	return arr

func _ready() -> void:
	if XrealShared.is_native_runtime():
		XrealShared.get_hand_tracker(get_tree())  # the trackers register with XRServer in its ready
	_joints["/user/hand_tracker/left"] = _make_hand(Color(0.30, 0.70, 1.0))
	_joints["/user/hand_tracker/right"] = _make_hand(Color(1.0, 0.55, 0.30))

func _process(_delta: float) -> void:
	for tracker_name in _joints.keys():
		var arr: Array = _joints[tracker_name]
		var tracker := XRServer.get_tracker(tracker_name) as XRHandTracker
		if tracker == null or not tracker.get_has_tracking_data():
			for mi in arr:
				mi.visible = false
			continue
		for i in JOINT_COUNT:
			# Joint poses are world/tracking-space; under this fixed parent they stay on the real hand.
			(arr[i] as MeshInstance3D).transform = tracker.get_hand_joint_transform(i)
			(arr[i] as MeshInstance3D).visible = true
