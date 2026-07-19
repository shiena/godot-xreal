extends Node3D
## Image-tracking demo / on-device verification (Air 2 Ultra). Driven by the phone-menu "画像" toggle +
## "画像切替" button (main.gd). On first enable it loads EVERY set in demo/image_tracking/reference.json
## (each set = one blob built by scripts/build_image_db.* / the editor dock from its images), builds a
## database per set (XrealSystem.init_image_database), activates the first, and overlays a world-locked
## quad on each tracked image. "画像切替" cycles the active set (set_image_database). Display/print one of
## the active set's images for the glasses to see.
##
## World-locked: child of Main (like the hand joints / anchors), so a marker sits on the real image as
## the head moves. OFF hides the markers but keeps the databases active so ON restores them.

const MANIFEST := "res://demo/image_tracking/reference.json"

var _system: Object                 # XrealSystem, injected by main.gd via setup()
var _ar: Object                     # XrealAR node (per-frame poller → signals), injected via setup()
var _initialized := false           # sets loaded + registered once
var _enabled := false
var _sets := []                     # [{name: String, handle: int}] — one registered DB per set
var _active_set := -1               # index into _sets of the currently-active set
var _markers := {}                  # image id(String) -> MeshInstance3D

## Injected once by main.gd after the rig spawns. `ar` is the shared XrealAR node whose
## image_added / image_updated / image_removed signals drive the overlay.
func setup(system: Object, ar: Object = null) -> void:
	_system = system
	_ar = ar
	if _ar:
		_ar.connect(&"image_added", _on_image_added)
		_ar.connect(&"image_updated", _on_image_updated)
		_ar.connect(&"image_removed", _on_image_removed)

## Toggle image-tracking mode. Returns the resulting state (false if the ABI/sets are unavailable, so
## the phone-menu toggle can flip itself back off).
func set_enabled(on: bool) -> bool:
	if not _system or not _system.has_method(&"is_image_tracking_available") or not _system.is_image_tracking_available():
		return false
	if on:
		if not _initialized:
			if not _load_sets():
				return false
			_initialized = true
		_enabled = true
		visible = true
	else:
		_enabled = false
		visible = false  # keep the databases active; just hide the markers
	if _ar:
		_ar.set(&"images", _enabled)  # only let XrealAR poll the image stream while we're on
	return _enabled

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
		push_warning("[image] no image sets loaded (build the blobs — editor dock / build_image_db)")
		return false
	_activate(0)
	print("[image] %d set(s) loaded; active='%s'" % [_sets.size(), _sets[0].name])
	return true

## Build + register one set's database. Returns its handle (0 on failure).
func _init_set(s: Dictionary) -> int:
	var name := str(s.get("name", "?"))
	var blob_path := MANIFEST.get_base_dir().path_join(str(s.get("blob", "")))
	if not FileAccess.file_exists(blob_path):
		push_warning("[image] set '%s' blob missing: %s — build it (editor dock / build_image_db)" % [name, blob_path])
		return 0
	var bf := FileAccess.open(blob_path, FileAccess.READ)
	var blob := bf.get_buffer(bf.get_length())
	bf.close()
	var guids := PackedStringArray()
	var sizes := PackedVector2Array()
	for img in s.get("images", []):
		guids.append(str(img.get("guid", "")))
		var w := float(img.get("width", 0.1))
		sizes.append(Vector2(w, w))
	var handle: int = _system.init_image_database(blob, guids, sizes)
	if handle == 0:
		push_warning("[image] set '%s' init_image_database failed (needs 6DoF + nr_plugins.json + backend)" % name)
	return handle

## Activate a set (switch the tracking DB) and clear the previous set's markers.
func _activate(index: int) -> void:
	if index < 0 or index >= _sets.size():
		return
	_active_set = index
	_system.set_image_database(_sets[index].handle)
	_clear_markers()  # a different set tracks different images
	print("[image] active set -> '%s' (handle=%d)" % [_sets[index].name, _sets[index].handle])

## Cycle to the next set (phone-menu "画像切替" button). No-op with 0/1 set.
func cycle_set() -> void:
	if _sets.size() <= 1:
		return
	_activate((_active_set + 1) % _sets.size())

func _read_manifest() -> Dictionary:
	if not FileAccess.file_exists(MANIFEST):
		push_warning("[image] manifest missing: %s" % MANIFEST)
		return {}
	var mf := FileAccess.open(MANIFEST, FileAccess.READ)
	var data = JSON.parse_string(mf.get_as_text())
	mf.close()
	return data if typeof(data) == TYPE_DICTIONARY else {}

## XrealAR signal: an image was first detected.
func _on_image_added(im: Dictionary) -> void:
	if not _enabled:
		return
	_update_marker(im)
	print("[image] detected %s" % str(im.get("source_image", "")))

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
	# The SDK reports the tracked-image pose with its normal along a different axis than Godot's QuadMesh
	# (+Z), so the raw pose lays the quad flat (on-device: horizontal, +Z/green facing down). Rotate -90°
	# about local X to stand the quad up coplanar with the image, normal toward the viewer. See the
	# device-verification checklist #7 (orientation).
	var t: Transform3D = im.get("transform", Transform3D.IDENTITY)
	mi.transform = t * Transform3D(Basis(Vector3(1.0, 0.0, 0.0), -PI / 2.0), Vector3.ZERO)
	var sz: Vector2 = im.get("size", Vector2(0.1, 0.1))
	var qm := mi.mesh as QuadMesh
	if qm and sz.length() > 0.001:
		qm.size = sz
	# Green while tracking, gray when limited/not tracking.
	var state: int = im.get("tracking_state", 0)
	var mat := mi.material_override as StandardMaterial3D
	mat.albedo_color = Color(0.3, 1.0, 0.5, 0.4) if state == 2 else Color(0.6, 0.6, 0.6, 0.3)

## A translucent quad sized to the tracked image (updated per frame), unshaded + double-sided.
func _make_marker() -> MeshInstance3D:
	var mi := MeshInstance3D.new()
	var q := QuadMesh.new()
	q.size = Vector2(0.1, 0.1)
	mi.mesh = q
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
	# Deactivate + free every registered database on clean shutdown.
	if _system and not _sets.is_empty():
		_system.set_image_database(0)
		for s in _sets:
			_system.release_image_database(s.handle)
		_sets.clear()
