extends Node
class_name XrealXRInputRouter

## Runtime-neutral bridge between Godot XR controller trackers and InputMap actions.
##
## An OpenXR runtime provides the `left_hand` and `right_hand` trackers itself. On XREAL this router
## creates trackers under those same reserved names and polls the native phone controller, so the
## scene and its gameplay code read one contract on either platform.

## Optional explicit wiring. Left empty, bind_controllers() picks the XRController3D nodes under the
## origin by their tracker name, so an application may name and nest its own however it likes.
@export var left_controller: XRController3D
@export var right_controller: XRController3D

const INPUT_ACTIONS := {
	&"trigger_click": &"xr_select",
	&"primary_click": &"xr_select",
	&"grip_click": &"xr_grab",
	&"menu_button": &"xr_menu",
}

## Phone-controller origin relative to the tracked head, in metres. Negative Y puts the origin
## below the head, at hand height. X is a magnitude; set_active_hand picks its sign.
##
## This used to read +0.32, on the finding that "the glasses buffer reads +Y as down". That was
## the eye image being mirrored vertically on its way to the compositor (fixed in src/gl.rs and
## src/vk_bridge.rs), which inverted every vertical offset along with the scene. With the image
## upright, Y means what it says.
@export var hand_offset := Vector3(0.28, -0.32, -0.3)
## Complementary-filter gain used to correct phone pitch and roll from gravity.
@export_range(0.0, 1.0, 0.01) var gravity_gain := 0.06
## Gyroscope rates below this threshold are treated as resting noise.
@export var gyro_deadzone := 0.012
## Rate used to learn the resting gyroscope bias.
@export_range(0.0, 1.0, 0.01) var bias_learn := 0.02

var _owned_trackers: Dictionary = {}
var _xreal_enabled := false
var _active_hand := XRPositionalTracker.TRACKER_HAND_RIGHT
var _button_state: Dictionary = {}
var _float_state: Dictionary = {}
var _axis := Vector2.ZERO
var _external_axis_active := false
var _pressed_sources: Dictionary = {}
var _system: Object
var _native_controller_started := false
var _native_start_retry_frames := 0
var _q := Quaternion.IDENTITY
var _reference := Quaternion.IDENTITY
var _has_reference := false
var _gyro_bias := Vector3.ZERO
var _valid_pose := false
var _pose := Transform3D.IDENTITY
var _sample_count := 0
# Per-button counter identifying the newest pulse, so an older one cannot end it early.
var _pulse_generation: Dictionary = {}

func _ready() -> void:
	for action in INPUT_ACTIONS.values():
		if not InputMap.has_action(action):
			InputMap.add_action(action)

## Adopt the application's controller nodes. Matching on `tracker` rather than on node names keeps
## this working whether they are called LeftHand, LeftAim or LeftController, and an application that
## separates aim and grip poses simply has more than one node per hand; the first of each wins.
func bind_controllers(origin: Node) -> void:
	if left_controller == null:
		left_controller = _find_controller(origin, &"left_hand")
	if right_controller == null:
		right_controller = _find_controller(origin, &"right_hand")
	_connect_controller(left_controller)
	_connect_controller(right_controller)

func _find_controller(root: Node, tracker: StringName) -> XRController3D:
	for child in root.get_children():
		var controller := child as XRController3D
		if controller != null and controller.tracker == tracker:
			return controller
		var found := _find_controller(child, tracker)
		if found != null:
			return found
	return null

func _exit_tree() -> void:
	for action in _pressed_sources.keys():
		Input.action_release(action)
	for tracker in _owned_trackers.values():
		XRServer.remove_tracker(tracker)
	_owned_trackers.clear()

func _connect_controller(controller: XRController3D) -> void:
	if controller == null or controller.button_pressed.is_connected(_on_button_pressed):
		return
	controller.button_pressed.connect(_on_button_pressed.bind(controller))
	controller.button_released.connect(_on_button_released.bind(controller))

## Create the standard controller trackers only for the XREAL backend. OpenXR owns its trackers.
## When supplied, XrealSystem is polled here for the native phone controller.
func enable_xreal_trackers(system: Object = null) -> void:
	_xreal_enabled = true
	_system = system
	_ensure_tracker(&"left_hand", XRPositionalTracker.TRACKER_HAND_LEFT)
	_ensure_tracker(&"right_hand", XRPositionalTracker.TRACKER_HAND_RIGHT)
	_publish_state()

