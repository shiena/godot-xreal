extends Node
## First-person-view streaming demo. Renders the AR scene from the head POV into a SubViewport and
## streams its GL texture with the libmedia_codec HW encoder (XrealSystem.stream_*), driven by the
## phone-menu "配信" toggle (カメラ tab). Like the SDK's cast this is an Eyes/RGB-camera feature — gated
## to One Series via is_camera_supported() so it never opens the encoder on the camera-less Air 2 Ultra.
##
## The encoder reads the GL texture on the render thread, so the per-frame push runs inside a
## RenderingServer.call_on_render_thread callback. See docs/plans/fpv-streaming-plan.md and the
## receiving server at scripts/stream_server/.

## Stream target: set to "rtp://<your-PC-IP>:5555" to send to scripts/stream_server (run receive.ps1),
## or leave "" to record a local .mp4 on-device (adb pull it) — the simplest first bring-up, no network.
const STREAM_TARGET := ""
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
var _viewport: SubViewport
var _camera: Camera3D
var _active := false

func setup(system: Object, tracker: Node3D) -> void:
	_system = system
	_tracker = tracker
	# RECORD_AUDIO is a runtime (dangerous) permission: the export plugin declares it in the manifest,
	# but the native encoder's mic capture (addMicphoneAudio) stays silent until it's granted at runtime.
	# Request it proactively at startup so the one-time dialog is dealt with before the user hits 配信.
	if STREAM_WITH_MIC and OS.has_feature("android") and not _mic_granted():
		OS.request_permission("android.permission.RECORD_AUDIO")

## True once RECORD_AUDIO is granted (always true off Android, where the encoder mic isn't used).
func _mic_granted() -> bool:
	if not OS.has_feature("android"):
		return true
	return "android.permission.RECORD_AUDIO" in OS.get_granted_permissions()

## Toggle streaming. Returns the resulting state (false if the encoder ABI is unavailable or start
## failed, so the phone-menu toggle can flip itself back off).
func set_enabled(on: bool) -> bool:
	if not _system or not _system.has_method(&"stream_start"):
		return false
	if on:
		# The SDK's first-person-view cast is an Eyes/RGB-camera feature (One Series only). Gate on the
		# same IsHMDFeatureSupported(RGB_CAMERA) check as the camera so the HW encoder is never opened on
		# the Air 2 Ultra (no Eyes) — avoiding the freeze the camera hit there.
		if _system.has_method(&"is_camera_supported") and not _system.is_camera_supported():
			push_warning("[demo] FPV streaming needs an Eye-equipped device (One Series) — unavailable")
			return false
		# Only tell the encoder to capture the mic if RECORD_AUDIO is actually granted — otherwise its
		# AudioRecord stays silent and no audio track is muxed. If the mic is wanted but not yet granted,
		# (re)request it and stream video-only this time; the async grant lands for the next 配信 toggle.
		var with_mic := STREAM_WITH_MIC
		if with_mic and OS.has_feature("android") and not _mic_granted():
			OS.request_permission("android.permission.RECORD_AUDIO")
			with_mic = false
			push_warning("[demo] mic not granted yet — streaming video-only; grant RECORD_AUDIO, then toggle 配信 again for audio")
		_ensure_viewport()
		var url := STREAM_TARGET
		if url.is_empty():
			url = OS.get_user_data_dir().path_join("fpv_stream.mp4")
		if not _system.stream_start(url, STREAM_W, STREAM_H, STREAM_BITRATE, STREAM_FPS, with_mic, STREAM_WITH_INTERNAL_AUDIO):
			return false
		print("[demo] FPV stream -> %s (mic=%s)" % [url, with_mic])
		_active = true
	else:
		_active = false
		_system.stream_stop()
	return _active

## A SubViewport that renders the shared AR world from a head-locked camera (the first-person view).
func _ensure_viewport() -> void:
	if _viewport != null:
		return
	_viewport = SubViewport.new()
	_viewport.size = Vector2i(STREAM_W, STREAM_H)
	_viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	_viewport.world_3d = get_tree().root.world_3d  # render the same 3D world the glasses show
	add_child(_viewport)
	_camera = Camera3D.new()
	_camera.current = true
	_viewport.add_child(_camera)

func _process(_delta: float) -> void:
	if not _active or _viewport == null:
		return
	# FPV camera follows the head.
	if _tracker and _camera:
		_camera.global_transform = _tracker.global_transform
	var viewport_rid := _viewport.get_viewport_rid()
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
