extends Node3D
## Demo for the Godot XREAL addon — a consumer of the per-feature components in
## addons/godot_xreal/features/*.
##
## The static content lives in sub-scenes instanced by demo/main.tscn:
##   - $ARScene (demo/ar_scene.tscn + ar_scene.gd) — the 3D world: WorldEnvironment (black
##     background — on the XREAL optical see-through display black reads as transparent), sun,
##     the ring of colored boxes (with colliders for the phone-pointer raycast), plus the
##     head-locked controller cursor and phone-IMU pointer, exposed as `cursor` / `phone_pointer`.
##   - $PhoneScreen (demo/phone_screen.tscn + phone_screen.gd) — the phone-only touch
##     controller layer; its signals are wired to the _on_tc_* handlers here via main.tscn.
##   - $Xreal* — the addon feature components (camera, planes, anchors, image tracking, mesh,
##     hands, photo/blend capture, FPV streaming), instanced straight from
##     addons/godot_xreal/features/*.tscn. Each is self-contained: this script only toggles them
##     from the phone menu and reflects their state back onto the toggles. Delete the instances
##     you don't need — they don't know about each other.
## The debug UI ($UI) also lives in main.tscn, its Recenter button wired the same way.
##
## This script does only the demo glue: detect the GDExtension, instance the addon camera rig
## (addons/godot_xreal/xreal_rig.tscn — an XrealHeadTracker with a Camera3D child), map the
## phone-menu controls to the feature components, and pump the controller IMU into the phone
## pointer per frame. On XREAL hardware the camera looks around with the wearer's head; on
## desktop the rig stays at identity and the features are inert, so the scene is still runnable.

# The GDExtension classes (XrealHeadTracker / XrealSystem / XrealAR / XrealHandTracker /
# XrealCameraFeed) only exist if the native extension loaded. We look everything up defensively
# so a missing/failed extension shows a diagnostic instead of a blank scene.
const RIG_SCENE := "res://addons/godot_xreal/xreal_rig.tscn"

# XrealHeadTracker key/action constants, mirrored locally so this script parses even
# when the GDExtension is absent (desktop editor).
const XREAL_KEY_MULTI := 1
const XREAL_KEY_MENU := 4
const XREAL_ACTION_LONG_PRESS := 3

var _tracker: Node3D
var _system: Object
var _extension_loaded := false
# One-shot AR-feature availability diagnostic: logs which native AR ABIs resolved on this device,
# a short delay after boot (so the session has come up). See docs/plans/ar-features-plan.md.
var _ar_diag_frames := 0
# Phase C path B: phone IMU (via NRController state) drives the 3D pointer (_ar.phone_pointer).
var _controller_started := false
var _imu_poll_count := 0
var _phone_pointer: Node3D
var _cursor_mat: StandardMaterial3D
# No-glasses watchdog: the head-tracking session only comes up with the glasses connected, so if
# tracking hasn't started within this window we assume they're absent, show a message and quit —
# rather than sitting forever in the session-bootstrap retry loop. Detection is heuristic-free: it
# keys on "did tracking actually start", not on any display name/resolution guess. Disarmed for
# good the moment the session goes live — on the `display_started` signal (the reliable "glasses up"
# event) OR the first `is_tracking()` true. A mid-session unplug is a separate, unhandled case.
# 15 s: session bring-up is ~4-6 s normally but a cold first launch after (re)install is slower, and
# a false "no glasses" quit while they ARE connected is worse than a couple extra seconds of wait.
const NO_GLASSES_TIMEOUT_S := 15.0
const NO_GLASSES_QUIT_DELAY_S := 3.0
var _boot_elapsed := 0.0
var _tracking_seen := false
var _no_glasses := false

@onready var _status: Label = $UI/Panel/Margin/VBox/Status
@onready var _ar: Node3D = $ARScene
@onready var _cursor: MeshInstance3D = $ARScene.cursor
# The addon feature components (instanced in main.tscn as children of Main — a world-fixed node,
# which the world-locked features require).
@onready var _camera: Node3D = $XrealCamera
@onready var _planes: Node3D = $XrealPlanes
@onready var _anchors: Node3D = $XrealAnchors
@onready var _image_tracking: Node3D = $XrealImageTracking
@onready var _mesh: Node3D = $XrealMesh
@onready var _photo_capture: Node = $XrealPhotoCapture
@onready var _blend_capture: Node = $XrealBlendCapture
@onready var _stream: Node = $XrealStream

