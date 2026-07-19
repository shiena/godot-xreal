extends Node3D
## Image tracking as a drop-in feature component (Air 2 Ultra). On first enable it loads EVERY set
## in the manifest JSON (each set = one blob built by scripts/build_image_db.* / the editor dock),
## builds a database per set (XrealSystem.init_image_database), activates the first, and overlays a
## world-locked quad on each tracked image. cycle_set() switches the active set. Point `manifest_path`
## at a reference.json (see demo/image_tracking/reference.json for the schema): top-level `sets[]`,
## each {name, blob, images:[{guid, width, height?}]} with blob paths relative to the manifest.
##
## World-locked: add this component under a world-fixed node (e.g. the scene root, NOT the head
## rig) so a marker sits on the real image as the head moves. OFF hides the markers but keeps the
## databases active so ON restores them.

## Emitted whenever an operation fails or the feature is unavailable (unbuilt/missing blob, DB init
## failure, manifest not set…), so the load site can react — show UI, log, flip a toggle. Carries
## the same human-readable text also pushed as a warning.
signal error(message: String)

## Emitted with the active set's name when the active tracking set changes (on load and on
## cycle_set), so the load site can label a "cycle" button with the current set.
signal set_changed(name: String)

## Enable at boot (applied in _ready). At runtime call set_enabled().
@export var enabled := false
## The reference-image manifest (JSON). Required — set_enabled(true) refuses while empty.
@export_file("*.json") var manifest_path := ""
## Optional override for the tracked-image overlay material. Each marker gets its own duplicate.
## A ShaderMaterial declaring a `tracking` bool uniform receives the per-marker tracking state
## (replacing the default material's green/gray albedo tint).
@export var marker_material: Material

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar: Object                     # the shared XrealAR poller
var _connected := false
var _initialized := false           # sets loaded + registered once
var _enabled := false
var _sets := []                     # [{name: String, handle: int}] — one registered DB per set
var _active_set := -1               # index into _sets of the currently-active set
var _markers := {}                  # image id(String) -> MeshInstance3D

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert
	if enabled:
		enabled = set_enabled(true)

## Toggle image-tracking mode. Returns the resulting state (false if the ABI / manifest / blobs are
## unavailable, so a UI toggle can flip itself back off).
func set_enabled(on: bool) -> bool:
	if not _system or not _system.has_method(&"is_image_tracking_available") or not _system.is_image_tracking_available():
		enabled = false
		if on:
			error.emit("[xreal-image] image tracking unavailable on this device (Air 2 Ultra only)")
		return false
	if on:
		if manifest_path.is_empty():
			_fail("[xreal-image] manifest_path not set — point it at a reference.json")
			enabled = false
			return false
		_ensure_ar()
		if not _initialized:
			if not _load_sets():
				enabled = false
				return false
			_initialized = true
		_enabled = true
		visible = true
	else:
		_enabled = false
		visible = false  # keep the databases active; just hide the markers
	if _ar:
		_ar.set(&"images", _enabled)  # only let the shared XrealAR poll the image stream while on
	enabled = _enabled
	return _enabled

## Resolve the shared XrealAR and connect its image signals once — BEFORE the stream switch goes
## on, so no change event is ever polled without a listener.
func _ensure_ar() -> void:
	if _connected:
		return
	_ar = XrealShared.get_ar(get_tree())
	if _ar == null:
		return
	_ar.connect(&"image_added", _on_image_added)
	_ar.connect(&"image_updated", _on_image_updated)
	_ar.connect(&"image_removed", _on_image_removed)
	_connected = true

## Load every set in the manifest, register a database for each, and activate the first.
func _load_sets() -> bool:
	var data := _read_manifest()
	var sets: Array = data.get("sets", [])
	# Backward-compat: a bare { blob, images } manifest is treated as one "default" set.
	if sets.is_empty() and data.has("images"):
		sets = [{"name": "default", "blob": data.get("blob", "reference.bin"), "images": data["images"]}]
	for s in sets:
		var handle := _init_set(s)
		if handle != 0:
			_sets.append({"name": str(s.get("name", "?")), "handle": handle})
	if _sets.is_empty():
		_fail("[xreal-image] no image sets loaded (build the blobs — editor dock / build_image_db)")
		return false
	_activate(0)
	print("[xreal-image] %d set(s) loaded; active='%s'" % [_sets.size(), _sets[0].name])
	return true

## Build + register one set's database. Returns its handle (0 on failure).
func _init_set(s: Dictionary) -> int:
	var name := str(s.get("name", "?"))
	var blob_path := manifest_path.get_base_dir().path_join(str(s.get("blob", "")))
	if not FileAccess.file_exists(blob_path):
		_fail("[xreal-image] set '%s' blob missing: %s — build it (editor dock / build_image_db)" % [name, blob_path])
		return 0
	var bf := FileAccess.open(blob_path, FileAccess.READ)
	var blob := bf.get_buffer(bf.get_length())
	bf.close()
	var guids := PackedStringArray()
	var sizes := PackedVector2Array()
	for img in s.get("images", []):
		guids.append(str(img.get("guid", "")))
		# Physical size X/Y (metres). Height defaults to width for back-compat with width-only manifests.
		var w := float(img.get("width", 0.1))
		var h := float(img.get("height", w))
		sizes.append(Vector2(w, h))
	var handle: int = _system.init_image_database(blob, guids, sizes)
	if handle == 0:
		_fail("[xreal-image] set '%s' init_image_database failed (needs 6DoF + nr_plugins.json + backend)" % name)
	return handle

