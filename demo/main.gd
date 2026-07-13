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
var _euler_label: Label
var _wear_prompt: Label
var _extension_loaded := false
# XREAL RGB camera as a Godot CameraFeed (spike — see docs/camera-feed-plan.md).
var _cam_feed: Object
var _cam_preview: TextureRect

func _ready() -> void:
	_try_register_android_bridge()
	_extension_loaded = ClassDB.class_exists(&"XrealSystem") and ClassDB.class_exists(&"XrealHeadTracker")
	if _extension_loaded:
		_system = ClassDB.instantiate(&"XrealSystem")
		# Stereo rendering mode from the project setting `xreal/stereo_rendering_mode`
		# (0 = Multipass, 2 = Multiview). Must be set BEFORE the XR rig starts — the native
		# session reads it once at bootstrap. Absent setting (-1) falls back to the native default
		# / the `debug.xreal.stereo_mode` system property. Add the setting in Project Settings, or
		# override it here, to switch modes without rebuilding.
		var stereo_mode := int(ProjectSettings.get_setting("xreal/stereo_rendering_mode", -1))
		if stereo_mode >= 0 and _system.has_method(&"set_stereo_rendering_mode"):
			_system.set_stereo_rendering_mode(stereo_mode)
			print("[demo] stereo_rendering_mode set to %d (from ProjectSettings)" % stereo_mode)
		# Head-tracking mode from the project setting `xreal/tracking_type`
		# (0 = 6DoF [recommended], 1 = 3DoF, 2 = 0DoF). Same rules as above -- read once at
		# bootstrap; absent (-1) falls back to the `debug.xreal.tracking_type` property / default.
		var tracking_type := int(ProjectSettings.get_setting("xreal/tracking_type", -1))
		if tracking_type >= 0 and _system.has_method(&"set_tracking_type"):
			_system.set_tracking_type(tracking_type)
			print("[demo] tracking_type set to %d (from ProjectSettings)" % tracking_type)
	else:
		push_error("[demo] godot_xreal GDExtension not loaded — XrealSystem/XrealHeadTracker missing. Build the Android .so (cargo ndk) and check the .gdextension paths.")
	_build_environment()
	_build_room()
	_spawn_rig()
	_setup_ui()
	if _extension_loaded:
		_setup_camera_feed()

func _try_register_android_bridge() -> void:
	if not OS.has_feature("android"):
		return
	if not Engine.has_singleton(&"AndroidRuntime"):
		print("[demo] AndroidRuntime singleton unavailable")
		return

	var runtime := Engine.get_singleton(&"AndroidRuntime")
	if runtime == null:
		print("[demo] AndroidRuntime singleton is null")
		return
	var activity = runtime.getActivity()
	if activity == null:
		print("[demo] Android activity unavailable")
		return

	var bridge = JavaClassWrapper.wrap("com.godot.game.XrealBridge")
	if bridge == null:
		print("[demo] XrealBridge Java class unavailable")
		return

	# XrealBridge methods are idempotent; this is a Godot-side fallback for template drift.
	var register_bridge := func() -> void:
		bridge.register(activity)
		bridge.startCompanionOnXrealDisplayIfNeeded(activity)
		print("[demo] XrealBridge registered via JavaClassWrapper")

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
		if _tracker.has_signal(&"glasses_connected"):
			_tracker.glasses_connected.connect(_on_glasses_connected)
			_tracker.glasses_disconnected.connect(_on_glasses_disconnected)
		# Glasses hardware inputs (One Pro: physical keys + wear sensor).
		if _tracker.has_signal(&"key_event"):
			_tracker.key_event.connect(_on_key_event)
			_tracker.wearing_changed.connect(_on_wearing_changed)
			_tracker.glasses_event.connect(_on_glasses_event)
	else:
		# Fallback so the scene is still visible (and the panel explains why).
		var camera := Camera3D.new()
		camera.current = true
		add_child(camera)

