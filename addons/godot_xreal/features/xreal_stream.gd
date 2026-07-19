extends Node
## First-person-view streaming as a drop-in feature component. Renders the head-POV AR into a
## SubViewport and streams its GL texture with the libmedia_codec HW encoder (XrealSystem.stream_*).
## When the RGB camera is ON (xreal_camera feature enabled) it instead streams the camera+AR blend
## (what a bystander sees, via xreal_blend_2d.gdshader); with the camera OFF it streams the AR view
## alone. The encoder feeds on our own SubViewport texture, not the camera, so streaming needs no
## RGB camera and works on the camera-less Air 2 Ultra too.
##
## The destination is XREAL's "StreamingReceiver" PC app, found by LAN discovery
## (xreal_stream_pairing.gd): FIND-SERVER broadcast, TCP EnterRoom/useAudio handshake, then RTP to
## rtp://<ip>:5555 (video 5555 / audio 5557). The receiver drops back to its idle screen if RTP
## doesn't arrive right after the handshake, so we stream_start immediately on `paired`.
##
## The encoder reads the GL texture on the render thread, so the per-frame push runs inside a
## RenderingServer.call_on_render_thread callback. See docs/plans/fpv-streaming-plan.md.
##
## The head rig is discovered per frame (XrealShared.find_head_tracker); the live camera feed per
## frame too (find_camera_feed), so camera on/off mid-stream just switches the streamed view.

## Emitted whenever streaming actually starts/stops (incl. async pairing success/failure), so a UI
## toggle can reflect the real state.
## Emitted when an operation fails or the feature is unavailable, so the load site can react
## (show UI, log, flip a toggle). Carries the same human-readable text also pushed as a warning.
signal error(message: String)


signal active_changed(active: bool)

## Include the microphone in the stream (captured natively by the encoder; needs RECORD_AUDIO).
@export var with_mic := true
## Target the receiver's ObserverView page (MRC composite) instead of FirstPersonView. Default OFF —
## FirstPersonView is the useful mode on XREAL One (its RGB camera does an aligned on-device blend).
## ObserverView is a niche/incomplete path (mainly for camera-less glasses): when true the stream
## pairs without the useAudio handshake, streams the virtual-only AR with alpha (useAlpha=true) so
## the PC composites it over its webcam, and applies the observer FOV the receiver pushes. It runs
## end to end, but the composite is NOT spatially aligned — the protocol carries no observer-camera
## pose. See docs/plans/observer-view-notes.md.
@export var observer_mode := false
@export var stream_width := 1280
@export var stream_height := 720
@export var stream_bitrate := 8_000_000
@export var stream_fps := 30

const RTP_PORT := 5555
## Include app audio (fed via stream_push_audio from an AudioEffectCapture). Off by default — feed
## it yourself if your app plays sound that should be cast.
const STREAM_WITH_INTERNAL_AUDIO := false

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
var _rgb_geom_done := false         # RGB blend geometry (FOV + offset) applied once — static per device

func _ready() -> void:
	_system = XrealShared.make_system()
	if _system == null:
		return  # off-device -> inert (set_enabled just reports false)
	# RECORD_AUDIO is a runtime (dangerous) permission: the export plugin declares it in the
	# manifest, but the native encoder's mic capture stays silent until it's granted at runtime.
	# Request it proactively at startup so the one-time dialog is dealt with before streaming.
	if with_mic and OS.has_feature("android") and not _mic_granted():
		OS.request_permission("android.permission.RECORD_AUDIO")
	# LAN-discovery pairing with the StreamingReceiver PC app.
	_pairing = Node.new()
	_pairing.name = "StreamPairing"
	_pairing.set_script(preload("res://addons/godot_xreal/features/xreal_stream_pairing.gd"))
	add_child(_pairing)
	_pairing.paired.connect(_on_paired)
	_pairing.failed.connect(_on_pair_failed)
	_pairing.lost.connect(_on_pair_lost)
	_pairing.camera_param.connect(_on_camera_param)

## True once RECORD_AUDIO is granted (always true off Android, where the encoder mic isn't used).
func _mic_granted() -> bool:
	if not OS.has_feature("android"):
		return true
	return "android.permission.RECORD_AUDIO" in OS.get_granted_permissions()

