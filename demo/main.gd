extends Node3D
## Demo for the Godot XREAL addon, a consumer of the per-feature components in
## addons/godot_xreal/features/*.
##
## The static content lives in sub-scenes instanced by demo/main.tscn:
##   - $ARScene (demo/ar_scene.tscn + ar_scene.gd) holds the 3D world: a WorldEnvironment whose
##     background is black (the XREAL optical see-through display reads black as transparent),
##     the sun, the ring of colored boxes (with colliders for the phone-pointer raycast), plus
##     the head-locked controller cursor and phone-IMU pointer, exposed as `cursor` and
##     `phone_pointer`.
##   - $PhoneScreen (demo/phone_screen.tscn + phone_screen.gd) is the phone-only touch
##     controller layer; main.tscn wires its signals to the _on_tc_* handlers here.
##   - $Xreal* are the addon feature components (camera, planes, anchors, image tracking, mesh,
##     hands, photo and blend capture, FPV streaming), instanced straight from
##     addons/godot_xreal/features/*.tscn. Each is self-contained: this script only toggles them
##     from the phone menu and reflects their state back onto the toggles. Delete the instances
##     you don't need, since they know nothing about each other.
## The debug UI ($UI) also lives in main.tscn, its Recenter button wired the same way.
##
## This script does only the demo glue: map the phone-menu controls to the drop-in feature
## components and pump the controller IMU into the phone pointer. XrealXRRuntime owns all backend
## initialization, so application code sees the same standard Godot XR hierarchy on every runtime.

# The GDExtension classes (XrealHeadTracker, XrealSystem, XrealAR, XrealHandTracker,
# XrealCameraFeed) exist only when the native extension loaded, so every lookup below is
# defensive: a missing or failed extension shows a diagnostic instead of a blank scene.

# Demo-side shared-storage saver (pure GDScript MediaStore through JavaClassWrapper): the capture,
# recorder and mesh components return the saved file's path and the demo forwards it into the phone
# gallery or, for a mesh snapshot, Documents. It stays out of the addon on purpose, because
# publishing an app's files to shared storage is the app's decision.
const StorageHelper := preload("res://demo/storage_helper.gd")

# XrealHeadTracker key/action constants, mirrored locally so this script parses even
# when the GDExtension is absent (desktop editor).
const XREAL_KEY_MULTI := 1
const XREAL_KEY_MENU := 4
const XREAL_ACTION_LONG_PRESS := 3

var _tracker: Node3D
var _system: Object
var _extension_loaded := false
# One-shot AR-feature availability diagnostic: logs which native AR ABIs resolved on this device,
# a short delay after boot (so the session has come up). See docs/develop/plans/ar-features-plan.md.
var _ar_diag_frames := 0
var _phone_pointer: Node3D
var _cursor_mat: StandardMaterial3D
# Desktop only: the phone pointer has no IMU to follow off device, so the preview window's mouse
# aims it instead, once Tab has handed the mouse over. Angles in degrees, zeroed by R.
const PREVIEW_POINTER_SENSITIVITY := 0.15
var _pointer_yaw := 0.0
var _pointer_pitch := 0.0
var _pointer_aimed := false
var _preview: Node  # the desktop preview window component, null on device
# No-glasses watchdog: the head-tracking session only comes up with the glasses connected, so if
# tracking has not started within this window we take them to be absent, show a message and quit
# instead of sitting forever in the session-bootstrap retry loop. Detection uses no heuristic: it
# keys on whether tracking actually started, not on a display name or resolution guess. It
# disarms for good the moment the session goes live, on the `display_started` signal (the
# reliable "glasses up" event) or on the first `is_tracking()` true. A mid-session unplug is a
# separate, unhandled case. The window is 15 s because session bring-up takes ~4-6 s normally but
# a cold first launch after a (re)install is slower, and a false "no glasses" quit while they ARE
# connected costs more than a couple of extra seconds of waiting.
const NO_GLASSES_TIMEOUT_S := 15.0
const NO_GLASSES_QUIT_DELAY_S := 3.0
var _boot_elapsed := 0.0
var _tracking_seen := false
var _no_glasses := false