## Poll and publish the native XREAL phone controller. Call after head tracking becomes live.
func poll_xreal_controller(delta: float, head_transform: Transform3D) -> void:
	if not _xreal_enabled or _system == null:
		return
	if not _native_controller_started:
		if _native_start_retry_frames > 0:
			_native_start_retry_frames -= 1
			return
		if not _system.has_method(&"start_controller"):
			return
		var diagnostic := str(_system.start_controller())
		_native_controller_started = diagnostic.contains("controller started") \
			or diagnostic.contains("controller already started")
		if not _native_controller_started:
			_native_start_retry_frames = 120
		return
	if not _system.has_method(&"poll_controller"):
		return
	var state: PackedFloat32Array = _system.poll_controller()
	if state.size() < 14 or state[0] <= 0.5:
		return
	_update_phone_pose(
		Vector3(state[1], state[2], state[3]),
		Vector3(state[4], state[5], state[6]), delta, head_transform)
	if not _external_axis_active:
		set_primary_axis(Vector2(state[11], state[12]) if state[10] > 0.5 else Vector2.ZERO)

func _update_phone_pose(
	accel: Vector3, gyro: Vector3, delta: float, head_transform: Transform3D
) -> void:
	if gyro.length() < 0.1:
		_gyro_bias = _gyro_bias.lerp(gyro, bias_learn)
	var corrected_gyro := gyro - _gyro_bias
	if corrected_gyro.length() < gyro_deadzone:
		corrected_gyro = Vector3.ZERO
	var angular_speed := corrected_gyro.length()
	if angular_speed > 0.000001 and delta > 0.0:
		_q = (_q * Quaternion(
			corrected_gyro / angular_speed, angular_speed * delta)).normalized()
	if accel.length() > 1.0:
		var measured_up := accel.normalized()
		var predicted_up := (_q.inverse() * Vector3.UP).normalized()
		var correction_axis := predicted_up.cross(measured_up)
		var correction_length := correction_axis.length()
		if correction_length > 0.000001:
			_q = (_q * Quaternion(
				correction_axis / correction_length,
				asin(clampf(correction_length, -1.0, 1.0)) * gravity_gain)).normalized()
	_sample_count += 1
	if not _has_reference and _sample_count >= 90:
		recenter_phone_controller()
	if not _has_reference:
		return
	var relative := _reference * _q
	var phone_to_godot := Basis(
		Vector3(1, 0, 0), Vector3(0, 0, -1), Vector3(0, 1, 0))
	var euler := (phone_to_godot * Basis(relative) * phone_to_godot.transposed()).get_euler()
	var aim_basis := Basis.from_euler(Vector3(-euler.x, euler.y, euler.z))
	var offset := hand_offset
	offset.x = absf(offset.x) * (
		1.0 if _active_hand == XRPositionalTracker.TRACKER_HAND_RIGHT else -1.0)
	_pose = Transform3D(aim_basis, head_transform.origin + head_transform.basis * offset)
	_valid_pose = true
	set_aim_transform(_pose)

## Make the current native phone orientation point forward.
func recenter_phone_controller() -> void:
	if _sample_count == 0:
		return
	_reference = _q.inverse()
	_has_reference = true

## Current synthetic phone-controller pose in tracking space.
func get_phone_controller_pose() -> Transform3D:
	return _pose

## Whether the native phone controller has produced a recentered pose.
func has_phone_controller_pose() -> bool:
	return _valid_pose

func _ensure_tracker(name: StringName, hand: XRPositionalTracker.TrackerHand) -> void:
	if XRServer.get_tracker(name) != null:
		return
	var tracker := XRControllerTracker.new()
	tracker.name = name
	tracker.hand = hand
	tracker.profile = "xreal/phone_controller"
	XRServer.add_tracker(tracker)
	_owned_trackers[name] = tracker

func _active_tracker() -> XRControllerTracker:
	var name := &"right_hand" if _active_hand == XRPositionalTracker.TRACKER_HAND_RIGHT else &"left_hand"
	return XRServer.get_tracker(name) as XRControllerTracker

func _inactive_tracker() -> XRControllerTracker:
	var name := &"left_hand" if _active_hand == XRPositionalTracker.TRACKER_HAND_RIGHT else &"right_hand"
	return XRServer.get_tracker(name) as XRControllerTracker

## Standard controller node currently representing the XREAL phone controller.
func get_active_controller() -> XRController3D:
	return right_controller if _active_hand == XRPositionalTracker.TRACKER_HAND_RIGHT \
		else left_controller

