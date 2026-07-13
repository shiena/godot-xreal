extends Node3D

## Minimal 3DoF demo for the Godot XREAL addon.
##
## Builds a ring of colored boxes, instances the addon camera rig
## (addons/godot_xreal/xreal_rig.tscn — an XrealHeadTracker with a Camera3D child),
## and shows live SDK status via XrealSystem. On XREAL hardware the camera looks
## around with the wearer's head; on desktop the rig stays at identity so the scene
## is still runnable.

# The GDExtension classes (XrealHeadTracker / XrealSystem) only exist if the native
# extension loaded. We look everything up defensively so a missing/failed extension
# shows a diagnostic on screen instead of a blank scene — exactly the case to debug
# the "gray screen" on device.
const RIG_SCENE := "res://addons/godot_xreal/xreal_rig.tscn"

# XrealHeadTracker key/action constants, mirrored locally so this script parses even
# when the GDExtension is absent (desktop editor).
const XREAL_KEY_MULTI := 1
const XREAL_KEY_MENU := 4
const XREAL_ACTION_LONG_PRESS := 3

var _tracker: Node3D
var _system: Object
var _status: Label
var _extension_loaded := false
# XREAL RGB camera as a Godot CameraFeed (see docs/camera-feed-plan.md), shown on a head-locked
# quad in front of the eye cameras via a YCbCr→RGB shader.
var _cam_feed: Object
var _cam_panel: MeshInstance3D
var _camera_enabled := false
# Set once the RGB capture fails to start (wedged glasses camera), so _process stops re-attempting
# setup — a hard failure isn't retried; re-plug the glasses and relaunch to recover.
var _cam_failed := false
const CAM_SHADER := "res://demo/xreal_ycbcr.gdshader"
# Phase C path B: phone IMU (via NRController state) drives a 3D pointer (demo/phone_pointer.gd).
const PHONE_POINTER := "res://demo/phone_pointer.gd"
var _phone_pointer_enabled := true
var _controller_started := false
var _imu_poll_count := 0
var _phone_pointer: Node3D
# On-screen touch controller (phone screen) — the Godot analog of XREAL's XREALVirtualController
# prefab. Pure GDScript touch UI; renders only on the phone's root viewport, so the glasses show
# the 3D world while the phone shows the controller. Drives a head-locked 3D cursor as a demo.
const TOUCH_CONTROLLER := "res://demo/touch_controller.gd"
var _touch_controller: Control
var _cursor: MeshInstance3D
var _cursor_mat: StandardMaterial3D

func _ready() -> void:
	_try_register_android_bridge()
	_extension_loaded = ClassDB.class_exists(&"XrealSystem") and ClassDB.class_exists(&"XrealHeadTracker")
	if _extension_loaded:
		_system = ClassDB.instantiate(&"XrealSystem")
		# (No stereo-mode selector: the port always uses Multipass. Multiview is shelved
		# -- docs/codex-righteye-analysis.md -- reachable only via `setprop debug.xreal.force_multiview 1`.)
		# Head-tracking mode from the project setting `xreal/tracking_type`
		# (0 = 6DoF [recommended], 1 = 3DoF, 2 = 0DoF). Same rules as above -- read once at
		# bootstrap; absent (-1) falls back to the `debug.xreal.tracking_type` property / default.
		var tracking_type := int(ProjectSettings.get_setting("xreal/tracking_type", -1))
		if tracking_type >= 0 and _system.has_method(&"set_tracking_type"):
			_system.set_tracking_type(tracking_type)
		# The RGB camera shares the tracking camera with 6DoF SLAM, so enabling the camera in 6DoF
		# breaks head tracking (NRSDK "GetPoseWithStates failed" -> identity pose). When the camera is
		# on, force 3DoF (IMU-only orientation; the DISP pose still carries full pitch/yaw/roll).
		_camera_enabled = bool(ProjectSettings.get_setting("xreal/enable_camera", true))
		if _camera_enabled and _system.has_method(&"set_tracking_type"):
			_system.set_tracking_type(1)  # 3DoF, so the RGB camera and head tracking can coexist
	else:
		push_error("[demo] godot_xreal GDExtension not loaded — XrealSystem/XrealHeadTracker missing. Build the Android .so (cargo ndk) and check the .gdextension paths.")
	_build_environment()
	_build_room()
	_spawn_rig()
	_setup_ui()
	if bool(ProjectSettings.get_setting("xreal/enable_touch_controller", true)):
		_setup_touch_controller()
	_phone_pointer_enabled = bool(ProjectSettings.get_setting("xreal/enable_phone_pointer", true))
	# The camera is set up lazily in _process, only once head tracking is live (see _camera_enabled),
	# so starting the capture never races the glasses display/tracking bring-up.