## Backstop for a toggle whose component never reports back. Every failure path in xreal_camera.gd
## and xreal_stream.gd does emit active_changed, apart from stopping a stream that is still
## pairing, which returns silently, so this covers the quiet paths rather than the normal route.
## It has to clear the slowest honest wait: pairing gives itself 4 s of discovery, 5 s to connect
## and 5 s of handshake before it gives up.
const SWITCH_TIMEOUT_MS := 20000
## Toggles that are mid-switch: tapped, but the component has not yet said what actually happened.
## Maps the control name to the deadline (Time.get_ticks_msec) past which it is handed back anyway.
var _switching: Dictionary = {}

@onready var _status: Label = $UI/Panel/Margin/VBox/Status
@onready var _ar: Node3D = $ARScene
@onready var _cursor: MeshInstance3D = $ARScene.cursor
@onready var _xr_runtime: XrealXRRuntime = $XROrigin3D/XrealXRRuntime
@onready var _xr_camera: XRCamera3D = $XROrigin3D/XRCamera3D
@onready var _xr_controllers: Array[XRController3D] = [
	$XROrigin3D/LeftController, $XROrigin3D/RightController,
]
# The addon feature components, instanced in main.tscn as children of Main, a world-fixed node,
# which the world-locked features require.
@onready var _camera: Node3D = $XrealCamera
@onready var _planes: Node3D = $XrealPlanes
@onready var _anchors: Node3D = $XrealAnchors
@onready var _image_tracking: Node3D = $XrealImageTracking
@onready var _mesh: Node3D = $XrealMesh
@onready var _photo_capture: Node = $XrealPhotoCapture
@onready var _blend_capture: Node = $XrealBlendCapture
@onready var _stream: Node = $XrealStream
@onready var _recorder: Node = $XrealVideoRecorder

func _ready() -> void:
	_extension_loaded = _xr_runtime.is_xreal_active()
	_system = _xr_runtime.get_xreal_system()
	_tracker = _xr_runtime.get_xreal_driver()
	_xr_runtime.display_started.connect(_on_display_started)
	_xr_runtime.key_event.connect(_on_key_event)
	_xr_runtime.wearing_changed.connect(_on_wearing_changed)
	# Read the buttons through the standard XR nodes, the way an OpenXR project would. Both
	# controllers are connected because the phone can act as either hand. The equivalent InputMap
	# actions (xr_select, xr_grab) are published too, but a click coming from the glasses callback
	# is a mid-process pulse, so polling is_action_just_pressed() would miss it; the signal is
	# synchronous and never does.
	for controller in _xr_controllers:
		controller.button_pressed.connect(_on_xr_button_pressed)
		controller.button_released.connect(_on_xr_button_released)
	# Async feature states (camera start is lazy, stream pairing is async) are reflected back onto
	# the phone-menu toggles through the components' active_changed signals.
	_camera.active_changed.connect(_on_feature_active.bind("camera"))
	_stream.active_changed.connect(_on_feature_active.bind("stream"))
	_recorder.active_changed.connect(_on_feature_active.bind("record"))
	# A finished recording (the finalized mp4's path) goes into the phone gallery, like the photos.
	_recorder.finished.connect(func(path: String) -> void: StorageHelper.save_video(path))
	# Surface each feature component's `error` signal at the load site, here the debug Status label
	# and logcat. A real app might disable a control or show a toast; the point is that the failure
	# is detectable rather than buried in a warning.
	for feature in [_camera, _planes, _anchors, _image_tracking, _mesh, _photo_capture, _blend_capture, _stream, _recorder]:
		if feature and feature.has_signal(&"error"):
			feature.error.connect(_on_feature_error)
	# Label the "Cycle Image" button with the active image-tracking set as it changes.
	if _image_tracking and _image_tracking.has_signal(&"set_changed"):
		_image_tracking.set_changed.connect(_on_image_set_changed)
	# Report where a mesh snapshot landed, which is what the user needs to pull it off the device.
	if _mesh and _mesh.has_signal(&"snapshot_saved"):
		_mesh.snapshot_saved.connect(_on_mesh_snapshot_saved)
	_setup_touch_controller()
	_setup_desktop_pointer()
	# Reflect the boot camera state on the phone-menu toggle (on only when the XrealCamera
	# instance was saved with `enabled` ticked; the other toggles start off).
	_set_controller_toggle("camera", _camera.enabled)
	# "Save Mesh" has nothing to write until meshing is on, so it starts inert and follows the Mesh
	# toggle from there.
	_set_controller_disabled("mesh_save", true)

