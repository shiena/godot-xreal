extends Node3D
## Plane detection as a drop-in feature component: enables the SDK's plane detection and overlays
## each detected plane's boundary polygon. On the see-through display the translucent fill reads as
## a tint on the real surface.
##
## The overlay is the SDK's real boundary polygon (GetPlaneBoundaryVertexData, which
## XREALPlaneSubsystem.cs advertises as supportsBoundaryVertices and the Unity sample draws) — the
## actual detected outline, not an axis-aligned `center`/`size` bounds box. On-device comparison
## picked the polygon: the bounds box (always at least as big as the surface) floated off the real
## edges, while the polygon tracks the true shape.
##
## World-locked: add this component under a world-fixed node (e.g. the scene root, NOT the head
## rig) so the overlays sit on the real surface as the head moves. Plane changes stream in through
## the shared XrealAR poller (XrealShared.get_ar) — its "planes" switch is gated on this toggle.

## Emitted when an operation fails or the feature is unavailable, so the load site can react
## (show UI, log, flip a toggle). Carries the same human-readable text also pushed as a warning.
signal error(message: String)


const PLANE_NONE := 0
const PLANE_BOTH := 3   # horizontal | vertical
const TRACKING_6DOF := 0

## Enable the plane overlay at boot (applied in _ready). At runtime call set_enabled().
@export var enabled := false
## Plane detection needs a live 6DoF session — turning it on switches tracking to 6DoF.
@export var switch_to_6dof := true

var _system: Object          # XrealSystem (this feature's own stateless instance)
var _ar: Object              # the shared XrealAR poller
var _connected := false
var _on := false
var _planes := {}            # plane id(String) -> the last Dictionary seen (to rebuild on toggle)
var _meshes := {}            # plane id(String) -> MeshInstance3D
var _mat: StandardMaterial3D

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert
	if enabled:
		enabled = set_enabled(true)

## Toggle the plane overlay. Returns the resulting state (false when the plane ABI is unavailable on
## this device, so a UI toggle can flip itself back off). Planes then stream in via the shared
## XrealAR (which may take a moment / need a live 6DoF session).
func set_enabled(on: bool) -> bool:
	# Scene-side teardown always happens BEFORE the native/stream calls, so a native call that errors
	# out can never strand an overlay in the world.
	if not on:
		_on = false
		_clear()
		_sync()
		enabled = false
		return false
	if not _start_detection():
		enabled = false
		return false
	_on = true
	_sync()
	_rebuild()
	enabled = true
	return true

## Bring plane detection up (idempotent). False when the ABI is unavailable on this device.
func _start_detection() -> bool:
	if _system == null:
		return false
	# Gate on the ABI being resolved (not on set_plane_detection_mode's return — the SDK discards
	# that value, and it reads false even when the mode takes; XREALPlaneSubsystem.cs). Enable
	# optimistically and let the plane stream surface whatever the SDK detects.
	if _system.has_method(&"is_plane_detection_available") and not _system.is_plane_detection_available():
		_fail("[xreal-planes] plane detection ABI unavailable on this device")
		return false
	_ensure_ar()
	if switch_to_6dof and _system.has_method(&"switch_tracking_type"):
		_system.switch_tracking_type(TRACKING_6DOF)
	return true

## Push the on/off state to the SDK + the shared poller.
func _sync() -> void:
	if not _on:
		_planes.clear()
	if _ar:
		_ar.set(&"planes", _on)  # only let the shared XrealAR poll the plane stream while on
	if _system and _system.has_method(&"set_plane_detection_mode"):
		_system.set_plane_detection_mode(PLANE_BOTH if _on else PLANE_NONE)

## Redraw every plane seen so far — so switching the overlay on shows the current planes immediately
## instead of waiting for the SDK's next change event (an established plane may not update for a while).
func _rebuild() -> void:
	for id in _planes:
		_update_plane(_planes[id])

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

## XrealAR signal (plane_added / plane_updated): overlay/refresh the plane. Logs the running
## live-plane count on first sight of a plane, for on-device verification.
func _on_plane_changed(plane: Dictionary) -> void:
	if not _on:
		return
	var id: String = plane.get("id", "")
	if id.is_empty():
		return
	var is_new := not _planes.has(id)
	_planes[id] = plane
	_update_plane(plane)
	if is_new:
		print("[xreal-planes] plane added %s (total %d)" % [id, _planes.size()])

## XrealAR signal (plane_removed): drop the plane's overlay.
func _on_plane_removed(id: String) -> void:
	if not _on:
		return
	_planes.erase(id)
	_remove(id)
	print("[xreal-planes] plane removed %s (total %d)" % [id, _planes.size()])

## Create/update the boundary-polygon overlay — the plane's real detected outline, fetched per plane
## from XrealSystem.get_plane_boundary(). The points are plane-local (X/Z) relative to the pose. Our
## Godot space is Unity's through M = diag(1,-1,-1), so a plane-local (x, 0, z) becomes (x, 0, -z).
func _update_plane(plane: Dictionary) -> void:
	var id: String = plane.get("id", "")
	if id.is_empty() or _system == null or not _system.has_method(&"get_plane_boundary"):
		return
	var pts: PackedVector2Array = _system.get_plane_boundary(id)
	if pts.size() < 3:
		return
	var tris := Geometry2D.triangulate_polygon(pts)
	if tris.is_empty():
		return
	var verts := PackedVector3Array()
	verts.resize(pts.size())
	for i in pts.size():
		verts[i] = Vector3(pts[i].x, 0.0, -pts[i].y)
	var arrays := []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = verts
	arrays[Mesh.ARRAY_INDEX] = tris
	var am := ArrayMesh.new()
	am.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays)
	var mi: MeshInstance3D = _meshes.get(id)
	if mi == null:
		mi = MeshInstance3D.new()
		mi.material_override = _material()
		add_child(mi)
		_meshes[id] = mi
	mi.mesh = am
	mi.transform = plane.get("transform", Transform3D.IDENTITY)

func _remove(id: String) -> void:
	var mi: MeshInstance3D = _meshes.get(id)
	if mi:
		mi.queue_free()
		_meshes.erase(id)

func _clear() -> void:
	for id in _meshes:
		(_meshes[id] as MeshInstance3D).queue_free()
	_meshes.clear()

## Shared translucent unshaded material (double-sided) — visible from both faces, so the polygon
## winding doesn't matter.
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
	if _on:
		if _system and _system.has_method(&"set_plane_detection_mode"):
			_system.set_plane_detection_mode(PLANE_NONE)
		if _ar and is_instance_valid(_ar):
			_ar.set(&"planes", false)

## Push a warning AND emit `error` so the load site can detect the failure (not just see the log).
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
