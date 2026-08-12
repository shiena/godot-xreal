@tool
extends VBoxContainer
## Editor dock: turns a depth-mesh snapshot written on the glasses into something the editor can
## open, an `ArrayMesh` resource or a .glb. It is the Godot answer to the Unity SDK's "Use Meshes in
## the Editor", which saves .obj files instead, and it keeps the per-vertex semantic classification
## that .obj has no way to carry.
##
## The workflow: run the demo on an Air 2 Ultra, turn Mesh on, scan the room, tap "Save Mesh". That
## writes one JSON file per tap (see xreal_mesh.gd's save_snapshot for the schema), and either
## location comes off the device with a plain adb pull:
##
##     adb pull /sdcard/Documents/godot-xreal
##
## for the demo, which publishes its snapshots to MediaStore, or
##
##     adb pull /sdcard/Android/data/<package>/files/MeshSave
##
## for the component's own default.
##
## The component writes to the app's external-storage folder and stops there; the demo then moves the
## file into shared storage under Documents, the way it does with captures and recordings, because
## Documents is what the phone's Files app can browse.
##
## Point this dock at one of those files and convert. From then on the real scan is in the scene,
## and iterating on anything that consumes the mesh no longer costs a redeploy and a rescan.
##
## The output splits on both axes at once, block and semantic class: surfaces come out named
## `block_<id>_<class>`, each with the flat-coloured material of its class. So a converted scan
## opened in Blender lists "wall", "floor" and "ceiling" among its materials, and each class can be
## selected, hidden or exported on its own rather than only looked at.

## The runtime component owns the snapshot format and the class palette; they are read from it here
## rather than restated, so the two halves cannot drift apart.
const MeshFeature := preload("res://addons/godot_xreal/features/xreal_mesh.gd")

const DEFAULT_OUTPUT_DIR := "res://mesh_snapshots"

var _snapshot_edit: LineEdit
var _output_edit: LineEdit
var _res_check: CheckBox
var _glb_check: CheckBox
var _legacy_flip_check: CheckBox
var _status: RichTextLabel
var _file_dialog: EditorFileDialog
var _materials := {}  # class id (-1 = unclassified) -> the one material shared by every surface of it

func _ready() -> void:
	_build_ui()

func _build_ui() -> void:
	add_theme_constant_override(&"separation", 6)

	var title := Label.new()
	title.text = "Mesh snapshot converter"
	title.add_theme_font_size_override(&"font_size", 15)
	add_child(title)

	var hint := Label.new()
	hint.text = "A \"Save Mesh\" JSON from the glasses -> ArrayMesh or .glb."
	hint.tooltip_text = ("Run the demo on an Air 2 Ultra, turn Mesh on, scan, then tap Save Mesh. "
		+ "Pull the file with: adb pull /sdcard/Documents/godot-xreal (the demo), or "
		+ "adb pull /sdcard/Android/data/<package>/files/MeshSave (the component's own default)")
	hint.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	# Cap the wrapped height. An autowrap Label reports its *wrapped* height as its minimum, and a
	# dock tab that has never been the active one was never laid out, leaving it about 17 px wide, so
	# it wraps to hundreds of lines and asks for thousands of px. The editor sizes hidden tabs too
	# (use_hidden_tabs_for_min_size), so that minimum pushes the dock below it, FileSystem, off-screen
	# until this tab is first opened. Same trap as vendor_import_dock's help label, capped tighter
	# here because this dock has more rows to fit under it.
	hint.max_lines_visible = 2
	hint.modulate = Color(1, 1, 1, 0.75)
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

	# Side by side: this dock has enough rows already, and a dock slot's minimum height is the
	# maximum over every tab in it, so each row saved here is a row FileSystem keeps.
	var formats := HBoxContainer.new()
	_res_check = CheckBox.new()
	_res_check.text = "ArrayMesh"
	_res_check.button_pressed = true
	_res_check.tooltip_text = ("Writes .res. One surface per block and class, the class ids kept "
		+ "verbatim in the resource metadata.")
	formats.add_child(_res_check)
	_glb_check = CheckBox.new()
	_glb_check.text = "glTF"
	_glb_check.tooltip_text = ("Writes .glb, for Blender and the like. The class colours travel as "
		+ "materials; the ids themselves do not survive the format.")
	formats.add_child(_glb_check)
	add_child(formats)

	# Off by default: the runtime writes canonical Godot space now. Only a snapshot saved before the
	# mesh conversion stopped negating Y needs this, and nothing else does.
	_legacy_flip_check = CheckBox.new()
	_legacy_flip_check.text = "Legacy snapshot (flip Y)"
	_legacy_flip_check.button_pressed = false
	_legacy_flip_check.tooltip_text = ("Snapshots used to come out mirrored, because the mesh "
		+ "conversion negated Y to compensate for an eye image that itself arrived mirrored. Both "
		+ "are fixed, so leave this off. Tick it only for a snapshot saved before that, which would "
		+ "otherwise open upside down and inside out.")
	add_child(_legacy_flip_check)

	var convert := Button.new()
	convert.text = "Convert"
	convert.pressed.connect(_on_convert)
	add_child(convert)

	_status = RichTextLabel.new()
	_status.bbcode_enabled = true
	_status.selection_enabled = true  # the written paths are worth copying out
	# A floor only, and deliberately no fit_content: fit_content makes the minimum height the
	# *content* height measured at the current width, which in a never-shown dock tab is about 17 px,
	# so a multi-line result asks for thousands of px and pushes FileSystem off-screen. The result
	# scrolls inside the label instead, and EXPAND_FILL still hands it every pixel the dock has.
	_status.custom_minimum_size = Vector2(0, 48)
	_status.size_flags_vertical = Control.SIZE_EXPAND_FILL
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
		_ok("Wrote %s\n%d surfaces, %d vertices\nclasses: %s"
			% [", ".join(written), mesh.get_surface_count(), _vertex_total(mesh), _classes_present()])

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

