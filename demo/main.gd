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

var _tracker: Node3D
var _system: Object
var _status: Label
var _extension_loaded := false

func _ready() -> void:
	_extension_loaded = ClassDB.class_exists(&"XrealSystem") and ClassDB.class_exists(&"XrealHeadTracker")
	if _extension_loaded:
		_system = ClassDB.instantiate(&"XrealSystem")
	else:
		push_error("[demo] godot_xreal GDExtension not loaded — XrealSystem/XrealHeadTracker missing. Build the Android .so (cargo ndk) and check the .gdextension paths.")
	_build_environment()
	_build_room()
	_spawn_rig()
	_setup_ui()

func _spawn_rig() -> void:
	if _extension_loaded:
		var rig := (load(RIG_SCENE) as PackedScene).instantiate()
		add_child(rig)
		_tracker = rig  # the rig's root node IS the XrealHeadTracker
	else:
		# Fallback so the scene is still visible (and the panel explains why).
		var camera := Camera3D.new()
		camera.current = true
		add_child(camera)

func _setup_ui() -> void:
	_status = $UI/Panel/Margin/VBox/Status
	($UI/Panel/Margin/VBox/Recenter as Button).pressed.connect(_on_recenter_pressed)

func _on_recenter_pressed() -> void:
	if _tracker and _tracker.has_method(&"recenter"):
		_tracker.recenter()

func _process(_delta: float) -> void:
	if _status == null:
		return
	if not _extension_loaded:
		_status.text = "GDExtension NOT loaded.\nXrealSystem / XrealHeadTracker missing.\nBuild the Android .so (cargo ndk) and\ncheck the .gdextension library paths."
		return
	var tracking := false
	if _tracker and _tracker.has_method(&"is_tracking"):
		tracking = _tracker.is_tracking()
	_status.text = "\n".join([
		"SDK available: %s" % _system.is_available(),
		"Session started: %s" % _system.is_session_started(),
		"Plugin version: %s" % _system.get_plugin_version(),
		"Device type: %d" % _system.get_device_type(),
		"Head tracking: %s" % tracking,
	])

func _build_environment() -> void:
	var env := Environment.new()
	env.background_mode = Environment.BG_COLOR
	env.background_color = Color(0.05, 0.06, 0.09)
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
