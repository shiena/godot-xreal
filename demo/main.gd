extends Node3D

## Minimal 3DoF demo for the Godot XREAL addon.
##
## The static content lives in two sub-scenes instanced by demo/main.tscn:
##   - $ARScene (demo/ar_scene.tscn + ar_scene.gd) — the 3D world: WorldEnvironment (black
##     background — on the XREAL optical see-through display black reads as transparent), sun,
##     the ring of colored boxes (with colliders for the phone-pointer raycast), plus the
##     head-locked camera preview panel, controller cursor and phone-IMU pointer, exposed as
##     `cam_panel` / `cursor` / `phone_pointer`.
##   - $PhoneScreen (demo/phone_screen.tscn + phone_screen.gd) — the phone-only touch
##     controller layer; its signals are re-emitted at the scene root and wired to the
##     _on_tc_* handlers here via main.tscn connections.
## The debug UI ($UI) also lives in main.tscn, its Recenter button wired the same way.
##
## This script does only what has to be dynamic: detect the GDExtension, instance the addon
## camera rig (addons/godot_xreal/xreal_rig.tscn — an XrealHeadTracker with a Camera3D child),
## reparent the head-locked nodes under the tracker, and pump the camera feed / controller IMU
## per frame. On XREAL hardware the camera looks around with the wearer's head; on desktop the
## rig stays at identity so the scene is still runnable.

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

# XrealSystem plane-detection mode flags and tracking types, mirrored locally (same reason).
const XREAL_PLANE_NONE := 0
const XREAL_PLANE_BOTH := 3   # horizontal | vertical
const XREAL_TRACKING_6DOF := 0
const XREAL_TRACKING_3DOF := 1

var _tracker: Node3D
var _system: Object
var _extension_loaded := false
# XREAL RGB camera as a Godot CameraFeed (see docs/plans/camera-feed-plan.md), shown on the
# head-locked cam_panel quad via its YCbCr→RGB ShaderMaterial (both defined in ar_scene.tscn).
var _cam_feed: Object
var _camera_enabled := false
# Set once the RGB capture fails to start (wedged glasses camera), so _process stops re-attempting
# setup — a hard failure isn't retried; re-plug the glasses and relaunch to recover.
var _cam_failed := false
# Plane detection on/off, driven by the phone-menu "平面検出" toggle. Needs a live 6DoF session
# (Air 2 Ultra); independent of the camera toggle. See docs/plans/ar-features-plan.md.
var _plane_enabled := false
# Running count of tracked planes (added − removed), logged for on-device verification.
var _plane_total := 0
# One-shot AR-feature availability diagnostic: logs which native AR ABIs resolved on this device,
# a short delay after boot (so the session has come up). See docs/plans/ar-features-plan.md.
var _ar_diag_frames := 0
# Spatial-anchor manager (demo/anchor_manager.gd), driven by the phone-menu "アンカー" toggle + "配置".
var _anchor_manager: Node3D
# Image-tracking manager (demo/image_manager.gd), driven by the phone-menu "画像" toggle.
var _image_manager: Node3D
# Depth-mesh manager (demo/mesh_manager.gd), driven by the phone-menu "メッシュ" toggle.
var _mesh_manager: Node3D
# Detected-plane visualization: a thin, semi-transparent box overlaid on each plane's bounds,
# keyed by plane id. World-locked (children of Main, like the hand joints) so they sit on the real
# surface as the head moves. On the see-through display the translucent fill reads as a tint.
const PLANE_BOX_THICKNESS := 0.01  # metres; a slab, not a cuboid
var _plane_boxes := {}          # plane id(String) -> MeshInstance3D
var _plane_container: Node3D
var _plane_mat: StandardMaterial3D
# Phase C path B: phone IMU (via NRController state) drives the 3D pointer (_ar.phone_pointer).
var _phone_pointer_enabled := true
var _controller_started := false
var _imu_poll_count := 0
var _phone_pointer: Node3D
var _cursor_mat: StandardMaterial3D

@onready var _status: Label = $UI/Panel/Margin/VBox/Status
@onready var _ar: Node3D = $ARScene
@onready var _cam_panel: MeshInstance3D = $ARScene.cam_panel
@onready var _cursor: MeshInstance3D = $ARScene.cursor