## One ArrayMesh whose surfaces are split on both axes at once, block and class, named
## `block_<id>_<class>`. Both are worth keeping: the block id says which piece of the scan a surface
## came from, and the class says what it is. Blocks the backend never classified become a single
## `block_<id>` surface.
##
## Returns null after reporting an empty result.
func _build_mesh(doc: Dictionary) -> ArrayMesh:
	var mesh := ArrayMesh.new()
	var raw_labels := {}
	_materials.clear()
	for entry in doc.get("blocks", []):
		var block: Dictionary = entry
		var id: String = block.get("id", "")
		var flip := _legacy_flip_check.button_pressed
		var verts := _to_vector3_array(block.get("vertices", ""), flip)
		var indices := _to_index_array(block.get("indices", ""), flip)
		if verts.is_empty() or indices.is_empty():
			continue
		var normals := _to_vector3_array(block.get("normals", ""), flip)
		if normals.size() != verts.size():
			normals = PackedVector3Array()
		var labels := _from_base64(block.get("labels", ""))
		if labels.size() == verts.size():
			raw_labels[id] = labels
			_add_class_surfaces(mesh, id, verts, normals, indices, labels)
		else:
			_add_surface(mesh, "block_%s" % id, verts, normals, indices, _flat_material())
	if mesh.get_surface_count() == 0:
		_error("Every block in the snapshot was empty.")
		return null
	# Splitting by class puts the classification in the structure, but only down to a whole surface.
	# Keep the per-vertex ids too, so a script can still ask about an individual vertex; resource
	# metadata survives the .res round trip.
	if not raw_labels.is_empty():
		mesh.set_meta(&"xreal_semantic_labels", raw_labels)
	return mesh

## Split one block's triangles by class and add a surface for each class present in it.
##
## A triangle can straddle two classes, because the SDK labels vertices rather than faces, so each
## one goes to whichever class holds at least two of its three corners. Three distinct classes on
## one triangle is vanishingly rare and falls back to the first corner. (The SDK's own sample reads
## all three corners and then uses the first unconditionally, so its boundaries are noisier.)
func _add_class_surfaces(mesh: ArrayMesh, id: String, verts: PackedVector3Array,
		normals: PackedVector3Array, indices: PackedInt32Array, labels: PackedByteArray) -> void:
	var by_class := {}  # class id -> Array of source vertex indices, three per triangle
	for t in range(0, indices.size() - 2, 3):
		var a := indices[t]
		var b := indices[t + 1]
		var c := indices[t + 2]
		var label := _triangle_class(labels[a], labels[b], labels[c])
		var tris: Array = by_class.get(label, [])
		if tris.is_empty():
			by_class[label] = tris
		tris.append(a)
		tris.append(b)
		tris.append(c)
	# Sorted, so the surface order is the class order rather than whatever the hash gives.
	var classes := by_class.keys()
	classes.sort()
	for label in classes:
		var sub := _extract(verts, normals, by_class[label])
		_add_surface(mesh, "block_%s_%s" % [id, _label_name(label)],
			sub[0], sub[1], sub[2], _class_material(label))

## The class of a triangle from its three corner classes: the majority, or the first corner when
## all three disagree.
static func _triangle_class(a: int, b: int, c: int) -> int:
	if a == b or a == c:
		return a
	return b if b == c else a

## Pull just the vertices a class's triangles touch into their own arrays, renumbering the indices.
## Returns [vertices, normals, indices]. The alternative is copying every vertex of the block into
## every one of its class surfaces, which is what the SDK's sample does and which multiplies the
## vertex data by the number of classes in the block.
func _extract(verts: PackedVector3Array, normals: PackedVector3Array,
		src_indices: Array) -> Array:
	var remap := PackedInt32Array()
	remap.resize(verts.size())
	remap.fill(-1)
	var out_verts := PackedVector3Array()
	var out_normals := PackedVector3Array()
	var out_indices := PackedInt32Array()
	out_indices.resize(src_indices.size())
	for i in src_indices.size():
		var src: int = src_indices[i]
		if remap[src] == -1:
			remap[src] = out_verts.size()
			out_verts.append(verts[src])
			if not normals.is_empty():
				out_normals.append(normals[src])
		out_indices[i] = remap[src]
	return [out_verts, out_normals, out_indices]

