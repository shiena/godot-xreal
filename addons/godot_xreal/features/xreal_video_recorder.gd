extends Node
## First-person-view mp4 recording as a drop-in feature component. It uses the same render pipeline
## as xreal_stream.gd, a head-POV AR SubViewport opportunistically composited over the RGB camera,
## but the libmedia_codec HW encoder writes a local mp4 rather than streaming to a receiver: the
## output is a plain file path instead of an rtp:// URL, so there is no pairing and it works
## offline.
##
## With the RGB camera ON, meaning the xreal_camera feature is enabled, it records the camera+AR
## blend, what a bystander sees, through xreal_blend_2d.gdshader; with the camera OFF it records
## the AR view alone. The choice is made per frame, so toggling the camera mid-recording simply
## switches the recorded view. Like streaming, it feeds on our own SubViewport texture, so it needs
## no RGB camera and works on the camera-less Air 2 Ultra too.
##
## set_enabled(true) starts recording into the user data dir, and set_enabled(false) finalizes the
## mp4 and emits `finished(path)`. What to do with the file, such as publishing it to the phone
## gallery as the demo does, is the app's choice.
##
## The HW encoder is process-global and single-instance, shared with xreal_stream, so starting one
## while the other runs is refused; see the `is_stream_active` guards here and there.

## Emitted when an operation fails or the feature is unavailable, so the load site can react by
## showing UI, logging, or flipping a toggle. It carries the same human-readable text that is also
## pushed as a warning.
signal error(message: String)

## Emitted whenever recording actually starts or stops, so a UI toggle can reflect the real state.
signal active_changed(active: bool)

## Emitted after a stop finalized the mp4, carrying its absolute path in the user data dir.
signal finished(path: String)

## Capture size and bitrate preset (SDK VideoCapture's Resolution Level). `CUSTOM` uses the
## explicit record_width, record_height and record_bitrate below; every other value overrides them
## at start.
@export var resolution_level: XrealShared.ResolutionLevel = XrealShared.ResolutionLevel.HIGH
@export var record_width := 1280
@export var record_height := 720
@export var record_bitrate := 8_000_000
@export var record_fps := 30

## What ends up in the file (SDK VideoCapture's Blend Mode). BLEND draws the holograms over the RGB
## camera image, RGB_ONLY records the camera alone, VIRTUAL_ONLY records the holograms alone.
## RGB_ONLY needs the camera feature switched on, and so does BLEND unless a green key replaces the
## camera.
enum BlendMode { BLEND, RGB_ONLY, VIRTUAL_ONLY }
@export var blend_mode: BlendMode = BlendMode.BLEND
## Replace the real world behind the holograms with a chroma key, for compositing in post. It is
## ignored for RGB_ONLY, which has no holograms to key against.
@export var green_background := false
@export var green_key: Color = Color(0.0, 1.0, 0.0)
## Which 3D render layers the capture camera sees (SDK VideoCapture's Culling Mask). It defaults to
## all 20 of Godot's layers; clear bits to keep objects out of the recording without hiding them in
## the glasses.
@export_flags_3d_render var record_cull_mask := 0xFFFFF
## Which audio ends up in the file (SDK VideoCapture's Audio State). The SDK's encoder captures and
## mixes both sources natively: MIC needs RECORD_AUDIO granted, and APP needs an Android
## MediaProjection, which means a consent dialog (see XrealShared.request_app_audio_consent).
## Either one is dropped silently when its prerequisite is missing.
@export var audio_state: XrealShared.AudioState = XrealShared.AudioState.NONE

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar_vp: SubViewport             # head-POV AR, transparent bg (holograms only)
var _ar_cam: Camera3D
var _comp_vp: SubViewport           # camera+AR blend composite, built lazily when the camera is on
var _comp_mat: ShaderMaterial
var _active := false
var _path := ""                     # the mp4 being written
var _rgb_offset := Vector3.ZERO     # RGB camera offset from the head (Godot space), for blend parallax
var _rgb_geom_done := false         # RGB blend geometry (FOV + offset) applied once, static per device
var _epoch := 0                     # bumped on every start/stop; a frame captured for a prior session is dropped

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert

