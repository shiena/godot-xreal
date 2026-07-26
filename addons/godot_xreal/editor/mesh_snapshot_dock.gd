@tool
extends VBoxContainer
## Editor dock: turns a depth-mesh snapshot written on the glasses into something the editor can
## open, an `ArrayMesh` resource or a .glb. It is the Godot answer to the Unity SDK's "Use Meshes in
## the Editor", which saves .obj files instead, and it keeps the per-vertex semantic classification
## that .obj has no way to carry.
##
## The workflow: run the demo on an Air 2 Ultra, turn Mesh on, scan the room, tap "Save Mesh". That
## writes one JSON file per tap (see xreal_mesh.gd's save_snapshot for the schema), on Android under
## the app's own external-storage folder so it comes off the device with a plain adb pull:
##
##     adb pull /sdcard/Android/data/<package>/files/MeshSave
##
## Point this dock at one of those files and convert. From then on the real scan is in the scene,
## and iterating on anything that consumes the mesh no longer costs a redeploy and a rescan.

## The runtime component owns the snapshot format and the class palette; they are read from it here
## rather than restated, so the two halves cannot drift apart.
const MeshFeature := preload("res://addons/godot_xreal/features/xreal_mesh.gd")

const DEFAULT_OUTPUT_DIR := "res://mesh_snapshots"

var _snapshot_edit: LineEdit
var _output_edit: LineEdit
var _res_check: CheckBox
var _glb_check: CheckBox
var _status: RichTextLabel
var _file_dialog: EditorFileDialog

func _ready() -> void:
	_build_ui()

func _build_ui() -> void:
	add_theme_constant_override(&"separation", 6)

	var title := Label.new()
	title.text = "Mesh snapshot converter"
	title.add_theme_font_size_override(&"font_size", 15)
	add_child(title)

	var hint := Label.new()
	hint.text = "Convert a \"Save Mesh\" JSON from the glasses into an ArrayMesh or a .glb."
	hint.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	add_child(hint)

	# The snapshot lives outside the project (it was pulled off a phone), so this browses the whole
	# filesystem rather than res://.
	var srow := HBoxContainer.new()
	var slabel := Label.new()
	slabel.text = "Snapshot:"
	srow.add_child(slabel)
	_snapshot_edit = LineEdit.new()
	_snapshot_edit.placeholder_text = "mesh_20260726_143012.json"
	_snapshot_edit.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	srow.add_child(_snapshot_edit)
	var browse := Button.new()
	browse.text = "…"
	browse.pressed.connect(_on_browse)
	srow.add_child(browse)
	add_child(srow)

	var orow := HBoxContainer.new()
	var olabel := Label.new()
	olabel.text = "Output:"
	orow.add_child(olabel)
	_output_edit = LineEdit.new()
	_output_edit.text = DEFAULT_OUTPUT_DIR
	_output_edit.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	orow.add_child(_output_edit)
	add_child(orow)

	_res_check = CheckBox.new()
	_res_check.text = "ArrayMesh (.res)"
	_res_check.button_pressed = true
	_res_check.tooltip_text = ("Native resource: one surface per mesh block, the class ids kept "
		+ "verbatim in the resource metadata.")
	add_child(_res_check)

	_glb_check = CheckBox.new()
	_glb_check.text = "glTF binary (.glb)"
	_glb_check.tooltip_text = ("Portable, for Blender and the like. The class colours travel as "
		+ "vertex colours; the ids themselves do not survive the format.")
	add_child(_glb_check)

	var convert := Button.new()
	convert.text = "Convert"
	convert.pressed.connect(_on_convert)
	add_child(convert)

	_status = RichTextLabel.new()
	_status.fit_content = true
	_status.custom_minimum_size = Vector2(0, 60)
	_status.bbcode_enabled = true
	add_child(_status)

