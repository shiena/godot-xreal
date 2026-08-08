extends Node
## First-person-view streaming as a drop-in feature component. It renders the head-POV AR into a
## SubViewport and streams that GL texture with the libmedia_codec HW encoder
## (XrealSystem.stream_*). With the RGB camera ON, meaning the xreal_camera feature is enabled, it
## streams the camera+AR blend instead, which is what a bystander sees, through
## xreal_blend_2d.gdshader; with the camera OFF it streams the AR view alone. The encoder feeds on
## our own SubViewport texture rather than the camera, so streaming needs no RGB camera and works
## on the camera-less Air 2 Ultra too.
##
## The destination is XREAL's "StreamingReceiver" PC app, found by LAN discovery
## (xreal_stream_pairing.gd): a FIND-SERVER broadcast, the TCP EnterRoom and useAudio handshake,
## then RTP to rtp://<ip>:5555 (video 5555, audio 5557). The receiver drops back to its idle screen
## if RTP does not arrive right after the handshake, so we stream_start immediately on `paired`.
##
## The encoder reads the GL texture on the render thread, so the per-frame push runs inside a
## RenderingServer.call_on_render_thread callback. See docs/develop/plans/fpv-streaming-plan.md.
##
## Both the common XR camera and the live camera feed are discovered per frame, through
## XrealShared.find_tracking_head() and find_camera_feed(), so toggling the camera mid-stream simply
## switches the streamed view.

## Emitted when an operation fails or the feature is unavailable, so the load site can react by
## showing UI, logging, or flipping a toggle. It carries the same human-readable text that is also
## pushed as a warning.
signal error(message: String)

## Emitted whenever streaming actually starts or stops, including async pairing success and
## failure, so a UI toggle can reflect the real state.
signal active_changed(active: bool)

## Which audio goes out with the stream (SDK VideoCapture's Audio State). The encoder captures MIC
## natively and needs RECORD_AUDIO; it captures APP natively too, through a MediaProjection. This
## replaces the old `with_mic` bool, where MIC behaves exactly like `with_mic = true`.
@export var audio_state: XrealShared.AudioState = XrealShared.AudioState.MIC
## Target the receiver's ObserverView page (MRC composite) instead of FirstPersonView. It defaults
## to OFF, because FirstPersonView is the useful mode on the XREAL One, whose RGB camera does an
## aligned on-device blend. ObserverView is a niche and incomplete path, mainly for camera-less
## glasses: when true the stream pairs without the useAudio handshake, streams the virtual-only AR
## with alpha (useAlpha=true) so the PC composites it over its webcam, and applies the observer FOV
## the receiver pushes. It runs end to end, but the composite is NOT spatially aligned, because the
## protocol carries no observer-camera pose. See docs/develop/plans/observer-view-notes.md.
@export var observer_mode := false
## Capture size and bitrate preset (SDK VideoCapture's Resolution Level). `CUSTOM` uses the
## explicit stream_width, stream_height and stream_bitrate below; every other value overrides them
## at start.
@export var resolution_level: XrealShared.ResolutionLevel = XrealShared.ResolutionLevel.HIGH
@export var stream_width := 1280
@export var stream_height := 720
@export var stream_bitrate := 8_000_000
@export var stream_fps := 30

## What goes out on the wire (SDK VideoCapture's Blend Mode). BLEND draws the holograms over the RGB
## camera image, RGB_ONLY sends the camera alone, VIRTUAL_ONLY sends the holograms alone.
enum BlendMode { BLEND, RGB_ONLY, VIRTUAL_ONLY }
@export var blend_mode: BlendMode = BlendMode.BLEND
## Replace the real world behind the holograms with a chroma key. Ignored for RGB_ONLY.
@export var green_background := false
@export var green_key: Color = Color(0.0, 1.0, 0.0)
## Which 3D render layers the capture camera sees (SDK VideoCapture's Culling Mask).
@export_flags_3d_render var stream_cull_mask := 0xFFFFF