## Set up the runtime side of the phone touch controller, meaning the head-locked 3D cursor and
## the host-preview camera. $PhoneScreen keeps its layout and signal wiring static in
## phone_screen.tscn and main.tscn, and it renders only on the phone's root viewport, so the
## glasses keep showing the 3D scene.
func _setup_touch_controller() -> void:
	# The head-locked cursor makes phone touches visible in the glasses, which proves the split, so
	# reparent it under whatever is playing the head: the tracker on device, the preview window's
	# head on desktop. With neither there is nothing to lock it to, so drop it.
	var head: Node3D = _xr_camera if _tracker else XrealShared.find_preview_head(get_tree())
	if head:
		_cursor.reparent(head, false)
		_cursor_mat = _cursor.material_override as StandardMaterial3D
	else:
		_cursor.queue_free()
		_cursor = null
	if _extension_loaded:
		_setup_phone_pointer()

	# The XREAL backend disables host 3D once its eye viewports are live. OpenXR renders this same
	# XRCamera3D directly through the root XR viewport.

## Desktop only: off device the phone pointer has no IMU to follow, so the preview window's mouse
## aims it instead. Tab hands that mouse between flying the camera and aiming the pointer, and R
## zeroes whichever of the two holds it.
func _setup_desktop_pointer() -> void:
	var preview := get_node_or_null(^"XrealDesktopPreview")
	if _extension_loaded or preview == null or not preview.has_signal(&"app_input"):
		return
	_preview = preview
	_preview.app_input.connect(_on_preview_app_input)
	_preview.flycam_active_changed.connect(_on_preview_flycam_changed)
	_setup_phone_pointer()

## Tab moved control of the preview window's mouse. Capture it while the app is aiming, so a long
## sweep does not run out of desk, and give it back when the flycam takes over.
func _on_preview_flycam_changed(active: bool) -> void:
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE if active else Input.MOUSE_MODE_CAPTURED

## Aim the phone pointer with the preview window's mouse; R points it forward again. This stands in
## for tilting the phone, which is what drives it on device.
func _on_preview_app_input(event: InputEvent) -> void:
	var motion := event as InputEventMouseMotion
	if motion:
		var s := PREVIEW_POINTER_SENSITIVITY
		_pointer_yaw = wrapf(_pointer_yaw - motion.relative.x * s, -180.0, 180.0)
		_pointer_pitch = clampf(_pointer_pitch - motion.relative.y * s, -89.0, 89.0)
		_pointer_aimed = true
		return
	var key := event as InputEventKey
	if key and key.pressed and not key.echo and key.physical_keycode == KEY_R:
		_pointer_yaw = 0.0
		_pointer_pitch = 0.0
		_pointer_aimed = true

func _on_tc_touchpad(value: Vector2) -> void:
	_xr_runtime.set_controller_axis(value)
	if _cursor:
		_cursor.position = Vector3(value.x * 0.8, value.y * 0.5, -2.0)

