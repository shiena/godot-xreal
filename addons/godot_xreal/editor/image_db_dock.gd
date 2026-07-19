@tool
extends VBoxContainer
## Editor dock: builds the image-tracking reference-image DB blob (the Godot analog of Unity's
## XREALImageLibraryBuildProcessor). Manages a manifest of reference images (image + physical width +
## a generated guid), then runs the vendored trackableImageTools CLI to compile the .bin the runtime
## loads via XrealSystem.init_image_database. See docs/plans/ar-features-plan.md §3.
##
## The images (.jpg/.png) and the built .bin are gitignored (SDK-derived / generated); the manifest
## (reference.json) is committed. trackableImageTools must be vendored first
## (scripts/vendor_xreal_libs.* → addons/godot_xreal/tools/).

const DEFAULT_MANIFEST := "res://demo/image_tracking/reference.json"

var _manifest_edit: LineEdit
var _set_selector: OptionButton
var _set_name_edit: LineEdit
var _list: VBoxContainer
var _status: RichTextLabel
var _file_dialog: EditorFileDialog
var _current_set := 0

func _ready() -> void:
	_build_ui()
	_refresh_list()

func _build_ui() -> void:
	add_theme_constant_override(&"separation", 6)

	var title := Label.new()
	title.text = "Image-tracking DB builder"
	title.add_theme_font_size_override(&"font_size", 15)
	add_child(title)

	var mrow := HBoxContainer.new()
	var mlabel := Label.new()
	mlabel.text = "Manifest:"
	mrow.add_child(mlabel)
	_manifest_edit = LineEdit.new()
	_manifest_edit.text = DEFAULT_MANIFEST
	_manifest_edit.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_manifest_edit.text_submitted.connect(func(_t): _refresh_list())
	mrow.add_child(_manifest_edit)
	add_child(mrow)

	# Set selector + add/remove (each set = one tracking DB / blob).
	var srow := HBoxContainer.new()
	var slabel := Label.new()
	slabel.text = "Set:"
	srow.add_child(slabel)
	_set_selector = OptionButton.new()
	_set_selector.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_set_selector.item_selected.connect(func(i): _current_set = i; _refresh_list())
	srow.add_child(_set_selector)
	var add_set := Button.new()
	add_set.text = "Add set"
	add_set.pressed.connect(_on_add_set)
	srow.add_child(add_set)
	var rm_set := Button.new()
	rm_set.text = "Remove"
	rm_set.pressed.connect(_on_remove_set)
	srow.add_child(rm_set)
	add_child(srow)

	# Rename the current set (its `name` in the manifest, i.e. the tracking-set label the runtime cycles).
	var nrow := HBoxContainer.new()
	var nlabel := Label.new()
	nlabel.text = "Set name:"
	nrow.add_child(nlabel)
	_set_name_edit = LineEdit.new()
	_set_name_edit.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_set_name_edit.placeholder_text = "(set name)"
	_set_name_edit.text_submitted.connect(func(_t): _rename_current_set())
	_set_name_edit.focus_exited.connect(_rename_current_set)
	nrow.add_child(_set_name_edit)
	add_child(nrow)

	var scroll := ScrollContainer.new()
	scroll.size_flags_vertical = Control.SIZE_EXPAND_FILL
	scroll.custom_minimum_size = Vector2(0, 160)
	_list = VBoxContainer.new()
	_list.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	scroll.add_child(_list)
	add_child(scroll)

	var buttons := HBoxContainer.new()
	var add_btn := Button.new()
	add_btn.text = "Add image"
	add_btn.pressed.connect(_on_add_pressed)
	buttons.add_child(add_btn)
	var build_btn := Button.new()
	build_btn.text = "Build blob"
	build_btn.pressed.connect(_on_build_pressed)
	buttons.add_child(build_btn)
	add_child(buttons)

	_status = RichTextLabel.new()
	_status.fit_content = true
	_status.custom_minimum_size = Vector2(0, 60)
	_status.bbcode_enabled = true
	add_child(_status)

	_file_dialog = EditorFileDialog.new()
	_file_dialog.file_mode = EditorFileDialog.FILE_MODE_OPEN_FILE
	_file_dialog.access = EditorFileDialog.ACCESS_RESOURCES
	_file_dialog.add_filter("*.jpg,*.jpeg,*.png", "Images")
	_file_dialog.file_selected.connect(_on_file_selected)
	add_child(_file_dialog)

# --- manifest I/O --------------------------------------------------------------------------------

func _manifest_path() -> String:
	return _manifest_edit.text if _manifest_edit else DEFAULT_MANIFEST

func _manifest_dir() -> String:
	return _manifest_path().get_base_dir()

