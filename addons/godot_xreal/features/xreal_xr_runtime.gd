class_name XrealXRRuntime
extends Node3D
## Bootstrap that drives an XROrigin3D hierarchy from the XREAL glasses.
##
## The application owns the hierarchy, exactly as an OpenXR project does: its own XROrigin3D,
## XRCamera3D and XRController3D nodes, named however it likes. Add this component anywhere under or
## beside them and it attaches to what it finds, with no initialization code. Starting from nothing,
## instance xr_origin.tscn instead, which is that same hierarchy with this component already in it.
##
## Attaching rather than owning is what makes a scene portable. The identical scene runs on an
## OpenXR headset with this component left out, where the OpenXR vendor plugin drives the same nodes.
## Nothing in this addon runs there.
##
## On XREAL it also polls and fuses the native phone IMU, publishes touchpad state, converts glasses
## keys and app-owned phone UI controls into standard XR/InputMap events, and retains the raw XREAL
## signals as compatibility hooks.

## The XREAL runtime could not start. `message` is the diagnostic also sent to push_error,
## readable later through get_last_error().
signal backend_failed(message: String)
## The glasses display and tracking came up (re-emitted from the backend driver).
signal display_started
## A physical glasses key event, re-emitted from the backend driver. `key` and `action` use
## XrealHeadTracker's `KEY_*` and `ACTION_*` constants.
signal key_event(key: int, action: int)
## The wear sensor changed state, re-emitted from the backend driver.
signal wearing_changed(wearing: bool)

## The runtime scene root joins this group; it detects duplicate runtime instances.
const GROUP_RUNTIME := &"xreal_shared_xr_runtime"
## XrealHeadTracker.KEY_MULTI: the glasses MULTI key.
const XREAL_KEY_MULTI := 1
## XrealHeadTracker.KEY_MENU: the glasses MENU key.
const XREAL_KEY_MENU := 4
## XrealHeadTracker.ACTION_CLICK: a single click of a glasses key.
const XREAL_ACTION_CLICK := 1

var _driver: Node3D
var _system: Object
var _error := ""
var _standby := false
# Root viewport whose 3D drawing this instance switched off, and the value to put back.
var _host_viewport: Viewport
var _host_3d_was_disabled := false

## The XROrigin3D to drive. Leave it empty to search: an ancestor first, then this node's own
## children, then the rest of the tree.
@export var xr_origin_path: NodePath

## The tracking-space origin this component drives, resolved in _ready.
var xr_origin: XROrigin3D
## The head camera under that origin. Godot only moves an XRCamera3D for a viewport that renders
## through an XR interface, which the XREAL path never does, so this component mirrors the driver
## onto it.
var xr_camera: XRCamera3D
## The bridge from controller trackers to XRController3D signals and InputMap actions.
@onready var input_router: XrealXRInputRouter = $XRInputRouter

func _ready() -> void:
	# Process after the backend driver, which is a child of this scene. Its _process writes the head
	# pose that _process below mirrors onto the standard camera, and a lower priority runs first even
	# against tree order, so reading it here costs no extra frame of lag.
	process_priority = 1
	if not _resolve_hierarchy():
		return
	_start_backend()

## Find the XR nodes to drive. The application owns them, so nothing here creates or renames one.
func _resolve_hierarchy() -> bool:
	xr_origin = get_node_or_null(xr_origin_path) as XROrigin3D
	if xr_origin == null:
		xr_origin = _find_origin()
	if xr_origin == null:
		_fail("[xreal-runtime] no XROrigin3D found. Add this component under one, set its "
			+ "xr_origin_path, or instance addons/godot_xreal/xr_origin.tscn for a ready hierarchy.")
		return false
	xr_camera = _first_of_type(xr_origin, "XRCamera3D") as XRCamera3D
	if xr_camera == null:
		_fail("[xreal-runtime] the XROrigin3D at %s has no XRCamera3D child" % xr_origin.get_path())
		return false
	# Feature components reach the head through this group rather than a node path, so publish
	# whatever camera the application named instead of requiring it to join the group itself.
	if not xr_camera.is_in_group(XrealShared.GROUP_XR_CAMERA):
		xr_camera.add_to_group(XrealShared.GROUP_XR_CAMERA)
	input_router.bind_controllers(xr_origin)
	return true

## The nearest XROrigin3D: an ancestor, then a descendant of this node, then anywhere in the tree.
func _find_origin() -> XROrigin3D:
	var node: Node = get_parent()
	while node != null:
		if node is XROrigin3D:
			return node
		node = node.get_parent()
	var below := _first_of_type(self, "XROrigin3D") as XROrigin3D
	if below != null:
		return below
	return _first_of_type(get_tree().root, "XROrigin3D") as XROrigin3D