func _ready() -> void:
	_try_register_android_bridge()
	_extension_loaded = ClassDB.class_exists(&"XrealSystem") and ClassDB.class_exists(&"XrealHeadTracker")
	if _extension_loaded:
		_system = ClassDB.instantiate(&"XrealSystem")
		# (No stereo-mode selector: the port always uses Multipass. Multiview is shelved
		# -- docs/archive/codex-righteye-analysis.md -- reachable only via `setprop debug.xreal.force_multiview 1`.)
		# Head-tracking mode from the project setting `xreal/tracking_type`
		# (0 = 6DoF [recommended], 1 = 3DoF, 2 = 0DoF). Same rules as above -- read once at
		# bootstrap; absent (-1) falls back to the `debug.xreal.tracking_type` property / default.
		var tracking_type := int(ProjectSettings.get_setting("xreal/tracking_type", -1))
		if tracking_type >= 0 and _system.has_method(&"set_tracking_type"):
			_system.set_tracking_type(tracking_type)
		# Camera and plane detection both default OFF — enable them explicitly from the phone-menu
		# toggles at runtime (_on_tc_camera / _on_tc_plane). The RGB camera shares the tracking camera
		# with 6DoF SLAM, so enabling it in 6DoF breaks head tracking (NRSDK "GetPoseWithStates failed"
		# -> identity pose); when the camera is on we force 3DoF (the DISP pose still carries full
		# pitch/yaw/roll). Override the default with the `xreal/enable_camera` project setting.
		_camera_enabled = bool(ProjectSettings.get_setting("xreal/enable_camera", false))
		if _camera_enabled and _system.has_method(&"set_tracking_type"):
			_system.set_tracking_type(1)  # 3DoF, so the RGB camera and head tracking can coexist
	else:
		push_error("[demo] godot_xreal GDExtension not loaded — XrealSystem/XrealHeadTracker missing. Build the Android .so (cargo ndk) and check the .gdextension paths.")
	_spawn_rig()
	if not _camera_enabled:
		# No camera at boot — keep the (hidden) preview panel so the phone-menu camera toggle
		# can still bring it up at runtime (rather than freeing it here).
		_cam_panel.visible = false
	if bool(ProjectSettings.get_setting("xreal/enable_touch_controller", true)):
		_setup_touch_controller()
		# Reflect the boot camera state on the phone-menu toggle (plane starts off).
		_set_controller_toggle("camera", _camera_enabled)
	else:
		$PhoneScreen.queue_free()
		_cursor.queue_free()
		_cursor = null
	_phone_pointer_enabled = bool(ProjectSettings.get_setting("xreal/enable_phone_pointer", true))
	if not _phone_pointer_enabled:
		_ar.phone_pointer.queue_free()

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
		# Multi-resume: auto-enter Picture-in-Picture on background so the glasses keep rendering
		# (the app stays paused-but-visible as a phone tile). See docs/plans/background-render-plan.md.
		bridge.enableAutoEnterPiP(activity)

	activity.runOnUiThread(runtime.createRunnableFromGodotCallable(register_bridge))