func _on_tc_touchpad_released() -> void:
	_xr_runtime.set_controller_axis(Vector2.ZERO, false)
	if _cursor:
		_cursor.position = Vector3(0.0, 0.0, -2.0)

func _on_tc_trigger(pressed: bool) -> void:
	_xr_runtime.set_controller_button(&"trigger_click", pressed)
	if _cursor_mat:
		_cursor_mat.albedo_color = Color(1.0, 0.4, 0.3) if pressed else Color(0.3, 0.85, 1.0)

## Right/left hand toggle from the on-screen controller, fed through the addon runtime.
func _on_tc_hand(is_right: bool) -> void:
	_xr_runtime.set_controller_hand(is_right)

func _on_tc_grip(pressed: bool) -> void:
	_xr_runtime.set_controller_button(&"grip_click", pressed)

## Canonical XR buttons, whatever fed them: the on-screen controller, a glasses key, or an OpenXR
## controller. trigger_click and primary_click both mean "select" (they share the xr_select action).
func _on_xr_button_pressed(input_name: String) -> void:
	match input_name:
		"trigger_click", "primary_click":
			if _phone_pointer and _phone_pointer.has_method(&"select"):
				_phone_pointer.select()
		"grip_click":
			if _cursor:
				_cursor.scale = Vector3.ONE * 1.6

func _on_xr_button_released(input_name: String) -> void:
	if input_name == "grip_click" and _cursor:
		_cursor.scale = Vector3.ONE

func _on_tc_menu() -> void:
	_xr_runtime.pulse_controller_button(&"menu_button")
	_on_recenter_pressed()

## Phone-menu "Camera" toggle, driving the XrealCamera component. set_enabled(true) only
## *requests* the camera, since the capture starts lazily once tracking is live; an async start
## failure comes back through active_changed(false), which _ready wires to the toggle. An
## immediate refusal, on a device without an RGB camera, flips the toggle back here.
func _on_tc_camera(on: bool) -> void:
	print("[demo] camera toggle -> %s" % ("on" if on else "off"))
	_begin_switch("camera")
	if _camera.set_enabled(on) != on:
		# Refused on the spot (no RGB camera): no active_changed will follow, so close the switch here.
		_set_controller_toggle("camera", false)
		_end_switch("camera")

## Phone-menu "Plane" toggle, driving the XrealPlanes boundary-polygon overlay, which switches
## tracking to 6DoF while on.
func _on_tc_plane(on: bool) -> void:
	print("[demo] plane toggle -> %s" % ("on" if on else "off"))
	if _planes.set_enabled(on) != on:
		_set_controller_toggle("plane", false)

## Phone-menu "Anchor" toggle, driving the XrealAnchors component. A pinch or the "Place" button
## then drops an anchor at the hand fingertip.
func _on_tc_anchor(on: bool) -> void:
	print("[demo] anchor toggle -> %s" % ("on" if on else "off"))
	if _anchors.set_enabled(on) != on:
		_set_controller_toggle("anchor", false)

## Phone-menu "Image" toggle, driving the XrealImageTracking component (main.tscn sets its
## manifest_path to the demo's reference.json).
func _on_tc_image(on: bool) -> void:
	print("[demo] image toggle -> %s" % ("on" if on else "off"))
	if _image_tracking.set_enabled(on) != on:
		_set_controller_toggle("image", false)

## Phone-menu "Mesh" toggle, driving the XrealMesh component (Air 2 Ultra only).
func _on_tc_mesh(on: bool) -> void:
	print("[demo] mesh toggle -> %s" % ("on" if on else "off"))
	var active: bool = _mesh.set_enabled(on)
	if active != on:
		_set_controller_toggle("mesh", false)
	# "Save Mesh" writes what is in the scene, so it tracks the toggle rather than the device: OFF
	# drops every block mesh, leaving nothing to save.
	_set_controller_disabled("mesh_save", not active)

