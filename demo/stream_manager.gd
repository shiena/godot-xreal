extends Node
## First-person-view streaming demo. Renders the head-POV AR into a SubViewport and streams its GL
## texture with the libmedia_codec HW encoder (XrealSystem.stream_*), driven by the phone-menu "Stream"
## toggle (Camera tab). When the RGB camera is ON it instead streams the camera+AR blend (what a
## bystander sees, via xreal_blend_2d.gdshader like blend_manager); with the camera OFF it streams the AR
## view alone. Like the SDK's cast this is an Eyes/RGB-camera feature — gated to One Series via
## is_camera_supported() so it never opens the encoder on the camera-less Air 2 Ultra.
##
## The destination is XREAL's "StreamingReceiver" PC app, found by LAN discovery: demo/stream_pairing.gd
## broadcasts FIND-SERVER, does the TCP EnterRoom/useAudio handshake, and reports the receiver's IP; we
## then stream to rtp://<ip>:5555 (video 5555 / audio 5557). The receiver drops back to its idle screen
## if RTP doesn't arrive right after the handshake, so we stream_start immediately on `paired`.
##
## The encoder reads the GL texture on the render thread, so the per-frame push runs inside a
## RenderingServer.call_on_render_thread callback. See docs/plans/fpv-streaming-plan.md.

## Emitted whenever streaming actually starts/stops (incl. async pairing success/failure), so the
## phone "Stream" toggle can reflect the real state.
signal active_changed(active: bool)

const RTP_PORT := 5555
const STREAM_W := 1280
const STREAM_H := 720
const STREAM_BITRATE := 8_000_000
const STREAM_FPS := 30
## Include the microphone in the stream (captured natively by the encoder; needs RECORD_AUDIO).
const STREAM_WITH_MIC := true
## Include app audio (fed via stream_push_audio from an AudioEffectCapture). Off — the demo plays no sound.
const STREAM_WITH_INTERNAL_AUDIO := false

var _system: Object                 # XrealSystem
var _tracker: Node3D                # head tracker (FPV camera follows it)
var _ar_vp: SubViewport             # head-POV AR, transparent bg (holograms only)
var _ar_cam: Camera3D
var _comp_vp: SubViewport           # camera+AR blend composite, built lazily when the camera is on
var _comp_mat: ShaderMaterial
var _feed: Object                   # XrealCameraFeed (Y/CbCr), injected by main.gd while the camera is on
var _pairing: Node                  # demo/stream_pairing.gd
var _active := false
var _with_mic := false              # mic state chosen at toggle time, used once paired

func setup(system: Object, tracker: Node3D) -> void:
	_system = system
	_tracker = tracker
	# RECORD_AUDIO is a runtime (dangerous) permission: the export plugin declares it in the manifest,
	# but the native encoder's mic capture (addMicphoneAudio) stays silent until it's granted at runtime.
	# Request it proactively at startup so the one-time dialog is dealt with before the user hits Stream.
	if STREAM_WITH_MIC and OS.has_feature("android") and not _mic_granted():
		OS.request_permission("android.permission.RECORD_AUDIO")
	# LAN-discovery pairing with the StreamingReceiver PC app.
	_pairing = Node.new()
	_pairing.name = "StreamPairing"
	_pairing.set_script(load("res://demo/stream_pairing.gd"))
	add_child(_pairing)
	_pairing.paired.connect(_on_paired)
	_pairing.failed.connect(_on_pair_failed)
	_pairing.lost.connect(_on_pair_lost)

## Injected by main.gd: the live RGB camera feed, or null when the camera is off. When set (and a frame
## has arrived), streaming switches to the camera+AR blend; when null, it streams the AR view alone.
func set_feed(feed: Object) -> void:
	_feed = feed

## True once RECORD_AUDIO is granted (always true off Android, where the encoder mic isn't used).
func _mic_granted() -> bool:
	if not OS.has_feature("android"):
		return true
	return "android.permission.RECORD_AUDIO" in OS.get_granted_permissions()

