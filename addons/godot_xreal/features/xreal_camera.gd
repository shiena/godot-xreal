extends Node3D
## XREAL glasses RGB camera as a drop-in feature component: owns the XrealCameraFeed lifecycle
## (registered with the CameraServer) and an optional head-locked live preview quad.
##
## Drop addons/godot_xreal/features/xreal_camera.tscn anywhere in the scene and call
## set_enabled(true) (or set `enabled` in the inspector). The capture itself starts lazily once
## head tracking is live — starting it before the glasses/tracking are up races the session.
## One instance per tree: the glasses have a single RGB camera (a second activation fails cleanly).
##
## Other feature components (photo capture, blend capture, streaming) discover the live feed via
## XrealShared.find_camera_feed() — no wiring needed. One Series only: devices without an RGB
## camera (e.g. Air 2 Ultra) refuse set_enabled(true).

## The live XrealCameraFeed after each start/stop (null on stop).
signal feed_changed(feed: Object)
## The actual camera state: true once the capture started, false on stop OR on an async start
## failure (wedged camera) — wire this to any UI toggle so it reflects reality.
signal active_changed(active: bool)

## Start the camera at boot (applied in _ready). At runtime call set_enabled().
@export var enabled := false
## Show the head-locked live preview quad (reparented under the XrealHeadTracker when live).
@export var show_preview := true

var _system: Object          # XrealSystem (this feature's own stateless instance)
var _feed: Object            # XrealCameraFeed while the capture runs
# Set once the RGB capture fails to start (wedged glasses camera), so _process stops re-attempting
# setup — a hard failure isn't retried; re-plug the glasses and re-enable to recover.
var _failed := false
var _want := false           # camera requested (feed creation is lazy in _process)
var _panel: MeshInstance3D

func _ready() -> void:
	# The .tscn already carries the group; re-add for script-only (code-built) instances.
	add_to_group(XrealShared.GROUP_CAMERA)
	_panel = $PreviewPanel
	_system = XrealShared.make_system()  # null off-device -> the component stays inert
	if enabled:
		enabled = set_enabled(true)

## The live XrealCameraFeed, or null while the camera is off / not yet started.
func get_feed() -> Object:
	return _feed

## True once the capture runs AND the first frame created the Y/CbCr textures.
func is_feed_live() -> bool:
	return _feed != null and _feed.get_y_texture() != null and _feed.get_cbcr_texture() != null

## Toggle the camera. Returns the resulting state — false right away when the device has no RGB
## camera. A true return only means "requested": the capture starts lazily once tracking is live,
## and a start failure (wedged camera) is reported through active_changed(false).
func set_enabled(on: bool) -> bool:
	if _system == null:
		enabled = false
		return false
	if on:
		# Gate on the device actually having an RGB camera (IsHMDFeatureSupported). The Air 2 Ultra
		# has none — opening it there froze the app.
		if _system.has_method(&"is_camera_supported") and not _system.is_camera_supported():
			push_warning("[xreal-camera] this device has no RGB camera (e.g. Air 2 Ultra) — camera unavailable")
			enabled = false
			return false
		_failed = false
		_want = true
		enabled = true
	else:
		_want = false
		enabled = false
		if _feed:
			if _feed.is_active():
				_feed.set_active(false)
			CameraServer.remove_feed(_feed)
			_feed = null
			feed_changed.emit(null)
		if _panel:
			_panel.visible = false
		active_changed.emit(false)
	return enabled