func is_active() -> bool:
	return _active

## Toggle recording. The resulting state comes back through active_changed, since a start can be
## refused, and a successful stop also emits finished(path) with the finalized mp4.
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
	# There is one process-global HW encoder, so starting while the FPV stream runs would feed this
	# second view into the receiver's stream instead of opening a second encoder.
	if _system.has_method(&"is_stream_active") and _system.is_stream_active():
		_fail("[xreal-record] HW encoder busy (FPV streaming?), so stop it first")
		active_changed.emit(false)
		return
	# This runs before _ensure_viewport(), because both SubViewports size themselves off record_width
	# and record_height, so the preset has to land first or the capture would be sized differently
	# from the encoder.
	_apply_resolution_level()
	_ensure_viewport()
	# Local date-time in the name (record_YYYYMMDD_HHMMSS.mp4) so the file reads naturally in the
	# gallery: "2026-07-20T14:25:30" becomes "20260720_142530".
	var stamp := Time.get_datetime_string_from_system().replace("-", "").replace(":", "").replace("T", "_")
	_path = OS.get_user_data_dir().path_join("record_%s.mp4" % stamp)
	# A local file path, carrying no rtp:// or rtmp:// scheme, makes the encoder write an mp4.
	# App audio is captured natively too, but only through an Android MediaProjection. Consent is a
	# system dialog and therefore asynchronous, so the first capture that wants app audio triggers it
	# and records microphone-only, and the next one has both. Passing the flag without a projection
	# would only enable an empty AAC path (docs/archive/codex-audio-mix-analysis.md), so it is dropped
	# here.
	var want_app := XrealShared.audio_wants_app(audio_state)
	if want_app and not XrealShared.is_app_audio_ready():
		XrealShared.request_app_audio_consent()
		push_warning("[xreal-record] app audio needs screen-capture consent; "
			+ "this recording carries the microphone only")
		want_app = false
	# Request the mic lazily, as the camera and the FPV stream do: when it is wanted but not yet
	# granted, fire the dialog now and record video-only this time, and the next recording has audio.
	if XrealShared.audio_wants_mic(audio_state) and OS.has_feature("android") and not XrealShared.is_mic_granted():
		OS.request_permission("android.permission.RECORD_AUDIO")
		push_warning("[xreal-record] mic not granted yet -- recording video-only; grant RECORD_AUDIO, then record again for audio")
	var want_mic := XrealShared.audio_wants_mic(audio_state) and XrealShared.is_mic_granted()
	if not _system.stream_start(_path, record_width, record_height, record_bitrate, record_fps, want_mic, want_app, false):
		_fail("[xreal-record] recorder start failed")
		active_changed.emit(false)
		return
	_epoch += 1
	_active = true
	print("[xreal-record] recording -> %s (app_audio=%s, mic=%s)" % [_path, want_app, want_mic])
	active_changed.emit(true)

func _stop() -> void:
	if not _active:
		return
	_active = false
	_epoch += 1
	_system.stream_stop()  # finalizes the mp4
	print("[xreal-record] recording stopped -> %s" % _path)
	active_changed.emit(false)
	finished.emit(_path)

## Fold the Resolution Level preset into record_width, record_height and record_bitrate, so
## everything downstream keeps reading one set of values: both SubViewports, the ColorRect and the
## encoder config. CUSTOM leaves the exported values alone.
func _apply_resolution_level() -> void:
	if resolution_level == XrealShared.ResolutionLevel.CUSTOM:
		return
	var preset := XrealShared.resolution_preset(resolution_level)
	record_width = preset.x
	record_height = preset.y
	record_bitrate = preset.z

