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

## Emitted after save_snapshot() writes a file, with its path and the number of blocks in it.
signal snapshot_saved(path: String, block_count: int)

## Enable at boot (applied in _ready). At runtime call set_enabled().
@export var enabled := false

## Where save_snapshot() writes. Empty picks the platform default: on Android the app's own folder
## on external storage (`/sdcard/Android/data/<package>/files/MeshSave`), which `adb pull` reads
## without root, and `user://mesh_snapshots` everywhere else. Godot maps `user://` to internal
## storage on Android, and a scan nobody can copy off the device is of no use in the editor.
@export var snapshot_dir := ""

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
## Class id -> name, the SDK's own enum spellings in lower case. The snapshot converter names each
## surface and material after these, so a scan opened in Blender reads "wall" and "floor" rather
## than a colour someone has to remember.
const LABEL_NAMES := {
	0: "background",
	1: "wall",
	2: "building",
	4: "floor",
	5: "ceiling",
	6: "highway",
	7: "sidewalk",
	8: "grass",
	10: "door",
	11: "table",
}
## Colour for a label value outside LABEL_COLORS, which only a future SDK taxonomy would produce.
## Deliberately an alarming red, so an unmapped class shows up instead of blending in.
const UNKNOWN_LABEL_COLOR := Color(1.0, 0.15, 0.15)
## Opacity of the whole overlay, low enough to read the real room through the scan.
const OVERLAY_ALPHA := 0.22

## Marker and schema revision written into every snapshot, checked by the editor converter before
## it reads anything else. Bump the version whenever the block schema changes.
const SNAPSHOT_FORMAT := "godot-xreal.mesh-snapshot"
const SNAPSHOT_VERSION := 1

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar: Object                     # the shared XrealAR poller
var _connected := false
var _initialized := false           # meshing enabled once
var _enabled := false
var _meshes := {}                   # block id(String) -> MeshInstance3D
var _labels := {}                   # block id(String) -> PackedByteArray, only while classified
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

# ------------------------------------------------------------------- snapshots ---

## Write every block currently in the scene to one JSON file and return its path, or "" on failure
## (reported through `error`). Meant for the same job as the Unity SDK's "Save Mesh": capture a real
## scan on the glasses, pull it to a PC, and iterate against it in the editor instead of rescanning
## a room for every change. Unlike that one it keeps the semantic classification, which .obj cannot
## carry, and one file holds the whole block set rather than a folder of meshes.
##
## Convert a saved file with the "XREAL Mesh Snapshot" editor dock, which turns it into an
## `ArrayMesh` resource or a .glb. The schema, all little-endian:
##
##     { "format": "godot-xreal.mesh-snapshot", "version": 1, "space": "godot",
##       "encoding": "base64", "saved_at": "<ISO 8601 UTC>",
##       "blocks": [ { "id": "<16 hex digits>", "vertex_count": int, "index_count": int,
##                     "vertices": "<base64 float32 xyz>", "normals": "<base64 float32 xyz>",
##                     "indices": "<base64 int32>", "labels": "<base64 uint8, may be empty>" } ] }
##
## The arrays are base64 rather than JSON numbers on purpose: a room scan runs to hundreds of
## thousands of floats, and writing those as text costs roughly ten times the bytes and long enough
## on the phone to stall the frame. Everything is already in Godot space with Godot winding, so a
## reader needs no conversion.
func save_snapshot() -> String:
	if _meshes.is_empty():
		error.emit("[xreal-mesh] no mesh blocks yet, so nothing to save; scan the room first")
		return ""
	var dir := _snapshot_dir()
	var mkdir := DirAccess.make_dir_recursive_absolute(dir)
	if mkdir != OK and mkdir != ERR_ALREADY_EXISTS:
		error.emit("[xreal-mesh] cannot create %s (error %d)" % [dir, mkdir])
		return ""
	var blocks := []
	for id in _meshes:
		var block := _block_to_dict(id)
		if not block.is_empty():
			blocks.append(block)
	if blocks.is_empty():
		error.emit("[xreal-mesh] every block was empty, so nothing was written")
		return ""
	var path := dir.path_join("mesh_%s.json" % _timestamp())
	var f := FileAccess.open(path, FileAccess.WRITE)
	if f == null:
		error.emit("[xreal-mesh] cannot write %s (error %d)" % [path, FileAccess.get_open_error()])
		return ""
	f.store_string(JSON.stringify({
		"format": SNAPSHOT_FORMAT,
		"version": SNAPSHOT_VERSION,
		"space": "godot",
		"encoding": "base64",
		"saved_at": Time.get_datetime_string_from_system(true),
		"blocks": blocks,
	}, "\t"))
	f.close()
	print("[xreal-mesh] snapshot -> %s (%d blocks)" % [path, blocks.size()])
	snapshot_saved.emit(path, blocks.size())
	return path

