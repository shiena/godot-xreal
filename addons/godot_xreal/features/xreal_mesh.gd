extends Node3D
## Depth meshing as a drop-in feature component (Air 2 Ultra). Enables meshing
## (XrealSystem.set_meshing_enabled) and builds/updates an ArrayMesh per block — a translucent
## overlay of the scanned environment. Mesh-block changes stream in through the shared XrealAR
## poller; its "mesh" stream is gated on this toggle so it only polls while meshing is on.
##
## World-locked: add this component under a world-fixed node (e.g. the scene root, NOT the head
## rig) so the mesh stays registered to the real room as the head moves. OFF drops every block mesh
## from the scene but leaves the SDK meshing, so ON repopulates from the next poll (GetMeshBlockInfo
## reports the whole current block set each time, not just what changed) without a rescan.

## Emitted when the feature is unavailable (e.g. meshing unsupported on this device), so the load
## site can react — show UI, log, flip a toggle.
signal error(message: String)

## Enable at boot (applied in _ready). At runtime call set_enabled().
@export var enabled := false

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar: Object                     # the shared XrealAR poller
var _connected := false
var _initialized := false           # meshing enabled once
var _enabled := false
var _meshes := {}                   # block id(String) -> MeshInstance3D
var _mat: StandardMaterial3D

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert
	if enabled:
		enabled = set_enabled(true)

## Toggle meshing. Returns the resulting state (false if unsupported — non-Air-2-Ultra — so a UI
## toggle can flip itself back off).
func set_enabled(on: bool) -> bool:
	# OFF tears down unconditionally, ahead of the capability probe: a probe that reads false (or a
	# missing _system) must never be able to strand the block meshes in the scene.
	if not on:
		_enabled = false
		_clear_meshes()
		if _ar:
			_ar.set(&"mesh", false)  # stop the shared XrealAR polling the mesh stream
		enabled = false
		return false
	if not _system or not _system.has_method(&"is_meshing_supported") or not _system.is_meshing_supported():
		enabled = false
		error.emit("[xreal-mesh] depth meshing unsupported on this device (Air 2 Ultra only)")
		return false
	_ensure_ar()
	if not _initialized:
		_system.set_meshing_enabled(true)
		_initialized = true
	_enabled = true
	if _ar:
		_ar.set(&"mesh", true)
	enabled = true
	return true

## Resolve the shared XrealAR and connect its mesh signals once — BEFORE the stream switch goes
## on, so no change event is ever polled without a listener.
func _ensure_ar() -> void:
	if _connected:
		return
	_ar = XrealShared.get_ar(get_tree())
	if _ar == null:
		return
	_ar.connect(&"mesh_block_changed", _on_mesh_changed)
	_ar.connect(&"mesh_block_removed", _on_mesh_removed)
	_connected = true

## XrealAR signal: a block was added/updated.
func _on_mesh_changed(b: Dictionary) -> void:
	if _enabled:
		_update_block(b)

## XrealAR signal: a block was removed.
func _on_mesh_removed(id: String) -> void:
	if _enabled:
		_remove_block(id)

func _update_block(b: Dictionary) -> void:
	var id: String = b.get("id", "")
	if id.is_empty():
		return
	if int(b.get("state", 0)) == 2:  # removed
		_remove_block(id)
		return
	var verts: PackedVector3Array = b.get("vertices", PackedVector3Array())
	var indices: PackedInt32Array = b.get("indices", PackedInt32Array())
	if verts.is_empty() or indices.is_empty():
		return
	var mi: MeshInstance3D = _meshes.get(id)
	if mi == null:
		mi = MeshInstance3D.new()
		mi.material_override = _material()
		add_child(mi)
		_meshes[id] = mi
	var arrays := []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = verts
	var normals: PackedVector3Array = b.get("normals", PackedVector3Array())
	if normals.size() == verts.size():
		arrays[Mesh.ARRAY_NORMAL] = normals
	arrays[Mesh.ARRAY_INDEX] = indices
	var am := ArrayMesh.new()
	am.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays)
	mi.mesh = am

func _remove_block(id: String) -> void:
	var mi: MeshInstance3D = _meshes.get(id)
	if mi:
		mi.queue_free()
		_meshes.erase(id)

func _clear_meshes() -> void:
	for id in _meshes:
		(_meshes[id] as MeshInstance3D).queue_free()
	_meshes.clear()

## Shared translucent unshaded material (double-sided) — reads as a light tint over the real room.
func _material() -> StandardMaterial3D:
	if _mat == null:
		_mat = StandardMaterial3D.new()
		_mat.albedo_color = Color(0.4, 0.8, 1.0, 0.22)
		_mat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
		_mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
		_mat.cull_mode = BaseMaterial3D.CULL_DISABLED
	return _mat

func _exit_tree() -> void:
	# Release the shared stream switch + stop meshing on clean shutdown.
	if _enabled and _ar and is_instance_valid(_ar):
		_ar.set(&"mesh", false)
	if _initialized and _system:
		_system.set_meshing_enabled(false)