func _spawn_rig() -> void:
	if _extension_loaded:
		var rig := (load(RIG_SCENE) as PackedScene).instantiate()
		add_child(rig)
		_tracker = rig  # the rig's root node IS the XrealHeadTracker
		# Hand-joint visualizer (Air 2 Ultra): small spheres at the tracked hand joints. The joint poses
		# are in world/tracking space (fixed as the head moves), so parent under Main (a fixed node) — NOT
		# the head rig. Under the rotating rig the head rotation cancels against the eye cameras and the
		# hand would be head-locked (stuck to the screen). Under a fixed node the rotating eye cameras see
		# the fixed hand, so it stays world-locked on the real hand.
		var hand_vis := Node3D.new()
		hand_vis.name = "HandVisualizer"
		hand_vis.set_script(load("res://demo/hand_visualizer.gd"))
		add_child(hand_vis)
		# Spatial-anchor manager (also world-locked under Main). Drives placement (pinch / 配置 button),
		# poll_anchors visualization, and save/restore — enabled from the phone-menu アンカー toggle.
		_anchor_manager = Node3D.new()
		_anchor_manager.name = "AnchorManager"
		_anchor_manager.set_script(load("res://demo/anchor_manager.gd"))
		add_child(_anchor_manager)
		_anchor_manager.setup(_system)
		# Image-tracking manager (also world-locked under Main). Loads the reference-image DB + overlays
		# a quad on each tracked image — enabled from the phone-menu 画像 toggle.
		_image_manager = Node3D.new()
		_image_manager.name = "ImageManager"
		_image_manager.set_script(load("res://demo/image_manager.gd"))
		add_child(_image_manager)
		_image_manager.setup(_system)
		# Depth-mesh manager (also world-locked under Main). Enables meshing + overlays an ArrayMesh per
		# scanned block — enabled from the phone-menu メッシュ toggle.
		_mesh_manager = Node3D.new()
		_mesh_manager.name = "MeshManager"
		_mesh_manager.set_script(load("res://demo/mesh_manager.gd"))
		add_child(_mesh_manager)
		_mesh_manager.setup(_system)
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

## Expose the XREAL glasses RGB camera as a Godot CameraFeed (docs/plans/camera-feed-plan.md), register it
## with the CameraServer, and show it on the head-locked cam_panel quad (defined in ar_scene.tscn).
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

	# The panel's shader samples the feed's Y (R8) + CbCr (RG8) ImageTextures DIRECTLY
	# (get_y_texture / get_cbcr_texture). A CameraTexture on a script-fed feed only shows Godot's
	# placeholder, so we bypass it — matching the XREAL SDK's YUVTransRGB sample. The textures are
	# wired in _process once the first frame has created them; until then the panel stays hidden so
	# the not-yet-fed shader never shows as a pink (unset-sampler) placeholder.
	# Reparent the panel under the tracker (the head node), so it follows the gaze; rendered by the
	# eye SubViewports (shared world). Its corner position/size are set in ar_scene.tscn.
	if _tracker and _cam_panel.get_parent() != _tracker:
		_cam_panel.reparent(_tracker, false)

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

## Phone-menu "カメラ" toggle → start/stop the XREAL RGB camera feed at runtime. The camera shares
## the tracking camera with 6DoF SLAM, so turning it on forces 3DoF (the camera and head tracking
## can then coexist — same rule as the boot path). Independent of the plane toggle.
func _on_tc_camera(on: bool) -> void:
	print("[demo] camera toggle -> %s" % ("on" if on else "off"))
	if on:
		# Gate on the device actually having an RGB camera (IsHMDFeatureSupported). The Air 2 Ultra
		# has none — opening it there froze the app — so refuse and flip the toggle back off.
		if _system and _system.has_method(&"is_camera_supported") and not _system.is_camera_supported():
			push_warning("[demo] this device has no RGB camera (e.g. Air 2 Ultra) — camera unavailable")
			_camera_enabled = false
			_set_controller_toggle("camera", false)
			return
		_cam_failed = false
		_camera_enabled = true
		if _system and _system.has_method(&"switch_tracking_type"):
			_system.switch_tracking_type(XREAL_TRACKING_3DOF)
		# The lazy setup in _process creates the feed on the next tracked frame.
	else:
		_camera_enabled = false
		if _cam_feed:
			if _cam_feed.is_active():
				_cam_feed.set_active(false)
			CameraServer.remove_feed(_cam_feed)
			_cam_feed = null
		if _cam_panel:
			_cam_panel.visible = false