## Head-POV AR viewport on a transparent background: holograms only, so it composites over the
## camera for the blend and, with no camera, reads back as holograms on black.
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
	_ar_cam.cull_mask = record_cull_mask
	_ar_vp.add_child(_ar_cam)

## Composite viewport blending the AR viewport over the RGB camera, using xreal_blend_2d.gdshader
## just as blend capture and streaming do, built lazily the first time the camera is on while
## recording.
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

## Drive the AR camera from the RGB camera's real geometry: the intrinsics give the vertical FOV
## and the pose relative to the head gives a small forward offset, so the blended holograms match
## the camera image. It is static per device, so it is applied once.
func _apply_rgb_geometry() -> void:
	if _rgb_geom_done or _ar_cam == null:
		return
	_rgb_offset = XrealShared.apply_rgb_camera_geometry(_system, _ar_cam)
	_rgb_geom_done = true

## True when the RGB camera feed is live, meaning the camera is on and a frame arrived, in which
## case we record the camera+AR blend.
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
			# Blend, with the camera ON: drive the AR camera from the RGB camera's real geometry, its FOV
			# and forward offset, so the holograms line up with the camera image instead of a default guess.
			_apply_rgb_geometry()
			_ar_cam.global_transform = tracker.global_transform.translated_local(_rgb_offset)
		else:
			# Plain AR with no camera: head-locked at the default FOV.
			_ar_cam.fov = 75.0
			_ar_cam.global_transform = tracker.global_transform
	# The blend viewport is what applies blend_mode and the green key, so it is needed whenever either
	# asks for something other than "holograms on transparent", which is exactly what _ar_vp already
	# is. That keeps the common VIRTUAL_ONLY and camera-off case on the cheap single-viewport path.
	var needs_comp := (
		(blending and blend_mode == BlendMode.BLEND)
		or blend_mode == BlendMode.RGB_ONLY
		or (green_background and blend_mode != BlendMode.RGB_ONLY)
	)
	var src_vp := _ar_vp
	if needs_comp:
		_ensure_comp()
		if blending:
			_comp_mat.set_shader_parameter(&"y_texture", feed.get_y_texture())
			_comp_mat.set_shader_parameter(&"cbcr_texture", feed.get_cbcr_texture())
		_comp_mat.set_shader_parameter(&"ar_texture", _ar_vp.get_texture())
		_comp_mat.set_shader_parameter(&"blend_mode", int(blend_mode))
		_comp_mat.set_shader_parameter(&"green_background", green_background)
		_comp_mat.set_shader_parameter(&"green_key", Vector3(green_key.r, green_key.g, green_key.b))
		_comp_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
		src_vp = _comp_vp
	elif _comp_vp != null:
		_comp_vp.render_target_update_mode = SubViewport.UPDATE_DISABLED  # idle the blend when unused
	var viewport_rid := src_vp.get_viewport_rid()
	var ts := Time.get_ticks_usec() * 1000  # nanoseconds
	var gen := _epoch  # a stop->restart bumps _epoch, so a frame captured for the old session is dropped
	# ViewportTexture.get_rid() is a proxy RID, and in the Compatibility renderer its copied tex_id
	# can stay 0, so resolve the viewport's real render-target color texture instead. Resolve the GL
	# name every frame to follow render-target reallocations, and push while the render EGL context
	# is current.
	RenderingServer.call_on_render_thread(func() -> void:
		var color_texture_rid := RenderingServer.viewport_get_texture(viewport_rid)
		var gl_tex_id := RenderingServer.texture_get_native_handle(color_texture_rid)
		if gen == _epoch and gl_tex_id != 0:
			_system.stream_push_frame(gl_tex_id, ts)
	)

func _exit_tree() -> void:
	# App teardown mid-recording: close the encoder so the mp4 is finalized. It stays in the user data
	# dir, and `finished` fires only on a regular stop.
	if _active and _system:
		_system.stream_stop()

## Push a warning AND emit `error`, so the load site can detect the failure instead of only seeing
## it in the log.
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
