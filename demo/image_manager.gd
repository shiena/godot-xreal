extends Node3D
## Image-tracking demo / on-device verification (Air 2 Ultra). Driven by the phone-menu "画像" toggle
## (main.gd). On first enable it loads the reference-image DB blob (built by scripts/build_image_db.ps1
## from demo/image_tracking/reference.json), activates it, and overlays a world-locked quad on each
## tracked image at its reported pose/size. Display/print one of the manifest's images for the glasses
## to see (default: demo/image_tracking/reference.jpg — the XREAL logo).
##
## World-locked: child of Main (like the hand joints / anchors), so a marker sits on the real image as
## the head moves. OFF hides the markers but keeps the database active so ON restores them.

const MANIFEST := "res://demo/image_tracking/reference.json"

var _system: Object                 # XrealSystem, injected by main.gd via setup()
var _initialized := false           # database loaded + activated once
var _enabled := false
var _handle := 0                    # active image-DB handle (0 = none)
var _markers := {}                  # image id(String) -> MeshInstance3D

## Injected once by main.gd after the rig spawns.
func setup(system: Object) -> void:
	_system = system

## Toggle image-tracking mode. Returns the resulting state (false if the ABI/blob is unavailable, so
## the phone-menu toggle can flip itself back off).
func set_enabled(on: bool) -> bool:
	if not _system or not _system.has_method(&"is_image_tracking_available") or not _system.is_image_tracking_available():
		return false
	if on:
		if not _initialized:
			if not _load_database():
				return false
			_initialized = true
		_enabled = true
		visible = true
	else:
		_enabled = false
		visible = false  # keep the DB active; just hide the markers
	return _enabled

## Load the manifest + blob, build the tracking database, and activate it.
func _load_database() -> bool:
	if not FileAccess.file_exists(MANIFEST):
		push_warning("[image] manifest missing: %s" % MANIFEST)
		return false
	var mf := FileAccess.open(MANIFEST, FileAccess.READ)
	var data = JSON.parse_string(mf.get_as_text())
	mf.close()
	if typeof(data) != TYPE_DICTIONARY:
		push_warning("[image] bad manifest %s" % MANIFEST)
		return false
	var blob_path := MANIFEST.get_base_dir().path_join(str(data.get("blob", "reference.bin")))
	if not FileAccess.file_exists(blob_path):
		push_warning("[image] DB blob missing: %s — run scripts/build_image_db.ps1" % blob_path)
		return false
	var bf := FileAccess.open(blob_path, FileAccess.READ)
	var blob := bf.get_buffer(bf.get_length())
	bf.close()
	var guids := PackedStringArray()
	var sizes := PackedVector2Array()
	for img in data.get("images", []):
		guids.append(str(img.get("guid", "")))
		var w := float(img.get("width", 0.1))
		sizes.append(Vector2(w, w))
	_handle = _system.init_image_database(blob, guids, sizes)
	if _handle == 0:
		push_warning("[image] init_image_database failed (needs 6DoF + nr_plugins.json + backend)")
		return false
	_system.set_image_database(_handle)
	print("[image] database ready handle=%d refs=%d images=%d" % [_handle, _system.image_reference_count(_handle), guids.size()])
	return true

func _process(_delta: float) -> void:
	if not _enabled or not _system:
		return
	var ch: Dictionary = _system.poll_images()
	for im in ch.get("added", []):
		_update_marker(im)
		print("[image] detected %s" % str((im as Dictionary).get("source_image", "")))
	for im in ch.get("updated", []):
		_update_marker(im)
	for id in ch.get("removed", []):
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
	mi.transform = im.get("transform", Transform3D.IDENTITY)
	var sz: Vector2 = im.get("size", Vector2(0.1, 0.1))
	var qm := mi.mesh as QuadMesh
	if qm and sz.length() > 0.001:
		qm.size = sz
	# green quad while tracking, gray when limited/not tracking.
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

func _exit_tree() -> void:
	# Deactivate + free the database on clean shutdown.
	if _handle != 0 and _system:
		_system.set_image_database(0)
		_system.release_image_database(_handle)
		_handle = 0