func _try_register_android_bridge() -> void:
	if not OS.has_feature("android"):
		return
	if not Engine.has_singleton(&"AndroidRuntime"):
		return

	var runtime := Engine.get_singleton(&"AndroidRuntime")
	if runtime == null:
		return
	var activity = runtime.getActivity()
	if activity == null:
		return

	var bridge = JavaClassWrapper.wrap("com.godot.game.XrealBridge")
	if bridge == null:
		return

	# XrealBridge methods are idempotent; this is a Godot-side fallback for template drift.
	var register_bridge := func() -> void:
		bridge.register(activity)
		bridge.startCompanionOnXrealDisplayIfNeeded(activity)

	activity.runOnUiThread(runtime.createRunnableFromGodotCallable(register_bridge))

func _spawn_rig() -> void:
	if _extension_loaded:
		var rig := (load(RIG_SCENE) as PackedScene).instantiate()
		add_child(rig)
		_tracker = rig  # the rig's root node IS the XrealHeadTracker
		# Recenter the view to the current head direction once tracking goes live.
		if _tracker.has_signal(&"display_started"):
			_tracker.display_started.connect(_on_display_started)
		# React to glasses hot-plug (connect/disconnect) at runtime.
		# Glasses hardware inputs (One Pro: physical keys + wear sensor).
		if _tracker.has_signal(&"key_event"):
			_tracker.key_event.connect(_on_key_event)
			_tracker.wearing_changed.connect(_on_wearing_changed)
	else:
		# Fallback so the scene is still visible (and the panel explains why).
		var camera := Camera3D.new()
		camera.current = true
		add_child(camera)

func _setup_ui() -> void:
	_status = $UI/Panel/Margin/VBox/Status
	$UI/Panel.visible = false
	($UI/Panel/Margin/VBox/Recenter as Button).pressed.connect(_on_recenter_pressed)

