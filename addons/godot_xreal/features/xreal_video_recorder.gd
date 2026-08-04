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
## Record both eyes side by side rather than one view, the equivalent of the SDK's
## `CaptureSide.Both`. The file keeps its width and halves its height, each eye filling one half,
## so it is squeezed horizontally the way side-by-side 3D formats are.
##
## Only the holograms separate: the glasses carry one RGB camera, so a blended recording shows the
## same real-world image in both halves. Stereo always goes through the composite viewport, even
## for VIRTUAL_ONLY, because that is what puts the two eyes side by side.
##
## Read when recording starts, so changing it mid-recording does nothing: the encoder is configured
## with the frame size at that moment and cannot be resized.
@export var stereo := false

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar_vps: Array[SubViewport] = []  # head-POV AR per recorded eye, transparent bg (holograms only)
var _ar_cams: Array[Camera3D] = []
var _comp_vp: SubViewport           # camera+AR blend composite, built lazily when the camera is on
var _comp_mats: Array[ShaderMaterial] = []
var _built_stereo := false          # the layout the current viewports were built for
var _active := false
var _path := ""                     # the mp4 being written
var _rgb_offset := Vector3.ZERO     # RGB camera offset from the head (Godot space), for blend parallax
var _eye_offsets := [Vector3.ZERO, Vector3.ZERO]  # per-eye display offsets, for the stereo parallax
var _rgb_geom_done := false         # RGB blend geometry (FOV + offset) applied once, static per device
var _epoch := 0                     # bumped on every start/stop; a frame captured for a prior session is dropped
var _vk_backend := false            # encoder backend 2 = Vulkan bridge (publish RIDs, not GL names)

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert
# (the encoder backend is sampled at start time, not here: the Vulkan bridge initializes after
# component _ready, so an early query would read 0 and lock the component onto the GL push path)

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
	# Refuse before the RECORD_AUDIO and app-audio consent dialogs when the HW encoder cannot run
	# on this renderer (Vulkan, until the vulkan-path stage-4 bridge). Rust keeps the hard gate
	# inside stream_start; checking here skips the pointless side effects and reports the specific
	# reason instead of a generic start failure.
	if (
		_system.has_method(&"is_render_texture_encoder_supported")
		and not _system.is_render_texture_encoder_supported()
	):
		_fail("[xreal-record] HW encoder needs the GL renderer; recording is unavailable under Vulkan")
		active_changed.emit(false)
		return
	# There is one process-global HW encoder, so starting while the FPV stream runs would feed this
	# second view into the receiver's stream instead of opening a second encoder.
	if _system.has_method(&"is_stream_active") and _system.is_stream_active():
		_fail("[xreal-record] HW encoder busy (FPV streaming?), so stop it first")
		active_changed.emit(false)
		return
	# Sampled here, at start time, because the Vulkan bridge initializes after _ready.
	_vk_backend = (
		_system.has_method(&"get_render_texture_encoder_backend")
		and _system.get_render_texture_encoder_backend() == 2
	)
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
	# would only enable an empty AAC path (docs/develop/archive/codex-audio-mix-analysis.md), so it is dropped
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
	# The encoder is configured with the composed frame size, which stereo halves in height, not with
	# the per-eye size.
	var enc := _out_size()
	if not _system.stream_start(_path, enc.x, enc.y, record_bitrate, record_fps, want_mic, want_app, false):
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
	_system.stream_stop()  # GL: finalizes the mp4 now; Vulkan: queues the tick-thread teardown
	if _vk_backend:
		# The mp4 is finalized by HWEncoderStop on the Vulkan tick, so wait for the encoder to
		# report idle before publishing the path, or the gallery would race the muxer.
		_finalize_vk()
		return
	print("[xreal-record] recording stopped -> %s" % _path)
	active_changed.emit(false)
	finished.emit(_path)

func _finalize_vk() -> void:
	var deadline := Time.get_ticks_msec() + 3000
	while _system.is_stream_active() and Time.get_ticks_msec() < deadline:
		await get_tree().process_frame
	if _system.is_stream_active():
		push_warning("[xreal-record] encoder stop did not finalize in time; mp4 may be truncated")
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

## Eyes recorded into one frame: 2 side by side in stereo, 1 otherwise.
func _eyes() -> int:
	return 2 if stereo else 1

## The recorded frame size, which is what the encoder is configured with. Stereo halves the height
## and fits both eyes across the width, matching the SDK's own sizing for CaptureSide.Both.
func _out_size() -> Vector2i:
	return Vector2i(record_width, record_height / _eyes())

## Head-POV AR viewport on a transparent background: holograms only, so it composites over the
## camera for the blend and, with no camera, reads back as holograms on black. Stereo builds one
## per eye at half of both dimensions, which the composite then lays out side by side.
func _ensure_viewport() -> void:
	if not _ar_vps.is_empty() and _built_stereo == stereo:
		return
	_teardown()
	_built_stereo = stereo
	var eye_size := Vector2i(record_width / _eyes(), record_height / _eyes())
	for eye in _eyes():
		var vp := SubViewport.new()
		vp.size = eye_size
		vp.transparent_bg = true
		vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
		vp.world_3d = get_tree().root.world_3d  # render the same 3D world the glasses show
		add_child(vp)
		var cam := Camera3D.new()
		cam.current = true
		cam.cull_mask = record_cull_mask
		vp.add_child(cam)
		_ar_vps.append(vp)
		_ar_cams.append(cam)