func _ready() -> void:
	XrealAndroidBridge.register()
	# The GDExtension is Android-only. On desktop the editor loads a dummy stub that DOES register
	# these classes (so the F1 help can document them), so class presence alone no longer means the
	# real extension is live — gate on the platform too, or the demo would drive no-op placeholders.
	_extension_loaded = OS.get_name() == "Android" \
		and ClassDB.class_exists(&"XrealSystem") and ClassDB.class_exists(&"XrealHeadTracker")
	if _extension_loaded:
		_system = ClassDB.instantiate(&"XrealSystem")
		# Boot-time settings from Project Settings (xreal/*), applied before the session starts.
		# Each "SDK Default" (-1) falls back to the matching debug.xreal.* property / native default.
		# Head-tracking mode (0 = 6DoF [recommended], 1 = 3DoF, 2 = 0DoF).
		var tracking_type := int(ProjectSettings.get_setting("xreal/tracking_type", -1))
		if tracking_type >= 0 and _system.has_method(&"set_tracking_type"):
			_system.set_tracking_type(tracking_type)
		# Stereo rendering mode (0 = Multipass [recommended], 2 = Multiview).
		var stereo_mode := int(ProjectSettings.get_setting("xreal/stereo_mode", -1))
		if stereo_mode >= 0 and _system.has_method(&"set_stereo_mode"):
			_system.set_stereo_mode(stereo_mode)
	else:
		push_error("[demo] godot_xreal GDExtension not loaded — XrealSystem/XrealHeadTracker missing. Build the Android .so (cargo ndk) and check the .gdextension paths.")
	_spawn_rig()
	# Async feature states (camera start is lazy, stream pairing is async) are reflected back onto
	# the phone-menu toggles through the components' active_changed signals.
	_camera.active_changed.connect(func(active: bool) -> void: _set_controller_toggle("camera", active))
	_stream.active_changed.connect(func(active: bool) -> void: _set_controller_toggle("stream", active))
	# Surface each feature component's `error` signal at the load site (here: the debug Status label
	# + logcat). A real app might disable a control or show a toast; the point is the failure is
	# detectable, not just a buried warning.
	for feature in [_camera, _planes, _anchors, _image_tracking, _mesh, _photo_capture, _blend_capture, _stream]:
		if feature and feature.has_signal(&"error"):
			feature.error.connect(_on_feature_error)
	# Label the "Cycle Image" button with the active image-tracking set as it changes.
	if _image_tracking and _image_tracking.has_signal(&"set_changed"):
		_image_tracking.set_changed.connect(_on_image_set_changed)
	_setup_touch_controller()
	# Reflect the boot camera state on the phone-menu toggle (on only when the XrealCamera
	# instance was saved with `enabled` ticked; the other toggles start off).
	_set_controller_toggle("camera", _camera.enabled)

func _spawn_rig() -> void:
	if _extension_loaded:
		var rig := (load(RIG_SCENE) as PackedScene).instantiate()
		add_child(rig)
		_tracker = rig  # the rig's root node IS the XrealHeadTracker
		# Recenter the view to the current head direction once tracking goes live.
		if _tracker.has_signal(&"display_started"):
			_tracker.display_started.connect(_on_display_started)
		# Glasses hardware inputs (One Pro: physical keys + wear sensor).
		if _tracker.has_signal(&"key_event"):
			_tracker.key_event.connect(_on_key_event)
			_tracker.wearing_changed.connect(_on_wearing_changed)
	else:
		# Fallback so the scene is still visible (and the panel explains why).
		var camera := Camera3D.new()
		camera.current = true
		add_child(camera)

## Set up the runtime side of the phone touch controller ($PhoneScreen — its layout and signal
## wiring are static in phone_screen.tscn / main.tscn; it only renders on the phone's root
## viewport, so the glasses keep showing the 3D scene): the head-locked 3D cursor and the
## host-preview camera.
func _setup_touch_controller() -> void:
	# The head-locked cursor makes phone touches visible in the glasses (proves the split):
	# reparent it under the tracker. Without a tracker (desktop fallback) there is nothing to
	# lock it to — drop it.
	if _tracker:
		_cursor.reparent(_tracker, false)
		_cursor_mat = _cursor.material_override as StandardMaterial3D
	else:
		_cursor.queue_free()
		_cursor = null

	# The phone shows the controller, not a 3D preview, so stop the rig's host-preview camera: the
	# root viewport no longer renders the world (one fewer full scene pass — the world was drawn 3×:
	# host preview + two eyes). The glasses are unaffected; they render from the extension's own
	# per-eye SubViewports.
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

