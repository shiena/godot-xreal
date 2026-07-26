extends Node3D
## XREAL glasses RGB camera as a drop-in feature component: it owns the XrealCameraFeed lifecycle,
## registered with the CameraServer. Showing the feed is the app's choice, since the component only
## exposes it; the demo draws a head-locked preview off the shared feed in demo/camera_preview.gd.
##
## Drop addons/godot_xreal/features/xreal_camera.tscn anywhere in the scene and call
## set_enabled(true), or set `enabled` in the inspector. The capture itself starts lazily once head
## tracking is live, because starting it before the glasses and tracking are up races the session.
## Keep one instance per tree: the glasses have a single RGB camera, and a second activation fails
## cleanly.
##
## The other feature components (photo capture, blend capture, streaming) discover the live feed
## through XrealShared.find_camera_feed(), with no wiring needed. One Series only: a device without
## an RGB camera, such as the Air 2 Ultra, refuses set_enabled(true).

## Emitted when an operation fails or the feature is unavailable, so the load site can react by
## showing UI, logging, or flipping a toggle. It carries the same human-readable text that is also
## pushed as a warning.
signal error(message: String)

## The live XrealCameraFeed after each start/stop (null on stop).
signal feed_changed(feed: Object)
## The actual camera state: true once the capture started, false on stop or on an async start
## failure such as a wedged camera. Wire this to any UI toggle so it reflects reality.
signal active_changed(active: bool)

## Start the camera at boot (applied in _ready). At runtime call set_enabled().
@export var enabled := false

var _system: Object          # XrealSystem (this feature's own stateless instance)
var _feed: Object            # XrealCameraFeed while the capture runs
# Set once the RGB capture fails to start (wedged glasses camera), so _process stops re-attempting
# setup. A hard failure is never retried: re-plug the glasses and re-enable to recover.
var _failed := false
var _want := false           # camera requested (feed creation is lazy in _process)
var _started_ms := 0         # when the capture started, for the first-frame watchdog below

## First-frame watchdog. The wedged glasses camera has TWO signatures: (a) Start… returns the
## failure sentinel, which _setup_feed catches, and (b) Start… "succeeds" but no frame ever
## arrives. Case (b) was observed 2026-07-21 after an app kill mid-capture: handle=0, zero frames,
## and the stuck pipeline even destabilised SLAM into a position runaway. If no frame lands within
## this window, treat it as wedged: fail loudly, with "wedged" in the message so the demo can pop a
## dialog on it, and shut the feed down instead of polling a dead camera forever.
const FIRST_FRAME_TIMEOUT_MS := 5000

func _ready() -> void:
	# The .tscn already carries the group; re-add it for script-only (code-built) instances.
	add_to_group(XrealShared.GROUP_CAMERA)
	_system = XrealShared.make_system()  # null off-device -> the component stays inert
	if enabled:
		enabled = set_enabled(true)

## The live XrealCameraFeed, or null while the camera is off or not yet started.
func get_feed() -> Object:
	return _feed

## True once the capture runs AND the first frame created the Y/CbCr textures.
func is_feed_live() -> bool:
	return _feed != null and _feed.get_y_texture() != null and _feed.get_cbcr_texture() != null

