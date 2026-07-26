extends Node
## Desktop-only preview: a second OS window showing what the glasses would draw, flown around with
## mouse-look and WASD.
##
## On device the two displays split the work: the phone's root viewport draws the touch UI (a
## CanvasLayer) and the extension's per-eye SubViewports draw the 3D world. Off device those eye
## viewports do not exist, so the glasses half has nowhere to go, and an app whose phone UI paints
## an opaque backdrop hides whatever the root viewport drew anyway. This component gives that half
## its own window.
##
## A [Window] IS a [Viewport], so pointing its [member Viewport.world_3d] at the root's renders the
## same 3D world from a second camera, not a copy of the scene. [member Camera3D.current] is
## per-viewport, so this leaves the root viewport's own rendering exactly as it was.
##
## Head-locked content (a cursor, a HUD quad) can be parented to [member head], which stands in for
## the XrealHeadTracker. Find it from anywhere with [method XrealShared.find_preview_head].
##
## Controls: right-drag looks, WASD/QE moves (Shift sprints), R returns the flycam to the origin,
## and Tab hands the window's mouse and keys to the app, which then receives them through
## [signal app_input] and drives whatever it likes with them. Tab hands them back.
##
## It frees itself on device, so leaving it in a shipped scene costs nothing.

## Tab moved control between the flycam and the app. The window title shows which has it.
signal flycam_active_changed(active: bool)
## An input event the preview window received while the flycam did NOT have control, forwarded so
## the app can drive its own thing with the same mouse. Tab itself never arrives here, because it is
## what switches back.
signal app_input(event: InputEvent)

## Shift multiplies the flycam speed by this while held.
const SPRINT_MULTIPLIER := 3.0
## Looking straight up or down would make yaw meaningless, so stop just short of the poles.
const PITCH_LIMIT_DEG := 89.0
## Gap left between the main window and the preview, in pixels.
const WINDOW_GAP := 24

## Turn off to keep the preview window closed (the component then does nothing at all).
@export var enabled := true
## Window title. The side that currently holds the mouse and keys follows it in brackets.
@export var window_title := "XREAL glasses preview"
## Size of the preview window, in pixels.
@export var window_size := Vector2i(1280, 720)
## Flycam speed in m/s.
@export var move_speed := 2.5
## Mouse-look speed, in degrees per pixel of mouse motion.
@export var look_sensitivity := 0.15
## Vertical FOV of the preview camera. XREAL glasses sit around 50 degrees, far narrower than
## Godot's 75 degree default, so this frames roughly what the wearer would see.
@export var camera_fov := 50.0

## Stand-in for the XrealHeadTracker: parent head-locked content here. Null on device and until
## _ready has run.
var head: Node3D
## Whether the flycam currently owns the window's mouse and keys. Tab toggles it.
var flycam_active := true

var _window: Window
var _camera: Camera3D
# Physical keycodes currently held. Physical, so WASD stays under the same fingers on a keyboard
# layout that is not QWERTY.
var _pressed := {}
var _looking := false
var _yaw := 0.0
var _pitch := 0.0

func _ready() -> void:
	# On device the glasses show the real thing, so this is desktop-only.
	if not enabled or XrealShared.is_native_runtime():
		set_process(false)
		queue_free()
		return
	# Subwindows are embedded INSIDE the main window by default, which would defeat the point. This
	# only ever runs on desktop, so the single-window platforms are never touched.
	get_tree().root.gui_embed_subwindows = false
	_build_window()

func _process(delta: float) -> void:
	if _window == null or not _window.visible or _pressed.is_empty():
		return
	# Strafe and forward follow the gaze; up and down stay world-vertical, which is what makes a
	# flycam predictable when looking down at the floor.
	var move := head.transform.basis * Vector3(_axis(KEY_D, KEY_A), 0.0, _axis(KEY_S, KEY_W))
	move.y += _axis(KEY_E, KEY_Q)
	if move.is_zero_approx():
		return
	var speed := move_speed * (SPRINT_MULTIPLIER if _pressed.has(KEY_SHIFT) else 1.0)
	head.position += move.normalized() * speed * delta