## Phone-menu "Save Mesh" button: write the current scan to a JSON snapshot for the editor dock.
## Where it landed comes back through snapshot_saved, and a failure through the error signal.
func _on_tc_mesh_save() -> void:
	print("[demo] save mesh")
	_mesh.save_snapshot()

## The mesh component wrote a snapshot. The full path goes on the Status label because that is what
## the user needs in order to `adb pull` it.
func _on_mesh_snapshot_saved(path: String, block_count: int) -> void:
	print("[demo] mesh snapshot -> %s (%d blocks)" % [path, block_count])
	# Publish it to shared storage the way the captures and recordings go, which moves the file out of
	# the app's own directory and into Documents/godot-xreal, where the Files app and a plain
	# `adb pull` reach it. A refusal is not fatal: the snapshot is still on disk at `path`, so the
	# status line reports whichever location actually holds it.
	var location := path
	if StorageHelper.save_document(path):
		location = "Documents/godot-xreal/%s" % path.get_file()
	if _status:
		_status.text = "Mesh saved (%d blocks): %s" % [block_count, location]

## Phone-menu "Record" toggle, driving the XrealVideoRecorder component: with the camera on it
## records the camera+AR blend, with the camera off the AR view alone. The resulting state comes
## back through active_changed, and a start is refused while streaming, since the two share one
## HW encoder. A stop delivers the finished mp4 through `finished`, which _ready wires into the
## phone gallery.
func _on_tc_record(on: bool) -> void:
	print("[demo] record toggle -> %s" % ("on" if on else "off"))
	_begin_switch("record")
	_recorder.set_enabled(on)

## Phone-menu "Stream" toggle, driving the XrealStream component. Pairing is async, so the
## component reports the resulting state back through its active_changed signal, wired in _ready,
## which flips the phone toggle to match. That includes a start refused while recording, since
## the two share one HW encoder.
func _on_tc_stream(on: bool) -> void:
	print("[demo] stream toggle -> %s" % ("on" if on else "off"))
	_begin_switch("stream")
	_stream.set_enabled(on)

## Phone-menu "Place" button: place a spatial anchor at the currently-tracked hand fingertip.
func _on_tc_place() -> void:
	_anchors.place_at_fingertip()

## Phone-menu "Cycle Image" button: cycle the active image-tracking set.
func _on_tc_image_cycle() -> void:
	_image_tracking.cycle_set()

## Phone-menu "Photo" button: capture a photo from the RGB camera (One Series), then put it in
## the phone gallery so it can be viewed (and deleted) right on the phone, with no adb needed.
func _on_tc_capture() -> void:
	var path: String = await _photo_capture.capture_photo()
	if path != "":
		StorageHelper.save_image(path)

## Phone-menu "Blend Photo" button: capture a blended camera+AR (mixed-reality) photo, into the
## phone gallery like the plain photo.
func _on_tc_blend() -> void:
	var path: String = await _blend_capture.capture_blended()
	if path != "":
		StorageHelper.save_image(path)

## Phone-menu "Exit": quit. The touch controller shows a Yes/No dialog first and emits this only
## on Yes. This is the exit for glasses without physical keys (the Air 2 Ultra has only an
## EC-dimming button).
func _on_tc_exit() -> void:
	get_tree().quit()

## No-glasses watchdog: if the head-tracking session has not started within NO_GLASSES_TIMEOUT_S,
## the glasses are not connected, so show a message on the phone and quit. Once tracking is seen
## it disarms permanently. It runs only with the real extension, never on desktop, where tracking
## is inert by design.
func _check_no_glasses(delta: float) -> void:
	if _tracking_seen or _no_glasses or not _extension_loaded or _tracker == null:
		return
	if _tracker.has_method(&"is_tracking") and _tracker.is_tracking():
		_tracking_seen = true
		return
	_boot_elapsed += delta
	if _boot_elapsed >= NO_GLASSES_TIMEOUT_S:
		_no_glasses = true
		_show_no_glasses_and_quit()