## Depth-first search for the first descendant of a class, so the application may nest and name its
## nodes freely.
func _first_of_type(root: Node, type_name: StringName) -> Node:
	for child in root.get_children():
		if child.is_class(type_name):
			return child
		var found := _first_of_type(child, type_name)
		if found != null:
			return found
	return null

func _start_backend() -> void:
	var existing := get_tree().get_first_node_in_group(GROUP_RUNTIME)
	if existing != null and existing != self:
		_stand_by(existing)
		return
	if _standby:
		# Take the viewport camera back, released while another instance owned the backend.
		_standby = false
		_error = ""
		xr_camera.current = true
	XrealAndroidBridge.register()
	_initialize_xreal()

func _initialize_xreal() -> void:
	# A project may still enable Godot's OpenXR at startup, for instance one shared with an OpenXR
	# target. Where that succeeds it claims the root viewport and the primary interface slot, and
	# XrealXrInterface refuses to displace an existing primary, so give both back first.
	var openxr := XRServer.find_interface(&"OpenXR")
	if openxr != null and openxr.is_initialized():
		get_viewport().use_xr = false
		if XRServer.primary_interface == openxr:
			XRServer.primary_interface = null
		openxr.uninitialize()
	if OS.get_name() != "Android":
		# Keep app-owned phone UI and InputMap testing live in the desktop preview.
		input_router.enable_xreal_trackers()
		# Deferred so that every sibling has run _ready and a desktop preview, if the scene has one,
		# has built its window.
		_release_host_3d.call_deferred()
		return
	if not ClassDB.class_exists(&"XrealSystem") or not ClassDB.class_exists(&"XrealHeadTracker"):
		_fail("[xreal-runtime] godot_xreal GDExtension is unavailable on Android")
		return
	_system = ClassDB.instantiate(&"XrealSystem")
	_apply_xreal_boot_settings()
	_driver = ClassDB.instantiate(&"XrealHeadTracker") as Node3D
	if _driver == null:
		_fail("[xreal-runtime] XrealHeadTracker could not be created")
		return
	_driver.name = "XrealBackendDriver"
	_driver.add_to_group(&"xreal_head_tracker")
	# Under the origin, not beside it: the driver's global transform is what aims the glasses eye
	# cameras, so parenting it here makes moving XROrigin3D move the view, the way a standard XR
	# scene expects. Its local transform stays the tracking-space head pose the interface publishes.
	#
	# Deferred because the origin is this node's parent now, and a parent is still busy adding its
	# children while their _ready runs, so an immediate add_child would be refused.
	xr_origin.add_child.call_deferred(_driver)
	input_router.enable_xreal_trackers(_system)
	if _driver.has_signal(&"display_started"):
		_driver.display_started.connect(_on_display_started)
	if _driver.has_signal(&"key_event"):
		_driver.key_event.connect(_on_key_event)
	if _driver.has_signal(&"wearing_changed"):
		_driver.wearing_changed.connect(_on_wearing_changed)

func _apply_xreal_boot_settings() -> void:
	var settings := {
		&"xreal/tracking_type": &"set_tracking_type",
		&"xreal/stereo_mode": &"set_stereo_mode",
		&"xreal/input_source": &"set_input_source",
	}
	for setting_name in settings:
		var value := int(XrealShared.read_setting(setting_name, -1))
		var method: StringName = settings[setting_name]
		if value >= 0 and _system.has_method(method):
			_system.call(method, value)

## Only one instance may own the backend, so a second one steps aside instead of initializing.
##
## Standing aside has to release the XRCamera3D as well, when this instance brought its own. A
## `current` camera takes the viewport as it enters the tree, and one no backend ever moves would
## draw the scene from the origin. Releasing it returns the viewport to the owner's camera.
##
## Two runtimes also overlap for a moment in an ordinary crossfade transition, where the outgoing
## scene is still in the tree while the incoming one loads. Retrying once the owner leaves keeps
## that case working; failing permanently would leave the surviving scene untracked for the rest of
## the session.
func _stand_by(existing: Node) -> void:
	_standby = true
	xr_camera.current = false
	_error = "[xreal-runtime] another xreal_xr_runtime.tscn owns the backend; standing by"
	push_warning(_error)
	if not existing.tree_exited.is_connected(_on_owner_left):
		existing.tree_exited.connect(_on_owner_left, CONNECT_ONE_SHOT)

func _on_owner_left() -> void:
	# The owner leaves its groups as part of the same tree exit, so re-check after the current call
	# stack unwinds rather than inside the signal, where it would still be found.
	_start_backend.call_deferred()