## Expose the XREAL glasses RGB camera as a Godot CameraFeed (docs/camera-feed-plan.md), register it
## with the CameraServer, and show it in a corner preview — modeled on ~/dev/godot-camerafeed-demo.
## The feed is driven per-frame from _process (poll_frame grabs the latest frame → set_rgb_image).
func _setup_camera_feed() -> void:
	if not ClassDB.class_exists(&"XrealCameraFeed"):
		return
	# Runtime CAMERA permission (also grant via `adb shell pm grant … android.permission.CAMERA`).
	if OS.has_feature("android"):
		OS.request_permission("android.permission.CAMERA")

	_cam_feed = ClassDB.instantiate(&"XrealCameraFeed")
	# Name it so it's identifiable among CameraServer.feeds() — the XREAL glasses camera is NOT an
	# Android Camera2 device, so it only exists as this feed (Godot's built-in CameraAndroid feeds,
	# if you enable CameraServer.monitoring_feeds, are the HOST device's cameras — routed by id/class).
	_cam_feed.set_name("XREAL Glasses RGB")
	CameraServer.add_feed(_cam_feed)
	_cam_feed.set_active(true)  # -> activate_feed() starts the XREAL capture
	if not _cam_feed.is_active():
		# The XREAL capture didn't start. On this device that means the glasses RGB camera is wedged:
		# an unclean prior exit (e.g. a render-thread crash) left it holding the capture, so NRSDK
		# rejects the new connection ("Recv Frame, -99"). Re-plug the glasses to reset it. Don't show
		# an unfed (pink) panel or spin re-attempting — disable the preview cleanly for this run.
		push_warning("[demo] XREAL RGB camera did not start (glasses camera wedged? re-plug to reset) — preview disabled")
		CameraServer.remove_feed(_cam_feed)
		_cam_feed = null
		_cam_failed = true
		return

	# The shader samples the feed's Y (R8) + CbCr (RG8) ImageTextures DIRECTLY (get_y_texture /
	# get_cbcr_texture). A CameraTexture on a script-fed feed only shows Godot's placeholder, so we
	# bypass it — matching the XREAL SDK's YUVTransRGB sample. The textures are wired in _process once
	# the first frame has created them.
	# Orientation and the R/B swap are baked into the shader (device-calibrated constants).
	var mat := ShaderMaterial.new()
	mat.shader = load(CAM_SHADER)

	# A small head-locked preview (16:9) pinned to the top-right of the view. Parented under the
	# tracker (the head node), so it follows the gaze; rendered by the eye SubViewports (shared world).
	# The eye cameras see roughly +/-0.8 m horizontal, +/-0.45 m vertical at this 2 m depth (a 1.6x0.9
	# quad would fill the whole view), so a ~1/3-size quad tucked near that corner reads as a PiP.
	var quad := QuadMesh.new()
	quad.size = Vector2(0.5333, 0.3)  # 16:9 preserved
	_cam_panel = MeshInstance3D.new()
	_cam_panel.name = "XrealCameraPanel"
	_cam_panel.mesh = quad
	_cam_panel.material_override = mat
	# Hidden until the first real frame wires the textures (in _process), so the not-yet-fed shader
	# never shows as a pink (unset-sampler) placeholder.
	_cam_panel.visible = false
	# Top-right corner, 2 m in front (Godot cameras look down -Z). The eye cameras invert Y (the pose
	# handedness (x,-y,z,w) flip), so on the glasses buffer +X is right but -Y is up: hence +x, -y.
	_cam_panel.position = Vector3(0.48, -0.30, -2.0)
	if _tracker:
		_tracker.add_child(_cam_panel)
	else:
		add_child(_cam_panel)

## Add the phone-side on-screen touch controller and wire it to a head-locked 3D cursor. The
## controller lives on its own CanvasLayer (below $UI, so the debug text stays on top) and only
## renders on the phone; the glasses keep showing the 3D scene — the two screens are now distinct.
func _setup_touch_controller() -> void:
	var layer := CanvasLayer.new()
	layer.name = "TouchControllerLayer"
	layer.layer = 0
	add_child(layer)
	_touch_controller = (load(TOUCH_CONTROLLER) as Script).new()
	_touch_controller.name = "TouchController"
	layer.add_child(_touch_controller)
	_touch_controller.trigger_changed.connect(_on_tc_trigger)
	_touch_controller.grip_changed.connect(_on_tc_grip)
	_touch_controller.menu_pressed.connect(_on_tc_menu)
	_touch_controller.touchpad_moved.connect(_on_tc_touchpad)
	_touch_controller.touchpad_released.connect(_on_tc_touchpad_released)
	_touch_controller.hand_selected.connect(_on_tc_hand)

	# A head-locked cursor so phone touches are visible in the glasses (proves the split).
	if _tracker:
		var mesh := BoxMesh.new()
		mesh.size = Vector3(0.18, 0.18, 0.18)
		_cursor = MeshInstance3D.new()
		_cursor.name = "ControllerCursor"
		_cursor.mesh = mesh
		_cursor_mat = StandardMaterial3D.new()
		_cursor_mat.albedo_color = Color(0.3, 0.85, 1.0)
		_cursor.material_override = _cursor_mat
		_cursor.position = Vector3(0.0, 0.0, -2.0)
		_tracker.add_child(_cursor)

	# The phone shows the controller, not a 3D preview, so stop the rig's host-preview camera: the
	# root viewport no longer renders the world (one fewer full scene pass — the world was drawn 3×:
	# host preview + two eyes). The glasses are unaffected; they render from the extension's own
	# per-eye SubViewports. (Only when the controller is on; otherwise the preview stays for debugging.)
	if _tracker:
		var host_cam := _tracker.get_node_or_null(^"Camera3D") as Camera3D
		if host_cam:
			host_cam.current = false

