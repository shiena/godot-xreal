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
const XREAL_KEY_MENU := 4
const XREAL_ACTION_LONG_PRESS := 3

var _tracker: Node3D
var _system: Object
var _status: Label
var _euler_label: Label
var _extension_loaded := false

func _ready() -> void:
	_try_register_android_bridge()
	_extension_loaded = ClassDB.class_exists(&"XrealSystem") and ClassDB.class_exists(&"XrealHeadTracker")
	if _extension_loaded:
		_system = ClassDB.instantiate(&"XrealSystem")
	else:
		push_error("[demo] godot_xreal GDExtension not loaded — XrealSystem/XrealHeadTracker missing. Build the Android .so (cargo ndk) and check the .gdextension paths.")
	_build_environment()
	_build_room()
	_spawn_rig()
	_setup_ui()

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

func _on_wearing_changed(wearing: bool) -> void:
	print("[demo] wearing changed: %s" % ("put on" if wearing else "taken off"))

func _on_glasses_event(action_type: int, para: int, para2: int, para3: float) -> void:
	# Catch-all diagnostic: one line per native glasses event (Phase A verification).
	print("[demo] glasses event: type=%d para=%d para2=%d para3=%f" % [action_type, para, para2, para3])

func _process(_delta: float) -> void:
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