## Toggle streaming. Pairing is async, so turning on only *starts* discovery; the actual stream
## starts on the `paired` signal (or active_changed(false) reports the failure).
func set_enabled(on: bool) -> void:
	if not on:
		_stop()
		return
	if _active:
		return
	if not _system or not _system.has_method(&"stream_start") or _pairing == null:
		active_changed.emit(false)
		return
	# NB: no RGB-camera gate here. We render our own head-POV AR into a SubViewport and hand that GL
	# texture to the (device-agnostic) libmedia_codec encoder — the camera is never touched unless
	# it happens to be on, in which case we opportunistically stream the camera+AR blend (_use_blend).
	if observer_mode:
		# ObserverView (MRC): no mic/useAudio; the PC composites our virtual-only+alpha render over
		# its webcam.
		_mic_now = false
		print("[xreal-stream] Observer stream: pairing with StreamingReceiver (ObserverView) ...")
		_pairing.start(false, true)
		return
	# Only announce/capture the mic if RECORD_AUDIO is granted — otherwise the encoder's AudioRecord
	# stays silent. If wanted but not granted, (re)request it and stream video-only this time.
	_mic_now = with_mic
	if _mic_now and OS.has_feature("android") and not _mic_granted():
		OS.request_permission("android.permission.RECORD_AUDIO")
		_mic_now = false
		_fail("[xreal-stream] mic not granted yet — streaming video-only; grant RECORD_AUDIO, then toggle streaming again for audio")
	print("[xreal-stream] FPV stream: pairing with StreamingReceiver (mic=%s) ..." % _mic_now)
	_pairing.start(_mic_now)

## Pairing succeeded — stream to the receiver right away (it idles out if RTP doesn't follow the
## handshake).
func _on_paired(server_ip: String) -> void:
	var url := "rtp://%s:%d" % [server_ip, RTP_PORT]
	_ensure_viewport()
	_apply_fov()  # in case the receiver's UpdateCameraParam arrived before the viewport existed
	# ObserverView streams the virtual-only AR with alpha (useAlpha) for the PC-webcam composite.
	if not _system.stream_start(url, stream_width, stream_height, stream_bitrate, stream_fps, _mic_now, STREAM_WITH_INTERNAL_AUDIO, observer_mode):
		_fail("[xreal-stream] stream_start failed for %s" % url)
		_pairing.stop()
		active_changed.emit(false)
		return
	_active = true
	print("[xreal-stream] stream -> %s (mode=%s, mic=%s)" % [url, "observer" if observer_mode else "fpv", _mic_now])
	active_changed.emit(true)

## ObserverView: apply the receiver's observer-camera FOV (tangent extents) to the AR camera. First
## bring-up uses a symmetric perspective (vertical FOV from top+bottom).
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

## Stop streaming + tear down the control link.
func _stop() -> void:
	var was := _active
	_active = false
	if _pairing:
		_pairing.stop()
	if _system and _system.has_method(&"stream_stop"):
		_system.stream_stop()
	if was:
		active_changed.emit(false)

## Head-POV AR viewport (transparent bg): holograms only, so it composites over the camera for the
## blend and, with no camera, reads back as holograms on black.
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
	_ar_vp.add_child(_ar_cam)

## Composite viewport blending the AR viewport over the RGB camera (xreal_blend_2d.gdshader, same
## as blend capture), built lazily the first time the camera is on. Streaming it casts what a
## bystander sees.
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

## Drive the AR camera from the RGB camera's real geometry (intrinsics -> vertical FOV,
## pose-from-head -> a small forward offset) so the blended holograms match the camera image.
## Static per device, applied once.
func _apply_rgb_geometry() -> void:
	if _rgb_geom_done or _ar_cam == null:
		return
	_rgb_offset = XrealShared.apply_rgb_camera_geometry(_system, _ar_cam)
	_rgb_geom_done = true

## True when the RGB camera feed is live (camera on + a frame arrived) → stream the camera+AR blend.
## Never in ObserverView: the composite happens on the PC (over its webcam).
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
	var tracker := XrealShared.find_head_tracker(get_tree())
	if tracker and _ar_cam:
		if blending:
			# Blend (camera ON): drive the AR camera from the RGB camera's real geometry (FOV +
			# forward offset) so the holograms line up with the camera image instead of a default guess.
			_apply_rgb_geometry()
			_ar_cam.global_transform = tracker.global_transform.translated_local(_rgb_offset)
		else:
			# Plain AR (no camera): head-locked with the default FOV. ObserverView sets its own FOV
			# (from the receiver) in _apply_fov, so leave it alone there.
			if not observer_mode:
				_ar_cam.fov = 75.0
			_ar_cam.global_transform = tracker.global_transform
	# Camera ON -> stream the camera+AR blend (what a bystander sees); camera OFF -> the AR view alone.
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
	if _active and _system:
		_system.stream_stop()
	if _pairing:
		_pairing.stop()

## Push a warning AND emit `error` so the load site can detect the failure (not just see the log).
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
