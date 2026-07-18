extends Node
## First-person-view streaming demo. Renders the AR scene from the head POV into a SubViewport and
## streams its GL texture with the libmedia_codec HW encoder (XrealSystem.stream_*), driven by the
## phone-menu "Stream" toggle (Camera tab). Like the SDK's cast this is an Eyes/RGB-camera feature —
## gated to One Series via is_camera_supported() so it never opens the encoder on the camera-less Air 2 Ultra.
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
var _viewport: SubViewport
var _camera: Camera3D
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
	if _pairing:
		_pairing.stop()