func _setup_ui() -> void:
	_status = $UI/Panel/Margin/VBox/Status
	$UI/Panel.visible = false
	($UI/Panel/Margin/VBox/Recenter as Button).pressed.connect(_on_recenter_pressed)

	_euler_label = Label.new()
	_euler_label.name = "EulerDebug"
	_euler_label.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	_euler_label.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	_euler_label.vertical_alignment = VERTICAL_ALIGNMENT_CENTER
	_euler_label.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	_euler_label.add_theme_font_size_override(&"font_size", 128)
	_euler_label.add_theme_color_override(&"font_color", Color.WHITE)
	_euler_label.add_theme_color_override(&"font_shadow_color", Color.BLACK)
	_euler_label.add_theme_constant_override(&"shadow_offset_x", 4)
	_euler_label.add_theme_constant_override(&"shadow_offset_y", 4)
	$UI.add_child(_euler_label)

	# "Put on the glasses" prompt. The forward reference is set by recenter, so if the app
	# starts while the glasses sit tilted on a desk the view ends up off-centre. Shown until
	# the wear sensor reports the glasses are on (then _on_wearing_changed recenters). Added
	# after _euler_label so it draws on top while visible.
	_wear_prompt = Label.new()
	_wear_prompt.name = "WearPrompt"
	_wear_prompt.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	_wear_prompt.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	_wear_prompt.vertical_alignment = VERTICAL_ALIGNMENT_CENTER
	_wear_prompt.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	_wear_prompt.add_theme_font_size_override(&"font_size", 110)
	_wear_prompt.add_theme_color_override(&"font_color", Color(1.0, 0.85, 0.2))
	_wear_prompt.add_theme_color_override(&"font_shadow_color", Color.BLACK)
	_wear_prompt.add_theme_constant_override(&"shadow_offset_x", 4)
	_wear_prompt.add_theme_constant_override(&"shadow_offset_y", 4)
	_wear_prompt.text = "グラスを装着して\n正面を見てください\n\nPut on the glasses\nand look forward"
	$UI.add_child(_wear_prompt)

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
	_cam_feed.set_active(true)  # -> activate_feed() starts capture; active=true enables set_rgb_image

	# Corner preview: a CameraTexture bound to THIS feed by id (this is the routing) + which_feed for
	# the RGB image our feed publishes. A plain TextureRect is enough for RGB (no YCbCr shader needed).
	var tex := CameraTexture.new()
	tex.camera_feed_id = _cam_feed.get_id()
	tex.which_feed = CameraServer.FEED_RGBA_IMAGE
	_cam_preview = TextureRect.new()
	_cam_preview.name = "CameraPreview"
	_cam_preview.texture = tex
	_cam_preview.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	_cam_preview.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT
	_cam_preview.flip_v = true  # the Y plane is top-down vs Godot's texture V; flip for an upright view
	_cam_preview.position = Vector2(32, 32)
	_cam_preview.size = Vector2(512, 288)
	$UI.add_child(_cam_preview)

	var cap := Label.new()
	cap.text = "XREAL CAM"
	cap.position = Vector2(32, 8)
	cap.add_theme_font_size_override(&"font_size", 28)
	cap.add_theme_color_override(&"font_color", Color(0.4, 1.0, 0.6))
	cap.add_theme_color_override(&"font_shadow_color", Color.BLACK)
	cap.add_theme_constant_override(&"shadow_offset_x", 2)
	cap.add_theme_constant_override(&"shadow_offset_y", 2)
	$UI.add_child(cap)
	print("[demo] XrealCameraFeed registered (id=%d, name=%s)" % [_cam_feed.get_id(), _cam_feed.get_name()])

func _on_recenter_pressed() -> void:
	if _tracker and _tracker.has_method(&"recenter"):
		_tracker.recenter()

func _on_display_started() -> void:
	# Glasses display + tracking are live: make the current head direction "forward".
	if _tracker and _tracker.has_method(&"recenter"):
		_tracker.recenter()
		print("[demo] display_started -> recenter")

func _on_glasses_connected() -> void:
	# Glasses plugged in at runtime (fires even if the app started without them). The native
	# session bootstrap retries automatically; display_started will follow once tracking is up.
	print("[demo] glasses connected")

func _on_glasses_disconnected() -> void:
	print("[demo] glasses disconnected")

func _on_key_event(key: int, action: int) -> void:
	print("[demo] key event: key=%d action=%d" % [key, action])
	# Long-press the MENU key to recenter (current head direction becomes "forward"),
	# replacing the on-screen button for a glasses-only workflow.
	if key == XREAL_KEY_MENU and action == XREAL_ACTION_LONG_PRESS:
		_on_recenter_pressed()
		print("[demo] MENU long-press -> recenter")
	# Long-press the MULTI key to quit the app (glasses-only exit).
	elif key == XREAL_KEY_MULTI and action == XREAL_ACTION_LONG_PRESS:
		print("[demo] MULTI long-press -> quit")
		get_tree().quit()

func _on_wearing_changed(wearing: bool) -> void:
	print("[demo] wearing changed: %s" % ("put on" if wearing else "taken off"))
	if _wear_prompt:
		_wear_prompt.visible = not wearing
	if wearing:
		# Recenter the instant the glasses are actually worn (and the wearer is looking
		# forward), so "forward" isn't captured while they sit tilted on a desk.
		_on_recenter_pressed()
		print("[demo] put on -> recenter")

func _on_glasses_event(action_type: int, para: int, para2: int, para3: float) -> void:
	# Catch-all diagnostic: one line per native glasses event (Phase A verification).
	print("[demo] glasses event: type=%d para=%d para2=%d para3=%f" % [action_type, para, para2, para3])

func _process(_delta: float) -> void:
	# Pump the XREAL camera feed. The session can come up a frame or two after _ready, so keep
	# (re)activating until it takes — set_rgb_image is a no-op while the feed is inactive.
	if _cam_feed:
		if not _cam_feed.is_active():
			_cam_feed.set_active(true)
		_cam_feed.poll_frame()
	if _euler_label == null:
		return
	if not _extension_loaded:
		_euler_label.text = "GDExtension\nNOT LOADED"
		return
	var tracking := false
	if _tracker and _tracker.has_method(&"is_tracking"):
		tracking = _tracker.is_tracking()
	if not tracking:
		_euler_label.text = "NO\nHEAD\nTRACKING"
		return
	if _tracker.has_method(&"debug_pose_text"):
		_euler_label.text = _tracker.debug_pose_text()
		return
	var euler := _tracker.rotation_degrees
	_euler_label.text = "NODE X %.1f\nNODE Y %.1f\nNODE Z %.1f" % [euler.x, euler.y, euler.z]

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