## Toggle the camera and return the resulting state, which is false right away when the device has
## no RGB camera. A true return only means "requested": the capture starts lazily once tracking is
## live, and a start failure such as a wedged camera is reported through active_changed(false).
func set_enabled(on: bool) -> bool:
	if _system == null:
		enabled = false
		return false
	if on:
		# Gate on the device actually having an RGB camera (IsHMDFeatureSupported). The Air 2 Ultra has
		# none, and opening it there froze the app.
		if _system.has_method(&"is_camera_supported") and not _system.is_camera_supported():
			_fail("[xreal-camera] this device has no RGB camera (e.g. Air 2 Ultra), so the camera is unavailable")
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
## Y (R8) and CbCr (RG8) ImageTextures DIRECTLY, matching the XREAL SDK's YUVTransRGB sample,
## because a CameraTexture on a script-fed feed only shows Godot's placeholder.
func _setup_feed() -> void:
	if not ClassDB.class_exists(&"XrealCameraFeed"):
		_failed = true
		return
	# No CAMERA permission is requested here on purpose: the glasses camera is a USB (UVC) device the
	# SDK drives through libusb (libnr_rgb_camera.so imports the libusb API, provided by
	# libnr_libusb.so), NOT a Camera2/HAL device, so android.permission.CAMERA does not gate it.
	# Verified on device 2026-07-25: with CAMERA revoked the capture still reported "capture started"
	# and streamed 2400+ frames, and the app never appeared as a cameraserver client.
	_feed = ClassDB.instantiate(&"XrealCameraFeed")
	# Name it so it is identifiable among CameraServer.feeds(). The XREAL glasses camera is NOT an
	# Android Camera2 device, so it exists only as this feed.
	_feed.set_name("XREAL Glasses RGB")
	CameraServer.add_feed(_feed)
	_feed.set_active(true)  # -> activate_feed() starts the XREAL capture
	if not _feed.is_active():
		# The XREAL capture did not start: an unclean prior exit left the glasses camera wedged
		# ("Recv Frame, -99"). Re-plug the glasses to reset it. Rather than show an unfed (pink) panel or
		# spin on re-attempts, disable cleanly for this run.
		_fail("[xreal-camera] XREAL RGB camera did not start (glasses camera wedged? re-plug the USB AND restart the app; the native session cannot rebind a replugged camera), camera disabled")
		CameraServer.remove_feed(_feed)
		_feed = null
		_failed = true
		return

	# Diagnostic: the RGB camera geometry (Unity space) from libXREALXRPlugin, which confirms that the
	# device and camera-param APIs return real data. See docs/plans/coordinate-systems-notes.md.
	if _system.has_method(&"get_camera_intrinsics"):
		var comp := 2  # XREALComponent.RGB_CAMERA
		print("[cam-geom] RGB res=%s intrinsics[fx,fy,cx,cy]=%s" % [_system.get_device_resolution(comp), _system.get_camera_intrinsics(comp)])
		print("[cam-geom] RGB pose_from_head[px,py,pz,qx,qy,qz,qw]=%s" % [_system.get_device_pose_from_head(comp)])
		print("[cam-geom] RGB projection=%s" % [_system.get_camera_projection_matrix(comp, 0.1, 100.0)])
	feed_changed.emit(_feed)
	active_changed.emit(true)
	_started_ms = Time.get_ticks_msec()

func _process(_delta: float) -> void:
	# Lazily start the capture ONLY once head tracking is live; before that the session races.
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
		# First-frame watchdog: Start… can "succeed" on a wedged camera that then never delivers a
		# frame (see FIRST_FRAME_TIMEOUT_MS). get_y_texture() stays null until the first frame.
		if _feed.get_y_texture() == null and Time.get_ticks_msec() - _started_ms > FIRST_FRAME_TIMEOUT_MS:
			_fail("[xreal-camera] camera started but no frame arrived within %ds: glasses camera wedged (re-plug the USB AND restart the app), camera disabled" % (FIRST_FRAME_TIMEOUT_MS / 1000))
			_feed.set_active(false)
			CameraServer.remove_feed(_feed)
			_feed = null
			_failed = true
			_want = false
			enabled = false
			feed_changed.emit(null)
			active_changed.emit(false)

func _exit_tree() -> void:
	# Best-effort camera release on a *graceful* shutdown, so the glasses RGB camera is handed back
	# instead of staying wedged. NOTE: a hard render-thread crash (SIGSEGV) cannot be intercepted, so
	# after a crash the camera stays held and must be re-plugged. This covers clean exits only.
	if _feed and _feed.is_active():
		_feed.set_active(false)
	if _feed:
		CameraServer.remove_feed(_feed)
		_feed = null

## Push a warning AND emit `error`, so the load site can detect the failure instead of only seeing
## it in the log.
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
