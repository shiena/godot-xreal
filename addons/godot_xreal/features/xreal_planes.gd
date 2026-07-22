extends Node3D
## Plane detection as a drop-in feature component: enables the SDK's plane detection and overlays
## a thin, semi-transparent box on each detected plane's bounds. On the see-through display the
## translucent fill reads as a tint on the real surface.
##
## World-locked: add this component under a world-fixed node (e.g. the scene root, NOT the head
## rig) so the boxes sit on the real surface as the head moves. Plane changes stream in through
## the shared XrealAR poller (XrealShared.get_ar) — its "planes" switch is gated on this toggle.

## Emitted when an operation fails or the feature is unavailable, so the load site can react
## (show UI, log, flip a toggle). Carries the same human-readable text also pushed as a warning.
signal error(message: String)


const PLANE_NONE := 0
const PLANE_BOTH := 3   # horizontal | vertical
const TRACKING_6DOF := 0
const PLANE_BOX_THICKNESS := 0.01  # metres; a slab, not a cuboid

## Enable at boot (applied in _ready). At runtime call set_enabled().
@export var enabled := false
## Plane detection needs a live 6DoF session — turning it on switches tracking to 6DoF.
@export var switch_to_6dof := true

var _system: Object          # XrealSystem (this feature's own stateless instance)
var _ar: Object              # the shared XrealAR poller
var _connected := false
var _enabled := false
var _boxes := {}             # plane id(String) -> MeshInstance3D
var _mat: StandardMaterial3D

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert
	if enabled:
		enabled = set_enabled(true)

## Toggle plane detection. Returns the resulting state (false when the plane ABI is unavailable on
## this device, so a UI toggle can flip itself back off). Planes then stream in via the shared
## XrealAR (which may take a moment / need a live 6DoF session).
func set_enabled(on: bool) -> bool:
	# OFF tears down unconditionally, ahead of the _system guard: losing the system handle must never
	# be able to strand the plane boxes in the scene. The scene-side teardown also runs BEFORE the
	# native call, so an SDK call that errors out cannot leave the boxes behind either.
	if not on:
		_enabled = false
		_clear_boxes()
		if _ar:
			_ar.set(&"planes", false)  # stop the shared XrealAR polling the plane stream
		if _system and _system.has_method(&"set_plane_detection_mode"):
			_system.set_plane_detection_mode(PLANE_NONE)
		enabled = false
		return false
	if _system == null:
		enabled = false
		return false
	# Gate on the ABI being resolved (not on set_plane_detection_mode's return — the SDK discards
	# that value, and it reads false even when the mode takes; XREALPlaneSubsystem.cs). Enable
	# optimistically and let the plane stream surface whatever the SDK detects.
	if _system.has_method(&"is_plane_detection_available") and not _system.is_plane_detection_available():
		_fail("[xreal-planes] plane detection ABI unavailable on this device")
		enabled = false
		return false
	_ensure_ar()
	if switch_to_6dof and _system.has_method(&"switch_tracking_type"):
		_system.switch_tracking_type(TRACKING_6DOF)
	if _system.has_method(&"set_plane_detection_mode"):
		_system.set_plane_detection_mode(PLANE_BOTH)
	_enabled = true
	if _ar:
		_ar.set(&"planes", true)
	enabled = true
	return true

## Resolve the shared XrealAR and connect its plane signals once — BEFORE the stream switch goes
## on, so no change event is ever polled without a listener.
func _ensure_ar() -> void:
	if _connected:
		return
	_ar = XrealShared.get_ar(get_tree())
	if _ar == null:
		return
	_ar.connect(&"plane_added", _on_plane_changed)
	_ar.connect(&"plane_updated", _on_plane_changed)
	_ar.connect(&"plane_removed", _on_plane_removed)
	_connected = true

## XrealAR signal (plane_added / plane_updated): overlay/refresh the plane's box. Logs the running
## live-plane count on first sight of a plane, for on-device verification.
func _on_plane_changed(plane: Dictionary) -> void:
	if not _enabled:
		return
	var id: String = plane.get("id", "")
	var is_new := not _boxes.has(id)
	_update_box(plane)
	if is_new:
		print("[xreal-planes] plane added %s (total %d)" % [id, _boxes.size()])

## XrealAR signal (plane_removed): drop the plane's box.
func _on_plane_removed(id: String) -> void:
	if not _enabled:
		return
	_remove_box(id)
	print("[xreal-planes] plane removed %s (total %d)" % [id, _boxes.size()])

## Create/update the translucent box overlaying one plane's bounds. The plane's `size` is its full
## width/height in the plane-local X/Z; `center` offsets the bounds from the pose in that same local
## frame. Coordinate convention (local X/Z, Y-up normal) is AR-Foundation-standard.
func _update_box(plane: Dictionary) -> void:
	var id: String = plane.get("id", "")
	if id.is_empty():
		return
	var mi: MeshInstance3D = _boxes.get(id)
	if mi == null:
		mi = MeshInstance3D.new()
		mi.mesh = BoxMesh.new()
		mi.material_override = _material()
		add_child(mi)
		_boxes[id] = mi
	var sz: Vector2 = plane.get("size", Vector2.ZERO)
	(mi.mesh as BoxMesh).size = Vector3(sz.x, PLANE_BOX_THICKNESS, sz.y)
	var center: Vector2 = plane.get("center", Vector2.ZERO)
	var t: Transform3D = plane.get("transform", Transform3D.IDENTITY)
	mi.transform = t.translated_local(Vector3(center.x, 0.0, center.y))

func _remove_box(id: String) -> void:
	var mi: MeshInstance3D = _boxes.get(id)
	if mi:
		mi.queue_free()
		_boxes.erase(id)

func _clear_boxes() -> void:
	for id in _boxes:
		(_boxes[id] as MeshInstance3D).queue_free()
	_boxes.clear()

## Shared translucent unshaded material (double-sided) — visible from both faces.
func _material() -> StandardMaterial3D:
	if _mat == null:
		_mat = StandardMaterial3D.new()
		_mat.albedo_color = Color(0.25, 0.7, 1.0, 0.35)
		_mat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
		_mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
		_mat.cull_mode = BaseMaterial3D.CULL_DISABLED
	return _mat

func _exit_tree() -> void:
	# Stop detection + release the shared stream switch (the shared XrealAR outlives us).
	if _enabled:
		if _system and _system.has_method(&"set_plane_detection_mode"):
			_system.set_plane_detection_mode(PLANE_NONE)
		if _ar and is_instance_valid(_ar):
			_ar.set(&"planes", false)

## Push a warning AND emit `error` so the load site can detect the failure (not just see the log).
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
