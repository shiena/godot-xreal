extends Node3D
## Spatial-anchor demo / on-device verification (Air 2 Ultra). Driven by the phone-menu "アンカー"
## toggle + "配置" button (main.gd), plus a pinch gesture — both place an anchor at the index
## fingertip via XrealSystem.acquire_anchor(). Each tracked anchor (from poll_anchors) gets a
## world-locked marker; save_anchor is retried until the SLAM map is good enough (INSUFFICIENT →
## no Guid), then the Guid is persisted to user://anchors.json and reloaded (load_anchor) next launch.
##
## World-locked: this node is a child of Main (like the hand joints — see main.gd / hand_visualizer),
## so a marker stays pinned to the same real-world spot as the head moves. That world-lock, and the
## save→restart→reload round-trip, are the two things this verifies.

# XRHandTracker joint ordinals (OpenXR): thumb tip / index tip.
const TIP_THUMB := 5
const TIP_INDEX := 10
# Pinch trigger with hysteresis so one pinch = one anchor (thumb–index tip distance, metres).
const PINCH_ON := 0.025
const PINCH_OFF := 0.045
const HANDS := ["/user/hand_tracker/right", "/user/hand_tracker/left"]
const SAVE_FILE := "user://anchors.json"

var _system: Object                 # XrealSystem, injected by main.gd via setup()
var _ar: Object                     # XrealAR node (per-frame poller → signals), injected via setup()
var _initialized := false           # one-time subsystem enable + mapping dir + reload done
var _enabled := false               # demo active (placement + polling + markers shown)
var _markers := {}                  # anchor id(String) -> MeshInstance3D
var _anchor_pose := {}              # anchor id(String) -> Transform3D (latest tracked pose)
var _saved_guids := {}              # anchor id(String) -> Guid string (once saved / loaded)
var _pending := {}                  # anchor id(String) -> true (placed, not yet saved)
var _pinching := {}                 # tracker name -> bool (hysteresis latch)
var _retry_frames := 0              # throttles the save-retry loop

## Injected once by main.gd after the rig spawns. `ar` is the shared XrealAR node whose
## anchor_added / anchor_updated / anchor_removed signals keep the markers in sync.
func setup(system: Object, ar: Object = null) -> void:
	_system = system
	_ar = ar
	if _ar:
		_ar.connect(&"anchor_added", _on_anchor_changed)
		_ar.connect(&"anchor_updated", _on_anchor_changed)
		_ar.connect(&"anchor_removed", _on_anchor_removed)

## Toggle anchor mode. Returns the resulting state (false if the anchor ABI is unavailable, so the
## phone-menu toggle can flip itself back off). OFF keeps the SDK subsystem enabled (so anchors stay
## tracked) and just hides the markers — turning it back ON restores them without re-placing.
func set_enabled(on: bool) -> bool:
	if not _system or not _system.has_method(&"is_anchor_available") or not _system.is_anchor_available():
		return false
	if on:
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
		_ar.set(&"anchors", _enabled)  # only let XrealAR poll the anchor stream while we're on
	return _enabled

## Phone-menu "配置" button → place at whichever hand is currently tracked (index fingertip).
func place_at_fingertip() -> void:
	if not _enabled:
		return
	for tname in HANDS:
		var tracker := XRServer.get_tracker(tname) as XRHandTracker
		if tracker and tracker.get_has_tracking_data():
			_place_anchor(tracker.get_hand_joint_transform(TIP_INDEX))
			return
	push_warning("[anchor] no hand tracked — hold a hand up to place")

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

## Pinch detection: on each hand, a thumb-tip↔index-tip close-then-open drops one anchor at the index
## fingertip.
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
			_vibrate(20)  # same short haptic as the 配置 button (touch_controller._vibrate)
			_place_anchor(index_t)
		elif was and d > PINCH_OFF:
			_pinching[tname] = false

## Short phone-vibration haptic (matches the on-screen buttons). No-op off Android.
func _vibrate(ms: int) -> void:
	if OS.has_feature("android"):
		Input.vibrate_handheld(ms)

## Create an anchor at a world `Transform3D` and show it. Persistence (save_anchor) is retried later
## from _retry_saves once the map quality is good enough — saving right away usually fails INSUFFICIENT.
func _place_anchor(pose: Transform3D) -> void:
	var a: Dictionary = _system.acquire_anchor(pose)
	if a.is_empty():
		push_warning("[anchor] acquire failed")
		return
	var id: String = a.get("id", "")
	if id.is_empty():
		return
	_update_marker(a)
	_pending[id] = true
	var q: int = _system.estimate_anchor_quality(id, pose)
	print("[anchor] placed %s quality=%d (%d tracked, %d saved)" % [id, q, _markers.size(), _saved_guids.size()])
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
		print("[anchor] saved %s -> %s" % [id, guid])

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

## A small unshaded box, elongated in Z so the anchor's facing is visible (proves it holds orientation
## as well as position).
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

## Reload previously-saved anchors: load_anchor restores each into the tracking set (so poll_anchors
## then reports it) and returns its current pose for an immediate marker.
func _load_saved() -> void:
	if not FileAccess.file_exists(SAVE_FILE):
		return
	var f := FileAccess.open(SAVE_FILE, FileAccess.READ)
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
			push_warning("[anchor] load failed for %s (different space?)" % s)
			continue
		var id: String = a.get("id", "")
		if id.is_empty():
			continue
		_saved_guids[id] = s
		_update_marker(a)
		loaded += 1
	print("[anchor] reloaded %d/%d saved anchor(s)" % [loaded, data.size()])

func _persist_guids() -> void:
	var arr := []
	for id in _saved_guids:
		arr.append(_saved_guids[id])
	var f := FileAccess.open(SAVE_FILE, FileAccess.WRITE)
	if f:
		f.store_string(JSON.stringify(arr))
		f.close()
