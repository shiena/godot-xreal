extends Node
## Frame blending, or mixed-reality capture, as a drop-in feature component: the Godot analog of
## the SDK's FrameBlender. It renders the AR scene from the head POV into a transparent-background
## SubViewport, then a composite SubViewport blends that OVER the RGB camera YCbCr feed
## (xreal_blend_2d.gdshader). capture_blended() saves the composite as a JPG, which is what a
## bystander would see: the camera image with the virtual content overlaid. This is an RGB-camera
## (Eyes) feature, so One Series only.
##
## It needs the camera running (xreal_camera.tscn enabled) and a common XR camera in the tree. Both are
## discovered at capture time through XrealShared.find_camera_feed() and find_tracking_head(), with
## no wiring needed.

## Emitted when an operation fails or the feature is unavailable, so the load site can react by
## showing UI, logging, or flipping a toggle. It carries the same human-readable text that is also
## pushed as a warning.
signal error(message: String)

## Capture both eyes side by side rather than one view, the equivalent of the SDK's
## `CaptureSide.Both`. The output keeps its width and halves its height, each eye filling one half,
## so the frame is squeezed horizontally the way side-by-side 3D formats are.
##
## Only the virtual content gains parallax: the glasses carry one RGB camera, so both halves share
## the same real-world image. That is also what the SDK produces.
##
## Changing this rebuilds the viewports on the next capture.
@export var stereo := false

const W := 1280
const H := 720

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar_vps: Array[SubViewport] = []  # one per captured eye: 1 entry mono, 2 stereo
var _ar_cams: Array[Camera3D] = []
var _comp_vp: SubViewport
var _comp_mats: Array[ShaderMaterial] = []
var _built_stereo := false          # the layout the current viewports were built for
var _rgb_offset := Vector3.ZERO    # RGB camera offset from the head (Godot space), for parallax
var _eye_offsets := [Vector3.ZERO, Vector3.ZERO]  # per-eye display offsets, for the stereo parallax
var _rgb_geom_done := false        # RGB geometry (FOV + offset) applied once, since it is static per device

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert

## Build (or rebuild) the viewports for the current `stereo` setting.
##
## Mono is one full-size AR viewport composited into a full-size output. Stereo is two half-width,
## half-height AR viewports composited side by side into a half-height output, which matches the
## SDK's own sizing: it renders each eye at half of both dimensions and lays them into a
## width x height/2 target.
func _ensure() -> bool:
	if _comp_vp != null and _built_stereo == stereo:
		return true
	_teardown()
	_built_stereo = stereo
	var eyes := 2 if stereo else 1
	var eye_size := Vector2i(W / eyes, H / eyes)
	var out_size := Vector2i(W, H / eyes)
	# Composite viewport: blends the AR viewport(s) over the camera.
	_comp_vp = SubViewport.new()
	_comp_vp.size = out_size
	_comp_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	add_child(_comp_vp)
	var shader := load("res://addons/godot_xreal/shaders/xreal_blend_2d.gdshader")
	for eye in eyes:
		# AR viewport: the shared 3D world from the head POV on a transparent background, holograms only.
		var vp := SubViewport.new()
		vp.size = eye_size
		vp.transparent_bg = true
		vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
		vp.world_3d = get_tree().root.world_3d
		add_child(vp)
		var cam := Camera3D.new()
		cam.current = true
		vp.add_child(cam)
		_ar_vps.append(vp)
		_ar_cams.append(cam)
		# One ColorRect per eye, side by side. Each carries its own material because the AR texture
		# differs per eye; the camera textures are the same on both, there being one camera.
		var mat := ShaderMaterial.new()
		mat.shader = shader
		var rect := ColorRect.new()
		rect.position = Vector2(eye * out_size.x / eyes, 0)
		rect.size = Vector2(out_size.x / eyes, out_size.y)
		rect.material = mat
		_comp_vp.add_child(rect)
		_comp_mats.append(mat)
	return true

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

## Drive the AR camera from the RGB camera's real geometry: intrinsics give the vertical FOV, and
## the pose relative to the head gives a small forward offset. The holograms then match the camera
## image instead of a default guess. It is static per device, so it is applied once.
func _apply_rgb_geometry() -> void:
	if _rgb_geom_done or _ar_cams.is_empty():
		return
	for cam in _ar_cams:
		_rgb_offset = XrealShared.apply_rgb_camera_geometry(_system, cam)
	_eye_offsets = XrealShared.eye_offsets(_system)
	_rgb_geom_done = true

## The live camera feed's Y/CbCr textures as [yt, ct], or an empty array when the camera is not
## ready: off device, unsupported device, feed off, or no frame yet. Each case warns.
func _feed_textures() -> Array:
	if _system == null:
		return []
	if _system.has_method(&"is_camera_supported") and not _system.is_camera_supported():
		_fail("[xreal-blend] this device has no RGB camera (One Series only)")
		return []
	var feed := XrealShared.find_camera_feed(get_tree())
	if feed == null or not feed.has_method(&"get_y_texture"):
		_fail("[xreal-blend] camera feed not ready (enable the camera first)")
		return []
	var yt = feed.get_y_texture()
	var ct = feed.get_cbcr_texture()
	if yt == null or ct == null:
		_fail("[xreal-blend] no camera frame yet")
		return []
	return [yt, ct]

## Capture the blended (camera + AR) composite to a JPG. Returns the path ("" on failure).
func capture_blended() -> String:
	var tex := _feed_textures()
	if tex.is_empty():
		return ""
	_ensure()
	_apply_rgb_geometry()
	var tracker := XrealShared.find_tracking_head(get_tree())
	for eye in _ar_cams.size():
		if tracker:
			# Sit the AR camera at the RGB camera's pose, the head plus its small forward offset, rather
			# than at the head alone, so the holograms line up with the camera image and the parallax is
			# right. In stereo each eye adds its own display offset on top, which is what separates them.
			var offset: Vector3 = _rgb_offset
			if _built_stereo:
				offset += _eye_offsets[eye] as Vector3
			_ar_cams[eye].global_transform = tracker.global_transform.translated_local(offset)
		_comp_mats[eye].set_shader_parameter(&"y_texture", tex[0])
		_comp_mats[eye].set_shader_parameter(&"cbcr_texture", tex[1])
		_comp_mats[eye].set_shader_parameter(&"ar_texture", _ar_vps[eye].get_texture())
	# Let both viewports render this frame before reading the composite back.
	await RenderingServer.frame_post_draw
	var img := _comp_vp.get_texture().get_image()
	if img == null:
		_fail("[xreal-blend] readback failed")
		return ""
	# No flip. This called img.flip_y(), described as correcting a bottom-up SubViewport read-back,
	# but the read-back is upright: what it actually cancelled was the camera texture's own
	# upside-down content, and it turned the AR layer over in the process (device-checked
	# 2026-08-13, a blend photo with the camera upright and the holograms inverted). The camera's
	# orientation is handled where it belongs now, in xreal_blend_2d.gdshader's V flip.
	# Local date-time in the name (blend_YYYYMMDD_HHMMSS.jpg) so the file reads naturally in the
	# gallery: "2026-07-20T14:25:30" becomes "20260720_142530".
	var stamp := Time.get_datetime_string_from_system().replace("-", "").replace(":", "").replace("T", "_")
	var path := OS.get_user_data_dir().path_join("blend_%s.jpg" % stamp)
	var err := img.save_jpg(path)
	if err != OK:
		_fail("[xreal-blend] save_jpg failed (err %d)" % err)
		return ""
	return path

## Push a warning AND emit `error`, so the load site can detect the failure instead of only seeing
## it in the log.
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