func _add_surface(mesh: ArrayMesh, surface_name: String, verts: PackedVector3Array,
		normals: PackedVector3Array, indices: PackedInt32Array, material: Material) -> void:
	var arrays := []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = verts
	if normals.size() == verts.size():
		arrays[Mesh.ARRAY_NORMAL] = normals
	arrays[Mesh.ARRAY_INDEX] = indices
	mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays)
	var surface := mesh.get_surface_count() - 1
	mesh.surface_set_name(surface, surface_name)
	mesh.surface_set_material(surface, material)

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

## One flat-coloured material per class, named after it, shared across every block that contains
## that class. Sharing matters for the .glb: a fresh material per surface would write one glTF
## material per block-class pair instead of one per class, and Blender's material list is the
## legend here.
##
## The colour lives in the material rather than in vertex colours, because a surface is
## single-class by construction now. glTF carries baseColorFactor everywhere, whereas vertex
## colours depend on the importer choosing to apply them, which Godot's own glTF importer does not.
func _class_material(label: int) -> StandardMaterial3D:
	if not _materials.has(label):
		var mat := _base_material()
		mat.resource_name = _label_name(label)
		mat.albedo_color = MeshFeature.LABEL_COLORS.get(label, MeshFeature.UNKNOWN_LABEL_COLOR)
		_materials[label] = mat
	return _materials[label]

## For a block the backend never classified: the runtime overlay's tint, opaque here.
func _flat_material() -> StandardMaterial3D:
	if not _materials.has(-1):
		var mat := _base_material()
		mat.resource_name = "unclassified"
		mat.albedo_color = Color(0.4, 0.8, 1.0)
		_materials[-1] = mat
	return _materials[-1]

## Unlike the runtime overlay these are opaque: nothing real is behind them here, and a see-through
## mesh only obscures itself.
func _base_material() -> StandardMaterial3D:
	var mat := StandardMaterial3D.new()
	mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	mat.cull_mode = BaseMaterial3D.CULL_DISABLED
	return mat

## The SDK's name for a class, or "class<n>" for a value a future taxonomy might add.
static func _label_name(label: int) -> String:
	return MeshFeature.LABEL_NAMES.get(label, "class%d" % label)

## Bytes from base64, tolerating the "" that an absent or empty array is written as: Marshalls
## treats empty input as an error and logs it.
static func _from_base64(encoded: String) -> PackedByteArray:
	return PackedByteArray() if encoded.is_empty() else Marshalls.base64_to_raw(encoded)

## Base64 float32 triples back to points, negating Y for a legacy snapshot.
##
## Snapshots are written in canonical Godot space. They were not always: the mesh conversion used to
## negate Y on top of the canonical Unity(LH) -> Godot(RH) negate-Z, compensating for an eye image
## the port then submitted mirrored vertically (see mesh_block_to_dict in src/system.rs and
## docs/develop/plans/coordinate-systems-notes.md). That made the file a mirror of the scan, so it
## opened upside down and left-right swapped anywhere outside the glasses: in this editor's
## viewport, in a .glb in Blender, or against physics. One such file measured the floor's vertices
## at y = +1.12 and the ceiling's at y = -1.45. Negating Y here undoes it, for those files only.
func _to_vector3_array(encoded: String, flip: bool) -> PackedVector3Array:
	var floats := _from_base64(encoded).to_float32_array()
	var out := PackedVector3Array()
	@warning_ignore("integer_division")
	out.resize(floats.size() / 3)
	var sy := -1.0 if flip else 1.0
	for i in out.size():
		out[i] = Vector3(floats[i * 3], sy * floats[i * 3 + 1], floats[i * 3 + 2])
	return out

## Base64 int32 back to triangle indices, reversed when `flip` un-mirrors the vertices.
##
## A snapshot's winding is correct for the space it is written in. Negating a single axis to reach
## canonical Godot space is a mirror, and a mirror swaps every triangle's front and back, so the
## winding has to follow or the whole scan converts inside out. Nothing on the glasses would show
## that, since the runtime overlay draws with CULL_DISABLED, but a .glb in Blender is lit from the
## wrong side.
##
## Snapshots taken before mesh_block_to_dict (src/system.rs) stopped reversing hold the opposite
## winding and so convert inside out through here. Deliberate: the format version stayed at 1 rather
## than teaching this reader both conventions, because those files were only ever a few test scans.
func _to_index_array(encoded: String, flip: bool) -> PackedInt32Array:
	var indices := _from_base64(encoded).to_int32_array()
	if not flip:
		return indices
	for t in range(0, indices.size() - 2, 3):
		var last := indices[t + 2]
		indices[t + 2] = indices[t + 1]
		indices[t + 1] = last
	return indices

## The classes the conversion produced, for the status line: it says at a glance whether the scan
## was classified at all, and which surfaces to look for.
func _classes_present() -> String:
	var names: Array[String] = []
	for label in _materials:
		names.append("unclassified" if label == -1 else _label_name(label))
	names.sort()
	return ", ".join(names)

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