## Expose the RGB camera as a Godot CameraFeed and start the capture. The panel's shader samples
## the feed's Y (R8) + CbCr (RG8) ImageTextures DIRECTLY (a CameraTexture on a script-fed feed
## only shows Godot's placeholder) — matching the XREAL SDK's YUVTransRGB sample.
func _setup_feed(tracker: Node3D) -> void:
	if not ClassDB.class_exists(&"XrealCameraFeed"):
		_failed = true
		return
	# Runtime CAMERA permission (also grant via `adb shell pm grant … android.permission.CAMERA`).
	if OS.has_feature("android"):
		OS.request_permission("android.permission.CAMERA")

	_feed = ClassDB.instantiate(&"XrealCameraFeed")
	# Name it so it's identifiable among CameraServer.feeds() — the XREAL glasses camera is NOT an
	# Android Camera2 device, so it only exists as this feed.
	_feed.set_name("XREAL Glasses RGB")
	CameraServer.add_feed(_feed)
	_feed.set_active(true)  # -> activate_feed() starts the XREAL capture
	if not _feed.is_active():
		# The XREAL capture didn't start: an unclean prior exit left the glasses camera wedged
		# ("Recv Frame, -99"). Re-plug the glasses to reset it. Don't show an unfed (pink) panel or
		# spin re-attempting — disable cleanly for this run.
		push_warning("[xreal-camera] XREAL RGB camera did not start (glasses camera wedged? re-plug to reset) — camera disabled")
		CameraServer.remove_feed(_feed)
		_feed = null
		_failed = true
		return

	# Head-locked preview: reparent the panel under the tracker so it follows the gaze; rendered
	# by the eye SubViewports (shared world). Its corner position/size are set in xreal_camera.tscn.
	if show_preview and _panel and tracker and _panel.get_parent() != tracker:
		_panel.reparent(tracker, false)
	# Diagnostic: the RGB camera geometry (Unity space) from libXREALXRPlugin — confirms the
	# device/camera-param APIs return real data. See docs/plans/coordinate-systems-notes.md.
	if _system.has_method(&"get_camera_intrinsics"):
		var comp := 2  # XREALComponent.RGB_CAMERA
		print("[cam-geom] RGB res=%s intrinsics[fx,fy,cx,cy]=%s" % [_system.get_device_resolution(comp), _system.get_camera_intrinsics(comp)])
		print("[cam-geom] RGB pose_from_head[px,py,pz,qx,qy,qz,qw]=%s" % [_system.get_device_pose_from_head(comp)])
		print("[cam-geom] RGB projection=%s" % [_system.get_camera_projection_matrix(comp, 0.1, 100.0)])
	feed_changed.emit(_feed)
	active_changed.emit(true)

func _process(_delta: float) -> void:
	# Lazily start the capture ONLY once head tracking is live — before that the session races.
	if _want and not _failed and _feed == null:
		var tracker := XrealShared.find_head_tracker(get_tree())
		if tracker and tracker.has_method(&"is_tracking") and tracker.is_tracking():
			_setup_feed(tracker)
			if _failed:
				_want = false
				enabled = false
				active_changed.emit(false)
	# Pump the feed; wire the Y/CbCr ImageTextures into the preview shader once the first frame
	# made them, then reveal the panel (hidden until now so a not-yet-fed shader never shows as a
	# pink unset-sampler placeholder). They update in place afterwards, so this happens once.
	if _feed:
		_feed.poll_frame()
		if _panel and show_preview and not _panel.visible:
			var mat: ShaderMaterial = _panel.material_override
			if mat:
				var yt = _feed.get_y_texture()
				var ct = _feed.get_cbcr_texture()
				if yt and ct:
					mat.set_shader_parameter(&"y_texture", yt)
					mat.set_shader_parameter(&"cbcr_texture", ct)
					_panel.visible = true

func _exit_tree() -> void:
	# Best-effort camera release on a *graceful* shutdown so the glasses RGB camera is handed back
	# instead of staying wedged. NOTE: a hard render-thread crash (SIGSEGV) can't be intercepted —
	# after a crash the camera stays held and must be re-plugged; this only covers clean exits.
	if _feed and _feed.is_active():
		_feed.set_active(false)
	# The preview panel lives under the tracker once live — take it down with us.
	if _panel and is_instance_valid(_panel) and _panel.get_parent() != self:
		_panel.queue_free()
