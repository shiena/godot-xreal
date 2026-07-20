extends Node
## First-person-view mp4 recording as a drop-in feature component. The same render pipeline as
## xreal_stream.gd (head-POV AR SubViewport, opportunistically composited over the RGB camera),
## but the libmedia_codec HW encoder writes a local mp4 (a plain file path as the output instead
## of an rtp:// URL) rather than streaming to a receiver — no pairing, works offline.
##
## With the RGB camera ON (xreal_camera feature enabled) it records the camera+AR blend (what a
## bystander sees, via xreal_blend_2d.gdshader); with the camera OFF it records the AR view alone —
## switched per frame, so toggling the camera mid-recording just switches the recorded view. Like
## streaming, it feeds on our own SubViewport texture, so it needs no RGB camera and works on the
## camera-less Air 2 Ultra too.
##
## set_enabled(true) starts recording into the user data dir; set_enabled(false) finalizes the mp4
## and emits `finished(path)` — what to do with the file (e.g. publish it to the phone gallery, as
## the demo does) is the app's choice. Recorded without audio: the encoder's mic capture could be
## enabled via stream_start's audio flags, but this component keeps the permission story simple.
##
## The HW encoder is process-global and single-instance, shared with xreal_stream: starting one
## while the other runs is refused (see the `is_stream_active` guards here and there).

## Emitted when an operation fails or the feature is unavailable, so the load site can react
## (show UI, log, flip a toggle). Carries the same human-readable text also pushed as a warning.
signal error(message: String)

## Emitted whenever recording actually starts/stops, so a UI toggle can reflect the real state.
signal active_changed(active: bool)

## Emitted after a stop finalized the mp4, with its absolute path (in the user data dir).
signal finished(path: String)

@export var record_width := 1280
@export var record_height := 720
@export var record_bitrate := 8_000_000
@export var record_fps := 30

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar_vp: SubViewport             # head-POV AR, transparent bg (holograms only)
var _ar_cam: Camera3D
var _comp_vp: SubViewport           # camera+AR blend composite, built lazily when the camera is on
var _comp_mat: ShaderMaterial
var _active := false
var _path := ""                     # the mp4 being written
var _rgb_offset := Vector3.ZERO     # RGB camera offset from the head (Godot space), for blend parallax
var _rgb_geom_done := false         # RGB blend geometry (FOV + offset) applied once — static per device

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert

func is_active() -> bool:
	return _active

## Toggle recording. The resulting state comes back through active_changed (a start can be
## refused); a successful stop also emits finished(path) with the finalized mp4.
func set_enabled(on: bool) -> void:
	if not on:
		_stop()
		return
	if _active:
		return
	if _system == null or not _system.has_method(&"stream_start"):
		_fail("[xreal-record] native encoder unavailable")
		active_changed.emit(false)
		return
	# One process-global HW encoder: starting while the FPV stream runs would feed this second view
	# into the receiver's stream instead of opening a second encoder.
	if _system.has_method(&"is_stream_active") and _system.is_stream_active():
		_fail("[xreal-record] HW encoder busy (FPV streaming?) — stop it first")
		active_changed.emit(false)
		return
	_ensure_viewport()
	_path = OS.get_user_data_dir().path_join("record_%d.mp4" % Time.get_ticks_msec())
	# A local file path (no rtp:// / rtmp:// scheme) makes the encoder write an mp4. No audio.
	if not _system.stream_start(_path, record_width, record_height, record_bitrate, record_fps, false, false, false):
		_fail("[xreal-record] recorder start failed")
		active_changed.emit(false)
		return
	_active = true
	print("[xreal-record] recording -> %s" % _path)
	active_changed.emit(true)

func _stop() -> void:
	if not _active:
		return
	_active = false
	_system.stream_stop()  # finalizes the mp4
	print("[xreal-record] recording stopped -> %s" % _path)
	active_changed.emit(false)
	finished.emit(_path)

## Head-POV AR viewport (transparent bg): holograms only, so it composites over the camera for the
## blend and, with no camera, reads back as holograms on black.
func _ensure_viewport() -> void:
	if _ar_vp != null:
		return
	_ar_vp = SubViewport.new()
	_ar_vp.size = Vector2i(record_width, record_height)
	_ar_vp.transparent_bg = true
	_ar_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	_ar_vp.world_3d = get_tree().root.world_3d  # render the same 3D world the glasses show
	add_child(_ar_vp)
	_ar_cam = Camera3D.new()
	_ar_cam.current = true
	_ar_vp.add_child(_ar_cam)