## Toggle streaming. Pairing is async, so turning on only *starts* discovery; the actual stream starts
## on the `paired` signal (or the toggle is flipped back via active_changed on failure).
func set_enabled(on: bool) -> void:
	if not on:
		_stop()
		return
	if _active:
		return
	if not _system or not _system.has_method(&"stream_start"):
		active_changed.emit(false)
		return
	# The SDK's first-person-view cast is an Eyes/RGB-camera feature (One Series only). Gate on the same
	# IsHMDFeatureSupported(RGB_CAMERA) check as the camera so the HW encoder is never opened on the Air 2
	# Ultra (no Eyes) — avoiding the freeze the camera hit there.
	if _system.has_method(&"is_camera_supported") and not _system.is_camera_supported():
		push_warning("[demo] FPV streaming needs an Eye-equipped device (One Series) — unavailable")
		active_changed.emit(false)
		return
	# Only announce/capture the mic if RECORD_AUDIO is granted — otherwise the encoder's AudioRecord stays
	# silent. If wanted but not granted, (re)request it and stream video-only this time (grant lands next).
	_with_mic = STREAM_WITH_MIC
	if _with_mic and OS.has_feature("android") and not _mic_granted():
		OS.request_permission("android.permission.RECORD_AUDIO")
		_with_mic = false
		push_warning("[demo] mic not granted yet — streaming video-only; grant RECORD_AUDIO, then toggle Stream again for audio")
	print("[demo] FPV stream: pairing with StreamingReceiver (mic=%s) ..." % _with_mic)
	_pairing.start(_with_mic)

## Pairing succeeded — stream to the receiver right away (it idles out if RTP doesn't follow the handshake).
func _on_paired(server_ip: String) -> void:
	var url := "rtp://%s:%d" % [server_ip, RTP_PORT]
	_ensure_viewport()
	if not _system.stream_start(url, STREAM_W, STREAM_H, STREAM_BITRATE, STREAM_FPS, _with_mic, STREAM_WITH_INTERNAL_AUDIO):
		push_warning("[demo] FPV stream_start failed for %s" % url)
		_pairing.stop()
		active_changed.emit(false)
		return
	_active = true
	print("[demo] FPV stream -> %s (mic=%s)" % [url, _with_mic])
	active_changed.emit(true)

func _on_pair_failed(reason: String) -> void:
	push_warning("[demo] FPV pairing failed: %s" % reason)
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

## Head-POV AR viewport (transparent bg): holograms only, so it composites over the camera for the blend
## and, with no camera, reads back as holograms on black — the same view we streamed before.
func _ensure_viewport() -> void:
	if _ar_vp != null:
		return
	_ar_vp = SubViewport.new()
	_ar_vp.size = Vector2i(STREAM_W, STREAM_H)
	_ar_vp.transparent_bg = true
	_ar_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	_ar_vp.world_3d = get_tree().root.world_3d  # render the same 3D world the glasses show
	add_child(_ar_vp)
	_ar_cam = Camera3D.new()
	_ar_cam.current = true
	_ar_vp.add_child(_ar_cam)

## Composite viewport blending the AR viewport over the RGB camera (xreal_blend_2d.gdshader, same as
## blend_manager), built lazily the first time the camera is on. Streaming it casts what a bystander sees.
func _ensure_comp() -> void:
	if _comp_vp != null:
		return
	_comp_vp = SubViewport.new()
	_comp_vp.size = Vector2i(STREAM_W, STREAM_H)
	_comp_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	add_child(_comp_vp)
	_comp_mat = ShaderMaterial.new()
	_comp_mat.shader = load("res://demo/xreal_blend_2d.gdshader")
	var rect := ColorRect.new()
	rect.size = Vector2(STREAM_W, STREAM_H)
	rect.material = _comp_mat
	_comp_vp.add_child(rect)

## True when the RGB camera feed is live (camera on + a frame arrived) → stream the camera+AR blend.
func _use_blend() -> bool:
	if _feed == null or not is_instance_valid(_feed) or not _feed.has_method(&"get_y_texture"):
		return false
	return _feed.get_y_texture() != null and _feed.get_cbcr_texture() != null

func _process(_delta: float) -> void:
	if not _active or _ar_vp == null:
		return
	# Head-locked FPV camera follows the head.
	if _tracker and _ar_cam:
		_ar_cam.global_transform = _tracker.global_transform
	# Camera ON -> stream the camera+AR blend (what a bystander sees); Camera OFF -> the AR view alone.
	var src_vp := _ar_vp
	if _use_blend():
		_ensure_comp()
		_comp_mat.set_shader_parameter(&"y_texture", _feed.get_y_texture())
		_comp_mat.set_shader_parameter(&"cbcr_texture", _feed.get_cbcr_texture())
		_comp_mat.set_shader_parameter(&"ar_texture", _ar_vp.get_texture())
		_comp_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
		src_vp = _comp_vp
	elif _comp_vp != null:
		_comp_vp.render_target_update_mode = SubViewport.UPDATE_DISABLED  # idle the blend when camera is off
	var viewport_rid := src_vp.get_viewport_rid()
	var ts := Time.get_ticks_usec() * 1000  # nanoseconds
	# ViewportTexture.get_rid() is a proxy RID. In the Compatibility renderer its copied tex_id can
	# remain 0, so resolve the viewport's real render-target color texture instead. Resolve the GL name
	# every frame to follow render-target reallocations, and push while the render EGL context is current.
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
