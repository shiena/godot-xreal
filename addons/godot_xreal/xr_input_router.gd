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
@export var hand_offset := Vector3(0.28, -0.32, -0.3)

## Aim a tracked hand as well, so a hand points the same ray a controller would. An OpenXR runtime
## synthesises the controller's aim and grip poses from the hands when no controller is held, and
## `left_hand` / `right_hand` carry them either way; this does the same here, per hand and only
## while that hand is tracked. The phone keeps every hand the cameras cannot see, so putting a hand
## down hands the ray back to it.
@export var hand_aim := true

## Where a hand ray is anchored, relative to the tracked head, in metres: roughly the shoulder.
## X is a magnitude, signed per hand.
##
## The ray runs from here through the hand rather than along the hand's own forward axis. Anchoring
## it to the body is what makes a hand ray steady enough to point with: the hand's own axis carries
## every tremor of the wrist, amplified by the distance to the target, while a shoulder-through-hand
## ray turns only as fast as the hand travels.
@export var shoulder_offset := Vector3(0.17, -0.20, 0.0)

## Thumb-to-index distance at which a pinch counts as a press, in metres. A pinch is published as
## `trigger_click` on that hand, the same button the phone's trigger raises, so `xr_select` fires
## either way and gameplay code needs no branch.
@export var pinch_press_m := 0.02

## Distance at which the pinch lets go, in metres. Wider than [member pinch_press_m] on purpose: a
## single threshold chatters while the fingers rest near it, which reads as a double click.
@export var pinch_release_m := 0.03
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
# Per-hand pinch state, held across frames so the release threshold can be the wider one.
var _pinch: Dictionary = {}
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
	if not _xreal_enabled:
		return
	# A hand is aimed whether or not the phone controller ever starts, which is why the phone's own
	# bail-outs live in their own function below rather than cutting this one short.
	_publish_hand_aim(head_transform)
	_poll_phone_controller(delta, head_transform)

func _poll_phone_controller(delta: float, head_transform: Transform3D) -> void:
	if _system == null:
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
	# The pitch used to be negated here, because tilting the phone up sent the beam down. That was
	# the eye image arriving mirrored vertically (fixed in src/gl.rs and src/vk_bridge.rs): the beam
	# was going the right way and the view was upside-down. With the image upright, pitch is direct.
	var aim_basis := Basis.from_euler(euler)
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
		_invalidate_controller_poses(old)
	_active_hand = XRPositionalTracker.TRACKER_HAND_RIGHT if is_right else XRPositionalTracker.TRACKER_HAND_LEFT
	_publish_state()
	if _valid_pose:
		set_aim_transform(_pose)

## Feed a canonical XR button. XRController3D emits the normal button signals in response.
func set_button(input_name: StringName, pressed: bool) -> void:
	if not _xreal_enabled:
		return
	_button_state[input_name] = pressed
	if input_name == &"trigger_click":
		# Shared with the pinch, so it goes through the merge rather than straight to the tracker.
		_apply_trigger(_active_hand)
		return
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
	# A tracked hand outranks the phone on the hand it belongs to. Without this the phone would
	# overwrite the hand's pose every frame and the ray would sit wherever the phone points.
	if hand_aim and _hand_tracker(_active_hand) != null:
		return
	_publish_controller_poses(tracker, transform, transform)

## Write one controller tracker's poses: `aim`, `grip`, and `default`.
##
## `default` carries the aim pose, because that is what an OpenXR runtime publishes there. Godot's
## OpenXR binds its `default_pose` action to `.../input/aim/pose` on every interaction profile and
## renames the action to `default` on the tracker (`openxr_interface.cpp`). Publishing it matters
## because [member XRNode3D.pose] defaults to `default`: without it an application that drops a
## plain XRController3D into its scene would get no pose at all, and would have to know to set
## `pose = "aim"` on XREAL alone. The bundled `xr_origin.tscn` sets it either way.
func _publish_controller_poses(
	tracker: XRControllerTracker, aim: Transform3D, grip: Transform3D
) -> void:
	tracker.set_pose(
		&"aim", aim, Vector3.ZERO, Vector3.ZERO,
		XRPose.XR_TRACKING_CONFIDENCE_HIGH)
	tracker.set_pose(
		&"default", aim, Vector3.ZERO, Vector3.ZERO,
		XRPose.XR_TRACKING_CONFIDENCE_HIGH)
	tracker.set_pose(
		&"grip", grip, Vector3.ZERO, Vector3.ZERO,
		XRPose.XR_TRACKING_CONFIDENCE_HIGH)

## Drop every pose `_publish_controller_poses` writes, so a controller node reading any of them
## goes inactive rather than freezing at the last one.
func _invalidate_controller_poses(tracker: XRControllerTracker) -> void:
	tracker.invalidate_pose(&"aim")
	tracker.invalidate_pose(&"default")
	tracker.invalidate_pose(&"grip")

## The tracked hand for one side, or null when it is absent, untracked, or hand aiming is off.
func _hand_tracker(hand: XRPositionalTracker.TrackerHand) -> XRHandTracker:
	if not hand_aim:
		return null
	var name := (&"/user/hand_tracker/right"
		if hand == XRPositionalTracker.TRACKER_HAND_RIGHT
		else &"/user/hand_tracker/left")
	var tracker := XRServer.get_tracker(name) as XRHandTracker
	if tracker == null or not tracker.has_tracking_data:
		return null
	return tracker