## Build the preview window and its camera rig. The window shares the root viewport's World3D, so
## it draws the same scene the glasses would.
func _build_window() -> void:
	_window = Window.new()
	_window.size = window_size
	_window.position = _beside_main_window()
	_window.world_3d = get_tree().root.world_3d
	# Closing just puts the preview away: the app keeps running on the main window, so a stray click
	# on the X does not end a debug session.
	_window.close_requested.connect(_window.hide)
	# A window that loses focus never receives the matching key-up events, so held keys would stick.
	_window.focus_exited.connect(_release_all)
	_window.window_input.connect(_on_window_input)
	add_child(_window)

	head = Node3D.new()
	head.name = "Head"
	head.add_to_group(XrealShared.GROUP_DESKTOP_PREVIEW)
	_window.add_child(head)

	_camera = Camera3D.new()
	_camera.fov = camera_fov
	_camera.current = true
	head.add_child(_camera)

	set_flycam_active(flycam_active)  # titles the window; nothing is connected yet, so the signal is a no-op

## Top-left corner for the preview: beside the main window, pulled back onto the screen when it
## would not fit there, so it never opens off-screen on a single monitor.
func _beside_main_window() -> Vector2i:
	var screen := DisplayServer.window_get_current_screen(DisplayServer.MAIN_WINDOW_ID)
	var usable := DisplayServer.screen_get_usable_rect(screen)
	var main_pos := DisplayServer.window_get_position(DisplayServer.MAIN_WINDOW_ID)
	var main_size := DisplayServer.window_get_size(DisplayServer.MAIN_WINDOW_ID)
	var limit := usable.position + usable.size - window_size
	return Vector2i(
		clampi(main_pos.x + main_size.x + WINDOW_GAP, usable.position.x, maxi(limit.x, usable.position.x)),
		clampi(main_pos.y, usable.position.y, maxi(limit.y, usable.position.y)))

## Preview-window input. It comes through the window's own signal rather than the global Input
## singleton, because that one reports keys held while ANOTHER window has focus.
func _on_window_input(event: InputEvent) -> void:
	var tab := event as InputEventKey
	if tab and tab.pressed and not tab.echo and tab.physical_keycode == KEY_TAB:
		set_flycam_active(not flycam_active)
		return
	if not flycam_active:
		app_input.emit(event)
		return
	var button := event as InputEventMouseButton
	if button and button.button_index == MOUSE_BUTTON_RIGHT:
		_looking = button.pressed
		# Capturing frees the pointer from the window edge, so a long look-around does not run out of
		# desk. It lasts only while the button is down.
		Input.mouse_mode = Input.MOUSE_MODE_CAPTURED if _looking else Input.MOUSE_MODE_VISIBLE
		return
	var motion := event as InputEventMouseMotion
	if motion:
		if _looking:
			_yaw = wrapf(_yaw - motion.relative.x * look_sensitivity, -180.0, 180.0)
			_pitch = clampf(
				_pitch - motion.relative.y * look_sensitivity, -PITCH_LIMIT_DEG, PITCH_LIMIT_DEG)
			head.rotation = Vector3(deg_to_rad(_pitch), deg_to_rad(_yaw), 0.0)
		return
	var key := event as InputEventKey
	if key and not key.echo:
		if not key.pressed:
			_pressed.erase(key.physical_keycode)
		elif key.physical_keycode == KEY_R:
			reset_view()  # one-shot, so it never joins the held-key set _process walks
		else:
			_pressed[key.physical_keycode] = true

## Put the flycam back at the origin looking down -Z, where the head sits on device before tracking
## moves it. Bound to R, and public so the app can offer its own way back.
func reset_view() -> void:
	_yaw = 0.0
	_pitch = 0.0
	head.transform = Transform3D.IDENTITY

## Hand the window's mouse and keys to the flycam or to the app. It drops any held key on the way, so
## neither side inherits a key it never saw pressed, and the title names who has control.
func set_flycam_active(active: bool) -> void:
	_release_all()
	flycam_active = active
	_window.title = "%s [%s]" % [window_title, "flycam" if active else "app"]
	flycam_active_changed.emit(active)

## Drop every held key and end any look in progress, for when the window stops receiving events.
func _release_all() -> void:
	_pressed.clear()
	if _looking:
		_looking = false
		Input.mouse_mode = Input.MOUSE_MODE_VISIBLE

## +1 while [param positive] is held, -1 while [param negative] is, 0 with both or neither.
func _axis(positive: Key, negative: Key) -> float:
	return float(_pressed.has(positive)) - float(_pressed.has(negative))