## Phone-menu "カメラ" toggle → the XrealCamera component. set_enabled(true) only *requests* the
## camera (the capture starts lazily once tracking is live); an async start failure comes back
## through active_changed(false), which is wired to the toggle in _ready. An immediate refusal
## (device without an RGB camera) flips the toggle back here.
func _on_tc_camera(on: bool) -> void:
	print("[demo] camera toggle -> %s" % ("on" if on else "off"))
	if _camera.set_enabled(on) != on:
		_set_controller_toggle("camera", false)

## Phone-menu "平面検出" toggle → the XrealPlanes component (switches tracking to 6DoF while on).
func _on_tc_plane(on: bool) -> void:
	print("[demo] plane toggle -> %s" % ("on" if on else "off"))
	if _planes.set_enabled(on) != on:
		_set_controller_toggle("plane", false)

## Phone-menu "アンカー" toggle → the XrealAnchors component. Pinch or the "配置" button then drop
## an anchor at the hand fingertip.
func _on_tc_anchor(on: bool) -> void:
	print("[demo] anchor toggle -> %s" % ("on" if on else "off"))
	if _anchors.set_enabled(on) != on:
		_set_controller_toggle("anchor", false)

## Phone-menu "画像" toggle → the XrealImageTracking component (its manifest_path is set to the
## demo's reference.json in main.tscn).
func _on_tc_image(on: bool) -> void:
	print("[demo] image toggle -> %s" % ("on" if on else "off"))
	if _image_tracking.set_enabled(on) != on:
		_set_controller_toggle("image", false)

## Phone-menu "メッシュ" toggle → the XrealMesh component (Air 2 Ultra only).
func _on_tc_mesh(on: bool) -> void:
	print("[demo] mesh toggle -> %s" % ("on" if on else "off"))
	if _mesh.set_enabled(on) != on:
		_set_controller_toggle("mesh", false)

## Phone-menu "Stream" toggle → the XrealStream component. Pairing is async, so the component
## reports the resulting state back through its active_changed signal (wired in _ready) — which
## flips the phone toggle to match.
func _on_tc_stream(on: bool) -> void:
	print("[demo] stream toggle -> %s" % ("on" if on else "off"))
	_stream.set_enabled(on)

## Phone-menu "配置" button → place a spatial anchor at the currently-tracked hand fingertip.
func _on_tc_place() -> void:
	_anchors.place_at_fingertip()

## Phone-menu "画像切替" button → cycle the active image-tracking set.
func _on_tc_image_cycle() -> void:
	_image_tracking.cycle_set()

## Phone-menu "撮影" button → capture a photo from the RGB camera (One Series).
func _on_tc_capture() -> void:
	_photo_capture.capture_photo()

## Phone-menu "合成撮影" button → capture a blended camera+AR (mixed-reality) photo.
func _on_tc_blend() -> void:
	_blend_capture.capture_blended()

## Phone-menu "Exit" → quit. The touch controller shows a Yes/No dialog first and only emits this on Yes.
## A phone-menu exit for glasses without physical keys (the Air 2 Ultra has only an EC-dimming button).
func _on_tc_exit() -> void:
	get_tree().quit()

## No-glasses watchdog: if the head-tracking session hasn't started within NO_GLASSES_TIMEOUT_S, the
## glasses aren't connected — show a message on the phone and quit. Once tracking is seen it disarms
## permanently. Only runs with the real extension (never on desktop, where tracking is inert by design).
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

## Cover the screen with a "no glasses — quitting" message, then quit after a short delay so the
## message is readable. Uses its own top CanvasLayer so it sits over the debug UI / controller.
func _show_no_glasses_and_quit() -> void:
	print("[demo] no XREAL glasses detected within %.0fs — quitting" % NO_GLASSES_TIMEOUT_S)
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
	get_tree().quit()

## The active image-tracking set changed — show its name on the phone-menu "Cycle Image" button.
func _on_image_set_changed(name: String) -> void:
	var ps := get_node_or_null(^"PhoneScreen")
	if ps and ps.has_method(&"set_button_label"):
		ps.set_button_label("image_cycle", "Cycle: %s" % name)

## A feature component reported an error (via its `error` signal) — show it on the debug Status
## label and log it, so the failure is visible at the load site instead of buried in warnings.
func _on_feature_error(message: String) -> void:
	print("[demo] feature error: %s" % message)
	if _status:
		_status.text = message

