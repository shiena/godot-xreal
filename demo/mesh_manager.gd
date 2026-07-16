extends Node3D
## Depth-mesh demo / on-device verification (Air 2 Ultra). Driven by the phone-menu "メッシュ" toggle
## (main.gd). Enables meshing (XrealSystem.set_meshing_enabled) and, each frame, builds/updates an
## ArrayMesh per block from poll_mesh_blocks — a translucent overlay of the scanned environment.
##
## World-locked: child of Main (like the hand joints / anchors), so the mesh stays registered to the
## real room as the head moves. OFF hides the meshes but keeps meshing running so ON restores them.

var _system: Object                 # XrealSystem, injected by main.gd via setup()
var _initialized := false           # meshing enabled once
var _enabled := false
var _meshes := {}                   # block id(String) -> MeshInstance3D
var _mat: StandardMaterial3D

## Injected once by main.gd after the rig spawns.
func setup(system: Object) -> void:
	_system = system

## Toggle meshing. Returns the resulting state (false if unsupported — non-Air-2-Ultra — so the
## phone-menu toggle can flip itself back off).
func set_enabled(on: bool) -> bool:
	if not _system or not _system.has_method(&"is_meshing_supported") or not _system.is_meshing_supported():
		return false
	if on:
		if not _initialized:
			_system.set_meshing_enabled(true)
			_initialized = true
		_enabled = true
		visible = true
	else:
		_enabled = false
		visible = false  # keep meshing running; just hide the overlay
	return _enabled

func _process(_delta: float) -> void:
	if not _enabled or not _system:
		return
	for b in _system.poll_mesh_blocks():
		_update_block(b)

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
	# Stop meshing on clean shutdown.
	if _initialized and _system:
		_system.set_meshing_enabled(false)