const RTP_PORT := 5555

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar_vp: SubViewport             # head-POV AR, transparent bg (holograms only)
var _ar_cam: Camera3D
var _comp_vp: SubViewport           # camera+AR blend composite, built lazily when the camera is on
var _comp_mat: ShaderMaterial
var _pairing: Node                  # xreal_stream_pairing.gd
var _active := false
var _mic_now := false               # mic state chosen at toggle time, used once paired
var _pending_fov := {}              # ObserverView: latest observer-camera FOV pushed by the receiver
var _rgb_offset := Vector3.ZERO     # RGB camera offset from the head (Godot space), for blend parallax
var _rgb_geom_done := false         # RGB blend geometry (FOV + offset) applied once, static per device
var _epoch := 0                     # bumped on every start/stop; a frame captured for a prior session is dropped
var _vk_backend := false            # encoder backend 2 = Vulkan bridge (publish RIDs, not GL names)

func _ready() -> void:
	_system = XrealShared.make_system()
	if _system == null:
		return  # off-device -> inert (set_enabled just reports false)
# (the encoder backend is sampled at start time, not here: the Vulkan bridge initializes after
# component _ready, so an early query would read 0 and lock the component onto the GL push path)
	# Mic permission (RECORD_AUDIO) is requested lazily on the Stream toggle (see set_enabled),
	# matching the camera: there is no startup dialog, so the app asks only when you actually start
	# streaming. Below, LAN-discovery pairing with the StreamingReceiver PC app.
	_pairing = Node.new()
	_pairing.name = "StreamPairing"
	_pairing.set_script(preload("res://addons/godot_xreal/features/xreal_stream_pairing.gd"))
	add_child(_pairing)
	_pairing.paired.connect(_on_paired)
	_pairing.failed.connect(_on_pair_failed)
	_pairing.lost.connect(_on_pair_lost)
	_pairing.camera_param.connect(_on_camera_param)

## True once RECORD_AUDIO is granted. It is always true off Android, where the encoder mic goes
## unused.
func _mic_granted() -> bool:
	return XrealShared.is_mic_granted()

## Toggle streaming. Pairing is async, so turning it on only *starts* discovery; the actual stream
## starts on the `paired` signal, and active_changed(false) reports a failure.
func set_enabled(on: bool) -> void:
	if not on:
		_stop()
		return
	if _active:
		return
	if not _system or not _system.has_method(&"stream_start") or _pairing == null:
		active_changed.emit(false)
		return
	# Refuse before pairing and the permission dialogs when the HW encoder cannot run on this
	# renderer (Vulkan, until the vulkan-path stage-4 bridge). Rust keeps the hard gate inside
	# stream_start; checking here skips the pointless side effects and reports the specific reason
	# instead of a generic start failure.
	if (
		_system.has_method(&"is_render_texture_encoder_supported")
		and not _system.is_render_texture_encoder_supported()
	):
		_fail("[xreal-stream] HW encoder needs the GL renderer; streaming is unavailable under Vulkan")
		active_changed.emit(false)
		return
	# One process-global HW encoder, shared with xreal_video_recorder: stream_start while it runs
	# would not open a second encoder but feed our frames into the running recording.
	if _system.has_method(&"is_stream_active") and _system.is_stream_active():
		_fail("[xreal-stream] HW encoder busy (recording?), so stop it first")
		active_changed.emit(false)
		return
	# Sampled here, at start time, because the Vulkan bridge initializes after _ready.
	_vk_backend = (
		_system.has_method(&"get_render_texture_encoder_backend")
		and _system.get_render_texture_encoder_backend() == 2
	)
	# NB: no RGB-camera gate here. We render our own head-POV AR into a SubViewport and hand that GL
	# texture to the device-agnostic libmedia_codec encoder, so the camera is never touched unless it
	# happens to be on, in which case we opportunistically stream the camera+AR blend (_use_blend).
	if observer_mode:
		# ObserverView (MRC): no mic and no useAudio, since the PC composites our virtual-only render,
		# alpha included, over its webcam.
		_mic_now = false
		print("[xreal-stream] Observer stream: pairing with StreamingReceiver (ObserverView) ...")
		_pairing.start(false, true)
		return
	# Announce and capture the mic only once RECORD_AUDIO is granted, or the encoder's AudioRecord
	# stays silent. When it is wanted but not granted, re-request it and stream video-only this time.
	_mic_now = XrealShared.audio_wants_mic(audio_state)
	if _mic_now and OS.has_feature("android") and not _mic_granted():
		OS.request_permission("android.permission.RECORD_AUDIO")
		_mic_now = false
		_fail("[xreal-stream] mic not granted yet, so streaming video-only; grant RECORD_AUDIO, then toggle streaming again for audio")
	print("[xreal-stream] FPV stream: pairing with StreamingReceiver (mic=%s) ..." % _mic_now)
	_pairing.start(_mic_now)

