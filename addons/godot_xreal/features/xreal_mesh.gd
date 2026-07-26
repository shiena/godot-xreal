extends Node3D
## Depth meshing as a drop-in feature component (Air 2 Ultra). It enables meshing through
## XrealSystem.set_meshing_enabled and builds or updates one ArrayMesh per block, a translucent
## overlay of the scanned environment, tinted per vertex by the semantic class the SDK assigns it
## (wall, floor, ceiling, door, table and so on). Mesh-block changes stream in through the shared
## XrealAR poller, whose "mesh" stream is gated on this toggle, so it polls only while meshing is on.
##
## World-locked: add this component under a world-fixed node, such as the scene root and NOT the
## head rig, so the mesh stays registered to the real room as the head moves. Switching OFF drops
## every block mesh from the scene but leaves the SDK meshing running, so switching ON repopulates
## from the next poll without a rescan: GetMeshBlockInfo reports the whole current block set each
## time, not just what changed.

## Emitted when the feature is unavailable, e.g. meshing is unsupported on this device, so the load
## site can react by showing UI, logging, or flipping a toggle.
signal error(message: String)

## Enable at boot (applied in _ready). At runtime call set_enabled().
@export var enabled := false

## Tint each vertex by its semantic class (wall, floor, ceiling, door, table and so on) instead of
## painting the whole scan one colour. It falls back to the flat tint per block whenever the backend
## ships no classification for it. Read when a block mesh is built, so a change takes effect from
## the next block update rather than repainting what is already on screen.
@export var colorize_by_label := true

## Per-vertex semantic class -> colour, keyed by the XrealSystem.MESH_LABEL_* values. They are
## spread around the hue circle rather than picked for realism, since the point is telling adjacent
## surfaces apart through a translucent overlay: an unclassified vertex reads as neutral grey and
## every named class as its own hue. Written as sRGB literals and converted once in _label_palette.
##
## Grey background, blue wall, purple building, green floor, cyan ceiling, slate highway, tan
## sidewalk, lime grass, orange door, pink table. The gaps at 3 and 9 are gaps in the SDK's own enum.
const LABEL_COLORS := {
	0: Color(0.60, 0.62, 0.65),   # BACKGROUND: whatever the classifier did not place
	1: Color(0.20, 0.45, 0.95),   # WALL
	2: Color(0.60, 0.30, 0.90),   # BUILDING
	4: Color(0.15, 0.80, 0.35),   # FLOOR
	5: Color(0.15, 0.85, 0.90),   # CEILING
	6: Color(0.40, 0.42, 0.50),   # HIGHWAY
	7: Color(0.80, 0.70, 0.35),   # SIDEWALK
	8: Color(0.55, 0.90, 0.15),   # GRASS
	10: Color(1.00, 0.55, 0.10),  # DOOR
	11: Color(0.95, 0.25, 0.65),  # TABLE
}
## Colour for a label value outside LABEL_COLORS, which only a future SDK taxonomy would produce.
## Deliberately an alarming red, so an unmapped class shows up instead of blending in.
const UNKNOWN_LABEL_COLOR := Color(1.0, 0.15, 0.15)
## Opacity of the whole overlay, low enough to read the real room through the scan.
const OVERLAY_ALPHA := 0.22

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar: Object                     # the shared XrealAR poller
var _connected := false
var _initialized := false           # meshing enabled once
var _enabled := false
var _meshes := {}                   # block id(String) -> MeshInstance3D
var _mat: StandardMaterial3D        # flat tint, for blocks with no classification
var _mat_labeled: StandardMaterial3D  # per-vertex class colour
var _palette: PackedColorArray      # LABEL_COLORS flattened to label value -> linear colour
var _labels_seen := false           # a classified block has arrived, so stop reporting
var _labels_logged := false         # the "no classification" note was printed once

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert
	if enabled:
		enabled = set_enabled(true)

## Toggle meshing and return the resulting state. It returns false on an unsupported device, that
## is anything but an Air 2 Ultra, so a UI toggle can flip itself back off.
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

## Resolve the shared XrealAR and connect its mesh signals once, BEFORE the stream switch goes on,
## so no change event is ever polled without a listener.
func _ensure_ar() -> void:
	if _connected:
		return
	_ar = XrealShared.get_ar(get_tree())
	if _ar == null:
		return
	_ar.connect(&"mesh_block_changed", _on_mesh_changed)
	_ar.connect(&"mesh_block_removed", _on_mesh_removed)
	_connected = true