func _on_browse() -> void:
	if _file_dialog == null:
		_file_dialog = EditorFileDialog.new()
		_file_dialog.file_mode = EditorFileDialog.FILE_MODE_OPEN_FILE
		_file_dialog.access = EditorFileDialog.ACCESS_FILESYSTEM
		_file_dialog.add_filter("*.json", "Mesh snapshot")
		_file_dialog.file_selected.connect(func(p: String) -> void: _snapshot_edit.text = p)
		add_child(_file_dialog)
	_file_dialog.popup_file_dialog()

func _on_convert() -> void:
	_status.clear()
	var path := _snapshot_edit.text.strip_edges()
	if path.is_empty():
		_error("Pick a snapshot file first.")
		return
	if not _res_check.button_pressed and not _glb_check.button_pressed:
		_error("Tick at least one output format.")
		return
	var doc := _read_snapshot(path)
	if doc.is_empty():
		return
	var mesh := _build_mesh(doc)
	if mesh == null:
		return
	var written := _write_outputs(mesh, path)
	if not written.is_empty():
		_ok("Wrote %s\n%d blocks, %d vertices."
			% [", ".join(written), mesh.get_surface_count(), _vertex_total(mesh)])

## Write whichever formats are ticked and return their paths, or [] once a failure was reported.
func _write_outputs(mesh: ArrayMesh, snapshot_path: String) -> Array[String]:
	var out_dir := _output_edit.text.strip_edges()
	var mkdir := DirAccess.make_dir_recursive_absolute(out_dir)
	if mkdir != OK and mkdir != ERR_ALREADY_EXISTS:
		_error("Cannot create %s (error %d)." % [out_dir, mkdir])
		return []
	var stem := snapshot_path.get_file().get_basename()
	var written: Array[String] = []
	if _res_check.button_pressed:
		var res_path := out_dir.path_join("%s.res" % stem)
		var err := ResourceSaver.save(mesh, res_path)
		if err != OK:
			_error("Cannot write %s (error %d)." % [res_path, err])
			return []
		written.append(res_path)
	if _glb_check.button_pressed:
		var glb_path := out_dir.path_join("%s.glb" % stem)
		if not _write_glb(mesh, glb_path):
			return []
		written.append(glb_path)
	# Only a res:// write shows up in the FileSystem dock, and only after a rescan.
	if out_dir.begins_with("res://"):
		EditorInterface.get_resource_filesystem().scan()
	return written

## Parse and validate a snapshot, or {} after reporting why not.
func _read_snapshot(path: String) -> Dictionary:
	var f := FileAccess.open(path, FileAccess.READ)
	if f == null:
		_error("Cannot read %s (error %d)." % [path, FileAccess.get_open_error()])
		return {}
	var parsed = JSON.parse_string(f.get_as_text())
	if typeof(parsed) != TYPE_DICTIONARY:
		_error("%s is not valid JSON." % path.get_file())
		return {}
	var doc: Dictionary = parsed
	if doc.get("format", "") != MeshFeature.SNAPSHOT_FORMAT:
		_error("%s is not a mesh snapshot (format=\"%s\")." % [path.get_file(), doc.get("format", "")])
		return {}
	# A newer writer may have added fields this reader ignores, which is fine, but it may also have
	# changed what the existing ones mean, which is not: say so rather than build a wrong mesh.
	if int(doc.get("version", 0)) > MeshFeature.SNAPSHOT_VERSION:
		_error("%s was written by a newer addon (version %d > %d)."
			% [path.get_file(), int(doc.get("version", 0)), MeshFeature.SNAPSHOT_VERSION])
		return {}
	if not (doc.get("blocks", []) as Array).size():
		_error("%s holds no blocks." % path.get_file())
		return {}
	return doc