func _on_tc_touchpad(value: Vector2) -> void:
	# Eye cameras invert Y (pose handedness), so -y maps the pad's "up" to up in the glasses.
	if _cursor:
		_cursor.position = Vector3(value.x * 0.8, -value.y * 0.5, -2.0)

func _on_tc_touchpad_released() -> void:
	if _cursor:
		_cursor.position = Vector3(0.0, 0.0, -2.0)

func _on_tc_trigger(pressed: bool) -> void:
	if _cursor_mat:
		_cursor_mat.albedo_color = Color(1.0, 0.4, 0.3) if pressed else Color(0.3, 0.85, 1.0)
	# Trigger click = select whatever the phone pointer is aiming at.
	if pressed and _phone_pointer and _phone_pointer.has_method(&"select"):
		_phone_pointer.select()

## Right/left hand toggle from the on-screen controller → flip the pointer's beam origin.
func _on_tc_hand(is_right: bool) -> void:
	if _phone_pointer and _phone_pointer.has_method(&"set_hand"):
		_phone_pointer.set_hand(is_right)

func _on_tc_grip(pressed: bool) -> void:
	if _cursor:
		_cursor.scale = Vector3.ONE * (1.6 if pressed else 1.0)

func _on_tc_menu() -> void:
	_on_recenter_pressed()
	if _phone_pointer:
		_phone_pointer.recenter()

## Instance the phone-IMU 3D pointer (demo/phone_pointer.gd). Added at the scene root so its aim is
## world-stable (driven by the phone), originating at the head (3DoF: head sits at the origin).
func _setup_phone_pointer() -> void:
	_phone_pointer = (load(PHONE_POINTER) as Script).new()
	_phone_pointer.name = "PhonePointer"
	add_child(_phone_pointer)

func _on_recenter_pressed() -> void:
	if _tracker and _tracker.has_method(&"recenter"):
		_tracker.recenter()

func _on_display_started() -> void:
	# Glasses display + tracking are live: make the current head direction "forward".
	if _tracker and _tracker.has_method(&"recenter"):
		_tracker.recenter()

func _on_key_event(key: int, action: int) -> void:
	# Long-press the MENU key to recenter (current head direction becomes "forward"),
	# replacing the on-screen button for a glasses-only workflow.
	if key == XREAL_KEY_MENU and action == XREAL_ACTION_LONG_PRESS:
		_on_recenter_pressed()
	# Long-press the MULTI key to quit the app (glasses-only exit).
	elif key == XREAL_KEY_MULTI and action == XREAL_ACTION_LONG_PRESS:
		get_tree().quit()

func _on_wearing_changed(wearing: bool) -> void:
	if wearing:
		# Recenter the instant the glasses are actually worn (and the wearer is looking
		# forward), so "forward" isn't captured while they sit tilted on a desk.
		_on_recenter_pressed()