## XrealAR signal: a block was added or updated.
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
		add_child(mi)
		_meshes[id] = mi
	var arrays := []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = verts
	var normals: PackedVector3Array = b.get("normals", PackedVector3Array())
	if normals.size() == verts.size():
		arrays[Mesh.ARRAY_NORMAL] = normals
	arrays[Mesh.ARRAY_INDEX] = indices
	# The labels parallel the vertices one-to-one when the backend classified this block, and are
	# empty when it did not, so the size comparison is the only capability check needed.
	var labels: PackedByteArray = b.get("labels", PackedByteArray())
	var labelled := colorize_by_label and labels.size() == verts.size()
	if labelled:
		arrays[Mesh.ARRAY_COLOR] = _vertex_colors(labels)
	_log_labels_once(labels.size(), verts.size())
	var am := ArrayMesh.new()
	am.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays)
	mi.mesh = am
	# Re-applied per update rather than at creation, because a block can gain (or lose) its
	# classification between polls, and the two materials treat vertex colour differently.
	mi.material_override = _material(labelled)

func _remove_block(id: String) -> void:
	var mi: MeshInstance3D = _meshes.get(id)
	if mi:
		mi.queue_free()
		_meshes.erase(id)

func _clear_meshes() -> void:
	for id in _meshes:
		(_meshes[id] as MeshInstance3D).queue_free()
	_meshes.clear()

## One colour per vertex, looked up from its semantic class.
func _vertex_colors(labels: PackedByteArray) -> PackedColorArray:
	var palette := _label_palette()
	var colors := PackedColorArray()
	colors.resize(labels.size())
	for i in labels.size():
		colors[i] = palette[labels[i]]
	return colors

## LABEL_COLORS as a flat lookup, built once. It holds an entry for every one of the 256 values a
## label byte can take, so the per-vertex lookup is a bare index with no bounds test or dictionary
## hash, and the gaps in the SDK's enum (3 and 9) resolve to the unmapped colour like any other
## value it never emits. Converted to linear here because ARRAY_COLOR bypasses the sRGB conversion
## that albedo_color goes through, and skipping it washes the palette out.
func _label_palette() -> PackedColorArray:
	if _palette.is_empty():
		_palette.resize(256)
		_palette.fill(UNKNOWN_LABEL_COLOR.srgb_to_linear())
		for label in LABEL_COLORS:
			_palette[int(label)] = (LABEL_COLORS[label] as Color).srgb_to_linear()
	return _palette

## Report once whether the classification actually arrived, since a backend that meshes without
## classifying is silent otherwise: the overlay just stays one colour with nothing to explain it.
## The "none" note prints once but does not close the question, so classification that only starts
## after a few blocks still gets reported when it does.
func _log_labels_once(label_count: int, vertex_count: int) -> void:
	if _labels_seen:
		return
	if label_count == vertex_count:
		_labels_seen = true
		print("[xreal-mesh] semantic labels on: %d per block vertex" % label_count)
	elif not _labels_logged:
		_labels_logged = true
		print("[xreal-mesh] no semantic labels yet (%d for %d vertices); using the flat tint"
			% [label_count, vertex_count])

## Translucent unshaded double-sided overlay, which reads as a light tint over the real room. The
## labelled variant keeps its albedo white so the per-vertex class colour passes through untouched,
## contributing only the alpha; the flat one carries the tint itself.
func _material(vertex_colored: bool) -> StandardMaterial3D:
	if vertex_colored:
		if _mat_labeled == null:
			_mat_labeled = _make_material(Color(1.0, 1.0, 1.0, OVERLAY_ALPHA))
			_mat_labeled.vertex_color_use_as_albedo = true
		return _mat_labeled
	if _mat == null:
		_mat = _make_material(Color(0.4, 0.8, 1.0, OVERLAY_ALPHA))
	return _mat

func _make_material(albedo: Color) -> StandardMaterial3D:
	var mat := StandardMaterial3D.new()
	mat.albedo_color = albedo
	mat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	mat.cull_mode = BaseMaterial3D.CULL_DISABLED
	return mat

func _exit_tree() -> void:
	# Release the shared stream switch and stop meshing on clean shutdown.
	if _enabled and _ar and is_instance_valid(_ar):
		_ar.set(&"mesh", false)
	if _initialized and _system:
		_system.set_meshing_enabled(false)
