extends Node3D
## XREAL glasses RGB camera as a drop-in feature component: owns the XrealCameraFeed lifecycle
## (registered with the CameraServer). Showing the feed is the app's choice — the component only
## exposes it; the demo draws a head-locked preview off the shared feed in demo/camera_preview.gd.
##
## Drop addons/godot_xreal/features/xreal_camera.tscn anywhere in the scene and call
## set_enabled(true) (or set `enabled` in the inspector). The capture itself starts lazily once
## head tracking is live — starting it before the glasses/tracking are up races the session.
## One instance per tree: the glasses have a single RGB camera (a second activation fails cleanly).
##
## Other feature components (photo capture, blend capture, streaming) discover the live feed via
## XrealShared.find_camera_feed() — no wiring needed. One Series only: devices without an RGB
## camera (e.g. Air 2 Ultra) refuse set_enabled(true).

## Emitted when an operation fails or the feature is unavailable, so the load site can react
## (show UI, log, flip a toggle). Carries the same human-readable text also pushed as a warning.
signal error(message: String)

## The live XrealCameraFeed after each start/stop (null on stop).
signal feed_changed(feed: Object)
## The actual camera state: true once the capture started, false on stop OR on an async start
## failure (wedged camera) — wire this to any UI toggle so it reflects reality.
signal active_changed(active: bool)

## Start the camera at boot (applied in _ready). At runtime call set_enabled().
@export var enabled := false

var _system: Object          # XrealSystem (this feature's own stateless instance)
var _feed: Object            # XrealCameraFeed while the capture runs
# Set once the RGB capture fails to start (wedged glasses camera), so _process stops re-attempting
# setup — a hard failure isn't retried; re-plug the glasses and re-enable to recover.
var _failed := false
var _want := false           # camera requested (feed creation is lazy in _process)
var _started_ms := 0         # when the capture started, for the first-frame watchdog below

## First-frame watchdog. The wedged glasses camera has TWO signatures: (a) Start… returns the
## failure sentinel (caught at _setup_feed), and (b) Start… "succeeds" but no frame ever arrives —
## observed 2026-07-21 after an app kill mid-capture (handle=0, zero frames, and the stuck pipeline
## even destabilised SLAM into a position runaway). If no frame lands within this window, treat it
## as wedged: fail loudly (the message carries "wedged" — the demo pops a dialog on it) and shut
## the feed down instead of polling a dead camera forever.
const FIRST_FRAME_TIMEOUT_MS := 5000

func _ready() -> void:
	# The .tscn already carries the group; re-add for script-only (code-built) instances.
	add_to_group(XrealShared.GROUP_CAMERA)
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
			_fail("[xreal-camera] this device has no RGB camera (e.g. Air 2 Ultra) — camera unavailable")
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
		active_changed.emit(false)
	return enabled

## Expose the RGB camera as a Godot CameraFeed and start the capture. Consumers sample the feed's
## Y (R8) + CbCr (RG8) ImageTextures DIRECTLY (a CameraTexture on a script-fed feed only shows
## Godot's placeholder) — matching the XREAL SDK's YUVTransRGB sample.
func _setup_feed() -> void:
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
		_fail("[xreal-camera] XREAL RGB camera did not start (glasses camera wedged? re-plug the USB AND restart the app — the native session can't rebind a replugged camera) — camera disabled")
		CameraServer.remove_feed(_feed)
		_feed = null
		_failed = true
		return

	# Diagnostic: the RGB camera geometry (Unity space) from libXREALXRPlugin — confirms the
	# device/camera-param APIs return real data. See docs/plans/coordinate-systems-notes.md.
	if _system.has_method(&"get_camera_intrinsics"):
		var comp := 2  # XREALComponent.RGB_CAMERA
		print("[cam-geom] RGB res=%s intrinsics[fx,fy,cx,cy]=%s" % [_system.get_device_resolution(comp), _system.get_camera_intrinsics(comp)])
		print("[cam-geom] RGB pose_from_head[px,py,pz,qx,qy,qz,qw]=%s" % [_system.get_device_pose_from_head(comp)])
		print("[cam-geom] RGB projection=%s" % [_system.get_camera_projection_matrix(comp, 0.1, 100.0)])
	feed_changed.emit(_feed)
	active_changed.emit(true)
	_started_ms = Time.get_ticks_msec()

func _process(_delta: float) -> void:
	# Lazily start the capture ONLY once head tracking is live — before that the session races.
	if _want and not _failed and _feed == null:
		var tracker := XrealShared.find_head_tracker(get_tree())
		if tracker and tracker.has_method(&"is_tracking") and tracker.is_tracking():
			_setup_feed()
			if _failed:
				_want = false
				enabled = false
				active_changed.emit(false)
	# Pump the feed so its Y/CbCr ImageTextures stay current for the consumers (preview, photo,
	# blend, streaming) that sample them.
	if _feed:
		_feed.poll_frame()
		# First-frame watchdog: Start… can "succeed" on a wedged camera that then never delivers
		# a frame (see FIRST_FRAME_TIMEOUT_MS). get_y_texture() is null until the first frame.
		if _feed.get_y_texture() == null and Time.get_ticks_msec() - _started_ms > FIRST_FRAME_TIMEOUT_MS:
			_fail("[xreal-camera] camera started but no frame arrived within %ds — glasses camera wedged (re-plug the USB AND restart the app) — camera disabled" % (FIRST_FRAME_TIMEOUT_MS / 1000))
			_feed.set_active(false)
			CameraServer.remove_feed(_feed)
			_feed = null
			_failed = true
			_want = false
			enabled = false
			feed_changed.emit(null)
			active_changed.emit(false)

func _exit_tree() -> void:
	# Best-effort camera release on a *graceful* shutdown so the glasses RGB camera is handed back
	# instead of staying wedged. NOTE: a hard render-thread crash (SIGSEGV) can't be intercepted —
	# after a crash the camera stays held and must be re-plugged; this only covers clean exits.
	if _feed and _feed.is_active():
		_feed.set_active(false)

## Push a warning AND emit `error` so the load site can detect the failure (not just see the log).
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