## One ArrayMesh, one surface per block, named after the block id so the origin of each piece stays
## visible in the editor. Returns null after reporting an empty result.
func _build_mesh(doc: Dictionary) -> ArrayMesh:
	var mesh := ArrayMesh.new()
	var raw_labels := {}
	for entry in doc.get("blocks", []):
		var block: Dictionary = entry
		var verts := _to_vector3_array(block.get("vertices", ""))
		var indices := _from_base64(block.get("indices", "")).to_int32_array()
		if verts.is_empty() or indices.is_empty():
			continue
		var arrays := []
		arrays.resize(Mesh.ARRAY_MAX)
		arrays[Mesh.ARRAY_VERTEX] = verts
		var normals := _to_vector3_array(block.get("normals", ""))
		if normals.size() == verts.size():
			arrays[Mesh.ARRAY_NORMAL] = normals
		var labels := _from_base64(block.get("labels", ""))
		if labels.size() == verts.size():
			arrays[Mesh.ARRAY_COLOR] = _label_colors(labels)
			raw_labels[block.get("id", "")] = labels
		arrays[Mesh.ARRAY_INDEX] = indices
		mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays)
		mesh.surface_set_name(mesh.get_surface_count() - 1, "block_%s" % block.get("id", ""))
		mesh.surface_set_material(mesh.get_surface_count() - 1, _material())
	if mesh.get_surface_count() == 0:
		_error("Every block in the snapshot was empty.")
		return null
	# ARRAY_COLOR carries the classes as colour, which loses the ids. Keep them beside it, so a
	# script can still ask what class a vertex is; resource metadata survives the .res round trip.
	if not raw_labels.is_empty():
		mesh.set_meta(&"xreal_semantic_labels", raw_labels)
	return mesh

## Write the mesh out as .glb through a throwaway scene, since GLTFDocument works on node trees.
func _write_glb(mesh: ArrayMesh, path: String) -> bool:
	var root := Node3D.new()
	root.name = "MeshSnapshot"
	var mi := MeshInstance3D.new()
	mi.name = "Mesh"
	mi.mesh = mesh
	root.add_child(mi)
	mi.owner = root
	var gltf := GLTFDocument.new()
	var state := GLTFState.new()
	var err := gltf.append_from_scene(root, state)
	if err == OK:
		err = gltf.write_to_filesystem(state, path)
	root.free()
	if err != OK:
		_error("Cannot write %s (error %d)." % [path, err])
		return false
	return true

## Opaque unshaded material that shows the vertex colours. Unlike the runtime overlay this is not
## translucent: nothing real is behind it here, and a see-through mesh only obscures itself.
func _material() -> StandardMaterial3D:
	var mat := StandardMaterial3D.new()
	mat.vertex_color_use_as_albedo = true
	mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	mat.cull_mode = BaseMaterial3D.CULL_DISABLED
	return mat

## Class ids to vertex colours, using the runtime component's palette so a converted scan reads the
## same as it did in the glasses.
func _label_colors(labels: PackedByteArray) -> PackedColorArray:
	var colors := PackedColorArray()
	colors.resize(labels.size())
	for i in labels.size():
		var c: Color = MeshFeature.LABEL_COLORS.get(labels[i], MeshFeature.UNKNOWN_LABEL_COLOR)
		colors[i] = c.srgb_to_linear()
	return colors

## Bytes from base64, tolerating the "" that an absent or empty array is written as: Marshalls
## treats empty input as an error and logs it.
static func _from_base64(encoded: String) -> PackedByteArray:
	return PackedByteArray() if encoded.is_empty() else Marshalls.base64_to_raw(encoded)

## Base64 float32 triples back to points. The snapshot is already in Godot space with Godot winding,
## so there is nothing to convert here.
func _to_vector3_array(encoded: String) -> PackedVector3Array:
	var floats := _from_base64(encoded).to_float32_array()
	var out := PackedVector3Array()
	@warning_ignore("integer_division")
	out.resize(floats.size() / 3)
	for i in out.size():
		out[i] = Vector3(floats[i * 3], floats[i * 3 + 1], floats[i * 3 + 2])
	return out

func _vertex_total(mesh: ArrayMesh) -> int:
	var total := 0
	for i in mesh.get_surface_count():
		total += (mesh.surface_get_arrays(i)[Mesh.ARRAY_VERTEX] as PackedVector3Array).size()
	return total

func _ok(message: String) -> void:
	_status.append_text("[color=#7ec87e]%s[/color]" % message)

func _error(message: String) -> void:
	_status.append_text("[color=#e07070]%s[/color]" % message)
	push_warning("[xreal-mesh-snapshot] %s" % message)