## Select which standard hand tracker represents the phone controller.
func set_active_hand(is_right: bool) -> void:
	if not _xreal_enabled:
		return
	var old := _active_tracker()
	if old:
		for input_name in _button_state.keys():
			old.set_input(input_name, false)
		for input_name in _float_state.keys():
			old.set_input(input_name, 0.0)
		old.set_input(&"primary", Vector2.ZERO)
		old.set_input(&"touchpad", Vector2.ZERO)
		old.invalidate_pose(&"aim")
		old.invalidate_pose(&"grip")
	_active_hand = XRPositionalTracker.TRACKER_HAND_RIGHT if is_right else XRPositionalTracker.TRACKER_HAND_LEFT
	_publish_state()
	if _valid_pose:
		set_aim_transform(_pose)

## Feed a canonical XR button. XRController3D emits the normal button signals in response.
func set_button(input_name: StringName, pressed: bool) -> void:
	if not _xreal_enabled:
		return
	_button_state[input_name] = pressed
	var tracker := _active_tracker()
	if tracker:
		tracker.set_input(input_name, pressed)

func set_float(input_name: StringName, value: float) -> void:
	if not _xreal_enabled:
		return
	_float_state[input_name] = value
	var tracker := _active_tracker()
	if tracker:
		tracker.set_input(input_name, value)

## Emit a canonical button pulse for controls that report clicks rather than down/up.
##
## The press is held across two process_frame signals, not one. A click arriving from the glasses
## callback is published during the backend driver's _process, so a node that processes earlier in
## the tree has already run for that frame; releasing on the first process_frame would retract the
## action before any such node could poll it. Two frames leave every node exactly one _process in
## which is_action_pressed() reads true. Because the press starts mid-process rather than in the
## input phase, is_action_just_pressed() is NOT reliable for these clicks: connect to
## XRController3D.button_pressed, or poll is_action_pressed().
## Clicks can also arrive faster than one pulse takes. A tracker input only signals on change, so
## re-raising a button that is still held would publish nothing at all; the button is dropped first
## so the second click reads as its own press. The generation counter then keeps the older pulse
## from releasing partway through the newer one.
func pulse_button(input_name: StringName) -> void:
	var generation: int = int(_pulse_generation.get(input_name, 0)) + 1
	_pulse_generation[input_name] = generation
	if bool(_button_state.get(input_name, false)):
		set_button(input_name, false)
	set_button(input_name, true)
	await get_tree().process_frame
	await get_tree().process_frame
	if is_inside_tree() and int(_pulse_generation.get(input_name, 0)) == generation:
		set_button(input_name, false)

func set_primary_axis(value: Vector2) -> void:
	if not _xreal_enabled:
		return
	_axis = value
	var tracker := _active_tracker()
	if tracker:
		tracker.set_input(&"primary", value)
		tracker.set_input(&"touchpad", value)

## Feed or release an app-owned phone touchpad without native polling overwriting it.
func set_external_primary_axis(value: Vector2, active: bool) -> void:
	_external_axis_active = active
	set_primary_axis(value if active else Vector2.ZERO)

## Publish the phone controller's synthetic 3DoF pose in XROrigin3D tracking space.
func set_aim_transform(transform: Transform3D) -> void:
	if not _xreal_enabled:
		return
	var tracker := _active_tracker()
	if tracker == null:
		return
	tracker.set_pose(
		&"aim", transform, Vector3.ZERO, Vector3.ZERO,
		XRPose.XR_TRACKING_CONFIDENCE_HIGH)
	tracker.set_pose(
		&"grip", transform, Vector3.ZERO, Vector3.ZERO,
		XRPose.XR_TRACKING_CONFIDENCE_HIGH)

func _publish_state() -> void:
	var tracker := _active_tracker()
	if tracker == null:
		return
	for input_name in _button_state.keys():
		tracker.set_input(input_name, _button_state[input_name])
	for input_name in _float_state.keys():
		tracker.set_input(input_name, _float_state[input_name])
	tracker.set_input(&"primary", _axis)
	tracker.set_input(&"touchpad", _axis)

func _on_button_pressed(input_name: String, controller: XRController3D) -> void:
	var action: StringName = INPUT_ACTIONS.get(StringName(input_name), &"")
	if action == &"" or not InputMap.has_action(action):
		return
	var sources: Dictionary = _pressed_sources.get(action, {})
	sources["%s:%s" % [controller.get_path(), input_name]] = true
	_pressed_sources[action] = sources
	Input.action_press(action)

func _on_button_released(input_name: String, controller: XRController3D) -> void:
	var action: StringName = INPUT_ACTIONS.get(StringName(input_name), &"")
	if action == &"" or not _pressed_sources.has(action):
		return
	var sources: Dictionary = _pressed_sources[action]
	sources.erase("%s:%s" % [controller.get_path(), input_name])
	if sources.is_empty():
		_pressed_sources.erase(action)
		Input.action_release(action)
	else:
		_pressed_sources[action] = sources