func _process(_delta: float) -> void:
	# Lazily set up the camera ONLY once head tracking is live — starting the capture before the
	# glasses/tracking are up races (and in 6DoF would fight the SLAM camera). See _setup_camera_feed.
	if _camera_enabled and not _cam_failed and _cam_feed == null and _tracker and _tracker.has_method(&"is_tracking") \
			and _tracker.is_tracking():
		_setup_camera_feed()
	# Phase C path B: phone IMU (via NRController state) drives the 3D pointer. Godot's own IMU returns
	# all-zero on this host, so we read accel (gravity → pitch/roll) + gyro (yaw) from the controller.
	if _phone_pointer_enabled and _tracker and _tracker.has_method(&"is_tracking") and _tracker.is_tracking() and _system:
		if not _controller_started and _system.has_method(&"start_controller"):
			_controller_started = true
			_system.start_controller()
			_setup_phone_pointer()
		elif _phone_pointer and _system.has_method(&"poll_controller"):
			var s: PackedFloat32Array = _system.poll_controller()
			if s.size() >= 7 and s[0] > 0.5:
				var accel := Vector3(s[1], s[2], s[3])
				var gyro := Vector3(s[4], s[5], s[6])
				_phone_pointer.update_imu(accel, gyro, _delta, _tracker.global_transform)
				_imu_poll_count += 1
				if _imu_poll_count == 90:  # ~1.5 s in: capture the current aim as "forward"
					_phone_pointer.recenter()
	# Pump the XREAL camera feed. The session can come up a frame or two after _ready, so keep
	# (re)activating until it takes — the feed must be active for a frame to be produced.
	if _cam_feed:
		_cam_feed.poll_frame()
		# Wire the feed's Y/CbCr ImageTextures into the panel shader once the first frame made them,
		# then reveal the panel (kept hidden until now so a not-yet-fed shader never shows as pink).
		# They update in place afterwards, so this only needs to happen once.
		if _cam_panel and not _cam_panel.visible:
			var mat: ShaderMaterial = _cam_panel.material_override
			if mat:
				var yt = _cam_feed.get_y_texture()
				var ct = _cam_feed.get_cbcr_texture()
				if yt and ct:
					mat.set_shader_parameter(&"y_texture", yt)
					mat.set_shader_parameter(&"cbcr_texture", ct)
					_cam_panel.visible = true

func _exit_tree() -> void:
	# Best-effort camera release on a *graceful* shutdown (MULTI-quit, window close, scene change) so
	# the glasses RGB camera is handed back instead of staying wedged. deactivate_feed() -> the native
	# rgb_camera_stop. NOTE: a hard render-thread crash (SIGSEGV) can't be intercepted — Android's
	# libsigchain swallows signal handlers on ART threads (see src/native.rs) — so after a crash the
	# camera stays held and must be re-plugged; this only covers clean exits.
	if _cam_feed and _cam_feed.is_active():
		_cam_feed.set_active(false)

func _build_environment() -> void:
	var env := Environment.new()
	env.background_mode = Environment.BG_COLOR
	# Solid black. On the XREAL optical see-through display black reads as transparent,
	# so the scene appears to float over the real world.
	env.background_color = Color(0.0, 0.0, 0.0)
	env.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
	env.ambient_light_color = Color(0.6, 0.6, 0.7)
	env.ambient_light_energy = 0.6

	var world_env := WorldEnvironment.new()
	world_env.environment = env
	add_child(world_env)

	var sun := DirectionalLight3D.new()
	sun.rotation_degrees = Vector3(-50.0, -30.0, 0.0)
	add_child(sun)

func _build_room() -> void:
	# A ring of boxes at eye level so head rotation reads as look-around.
	const COUNT := 12
	for i in COUNT:
		var angle := TAU * float(i) / float(COUNT)
		var box := MeshInstance3D.new()
		box.mesh = BoxMesh.new()

		var material := StandardMaterial3D.new()
		material.albedo_color = Color.from_hsv(float(i) / float(COUNT), 0.7, 0.9)
		box.material_override = material

		box.position = Vector3(sin(angle) * 4.0, 0.0, -cos(angle) * 4.0)
		add_child(box)
		# Collider so the phone-pointer raycast can hit it (StaticBody3D parented under the box).
		var body := StaticBody3D.new()
		var col := CollisionShape3D.new()
		var box_shape := BoxShape3D.new()
		box_shape.size = (box.mesh as BoxMesh).size
		col.shape = box_shape
		body.add_child(col)
		box.add_child(body)