## Push a toggle's on/off state onto the phone-menu controller (keeps the UI in sync when the app,
## not the user, changes it — e.g. a failed camera start or an unsupported plane mode).
func _set_controller_toggle(name: String, on: bool) -> void:
	var ps := get_node_or_null(^"PhoneScreen")
	if ps and ps.has_method(&"set_toggle"):
		ps.set_toggle(name, on)

## Grey out (make inert) the phone-menu controls whose capability the device lacks, once the session
## is up and the capabilities are known. Each control maps to a native capability query: camera / plane
## / anchor / image / mesh. Camera-dependent capture buttons (Photo / Blend Photo) follow the camera.
## Streaming is always available (it casts the AR view even without a camera), so it is never disabled.
func _apply_capabilities(cam: bool, plane: bool, anchor: bool, image: bool, mesh: bool) -> void:
	var ps := get_node_or_null(^"PhoneScreen")
	if ps == null or not ps.has_method(&"set_disabled"):
		return
	var avail := {
		"camera": cam, "capture": cam, "blend": cam,
		"plane": plane, "anchor": anchor, "place": anchor,
		"image": image, "image_cycle": image, "mesh": mesh,
	}
	for control_name in avail:
		ps.set_disabled(control_name, not bool(avail[control_name]))

## Reveal the phone-IMU 3D pointer (demo/phone_pointer.gd — defined in ar_scene.tscn,
## hidden until the NRController has started so no beam shows before it can be driven).
func _setup_phone_pointer() -> void:
	_phone_pointer = _ar.phone_pointer
	# Leave it hidden here. phone_pointer.gd reveals the beam on its first aimed frame (once recenter
	# has run and the origin sits at the hand offset), so the beam never shows at the default head
	# position and blocks the view before it can be aimed.

func _on_recenter_pressed() -> void:
	if _tracker and _tracker.has_method(&"recenter"):
		_tracker.recenter()

func _on_display_started() -> void:
	# Glasses display + tracking are live — disarm the no-glasses watchdog (reliable "glasses up"
	# event, in case is_tracking() lags past the timeout on a slow cold start).
	_tracking_seen = true
	# Make the current head direction "forward".
	if _tracker and _tracker.has_method(&"recenter"):
		_tracker.recenter()

func _on_key_event(key: int, action: int) -> void:
	# Long-press the MENU key to recenter (current head direction becomes "forward"),
	# replacing the on-screen button for a glasses-only workflow.
	if key == XREAL_KEY_MENU and action == XREAL_ACTION_LONG_PRESS:
		_on_recenter_pressed()
	# Long-press the MULTI key to quit the app (glasses-only exit). NB: only the One series has these
	# physical keys — the Air 2 Ultra has just an EC-dimming button, so it exits via the phone-menu Exit.
	elif key == XREAL_KEY_MULTI and action == XREAL_ACTION_LONG_PRESS:
		get_tree().quit()

func _on_wearing_changed(wearing: bool) -> void:
	if wearing:
		# Recenter the instant the glasses are actually worn (and the wearer is looking
		# forward), so "forward" isn't captured while they sit tilted on a desk.
		_on_recenter_pressed()

func _process(_delta: float) -> void:
	_check_no_glasses(_delta)
	# One-shot AR-feature availability diagnostic, ~2 s in (once the session has had time to come up),
	# so a glance at logcat shows which native AR ABIs this device exposes.
	if _ar_diag_frames >= 0 and _system:
		_ar_diag_frames += 1
		if _ar_diag_frames == 120:
			_ar_diag_frames = -1  # done
			var cam: bool = _system.is_camera_supported() if _system.has_method(&"is_camera_supported") else false
			var plane: bool = _system.is_plane_detection_available() if _system.has_method(&"is_plane_detection_available") else false
			var anchor: bool = _system.is_anchor_available() if _system.has_method(&"is_anchor_available") else false
			var image: bool = _system.is_image_tracking_available() if _system.has_method(&"is_image_tracking_available") else false
			var mesh: bool = _system.is_meshing_supported() if _system.has_method(&"is_meshing_supported") else false
			print("[demo] AR features: camera=%s plane=%s anchor=%s image=%s mesh=%s" % [cam, plane, anchor, image, mesh])
			_apply_capabilities(cam, plane, anchor, image, mesh)
	# Phase C path B: phone IMU (via NRController state) drives the 3D pointer. Godot's own IMU returns
	# all-zero on this host, so we read accel (gravity → pitch/roll) + gyro (yaw) from the controller.
	if _tracker and _tracker.has_method(&"is_tracking") and _tracker.is_tracking() and _system:
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