## Cover the screen with a "no glasses, quitting" message, then quit after a short delay so the
## message is readable. It uses its own top CanvasLayer, so it sits over the debug UI and the
## controller.
func _show_no_glasses_and_quit() -> void:
	print("[demo] no XREAL glasses detected within %.0fs, quitting" % NO_GLASSES_TIMEOUT_S)
	var layer := CanvasLayer.new()
	layer.layer = 128
	var bg := ColorRect.new()
	bg.color = Color(0, 0, 0, 1)
	bg.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	layer.add_child(bg)
	var label := Label.new()
	label.text = "No XREAL glasses connected.\nExiting the app."
	label.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	label.vertical_alignment = VERTICAL_ALIGNMENT_CENTER
	label.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	# Scale the font to the screen (≈5% of the shorter side) so it's legible at any resolution
	# instead of the tiny theme default.
	var vp := get_viewport().get_visible_rect().size
	label.add_theme_font_size_override(&"font_size", int(minf(vp.x, vp.y) * 0.05))
	label.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	layer.add_child(label)
	add_child(layer)
	await get_tree().create_timer(NO_GLASSES_QUIT_DELAY_S).timeout
	# Tracking may have come up during the readability delay, e.g. the glasses were just plugged in,
	# so do not quit: drop the overlay and re-arm the watchdog, which disarms permanently on the next
	# tracked frame.
	if _tracker and _tracker.has_method(&"is_tracking") and _tracker.is_tracking():
		layer.queue_free()
		_no_glasses = false
		return
	get_tree().quit()

## The active image-tracking set changed, so show its name on the phone-menu "Cycle Image" button.
func _on_image_set_changed(image_set_name: String) -> void:
	var ps := get_node_or_null(^"PhoneScreen")
	if ps and ps.has_method(&"set_button_label"):
		ps.set_button_label("image_cycle", "Cycle: %s" % image_set_name)

## A feature component reported an error through its `error` signal, so show it on the debug
## Status label and log it, which keeps the failure visible at the load site instead of buried in
## warnings. Failures that need USER action, currently the wedged glasses camera that only a USB
## re-plug clears, additionally pop a modal dialog on the phone screen, because a status-label
## line is too easy to miss for an error whose fix is physical.
func _on_feature_error(message: String) -> void:
	print("[demo] feature error: %s" % message)
	if _status:
		_status.text = message
	# "wedged" is the marker xreal_camera.gd puts in its camera-failure messages. It fires on the
	# Camera:ON tap, since the start is lazy, and that is the earliest point the wedge is detectable
	# at all: nothing touches the camera at app launch, so launch-time detection would need a
	# start/stop probe. The dialog text is deliberately short and jargon-free; the technical detail
	# stays in the log.
	if message.contains("wedged"):
		_show_error_dialog("Camera unavailable.\nReplug the glasses USB cable,\nthen restart this app.")

## Modal error notice on the phone screen, display 0, where the user is tapping. It is reused
## across errors and sized like the rest of the phone UI, because theme-default dialog text is
## unreadably small on a 480 dpi phone.
var _error_dialog: AcceptDialog

func _show_error_dialog(text: String) -> void:
	if _error_dialog == null:
		_error_dialog = AcceptDialog.new()
		_error_dialog.title = "XREAL"
		var vp := get_viewport().get_visible_rect().size
		var font_px := int(minf(vp.x, vp.y) * 0.04)
		var label := _error_dialog.get_label()
		label.add_theme_font_size_override(&"font_size", font_px)
		# Wrap instead of stretching the window past the screen edge (the default label never wraps).
		label.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
		label.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
		_error_dialog.get_ok_button().add_theme_font_size_override(&"font_size", font_px)
		add_child(_error_dialog)
	_error_dialog.dialog_text = text
	# Fixed width (85% of the shorter screen side) so wrapping has something to wrap against.
	var s := get_viewport().get_visible_rect().size
	_error_dialog.popup_centered(Vector2i(int(minf(s.x, s.y) * 0.85), 0))