## Desktop only: stop the root viewport from drawing the 3D world a second time.
##
## An XRCamera3D is normally `current`, so off device that camera sits at the origin and
## still costs a full scene pass, drawn behind whatever 2D the app puts on the phone screen while
## the preview window shows the view that matters. The backend driver switches the same viewport
## off on device once the eye viewports take over, under the same xreal/disable_host_viewport_3d
## opt-out. Only the drawing is switched off, never `current`: the driver reads the viewport's
## camera for the eye cameras' FOV and clipping planes.
func _release_host_3d() -> void:
	if not bool(XrealShared.read_setting("xreal/disable_host_viewport_3d", true)):
		return
	# With no preview window there is nowhere else for the 3D to go, so leave the root viewport as
	# the app's only view.
	if XrealShared.find_preview_head(get_tree()) == null:
		return
	_host_viewport = get_viewport()
	_host_3d_was_disabled = _host_viewport.is_3d_disabled()
	_host_viewport.set_disable_3d(true)

func _exit_tree() -> void:
	if _host_viewport != null:
		if is_instance_valid(_host_viewport):
			_host_viewport.set_disable_3d(_host_3d_was_disabled)
		_host_viewport = null

func _fail(message: String) -> void:
	_error = message
	push_error(message)
	backend_failed.emit(message)

func _on_display_started() -> void:
	display_started.emit()

func _on_key_event(key: int, action: int) -> void:
	# The glasses callback reports click types rather than down/up, so publish one-frame pulses.
	# Single clicks only: double clicks and long presses keep their XREAL-specific meanings
	# (long-press MENU recenters, long-press MULTI quits) and stay on the compatibility signal.
	if action == XREAL_ACTION_CLICK:
		if key == XREAL_KEY_MENU:
			input_router.pulse_button(&"menu_button")
		elif key == XREAL_KEY_MULTI:
			input_router.pulse_button(&"primary_click")
	key_event.emit(key, action)

func _on_wearing_changed(wearing: bool) -> void:
	wearing_changed.emit(wearing)

## The XREAL backend driver, or null off device.
func get_xreal_driver() -> Node3D:
	return _driver

## The shared XrealSystem facade, or null when the XREAL native runtime is inactive.
func get_xreal_system() -> Object:
	return _system

## Whether the native XREAL backend driver is running.
func is_xreal_active() -> bool:
	return _driver != null

## The most recent initialization diagnostic, or "" once the backend is running. It also reports a
## second instance standing by, which is recoverable and so raises no backend_failed.
func get_last_error() -> String:
	return _error

## Recenter the phone controller and, on device, the head pose.
func recenter() -> void:
	input_router.recenter_phone_controller()
	if _driver != null and _driver.has_method(&"recenter"):
		_driver.call(&"recenter")

## Feed a phone UI or another app-owned XREAL controller button through the standard XR path.
func set_controller_button(input_name: StringName, pressed: bool) -> void:
	input_router.set_button(input_name, pressed)
	if input_name == &"trigger_click":
		input_router.set_float(&"trigger", 1.0 if pressed else 0.0)
	elif input_name == &"grip_click":
		input_router.set_float(&"grip", 1.0 if pressed else 0.0)

## Feed or release an app-owned XREAL phone touchpad through the standard XR path.
func set_controller_axis(value: Vector2, active := true) -> void:
	input_router.set_external_primary_axis(value, active)

## Select which standard hand controller receives app-owned and native XREAL input.
func set_controller_hand(is_right: bool) -> void:
	input_router.set_active_hand(is_right)

## Publish a one-frame app-owned button click through XRController3D and InputMap.
func pulse_controller_button(input_name: StringName) -> void:
	input_router.pulse_button(input_name)

## Standard controller node currently representing the XREAL phone controller.
func get_active_controller() -> XRController3D:
	return input_router.get_active_controller()

func _process(delta: float) -> void:
	if _driver == null or not _driver.has_method(&"is_tracking") or not _driver.is_tracking():
		return
	# Godot moves an XRCamera3D only while its viewport renders through the XR interface, and the
	# XREAL backend never does that: it presents through its own eye viewports. Left alone the node
	# would sit at the origin, and everything that treats it as the head - head-locked content, the
	# blend and stream cameras, the controller anchor below - would read a head that never moves.
	# The driver's local transform is already the tracking-space pose, and both nodes hang off the
	# same XROrigin3D, so it transfers directly.
	xr_camera.transform = _driver.transform
	input_router.poll_xreal_controller(delta, xr_camera.transform)