## Pairing succeeded, so stream to the receiver right away; it idles out if RTP does not follow the
## handshake.
func _on_paired(server_ip: String) -> void:
	# Pairing is async, so a recording may have grabbed the process-global encoder meanwhile.
	if _system.has_method(&"is_stream_active") and _system.is_stream_active():
		_fail("[xreal-stream] HW encoder became busy during pairing (recording?), so not streaming")
		_pairing.stop()
		active_changed.emit(false)
		return
	var url := "rtp://%s:%d" % [server_ip, RTP_PORT]
	_apply_resolution_level()
	_ensure_viewport()
	_apply_fov()  # in case the receiver's UpdateCameraParam arrived before the viewport existed
	# ObserverView streams the virtual-only AR with alpha (useAlpha) for the PC-webcam composite.
	# See xreal_video_recorder.gd: app audio is native too, gated on a MediaProjection whose consent
	# dialog is asynchronous, so the first stream that wants it triggers consent and goes mic-only.
	var want_app := XrealShared.audio_wants_app(audio_state)
	if want_app and not XrealShared.is_app_audio_ready():
		XrealShared.request_app_audio_consent()
		push_warning("[xreal-stream] app audio needs screen-capture consent; "
			+ "this stream carries the microphone only")
		want_app = false
	if not _system.stream_start(url, stream_width, stream_height, stream_bitrate, stream_fps, _mic_now, want_app, observer_mode):
		_fail("[xreal-stream] stream_start failed for %s" % url)
		_pairing.stop()
		active_changed.emit(false)
		return
	_epoch += 1
	_active = true
	print("[xreal-stream] stream -> %s (mode=%s, mic=%s)" % [url, "observer" if observer_mode else "fpv", _mic_now])
	active_changed.emit(true)

## ObserverView: apply the receiver's observer-camera FOV, given as tangent extents, to the AR
## camera. First bring-up uses a symmetric perspective, taking the vertical FOV from top and
## bottom.
func _on_camera_param(fov: Dictionary) -> void:
	_pending_fov = fov
	_apply_fov()

func _apply_fov() -> void:
	if _ar_cam == null or _pending_fov.is_empty():
		return
	var top := float(_pending_fov.get("top", 0.0))
	var bottom := float(_pending_fov.get("bottom", 0.0))
	if top > 0.0 and bottom > 0.0:
		_ar_cam.fov = rad_to_deg(atan(top) + atan(bottom))  # vertical FOV; SubViewport keeps the 16:9 aspect
		print("[xreal-stream] observer FOV applied -> vfov=%.1f deg" % _ar_cam.fov)

func _on_pair_failed(reason: String) -> void:
	_fail("[xreal-stream] FPV pairing failed: %s" % reason)
	_active = false
	active_changed.emit(false)

func _on_pair_lost() -> void:
	if _active and _system:
		_system.stream_stop()
	_active = false
	active_changed.emit(false)

