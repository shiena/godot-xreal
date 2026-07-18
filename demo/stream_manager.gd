extends Node
## First-person-view streaming demo. Renders the head-POV AR into a SubViewport and streams its GL
## texture with the libmedia_codec HW encoder (XrealSystem.stream_*), driven by the phone-menu "Stream"
## toggle (Camera tab). When the RGB camera is ON it instead streams the camera+AR blend (what a
## bystander sees, via xreal_blend_2d.gdshader like blend_manager); with the camera OFF it streams the AR
## view alone. The encoder feeds on our own SubViewport texture, not the camera, so streaming needs no
## RGB camera and works on the camera-less Air 2 Ultra too (it just streams the AR-only view there).
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

## Target the receiver's ObserverView page (MRC composite) instead of FirstPersonView. Default OFF —
## FirstPersonView is the useful mode on XREAL One (its RGB camera does an aligned on-device blend).
## ObserverView is a niche/incomplete path (mainly for camera-less glasses): when true the Stream toggle
## pairs without the useAudio handshake, streams the virtual-only AR with alpha (useAlpha=true) so the PC
## composites it over its webcam, and applies the observer FOV the receiver pushes. It runs end to end,
## but the composite is NOT spatially aligned — the protocol carries no observer-camera pose and the PC
## webcam isn't tracked, so real alignment needs an app-level calibration/marker step we don't implement.
## See docs/plans/observer-view-notes.md.
const OBSERVER_MODE := false

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
var _pending_fov := {}              # ObserverView: latest observer-camera FOV pushed by the receiver
var _rgb_offset := Vector3.ZERO     # RGB camera offset from the head (Godot space), for blend parallax
var _rgb_geom_done := false          # RGB blend geometry (FOV + offset) applied once — static per device

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
	_pairing.camera_param.connect(_on_camera_param)

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
	# NB: no RGB-camera gate here. We render our own head-POV AR into a SubViewport and hand that GL
	# texture to the (device-agnostic) libmedia_codec encoder — the camera is never touched unless it
	# happens to be on, in which case we opportunistically stream the camera+AR blend (_use_blend). With
	# no camera (e.g. the Air 2 Ultra) it streams the AR-only view, so streaming works there too.
	if OBSERVER_MODE:
		# ObserverView (MRC): no mic/useAudio; the PC composites our virtual-only+alpha render over its webcam.
		_with_mic = false
		print("[demo] Observer stream: pairing with StreamingReceiver (ObserverView) ...")
		_pairing.start(false, true)
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
	_apply_fov()  # in case the receiver's UpdateCameraParam arrived before the viewport existed
	# ObserverView streams the virtual-only AR with alpha (useAlpha) for the PC-webcam composite.
	if not _system.stream_start(url, STREAM_W, STREAM_H, STREAM_BITRATE, STREAM_FPS, _with_mic, STREAM_WITH_INTERNAL_AUDIO, OBSERVER_MODE):
		push_warning("[demo] stream_start failed for %s" % url)
		_pairing.stop()
		active_changed.emit(false)
		return
	_active = true
	print("[demo] stream -> %s (mode=%s, mic=%s)" % [url, "observer" if OBSERVER_MODE else "fpv", _with_mic])
	active_changed.emit(true)

## ObserverView: apply the receiver's observer-camera FOV (tangent extents) to the AR camera. First
## bring-up uses a symmetric perspective (vertical FOV from top+bottom); off-centre refinement is later.
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
		print("[demo] observer FOV applied -> vfov=%.1f deg" % _ar_cam.fov)

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

## Drive the AR camera from the RGB camera's real geometry (intrinsics -> vertical FOV, pose-from-head
## -> a small forward offset) so the blended holograms match the camera image. Static per device, applied
## once. See docs/plans/coordinate-systems-notes.md.
func _apply_rgb_geometry() -> void:
	if _rgb_geom_done or _ar_cam == null or _system == null or not _system.has_method(&"get_camera_intrinsics"):
		return
	var comp := 2  # XREALComponent.RGB_CAMERA
	var res: Vector2i = _system.get_device_resolution(comp)
	var intr: PackedFloat32Array = _system.get_camera_intrinsics(comp)  # [fx, fy, cx, cy]
	if res.y > 0 and intr.size() == 4 and intr[1] > 0.0:
		_ar_cam.fov = rad_to_deg(2.0 * atan((res.y * 0.5) / intr[1]))  # vertical FOV from fy (KEEP_HEIGHT)
		print("[demo] blend AR FOV matched to RGB camera = %.1f deg" % _ar_cam.fov)
	var pose: PackedFloat32Array = _system.get_device_pose_from_head(comp)  # [px,py,pz, qx,qy,qz,qw] Unity
	if pose.size() == 7:
		_rgb_offset = Vector3(pose[0], -pose[1], -pose[2])  # Unity->Godot (this port's (x,-y,-z)); ~cm parallax
	_rgb_geom_done = true

## True when the RGB camera feed is live (camera on + a frame arrived) → stream the camera+AR blend.
## Never in ObserverView: the composite happens on the PC (over its webcam), so we stream virtual-only.
func _use_blend() -> bool:
	if OBSERVER_MODE:
		return false
	if _feed == null or not is_instance_valid(_feed) or not _feed.has_method(&"get_y_texture"):
		return false
	return _feed.get_y_texture() != null and _feed.get_cbcr_texture() != null

func _process(_delta: float) -> void:
	if not _active or _ar_vp == null:
		return
	var blending := _use_blend()
	if _tracker and _ar_cam:
		if blending:
			# Blend (Camera ON): drive the AR camera from the RGB camera's real geometry (FOV + forward
			# offset) so the holograms line up with the camera image instead of a default guess.
			_apply_rgb_geometry()
			_ar_cam.global_transform = _tracker.global_transform.translated_local(_rgb_offset)
		else:
			# Plain AR (no camera): head-locked with the default FOV. ObserverView sets its own FOV
			# (from the receiver) in _apply_fov, so leave it alone there.
			if not OBSERVER_MODE:
				_ar_cam.fov = 75.0
			_ar_cam.global_transform = _tracker.global_transform
	# Camera ON -> stream the camera+AR blend (what a bystander sees); Camera OFF -> the AR view alone.
	var src_vp := _ar_vp
	if blending:
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