## Push a toggle's on/off state onto the phone-menu controller. It keeps the UI in sync when the
## app, not the user, changes it, e.g. after a failed camera start or an unsupported plane mode.
func _set_controller_toggle(control_name: String, on: bool) -> void:
	var ps := get_node_or_null(^"PhoneScreen")
	if ps and ps.has_method(&"set_toggle"):
		ps.set_toggle(control_name, on)

## Grey out a phone-menu control, for a state its own toggle governs rather than the device: "Save
## Mesh" is inert until meshing is on. Capability gating goes through _apply_capabilities instead.
func _set_controller_disabled(control_name: String, disabled: bool) -> void:
	var ps := get_node_or_null(^"PhoneScreen")
	if ps and ps.has_method(&"set_disabled"):
		ps.set_disabled(control_name, disabled)

## Mark a phone-menu control as mid-switch (inert, labelled "…").
func _set_controller_busy(control_name: String, busy: bool) -> void:
	var ps := get_node_or_null(^"PhoneScreen")
	if ps and ps.has_method(&"set_busy"):
		ps.set_busy(control_name, busy)

## A feature component settled on a state: mirror it onto the phone toggle, and end the switch that
## was waiting to hear it.
func _on_feature_active(active: bool, control_name: String) -> void:
	_set_controller_toggle(control_name, active)
	_end_switch(control_name)

## Take a toggle out of service until its component reports the resulting state. Camera and stream
## both take real time to change: the camera only starts once head tracking is live, and streaming
## has to finish pairing first. Left tappable, that window let taps stack up into a start/stop/start
## churn on hardware that is slow to open and easy to wedge (a camera killed mid-start stays held
## until the USB is re-plugged).
func _begin_switch(control_name: String) -> void:
	_switching[control_name] = Time.get_ticks_msec() + SWITCH_TIMEOUT_MS
	_set_controller_busy(control_name, true)

## The switch is over, so the control goes back into service. Whether the device supports it at all
## is tracked separately by the controller, so this cannot resurrect a capability-disabled control.
func _end_switch(control_name: String) -> void:
	if not _switching.erase(control_name):
		return  # not switching - an active_changed the app caused rather than the user
	_set_controller_busy(control_name, false)

## Nothing may leave a control dead forever, so give up waiting eventually. Reaching this is a bug
## or a silent component path, hence the warning: the toggle is usable again either way.
func _check_switch_timeouts() -> void:
	if _switching.is_empty():
		return
	var now := Time.get_ticks_msec()
	for control_name in _switching.keys():  # keys() copies, so _end_switch may erase while we walk it
		if now >= int(_switching[control_name]):
			push_warning("[demo] %s never reported a state within %d s - re-enabling its toggle"
				% [control_name, SWITCH_TIMEOUT_MS / 1000.0])
			_end_switch(control_name)

## Grey out (make inert) the phone-menu controls whose capability the device lacks, once the
## session is up and `XrealSystem.get_capabilities()` can answer. Each control maps to one entry of
## that snapshot. The camera-dependent capture buttons (Photo, Blend Photo) follow the camera.
## Streaming and recording cast and record the AR view even without a camera, so no *device*
## disables them; the *renderer* can, though: under Vulkan the HW encoder has no GL texture to read
## (until the vulkan-path stage-4 bridge), so both follow the encoder capability.
##
## Missing keys fall back the way the control should behave on an extension that predates them:
## a device capability to "absent", the renderer-side encoder to "usable".
func _apply_capabilities(caps: Dictionary) -> void:
	var ps := get_node_or_null(^"PhoneScreen")
	if ps == null or not ps.has_method(&"set_disabled"):
		return
	var cam: bool = caps.get("rgb_camera", false)
	var anchor: bool = caps.get("spatial_anchors", false)
	var image: bool = caps.get("image_tracking", false)
	var enc: bool = caps.get("render_texture_encoder", true)
	var avail := {
		"camera": cam, "capture": cam, "blend": cam,
		"plane": caps.get("plane_detection", false),
		"anchor": anchor, "place": anchor,
		"image": image, "image_cycle": image,
		"mesh": caps.get("depth_mesh", false),
		"stream": enc, "record": enc,
	}
	for control_name in avail:
		ps.set_disabled(control_name, not bool(avail[control_name]))