## Stop streaming and tear down the control link.
func _stop() -> void:
	var was := _active
	_active = false
	_epoch += 1
	if _pairing:
		_pairing.stop()
	if _system and _system.has_method(&"stream_stop"):
		_system.stream_stop()
	if was:
		active_changed.emit(false)

## Head-POV AR viewport on a transparent background: holograms only, so it composites over the
## camera for the blend and, with no camera, reads back as holograms on black.
## Fold the Resolution Level preset into stream_width, stream_height and stream_bitrate, so
## everything downstream reads one set of values. CUSTOM leaves the exported values alone.
func _apply_resolution_level() -> void:
	if resolution_level == XrealShared.ResolutionLevel.CUSTOM:
		return
	var preset := XrealShared.resolution_preset(resolution_level)
	stream_width = preset.x
	stream_height = preset.y
	stream_bitrate = preset.z

func _ensure_viewport() -> void:
	if _ar_vp != null:
		return
	_ar_vp = SubViewport.new()
	_ar_vp.size = Vector2i(stream_width, stream_height)
	_ar_vp.transparent_bg = true
	_ar_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	_ar_vp.world_3d = get_tree().root.world_3d  # render the same 3D world the glasses show
	add_child(_ar_vp)
	_ar_cam = Camera3D.new()
	_ar_cam.current = true
	_ar_cam.cull_mask = stream_cull_mask
	_ar_vp.add_child(_ar_cam)

## Composite viewport blending the AR viewport over the RGB camera, using xreal_blend_2d.gdshader
## just as blend capture does, built lazily the first time the camera is on. Streaming it casts
## what a bystander sees.
func _ensure_comp() -> void:
	if _comp_vp != null:
		return
	_comp_vp = SubViewport.new()
	_comp_vp.size = Vector2i(stream_width, stream_height)
	_comp_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	add_child(_comp_vp)
	_comp_mat = ShaderMaterial.new()
	_comp_mat.shader = load("res://addons/godot_xreal/shaders/xreal_blend_2d.gdshader")
	var rect := ColorRect.new()
	rect.size = Vector2(stream_width, stream_height)
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
## case we stream the camera+AR blend. Never in ObserverView, where the PC does the composite over
## its webcam.
func _use_blend(feed: Object) -> bool:
	if observer_mode:
		return false
	if feed == null or not is_instance_valid(feed) or not feed.has_method(&"get_y_texture"):
		return false
	return feed.get_y_texture() != null and feed.get_cbcr_texture() != null

func _process(_delta: float) -> void:
	if not _active or _ar_vp == null:
		return
	var feed := XrealShared.find_camera_feed(get_tree())
	var blending := _use_blend(feed)
	var tracker := XrealShared.find_tracking_head(get_tree())
	if tracker and _ar_cam:
		if blending:
			# Blend, with the camera ON: drive the AR camera from the RGB camera's real geometry, its FOV
			# and forward offset, so the holograms line up with the camera image instead of a default guess.
			_apply_rgb_geometry()
			_ar_cam.global_transform = tracker.global_transform.translated_local(_rgb_offset)
		else:
			# Plain AR with no camera: head-locked at the default FOV. ObserverView sets its own FOV, pushed
			# by the receiver, in _apply_fov, so leave it alone there.
			if not observer_mode:
				_ar_cam.fov = 75.0
			_ar_cam.global_transform = tracker.global_transform
	# Camera ON streams the camera+AR blend, what a bystander sees; camera OFF streams the AR view
	# alone. See the recorder for why only some mode and key combinations need the blend viewport.
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
		_comp_vp.render_target_update_mode = SubViewport.UPDATE_DISABLED  # idle the blend when camera is off
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
	if _active and _system:
		_system.stream_stop()
	if _pairing:
		_pairing.stop()

## Push a warning AND emit `error`, so the load site can detect the failure instead of only seeing
## it in the log.
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