## Load the manifest as a `{ sets: [...] }` dict (normalizing a legacy `{ blob, images }` into one set).
func _load_manifest() -> Dictionary:
	var path := _manifest_path()
	var data = null
	if FileAccess.file_exists(path):
		var f := FileAccess.open(path, FileAccess.READ)
		data = JSON.parse_string(f.get_as_text())
		f.close()
	if typeof(data) != TYPE_DICTIONARY:
		data = {}
	if not data.has("sets"):
		if data.has("images"):
			data = {"sets": [{"name": "default", "blob": data.get("blob", "reference.bin"), "images": data["images"]}]}
		else:
			data = {"sets": []}
	return data

## The current set dict (creating a default one if the manifest has none).
func _cur_set(data: Dictionary) -> Dictionary:
	var sets: Array = data["sets"]
	if sets.is_empty():
		sets.append({"name": "default", "blob": "reference.bin", "images": []})
		_current_set = 0
	_current_set = clampi(_current_set, 0, sets.size() - 1)
	return sets[_current_set]

func _save_manifest(data: Dictionary) -> void:
	var f := FileAccess.open(_manifest_path(), FileAccess.WRITE)
	if f:
		f.store_string(JSON.stringify(data, "  "))
		f.close()
		EditorInterface.get_resource_filesystem().scan()

# --- image list ----------------------------------------------------------------------------------

func _refresh_list() -> void:
	var data := _load_manifest()
	var sets: Array = data["sets"]
	# Populate the set selector.
	_set_selector.clear()
	for s in sets:
		_set_selector.add_item(str(s.get("name", "?")))
	if not sets.is_empty():
		_current_set = clampi(_current_set, 0, sets.size() - 1)
		_set_selector.select(_current_set)
	# Reflect the current set's name in the rename field.
	if _set_name_edit:
		_set_name_edit.text = str(_cur_set(data).get("name", "")) if not sets.is_empty() else ""
	# Show the current set's images.
	for c in _list.get_children():
		c.queue_free()
	var images: Array = _cur_set(data).get("images", []) if not sets.is_empty() else []
	for i in images.size():
		_list.add_child(_make_row(i, images[i]))
	if images.is_empty():
		var empty := Label.new()
		empty.text = "(No images — use \"Add image\".)"
		empty.modulate = Color(1, 1, 1, 0.6)
		_list.add_child(empty)

func _make_row(index: int, img: Dictionary) -> Control:
	var row := HBoxContainer.new()
	var name_label := Label.new()
	name_label.text = str(img.get("image", "?"))
	name_label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	name_label.clip_text = true
	row.add_child(name_label)

	# Physical size X / Y in metres — the per-image size passed to init_image_database at runtime
	# (ManagedReferenceImage.size). Height defaults to width for back-compat with width-only manifests.
	var w := float(img.get("width", 0.2))
	var h := float(img.get("height", w))
	row.add_child(_size_field("X (m):", w, func(v): _set_image_size(index, false, v)))
	row.add_child(_size_field("Y (m):", h, func(v): _set_image_size(index, true, v)))

	var rm := Button.new()
	rm.text = "Remove"
	rm.pressed.connect(func(): _remove(index))
	row.add_child(rm)
	return row

## A "<label> [SpinBox]" metres pair for a physical-size field; calls `on_changed(value)` on edit.
func _size_field(label: String, value: float, on_changed: Callable) -> Control:
	var box := HBoxContainer.new()
	var l := Label.new()
	l.text = label
	box.add_child(l)
	var sb := SpinBox.new()
	sb.min_value = 0.01
	sb.max_value = 100.0
	sb.step = 0.01
	sb.value = value
	sb.value_changed.connect(on_changed)
	box.add_child(sb)
	return box

## Update one image's physical width (`is_y=false`) or height (`is_y=true`) in the current set.
func _set_image_size(index: int, is_y: bool, v: float) -> void:
	var data := _load_manifest()
	var images: Array = _cur_set(data).get("images", [])
	if index >= 0 and index < images.size():
		var key := "height" if is_y else "width"
		images[index][key] = v
		_save_manifest(data)

func _remove(index: int) -> void:
	var data := _load_manifest()
	var images: Array = _cur_set(data).get("images", [])
	if index >= 0 and index < images.size():
		images.remove_at(index)
		_save_manifest(data)
		_refresh_list()

# --- sets --------------------------------------------------------------------------------------

func _on_add_set() -> void:
	var data := _load_manifest()
	var sets: Array = data["sets"]
	var n := sets.size() + 1
	sets.append({"name": "set%d" % n, "blob": "set%d.bin" % n, "images": []})
	_current_set = sets.size() - 1
	_save_manifest(data)
	_refresh_list()
	_set_status("Added set: set%d" % n)

func _on_remove_set() -> void:
	var data := _load_manifest()
	var sets: Array = data["sets"]
	if _current_set >= 0 and _current_set < sets.size():
		var removed := str(sets[_current_set].get("name", "?"))
		sets.remove_at(_current_set)
		_current_set = maxi(0, _current_set - 1)
		_save_manifest(data)
		_refresh_list()
		_set_status("Removed set: %s" % removed)