## Reveal the phone-IMU 3D pointer (demo/phone_pointer.gd), defined in ar_scene.tscn and hidden
## until the NRController has started, so no beam shows before it can be driven.
func _setup_phone_pointer() -> void:
	_phone_pointer = _ar.phone_pointer
	# Leave it hidden here. phone_pointer.gd reveals the beam on its first aimed frame (once recenter
	# has run and the origin sits at the hand offset), so the beam never shows at the default head
	# position and blocks the view before it can be aimed.

func _on_recenter_pressed() -> void:
	_xr_runtime.recenter()

func _on_display_started() -> void:
	# The glasses display and tracking are live, so disarm the no-glasses watchdog. This is the
	# reliable "glasses up" event, for when is_tracking() lags past the timeout on a slow cold start.
	_tracking_seen = true
	# Make the current head direction "forward".
	_xr_runtime.recenter()

func _on_key_event(key: int, action: int) -> void:
	# Long-press the MENU key to recenter (current head direction becomes "forward"),
	# replacing the on-screen button for a glasses-only workflow.
	if key == XREAL_KEY_MENU and action == XREAL_ACTION_LONG_PRESS:
		_on_recenter_pressed()
	# Long-press the MULTI key to quit the app (glasses-only exit). NB: only the One series has these
	# physical keys. The Air 2 Ultra has just an EC-dimming button, so it exits through the
	# phone-menu Exit.
	elif key == XREAL_KEY_MULTI and action == XREAL_ACTION_LONG_PRESS:
		get_tree().quit()

func _on_wearing_changed(wearing: bool) -> void:
	if wearing:
		# Recenter the instant the glasses are actually worn (and the wearer is looking
		# forward), so "forward" isn't captured while they sit tilted on a desk.
		_on_recenter_pressed()

func _process(_delta: float) -> void:
	_check_no_glasses(_delta)
	_check_switch_timeouts()
	# One-shot AR-feature availability diagnostic, ~2 s in (once the session has had time to come up),
	# so a glance at logcat shows which native AR ABIs this device exposes.
	if _ar_diag_frames >= 0 and _system:
		_ar_diag_frames += 1
		if _ar_diag_frames == 120:
			_ar_diag_frames = -1  # done
			var caps: Dictionary = (_system.get_capabilities()
				if _system.has_method(&"get_capabilities") else {})
			print("[demo] capabilities: %s" % caps)
			_apply_capabilities(caps)
	# Desktop only: re-apply the mouse-aimed pointer every frame, so it keeps hanging off the preview
	# head and follows the flycam even while the mouse is flying it rather than aiming.
	if _pointer_aimed:
		_phone_pointer.aim_from(
			Basis.from_euler(Vector3(deg_to_rad(_pointer_pitch), deg_to_rad(_pointer_yaw), 0.0)),
			(_preview.head as Node3D).global_transform)
	# The addon owns XREAL controller polling and fusion. The demo consumes only the standard
	# XRController3D pose to draw its optional ray visualization.
	if _phone_pointer and _extension_loaded:
		var controller := _xr_runtime.get_active_controller()
		if controller != null and controller.get_is_active():
			_phone_pointer.aim_from_transform(controller.global_transform)