func _teardown() -> void:
	for vp in _ar_vps:
		vp.queue_free()
	_ar_vps.clear()
	_ar_cams.clear()
	_comp_mats.clear()
	if _comp_vp:
		_comp_vp.queue_free()
		_comp_vp = null
	_rgb_geom_done = false  # the FOV is applied per camera, and the cameras are new

## Composite viewport blending the AR viewport over the RGB camera, using xreal_blend_2d.gdshader
## just as blend capture and streaming do, built lazily the first time the camera is on while
## recording. In stereo it holds one ColorRect per eye, side by side.
func _ensure_comp() -> void:
	if _comp_vp != null:
		return
	var out_size := _out_size()
	_comp_vp = SubViewport.new()
	_comp_vp.size = out_size
	_comp_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	add_child(_comp_vp)
	var shader := load("res://addons/godot_xreal/shaders/xreal_blend_2d.gdshader")
	for eye in _ar_vps.size():
		var mat := ShaderMaterial.new()
		mat.shader = shader
		var rect := ColorRect.new()
		rect.position = Vector2(eye * out_size.x / _ar_vps.size(), 0)
		rect.size = Vector2(out_size.x / _ar_vps.size(), out_size.y)
		rect.material = mat
		_comp_vp.add_child(rect)
		_comp_mats.append(mat)

## Drive the AR cameras from the RGB camera's real geometry: the intrinsics give the vertical FOV
## and the pose relative to the head gives a small forward offset, so the blended holograms match
## the camera image. It is static per device, so it is applied once.
func _apply_rgb_geometry() -> void:
	if _rgb_geom_done or _ar_cams.is_empty():
		return
	for cam in _ar_cams:
		_rgb_offset = XrealShared.apply_rgb_camera_geometry(_system, cam)
	_eye_offsets = XrealShared.eye_offsets(_system)
	_rgb_geom_done = true

## True when the RGB camera feed is live, meaning the camera is on and a frame arrived, in which
## case we record the camera+AR blend.
func _use_blend(feed: Object) -> bool:
	if feed == null or not is_instance_valid(feed) or not feed.has_method(&"get_y_texture"):
		return false
	return feed.get_y_texture() != null and feed.get_cbcr_texture() != null

func _process(_delta: float) -> void:
	if not _active or _ar_vps.is_empty():
		return
	var feed := XrealShared.find_camera_feed(get_tree())
	var blending := _use_blend(feed)
	var tracker := XrealShared.find_head_tracker(get_tree())
	if tracker:
		if blending:
			# Blend, with the camera ON: drive the AR camera from the RGB camera's real geometry, its FOV
			# and forward offset, so the holograms line up with the camera image instead of a default guess.
			_apply_rgb_geometry()
		for eye in _ar_cams.size():
			# In stereo each eye adds its own display offset, which is what separates the two views.
			var offset := _rgb_offset if blending else Vector3.ZERO
			if _built_stereo:
				offset += _eye_offsets[eye] as Vector3
			if not blending:
				_ar_cams[eye].fov = 75.0  # plain AR with no camera: head-locked at the default FOV
			_ar_cams[eye].global_transform = tracker.global_transform.translated_local(offset)
	# The blend viewport is what applies blend_mode and the green key, so it is needed whenever either
	# asks for something other than "holograms on transparent", which is exactly what the AR viewport
	# already is. That keeps the common VIRTUAL_ONLY and camera-off case on the cheap single-viewport
	# path. Stereo always needs it, because laying the eyes side by side is its job.
	var needs_comp := (
		_built_stereo
		or (blending and blend_mode == BlendMode.BLEND)
		or blend_mode == BlendMode.RGB_ONLY
		or (green_background and blend_mode != BlendMode.RGB_ONLY)
	)
	var src_vp := _ar_vps[0]
	if needs_comp:
		_ensure_comp()
		for eye in _comp_mats.size():
			if blending:
				_comp_mats[eye].set_shader_parameter(&"y_texture", feed.get_y_texture())
				_comp_mats[eye].set_shader_parameter(&"cbcr_texture", feed.get_cbcr_texture())
			_comp_mats[eye].set_shader_parameter(&"ar_texture", _ar_vps[eye].get_texture())
			_comp_mats[eye].set_shader_parameter(&"blend_mode", int(blend_mode))
			_comp_mats[eye].set_shader_parameter(&"green_background", green_background)
			_comp_mats[eye].set_shader_parameter(&"green_key", Vector3(green_key.r, green_key.g, green_key.b))
		_comp_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
		src_vp = _comp_vp
	elif _comp_vp != null:
		_comp_vp.render_target_update_mode = SubViewport.UPDATE_DISABLED  # idle the blend when unused
	var viewport_rid := src_vp.get_viewport_rid()
	var ts := Time.get_ticks_usec() * 1000  # nanoseconds
	if _vk_backend:
		# Vulkan bridge: publish the source viewport; the native side copies its VkImage at end
		# of frame and encodes it one frame later (vulkan-path-plan.md stage 4).
		_system.stream_publish_viewport(viewport_rid, ts)
		return
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