## Phone-menu "平面検出" toggle → enable/disable plane detection at runtime. Needs a live 6DoF
## session, so turning it on switches tracking to 6DoF. Gated on the plane C ABI being available;
## planes then stream in via poll_planes (which may take a moment / need a live 6DoF session).
func _on_tc_plane(on: bool) -> void:
	print("[demo] plane toggle -> %s" % ("on" if on else "off"))
	if not _system:
		_set_controller_toggle("plane", false)
		return
	if on:
		# Gate on the ABI being resolved (not on set_plane_detection_mode's return — the SDK discards
		# that value too, and it reads false even when the mode takes; XREALPlaneSubsystem.cs). Enable
		# optimistically and let poll_planes surface whatever the SDK detects.
		if _system.has_method(&"is_plane_detection_available") and not _system.is_plane_detection_available():
			push_warning("[demo] plane detection ABI unavailable on this device — toggle disabled")
			_set_controller_toggle("plane", false)
			return
		if _system.has_method(&"switch_tracking_type"):
			_system.switch_tracking_type(XREAL_TRACKING_6DOF)
		if _system.has_method(&"set_plane_detection_mode"):
			_system.set_plane_detection_mode(XREAL_PLANE_BOTH)
		_plane_enabled = true
	else:
		_plane_enabled = false
		if _system.has_method(&"set_plane_detection_mode"):
			_system.set_plane_detection_mode(XREAL_PLANE_NONE)
		_clear_plane_boxes()

## Phone-menu "アンカー" toggle → enable/disable spatial-anchor mode (demo/anchor_manager.gd).
## Pinch or the "配置" button then drop an anchor at the hand fingertip. Unavailable without the
## anchor ABI: the toggle flips itself back off.
func _on_tc_anchor(on: bool) -> void:
	print("[demo] anchor toggle -> %s" % ("on" if on else "off"))
	if _anchor_manager == null:
		_set_controller_toggle("anchor", false)
		return
	var enabled: bool = _anchor_manager.set_enabled(on)
	if on and not enabled:
		push_warning("[demo] spatial anchors unavailable on this device — toggle disabled")
		_set_controller_toggle("anchor", false)

## Phone-menu "画像" toggle → enable/disable image tracking (demo/image_manager.gd). Loads the
## reference-image DB blob and overlays a quad on each tracked image. Unavailable without the image
## ABI / DB blob: the toggle flips itself back off.
func _on_tc_image(on: bool) -> void:
	print("[demo] image toggle -> %s" % ("on" if on else "off"))
	if _image_manager == null:
		_set_controller_toggle("image", false)
		return
	var enabled: bool = _image_manager.set_enabled(on)
	if on and not enabled:
		push_warning("[demo] image tracking unavailable (device / DB blob) — toggle disabled")
		_set_controller_toggle("image", false)

## Phone-menu "メッシュ" toggle → enable/disable depth meshing (demo/mesh_manager.gd). Overlays an
## ArrayMesh of the scanned environment. Unavailable off the Air 2 Ultra: the toggle flips back off.
func _on_tc_mesh(on: bool) -> void:
	print("[demo] mesh toggle -> %s" % ("on" if on else "off"))
	if _mesh_manager == null:
		_set_controller_toggle("mesh", false)
		return
	var enabled: bool = _mesh_manager.set_enabled(on)
	if on and not enabled:
		push_warning("[demo] depth meshing unavailable (non-Air-2-Ultra / perception down) — toggle disabled")
		_set_controller_toggle("mesh", false)

## Phone-menu "配置" button → place a spatial anchor at the currently-tracked hand fingertip.
func _on_tc_place() -> void:
	if _anchor_manager:
		_anchor_manager.place_at_fingertip()

## Create/update the translucent box overlaying one plane's bounds. The plane's `size` is its full
## width/height in the plane-local X/Z; `center` offsets the bounds from the pose in that same local
## frame. Coordinate convention (local X/Z, Y-up normal) is AR-Foundation-standard but unverified on
## device — flip here if the boxes don't sit on the real surface.
func _update_plane_box(plane: Dictionary) -> void:
	var id: String = plane.get("id", "")
	if id.is_empty():
		return
	_ensure_plane_visual()
	var mi: MeshInstance3D = _plane_boxes.get(id)
	if mi == null:
		mi = MeshInstance3D.new()
		mi.mesh = BoxMesh.new()
		mi.material_override = _plane_mat
		_plane_container.add_child(mi)
		_plane_boxes[id] = mi
	var sz: Vector2 = plane.get("size", Vector2.ZERO)
	(mi.mesh as BoxMesh).size = Vector3(sz.x, PLANE_BOX_THICKNESS, sz.y)
	var center: Vector2 = plane.get("center", Vector2.ZERO)
	var t: Transform3D = plane.get("transform", Transform3D.IDENTITY)
	mi.transform = t.translated_local(Vector3(center.x, 0.0, center.y))