## One block as its JSON object, or {} when it carries no surface. The geometry is read back off
## the ArrayMesh, which holds exactly what was handed to it, so only the class ids need their own
## copy.
func _block_to_dict(id: String) -> Dictionary:
	var mesh: ArrayMesh = (_meshes[id] as MeshInstance3D).mesh
	if mesh == null or mesh.get_surface_count() == 0:
		return {}
	var arrays := mesh.surface_get_arrays(0)
	var verts: PackedVector3Array = arrays[Mesh.ARRAY_VERTEX]
	var indices: PackedInt32Array = arrays[Mesh.ARRAY_INDEX]
	var normals := PackedVector3Array()
	if arrays[Mesh.ARRAY_NORMAL] != null:
		normals = arrays[Mesh.ARRAY_NORMAL]
	var labels: PackedByteArray = _labels.get(id, PackedByteArray())
	return {
		"id": id,
		"vertex_count": verts.size(),
		"index_count": indices.size(),
		"vertices": _to_base64(verts.to_byte_array()),
		"normals": _to_base64(normals.to_byte_array()),
		"indices": _to_base64(indices.to_byte_array()),
		"labels": _to_base64(labels),
	}

## Base64 for a byte run, and "" for an empty one. Marshalls.raw_to_base64 treats empty input as an
## error and logs it, which an unclassified block would otherwise trigger on every save.
static func _to_base64(bytes: PackedByteArray) -> String:
	return "" if bytes.is_empty() else Marshalls.raw_to_base64(bytes)

## The configured snapshot directory, or the platform default described on `snapshot_dir`.
func _snapshot_dir() -> String:
	if not snapshot_dir.is_empty():
		return snapshot_dir
	var external := _android_external_files_dir()
	return external.path_join("MeshSave") if not external.is_empty() else "user://mesh_snapshots"

## `Context.getExternalFilesDir`, the app's own directory on external storage, or "" when it is
## unavailable. It needs no permission, is wiped with the app, and `adb pull` reads it on a stock
## device, which is exactly what `user://` on Android is not.
##
## The argument is "" and NOT the `null` the Android docs ask for, because JavaClassWrapper picks
## the Java overload from the Variant types it is handed and a nil Variant is compatible with an
## Object parameter alone, never a String one. `getExternalFilesDir(null)` therefore matches no
## method at all and returns null without raising, which silently sent every snapshot to the
## `user://` fallback that no one can pull off a stock device. Empty is equivalent on the Java
## side, where `new File(dir, "")` resolves straight back to `dir`.
func _android_external_files_dir() -> String:
	if OS.get_name() != "Android":
		return ""
	var activity := XrealAndroidBridge.get_activity()
	if activity == null:
		return ""
	var dir = activity.getExternalFilesDir("")
	if dir == null:
		push_warning("[xreal-mesh] getExternalFilesDir is unavailable, so snapshots fall back to "
			+ "user://, which is internal storage and out of reach of `adb pull`")
		return ""
	return str(dir.getAbsolutePath())

## Local wall-clock stamp for the file name, "20260726_143012", which sorts chronologically.
func _timestamp() -> String:
	return Time.get_datetime_string_from_system().replace("-", "").replace(":", "").replace("T", "_")

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
	var classified := labels.size() == verts.size()
	# Held per block for save_snapshot, which needs the class ids themselves: they do not survive
	# the trip through ARRAY_COLOR. The geometry is read back off the ArrayMesh instead, so this is
	# the only part of the block worth a second copy.
	if classified:
		_labels[id] = labels
	else:
		_labels.erase(id)
	var labelled := colorize_by_label and classified
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
	_labels.erase(id)

func _clear_meshes() -> void:
	for id in _meshes:
		(_meshes[id] as MeshInstance3D).queue_free()
	_meshes.clear()
	_labels.clear()

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