## Rename the current set to the text in the set-name field (updates the manifest + selector). No-op on
## an empty name or when unchanged.
func _rename_current_set() -> void:
	if _set_name_edit == null:
		return
	var new_name := _set_name_edit.text.strip_edges()
	if new_name.is_empty():
		return
	var data := _load_manifest()
	var sets: Array = data["sets"]
	if _current_set < 0 or _current_set >= sets.size():
		return
	if str(sets[_current_set].get("name", "")) == new_name:
		return
	sets[_current_set]["name"] = new_name
	_save_manifest(data)
	_refresh_list()
	_set_status("Renamed set -> %s" % new_name)

# --- add image -----------------------------------------------------------------------------------

func _on_add_pressed() -> void:
	_file_dialog.popup_file_dialog()

func _on_file_selected(res_path: String) -> void:
	# Copy the picked image next to the manifest (so the DB build + the packed demo are self-contained),
	# then add an entry to the current set with a fresh guid + default width.
	var dir := _manifest_dir()
	DirAccess.make_dir_recursive_absolute(dir)
	var dest_name := res_path.get_file()
	var dest := dir.path_join(dest_name)
	if res_path != dest:
		var err := DirAccess.copy_absolute(res_path, dest)
		if err != OK:
			_set_status("[color=red]Image copy failed: %s (err %d)[/color]" % [dest, err])
			return
	var data := _load_manifest()
	_cur_set(data)["images"].append({
		"guid": _gen_guid(),
		"width": 0.2,
		"height": 0.2,
		"image": dest_name,
		"label": dest_name.get_basename(),
	})
	_save_manifest(data)
	_refresh_list()
	_set_status("Added: %s (set the width, then \"Build blob\")" % dest_name)

func _gen_guid() -> String:
	var g := ""
	for i in 4:
		g += "%08x" % (randi() & 0xffffffff)
	return g

# --- build ---------------------------------------------------------------------------------------

func _tool_path() -> String:
	var name := "trackableImageTools.exe" if OS.get_name() == "Windows" else "trackableImageTools"
	return ProjectSettings.globalize_path("res://addons/godot_xreal/tools/".path_join(name))

func _on_build_pressed() -> void:
	var tool_path := _tool_path()
	if not FileAccess.file_exists(tool_path):
		_set_status("[color=red]trackableImageTools not found: %s\nRun scripts/vendor_xreal_libs (or the XREAL Import dock)[/color]" % tool_path)
		return
	var data := _load_manifest()
	if data["sets"].is_empty():
		_set_status("[color=orange]No sets (use \"Add set\")[/color]")
		return
	var cur := _cur_set(data)
	if bool(cur.get("prebuilt", false)):
		_set_status("[color=orange]Set '%s' is prebuilt (%s) — no build needed[/color]" % [cur.get("name", "?"), cur.get("blob", "")])
		return
	var images: Array = cur.get("images", [])
	if images.is_empty():
		_set_status("[color=orange]This set has no images[/color]")
		return
	var dir_abs := ProjectSettings.globalize_path(_manifest_dir())
	# Image-list config: <guid>|<abs image path>|<width> per line.
	var lines := PackedStringArray()
	for img in images:
		var img_abs := dir_abs.path_join(str(img.get("image", "")))
		if not FileAccess.file_exists(ProjectSettings.localize_path(img_abs)) and not FileAccess.file_exists(img_abs):
			_set_status("[color=red]Image not found: %s[/color]" % img_abs)
			return
		lines.append("%s|%s|%s" % [img.get("guid", ""), img_abs, img.get("width", 0.2)])
	var list_path := OS.get_cache_dir().path_join("xreal_imglist_%d.txt" % (Time.get_ticks_usec()))
	var lf := FileAccess.open(list_path, FileAccess.WRITE)
	lf.store_string("\n".join(lines))
	lf.close()

	var blob_abs := dir_abs.path_join(str(cur.get("blob", "reference.bin")))
	var output := []
	_set_status("Building …")
	var code := OS.execute(tool_path, ["--images_config_file", list_path, "--save_path", blob_abs], output, true)
	DirAccess.remove_absolute(list_path)
	var log := "\n".join(output)
	var scores := ""
	for line in log.split("\n"):
		if line.contains("Total Score"):
			scores += line.strip_edges() + "\n"
	var ok := code == 0 and FileAccess.file_exists(ProjectSettings.localize_path(blob_abs))
	if not ok:
		# Fall back to checking the absolute path (blob_abs is outside res:// only if manifest is).
		ok = code == 0 and FileAccess.file_exists(blob_abs)
	if ok:
		EditorInterface.get_resource_filesystem().scan()
		_set_status("[color=green]Built: %s (set '%s' / %d image(s))[/color]\n%s" % [cur.get("blob"), cur.get("name", "?"), images.size(), scores])
	else:
		_set_status("[color=red]Build failed (exit %d)[/color]\n%s" % [code, log.left(600)])

func _set_status(bbcode: String) -> void:
	if _status:
		_status.text = bbcode