## Activate a set (switch the tracking DB) and clear the previous set's markers.
func _activate(index: int) -> void:
	if index < 0 or index >= _sets.size():
		return
	_active_set = index
	_system.set_image_database(_sets[index].handle)
	_clear_markers()  # a different set tracks different images
	set_changed.emit(str(_sets[index].name))
	print("[xreal-image] active set -> '%s' (handle=%d)" % [_sets[index].name, _sets[index].handle])

## Cycle to the next set. No-op with 0/1 set.
func cycle_set() -> void:
	if _sets.size() <= 1:
		return
	_activate((_active_set + 1) % _sets.size())

func _read_manifest() -> Dictionary:
	if not FileAccess.file_exists(manifest_path):
		_fail("[xreal-image] manifest missing: %s" % manifest_path)
		return {}
	var mf := FileAccess.open(manifest_path, FileAccess.READ)
	var data = JSON.parse_string(mf.get_as_text())
	mf.close()
	return data if typeof(data) == TYPE_DICTIONARY else {}

## XrealAR signal: an image was first detected.
func _on_image_added(im: Dictionary) -> void:
	if not _enabled:
		return
	_update_marker(im)
	print("[xreal-image] detected %s" % str(im.get("source_image", "")))

## XrealAR signal: a tracked image's pose/state updated.
func _on_image_updated(im: Dictionary) -> void:
	if _enabled:
		_update_marker(im)

## XrealAR signal: a tracked image was lost.
func _on_image_removed(id: String) -> void:
	if _enabled:
		_remove_marker(id)

func _update_marker(im: Dictionary) -> void:
	var id: String = im.get("id", "")
	if id.is_empty():
		return
	var mi: MeshInstance3D = _markers.get(id)
	if mi == null:
		mi = _make_marker()
		add_child(mi)
		_markers[id] = mi
	# The SDK reports the tracked-image pose with its normal along a different axis than Godot's
	# QuadMesh, so the raw pose lays the quad flat. Post-multiply -90° about local X to stand it up
	# coplanar with the image: this makes it a Godot-friendly frame — device-verified with the
	# marker cues, +X (yellow) → the image's right and +Y (white) → up, a proper (non-mirrored)
	# basis, so child content attaches with the expected orientation. (Which face reads green/red is
	# just the QuadMesh winding — cosmetic, not an orientation error.)
	var t: Transform3D = im.get("transform", Transform3D.IDENTITY)
	mi.transform = t * Transform3D(Basis(Vector3(1.0, 0.0, 0.0), -PI / 2.0), Vector3.ZERO)
	var sz: Vector2 = im.get("size", Vector2(0.1, 0.1))
	var qm := mi.mesh as QuadMesh
	if qm and sz.length() > 0.001:
		qm.size = sz
	_tint_marker(mi, int(im.get("tracking_state", 0)) == 2)

## Tracking-state tint. Default material: green while tracking, gray when limited/not tracking.
## A custom `marker_material` (ShaderMaterial) gets the state as its `tracking` uniform instead.
func _tint_marker(mi: MeshInstance3D, tracking: bool) -> void:
	var mat := mi.material_override
	if mat is StandardMaterial3D:
		mat.albedo_color = Color(0.3, 1.0, 0.5, 0.4) if tracking else Color(0.6, 0.6, 0.6, 0.3)
	elif mat is ShaderMaterial:
		mat.set_shader_parameter(&"tracking", tracking)

## A translucent quad sized to the tracked image (updated per frame), unshaded + double-sided.
func _make_marker() -> MeshInstance3D:
	var mi := MeshInstance3D.new()
	var q := QuadMesh.new()
	q.size = Vector2(0.1, 0.1)
	mi.mesh = q
	if marker_material:
		mi.material_override = marker_material.duplicate()  # own copy: per-marker tracking tint
	else:
		var mat := StandardMaterial3D.new()
		mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
		mat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
		mat.cull_mode = BaseMaterial3D.CULL_DISABLED
		mat.albedo_color = Color(0.3, 1.0, 0.5, 0.4)
		mi.material_override = mat
	return mi

func _remove_marker(id: String) -> void:
	var mi: MeshInstance3D = _markers.get(id)
	if mi:
		mi.queue_free()
		_markers.erase(id)

func _clear_markers() -> void:
	for id in _markers:
		(_markers[id] as MeshInstance3D).queue_free()
	_markers.clear()

func _exit_tree() -> void:
	# Release the shared stream switch, then deactivate + free every registered database.
	if _enabled and _ar and is_instance_valid(_ar):
		_ar.set(&"images", false)
	if _system and not _sets.is_empty():
		_system.set_image_database(0)
		for s in _sets:
			_system.release_image_database(s.handle)
		_sets.clear()

## Push a warning AND emit `error` so the load site can detect the failure (not just see the log).
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
