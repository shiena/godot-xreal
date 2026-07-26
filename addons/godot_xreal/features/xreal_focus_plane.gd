extends Node3D
## The compositor's reprojection focus plane as a drop-in feature component (all glasses).
##
## Before every VSync the compositor warps the last rendered frame onto the newest head pose, and it
## does that against a single plane. Content sitting far from that plane is what smears, doubles and
## judders as the head moves. The SDK pins the plane at a fixed 1.4 m unless an app moves it, so an
## app whose content lives at arm's length, or across a room, pays for that mismatch every frame.
##
## Drop this in and the plane follows what the user is looking at: a ray goes forward from the head
## each frame, and the first collider it hits becomes the plane. This is the Godot equivalent of the
## SDK's FocusManager, and like it the ray only sees **physics colliders**, so purely visual meshes
## are invisible to it. Nothing hit means the plane falls back to [member default_distance].
##
## Place it anywhere; it reads the head's transform rather than its own, so it does not care about
## its parent. It is inert off device.
##
## **Pending on-device verification**: the plane is set in head-local space through this port's
## Unity mapping, which has not been checked against real reprojection yet. Turn it off if the
## glasses look worse rather than better with it on, and say so in an issue.

## Emitted when the feature cannot run, e.g. the native session never exposed SetFocusPlane.
signal error(message: String)

## Drive the focus plane every frame. Off leaves the compositor on its own 1.4 m default.
@export var enabled := true

## Raycast forward from the head to find the plane distance. Off pins the plane at
## [member default_distance] instead, which is what a fixed-distance UI app wants and what avoids
## the per-frame ray entirely.
@export var use_raycast := true

## Where the plane sits when the ray hits nothing, or when [member use_raycast] is off, in metres.
## The SDK's own fixed default is 1.4, which suits content around 1 to 3 m.
@export var default_distance := 1.4

## Ray length, in metres. Past this the fallback applies.
@export var max_distance := 100.0

## Physics layers the ray tests against. Narrow it when the scene has colliders that should not
## count as focus targets, such as an interaction volume around the user.
@export_flags_3d_physics var collision_mask := 0xFFFFFFFF

## Tilt the plane to match the surface it hit, rather than keeping it square to the gaze. The SDK
## sample offers the same switch and leaves it off: a tilted plane helps only when the content
## really lies along that surface, and hurts when the ray grazes something at an angle.
@export var use_hit_normal := false

## Plane normal for the square-to-gaze case. The Godot camera looks down -Z, so +Z faces the viewer.
const GAZE_NORMAL := Vector3(0, 0, 1)

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _reported := false              # the "export missing" error is said once, not every frame

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert

func _process(_delta: float) -> void:
	if not enabled or _system == null:
		return
	var head := _head()
	if head == null:
		return  # the rig may spawn late, so try again next frame
	var point := Vector3(0, 0, -default_distance)
	var normal := GAZE_NORMAL
	if use_raycast:
		var hit := _raycast(head)
		if not hit.is_empty():
			# Head-local, which is the space the compositor wants; the Rust side maps it to Unity's.
			point = head.global_transform.affine_inverse() * (hit["position"] as Vector3)
			if use_hit_normal:
				normal = head.global_transform.basis.inverse() * (hit["normal"] as Vector3)
	if not _system.set_focus_plane(point, normal) and not _reported:
		_reported = true
		error.emit("[xreal-focus] SetFocusPlane unavailable; the compositor keeps its 1.4 m default")

## First collider along the head's forward axis, or {} when the ray hits nothing.
func _raycast(head: Node3D) -> Dictionary:
	var space := get_world_3d().direct_space_state
	if space == null:
		return {}
	var origin := head.global_position
	var query := PhysicsRayQueryParameters3D.create(
		origin, origin - head.global_basis.z * max_distance, collision_mask)
	return space.intersect_ray(query)

## The head to cast from: the tracker on device, the desktop preview's stand-in off it.
func _head() -> Node3D:
	var head := XrealShared.find_head_tracker(get_tree())
	return head if head != null else XrealShared.find_preview_head(get_tree())
