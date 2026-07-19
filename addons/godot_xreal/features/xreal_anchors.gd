extends Node3D
## Spatial anchors as a drop-in feature component (Air 2 Ultra). place_at_fingertip() — or a pinch
## gesture — places an anchor at the index fingertip via XrealSystem.acquire_anchor(). Each tracked
## anchor gets a world-locked marker; save_anchor is retried until the SLAM map is good enough
## (INSUFFICIENT → no Guid), then the Guid is persisted to `save_file` and reloaded (load_anchor)
## next launch.
##
## World-locked: add this component under a world-fixed node (e.g. the scene root, NOT the head
## rig) so a marker stays pinned to the same real-world spot as the head moves. Anchor changes
## stream in through the shared XrealAR poller; the shared XrealHandTracker is ensured on first
## enable so the fingertip/pinch placement works with just this scene dropped in.

# XRHandTracker joint ordinals (OpenXR): thumb tip / index tip.
## Emitted when an operation fails or the feature is unavailable, so the load site can react
## (show UI, log, flip a toggle). Carries the same human-readable text also pushed as a warning.
signal error(message: String)


const TIP_THUMB := 5
const TIP_INDEX := 10
# Pinch trigger with hysteresis so one pinch = one anchor (thumb–index tip distance, metres).
const PINCH_ON := 0.025
const PINCH_OFF := 0.045
const HANDS := ["/user/hand_tracker/right", "/user/hand_tracker/left"]

## Enable at boot (applied in _ready). At runtime call set_enabled().
@export var enabled := false
## Where the saved anchor Guids persist across launches.
@export var save_file := "user://anchors.json"

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar: Object                     # the shared XrealAR poller
var _connected := false
var _initialized := false           # one-time subsystem enable + mapping dir + reload done
var _enabled := false               # active (placement + polling + markers shown)
var _markers := {}                  # anchor id(String) -> MeshInstance3D
var _anchor_pose := {}              # anchor id(String) -> Transform3D (latest tracked pose)
var _saved_guids := {}              # anchor id(String) -> Guid string (once saved / loaded)
var _pending := {}                  # anchor id(String) -> true (placed, not yet saved)
var _pinching := {}                 # tracker name -> bool (hysteresis latch)
var _retry_frames := 0              # throttles the save-retry loop

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert
	if enabled:
		enabled = set_enabled(true)

## Toggle anchor mode. Returns the resulting state (false if the anchor ABI is unavailable, so a
## UI toggle can flip itself back off). OFF keeps the SDK subsystem enabled (so anchors stay
## tracked) and just hides the markers — turning it back ON restores them without re-placing.
func set_enabled(on: bool) -> bool:
	if not _system or not _system.has_method(&"is_anchor_available") or not _system.is_anchor_available():
		enabled = false
		if on:
			error.emit("[xreal-anchors] spatial anchors unavailable on this device (Air 2 Ultra only)")
		return false
	if on:
		_ensure_ar()
		XrealShared.get_hand_tracker(get_tree())  # fingertip/pinch placement needs the hand trackers
		if not _initialized:
			_system.set_anchor_enabled(true)
			_system.set_anchor_mapping_dir(OS.get_user_data_dir())
			_load_saved()
			_initialized = true
		_enabled = true
		visible = true
	else:
		_enabled = false
		visible = false  # hide markers but keep them + the subsystem alive
	if _ar:
		_ar.set(&"anchors", _enabled)  # only let the shared XrealAR poll the anchor stream while on
	enabled = _enabled
	return _enabled

## Resolve the shared XrealAR and connect its anchor signals once — BEFORE the stream switch goes
## on, so no change event is ever polled without a listener.
func _ensure_ar() -> void:
	if _connected:
		return
	_ar = XrealShared.get_ar(get_tree())
	if _ar == null:
		return
	_ar.connect(&"anchor_added", _on_anchor_changed)
	_ar.connect(&"anchor_updated", _on_anchor_changed)
	_ar.connect(&"anchor_removed", _on_anchor_removed)
	_connected = true

## Place at whichever hand is currently tracked (index fingertip).
func place_at_fingertip() -> void:
	if not _enabled:
		return
	for tname in HANDS:
		var tracker := XRServer.get_tracker(tname) as XRHandTracker
		if tracker and tracker.get_has_tracking_data():
			_place_anchor(tracker.get_hand_joint_transform(TIP_INDEX))
			return
	_fail("[xreal-anchors] no hand tracked — hold a hand up to place")

func _process(_delta: float) -> void:
	if not _enabled or not _system:
		return
	_check_pinch()
	# Spatial anchors can only be persisted once the SLAM map around them is good enough, so keep
	# retrying save for placed-but-unsaved anchors (~every 0.5 s) until quality reaches SUFFICIENT.
	_retry_frames += 1
	if _retry_frames >= 30:
		_retry_frames = 0
		_retry_saves()

## Pinch detection: on each hand, a thumb-tip↔index-tip close-then-open drops one anchor at the
## index fingertip.
func _check_pinch() -> void:
	for tname in HANDS:
		var tracker := XRServer.get_tracker(tname) as XRHandTracker
		if tracker == null or not tracker.get_has_tracking_data():
			_pinching[tname] = false
			continue
		var thumb := tracker.get_hand_joint_transform(TIP_THUMB).origin
		var index_t := tracker.get_hand_joint_transform(TIP_INDEX)
		var d := thumb.distance_to(index_t.origin)
		var was: bool = _pinching.get(tname, false)
		if not was and d < PINCH_ON:
			_pinching[tname] = true
			_vibrate(20)  # short phone haptic, matching on-screen buttons
			_place_anchor(index_t)
		elif was and d > PINCH_OFF:
			_pinching[tname] = false