func _remove_plane_box(id: String) -> void:
	var mi: MeshInstance3D = _plane_boxes.get(id)
	if mi:
		mi.queue_free()
		_plane_boxes.erase(id)

func _clear_plane_boxes() -> void:
	for id in _plane_boxes:
		(_plane_boxes[id] as MeshInstance3D).queue_free()
	_plane_boxes.clear()
	_plane_total = 0

## Lazily build the world-locked container (child of Main, so the boxes stay on the real surface as
## the head moves — same reason the hand joints parent under Main) and the shared translucent material.
func _ensure_plane_visual() -> void:
	if _plane_container == null:
		_plane_container = Node3D.new()
		_plane_container.name = "PlaneVisualizer"
		add_child(_plane_container)
	if _plane_mat == null:
		_plane_mat = StandardMaterial3D.new()
		_plane_mat.albedo_color = Color(0.25, 0.7, 1.0, 0.35)
		_plane_mat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
		_plane_mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
		_plane_mat.cull_mode = BaseMaterial3D.CULL_DISABLED  # visible from both faces

## Push a toggle's on/off state onto the phone-menu controller (keeps the UI in sync when the app,
## not the user, changes it — e.g. a failed camera start or an unsupported plane mode).
func _set_controller_toggle(name: String, on: bool) -> void:
	var ps := get_node_or_null(^"PhoneScreen")
	if ps and ps.has_method(&"set_toggle"):
		ps.set_toggle(name, on)

## Reveal the phone-IMU 3D pointer (demo/phone_pointer.gd — defined in ar_scene.tscn,
## hidden until the NRController has started so no beam shows before it can be driven).
func _setup_phone_pointer() -> void:
	_phone_pointer = _ar.phone_pointer
	_phone_pointer.visible = true

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
	# One-shot AR-feature availability diagnostic, ~2 s in (once the session has had time to come up),
	# so a glance at logcat shows which native AR ABIs this device exposes.
	if _ar_diag_frames >= 0 and _system:
		_ar_diag_frames += 1
		if _ar_diag_frames == 120:
			_ar_diag_frames = -1  # done
			var cam: bool = _system.is_camera_supported() if _system.has_method(&"is_camera_supported") else false
			var plane: bool = _system.is_plane_detection_available() if _system.has_method(&"is_plane_detection_available") else false
			var anchor: bool = _system.is_anchor_available() if _system.has_method(&"is_anchor_available") else false
			print("[demo] AR features: camera=%s plane=%s anchor=%s" % [cam, plane, anchor])
	# Lazily set up the camera ONLY once head tracking is live — starting the capture before the
	# glasses/tracking are up races (and in 6DoF would fight the SLAM camera). See _setup_camera_feed.
	if _camera_enabled and not _cam_failed and _cam_feed == null and _tracker and _tracker.has_method(&"is_tracking") \
			and _tracker.is_tracking():
		_setup_camera_feed()
		if _cam_failed:
			# Start failed (wedged glasses camera, or unsupported as on Air 2 Ultra) — reflect the
			# phone-menu camera toggle back to off so its state matches reality.
			_camera_enabled = false
			_set_controller_toggle("camera", false)
	# Drive plane detection while its toggle is on: poll the change queue every frame (the SDK only
	# produces new changes when polled), overlay a translucent box on each plane's bounds, and log
	# the running plane count for on-device verification.
	if _plane_enabled and _system and _system.has_method(&"poll_planes"):
		var changes: Dictionary = _system.poll_planes()
		var added: Array = changes.get("added", [])
		var updated: Array = changes.get("updated", [])
		var removed: Array = changes.get("removed", [])
		for plane in added:
			_update_plane_box(plane)
		for plane in updated:
			_update_plane_box(plane)
		for id in removed:
			_remove_plane_box(id)
		if added.size() > 0 or removed.size() > 0:
			_plane_total += added.size() - removed.size()
			print("[demo] planes: +%d ~%d -%d (total %d)" % [added.size(), updated.size(), removed.size(), _plane_total])
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