## Composite viewport blending the AR viewport over the RGB camera (xreal_blend_2d.gdshader, same
## as blend capture / streaming), built lazily the first time the camera is on while recording.
func _ensure_comp() -> void:
	if _comp_vp != null:
		return
	_comp_vp = SubViewport.new()
	_comp_vp.size = Vector2i(record_width, record_height)
	_comp_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	add_child(_comp_vp)
	_comp_mat = ShaderMaterial.new()
	_comp_mat.shader = load("res://addons/godot_xreal/shaders/xreal_blend_2d.gdshader")
	var rect := ColorRect.new()
	rect.size = Vector2(record_width, record_height)
	rect.material = _comp_mat
	_comp_vp.add_child(rect)

## Drive the AR camera from the RGB camera's real geometry (intrinsics -> vertical FOV,
## pose-from-head -> a small forward offset) so the blended holograms match the camera image.
## Static per device, applied once.
func _apply_rgb_geometry() -> void:
	if _rgb_geom_done or _ar_cam == null:
		return
	_rgb_offset = XrealShared.apply_rgb_camera_geometry(_system, _ar_cam)
	_rgb_geom_done = true

## True when the RGB camera feed is live (camera on + a frame arrived) → record the camera+AR blend.
func _use_blend(feed: Object) -> bool:
	if feed == null or not is_instance_valid(feed) or not feed.has_method(&"get_y_texture"):
		return false
	return feed.get_y_texture() != null and feed.get_cbcr_texture() != null

func _process(_delta: float) -> void:
	if not _active or _ar_vp == null:
		return
	var feed := XrealShared.find_camera_feed(get_tree())
	var blending := _use_blend(feed)
	var tracker := XrealShared.find_head_tracker(get_tree())
	if tracker and _ar_cam:
		if blending:
			# Blend (camera ON): drive the AR camera from the RGB camera's real geometry (FOV +
			# forward offset) so the holograms line up with the camera image instead of a default guess.
			_apply_rgb_geometry()
			_ar_cam.global_transform = tracker.global_transform.translated_local(_rgb_offset)
		else:
			# Plain AR (no camera): head-locked with the default FOV.
			_ar_cam.fov = 75.0
			_ar_cam.global_transform = tracker.global_transform
	# Camera ON -> record the camera+AR blend (what a bystander sees); camera OFF -> the AR view alone.
	var src_vp := _ar_vp
	if blending:
		_ensure_comp()
		_comp_mat.set_shader_parameter(&"y_texture", feed.get_y_texture())
		_comp_mat.set_shader_parameter(&"cbcr_texture", feed.get_cbcr_texture())
		_comp_mat.set_shader_parameter(&"ar_texture", _ar_vp.get_texture())
		_comp_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
		src_vp = _comp_vp
	elif _comp_vp != null:
		_comp_vp.render_target_update_mode = SubViewport.UPDATE_DISABLED  # idle the blend when camera is off
	var viewport_rid := src_vp.get_viewport_rid()
	var ts := Time.get_ticks_usec() * 1000  # nanoseconds
	# ViewportTexture.get_rid() is a proxy RID. In the Compatibility renderer its copied tex_id can
	# remain 0, so resolve the viewport's real render-target color texture instead. Resolve the GL
	# name every frame to follow render-target reallocations, and push while the render EGL context
	# is current.
	RenderingServer.call_on_render_thread(func() -> void:
		var color_texture_rid := RenderingServer.viewport_get_texture(viewport_rid)
		var gl_tex_id := RenderingServer.texture_get_native_handle(color_texture_rid)
		if gl_tex_id != 0:
			_system.stream_push_frame(gl_tex_id, ts)
	)

func _exit_tree() -> void:
	# App teardown mid-recording: close the encoder so the mp4 is finalized (it stays in the user
	# data dir; `finished` only fires on a regular stop).
	if _active and _system:
		_system.stream_stop()

## Push a warning AND emit `error` so the load site can detect the failure (not just see the log).
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