## Aim both controller trackers from the hands that are tracked, and clear the ones that are not.
func _publish_hand_aim(head_transform: Transform3D) -> void:
	for hand: XRPositionalTracker.TrackerHand in [
		XRPositionalTracker.TRACKER_HAND_LEFT, XRPositionalTracker.TRACKER_HAND_RIGHT
	]:
		var name := (&"right_hand"
			if hand == XRPositionalTracker.TRACKER_HAND_RIGHT
			else &"left_hand")
		var controller := XRServer.get_tracker(name) as XRControllerTracker
		if controller == null:
			continue
		var hand_tracker := _hand_tracker(hand)
		if hand_tracker == null:
			# A hand that leaves the cameras' view mid-pinch would otherwise hold the button down
			# for good, so let go first.
			if _pinch.get(hand, false):
				_pinch[hand] = false
				_apply_trigger(hand)
			# The phone still drives the hand it is standing in for; the other has no pose at all,
			# which is the state it was in before hand tracking existed.
			if hand != _active_hand:
				_invalidate_controller_poses(controller)
			continue
		_update_pinch(hand, hand_tracker)
		var grip: Transform3D = hand_tracker.get_hand_joint_transform(
			XRHandTracker.HAND_JOINT_PALM)
		var aim := _hand_aim_pose(hand_tracker, hand, head_transform)
		_publish_controller_poses(controller, aim, grip)

## Watch one hand's thumb and index finger and publish the pinch as a button press.
func _update_pinch(hand: XRPositionalTracker.TrackerHand, hand_tracker: XRHandTracker) -> void:
	var thumb: Vector3 = hand_tracker.get_hand_joint_transform(
		XRHandTracker.HAND_JOINT_THUMB_TIP).origin
	var index: Vector3 = hand_tracker.get_hand_joint_transform(
		XRHandTracker.HAND_JOINT_INDEX_FINGER_TIP).origin
	var was: bool = _pinch.get(hand, false)
	var limit := pinch_release_m if was else pinch_press_m
	var now := thumb.distance_to(index) < limit
	if now == was:
		return
	_pinch[hand] = now
	_apply_trigger(hand)

## Write one hand's `trigger_click`, holding it down while either source asks for it.
##
## The phone and a pinch both mean "select", and on the hand the phone is standing in for they can
## overlap. Writing the merged value from one place keeps a release by one source from cancelling
## the other's press.
func _apply_trigger(hand: XRPositionalTracker.TrackerHand) -> void:
	var name := (&"right_hand"
		if hand == XRPositionalTracker.TRACKER_HAND_RIGHT
		else &"left_hand")
	var controller := XRServer.get_tracker(name) as XRControllerTracker
	if controller == null:
		return
	var phone: bool = hand == _active_hand and _button_state.get(&"trigger_click", false)
	controller.set_input(&"trigger_click", phone or _pinch.get(hand, false))

## One hand's ray: it leaves the base of the index finger and runs away from the shoulder.
##
## The origin is the index finger's proximal joint, so the ray leaves the hand where a pointing
## finger would rather than from the wrist. The direction comes from the shoulder anchor, for the
## steadiness described on [member shoulder_offset]. A hand held exactly at shoulder height leaves
## nothing to aim with, so that degenerate case falls back to the palm's own forward axis.
func _hand_aim_pose(
	hand_tracker: XRHandTracker,
	hand: XRPositionalTracker.TrackerHand,
	head_transform: Transform3D
) -> Transform3D:
	var origin: Vector3 = hand_tracker.get_hand_joint_transform(
		XRHandTracker.HAND_JOINT_INDEX_FINGER_PHALANX_PROXIMAL).origin
	var offset := shoulder_offset
	offset.x = absf(offset.x) * (
		1.0 if hand == XRPositionalTracker.TRACKER_HAND_RIGHT else -1.0)
	var shoulder: Vector3 = head_transform.origin + head_transform.basis * offset
	var forward: Vector3 = origin - shoulder
	if forward.length() < 0.05 or absf(forward.normalized().dot(Vector3.UP)) > 0.999:
		var palm: Transform3D = hand_tracker.get_hand_joint_transform(
			XRHandTracker.HAND_JOINT_PALM)
		return Transform3D(palm.basis, origin)
	return Transform3D(Basis.looking_at(forward, Vector3.UP), origin)

func _publish_state() -> void:
	var tracker := _active_tracker()
	if tracker == null:
		return
	for input_name in _button_state.keys():
		if input_name == &"trigger_click":
			continue  # merged with the pinch below
		tracker.set_input(input_name, _button_state[input_name])
	for input_name in _float_state.keys():
		tracker.set_input(input_name, _float_state[input_name])
	tracker.set_input(&"primary", _axis)
	tracker.set_input(&"touchpad", _axis)
	_apply_trigger(XRPositionalTracker.TRACKER_HAND_LEFT)
	_apply_trigger(XRPositionalTracker.TRACKER_HAND_RIGHT)

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