## Short phone-vibration haptic. No-op off Android.
func _vibrate(ms: int) -> void:
	if OS.has_feature("android"):
		Input.vibrate_handheld(ms)

## Create an anchor at a world `Transform3D` and show it. Persistence (save_anchor) is retried later
## from _retry_saves once the map quality is good enough — saving right away usually fails INSUFFICIENT.
func _place_anchor(pose: Transform3D) -> void:
	var a: Dictionary = _system.acquire_anchor(pose)
	if a.is_empty():
		_fail("[xreal-anchors] acquire failed")
		return
	var id: String = a.get("id", "")
	if id.is_empty():
		return
	_update_marker(a)
	_pending[id] = true
	var q: int = _system.estimate_anchor_quality(id, pose)
	print("[xreal-anchors] placed %s quality=%d (%d tracked, %d saved)" % [id, q, _markers.size(), _saved_guids.size()])
	_try_save(id)

## Retry saving every placed-but-unsaved anchor; once save_anchor returns a Guid the map is good
## enough and it is persisted to disk.
func _retry_saves() -> void:
	for id in _pending.keys():
		_try_save(id)

func _try_save(id: String) -> void:
	if not _pending.has(id):
		return
	var guid: String = _system.save_anchor(id)
	if not guid.is_empty():
		_saved_guids[id] = guid
		_pending.erase(id)
		_persist_guids()
		_update_marker_tint(id)  # flip to "saved" green
		print("[xreal-anchors] saved %s -> %s" % [id, guid])

## XrealAR signal: an anchor was added / its tracked pose updated (the source of truth for pose).
func _on_anchor_changed(a: Dictionary) -> void:
	if _enabled:
		_update_marker(a)

## XrealAR signal: an anchor was removed from the tracking set.
func _on_anchor_removed(id: String) -> void:
	if _enabled:
		_remove_marker(id)

func _update_marker(a: Dictionary) -> void:
	var id: String = a.get("id", "")
	if id.is_empty():
		return
	var mi: MeshInstance3D = _markers.get(id)
	if mi == null:
		mi = _make_marker()
		add_child(mi)
		_markers[id] = mi
	var t: Transform3D = a.get("transform", Transform3D.IDENTITY)
	mi.transform = t
	_anchor_pose[id] = t
	_tint(mi, id, a.get("tracking_state", 0))

## Tint: green = saved/persisted, cyan = live tracking, gray = limited/not tracking.
func _tint(mi: MeshInstance3D, id: String, state: int) -> void:
	var mat := mi.material_override as StandardMaterial3D
	if _saved_guids.has(id):
		mat.albedo_color = Color(0.3, 1.0, 0.4)
	elif state == 2:  # Tracking
		mat.albedo_color = Color(0.3, 0.8, 1.0)
	else:
		mat.albedo_color = Color(0.6, 0.6, 0.6)

func _update_marker_tint(id: String) -> void:
	var mi: MeshInstance3D = _markers.get(id)
	if mi:
		_tint(mi, id, 2)

func _remove_marker(id: String) -> void:
	var mi: MeshInstance3D = _markers.get(id)
	if mi:
		mi.queue_free()
		_markers.erase(id)
	_anchor_pose.erase(id)
	_pending.erase(id)

## A small unshaded box, elongated in Z so the anchor's facing is visible (proves it holds
## orientation as well as position).
func _make_marker() -> MeshInstance3D:
	var mi := MeshInstance3D.new()
	var box := BoxMesh.new()
	box.size = Vector3(0.05, 0.05, 0.09)
	mi.mesh = box
	var mat := StandardMaterial3D.new()
	mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	mat.albedo_color = Color(0.3, 0.8, 1.0)
	mi.material_override = mat
	return mi

## Reload previously-saved anchors: load_anchor restores each into the tracking set (so the anchor
## stream then reports it) and returns its current pose for an immediate marker.
func _load_saved() -> void:
	if not FileAccess.file_exists(save_file):
		return
	var f := FileAccess.open(save_file, FileAccess.READ)
	if f == null:
		return
	var text := f.get_as_text()
	f.close()
	var data = JSON.parse_string(text)
	if typeof(data) != TYPE_ARRAY:
		return
	var loaded := 0
	for guid in data:
		var s := str(guid)
		var a: Dictionary = _system.load_anchor(s)
		if a.is_empty():
			_fail("[xreal-anchors] load failed for %s (different space?)" % s)
			continue
		var id: String = a.get("id", "")
		if id.is_empty():
			continue
		_saved_guids[id] = s
		_update_marker(a)
		loaded += 1
	print("[xreal-anchors] reloaded %d/%d saved anchor(s)" % [loaded, data.size()])

func _persist_guids() -> void:
	var arr := []
	for id in _saved_guids:
		arr.append(_saved_guids[id])
	var f := FileAccess.open(save_file, FileAccess.WRITE)
	if f:
		f.store_string(JSON.stringify(arr))
		f.close()

func _exit_tree() -> void:
	# Release the shared stream switch (the shared XrealAR outlives us).
	if _enabled and _ar and is_instance_valid(_ar):
		_ar.set(&"anchors", false)

## Push a warning AND emit `error` so the load site can detect the failure (not just see the log).
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
