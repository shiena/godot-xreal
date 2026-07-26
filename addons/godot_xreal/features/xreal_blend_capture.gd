extends Node
## Frame blending, or mixed-reality capture, as a drop-in feature component: the Godot analog of
## the SDK's FrameBlender. It renders the AR scene from the head POV into a transparent-background
## SubViewport, then a composite SubViewport blends that OVER the RGB camera YCbCr feed
## (xreal_blend_2d.gdshader). capture_blended() saves the composite as a JPG, which is what a
## bystander would see: the camera image with the virtual content overlaid. This is an RGB-camera
## (Eyes) feature, so One Series only.
##
## It needs the camera running (xreal_camera.tscn enabled) and the head rig in the tree. Both are
## discovered at capture time through XrealShared.find_camera_feed() and find_head_tracker(), with
## no wiring needed.

## Emitted when an operation fails or the feature is unavailable, so the load site can react by
## showing UI, logging, or flipping a toggle. It carries the same human-readable text that is also
## pushed as a warning.
signal error(message: String)


const W := 1280
const H := 720

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar_vp: SubViewport
var _ar_cam: Camera3D
var _comp_vp: SubViewport
var _comp_mat: ShaderMaterial
var _rgb_offset := Vector3.ZERO    # RGB camera offset from the head (Godot space), for parallax
var _rgb_geom_done := false        # RGB geometry (FOV + offset) applied once, since it is static per device

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert

func _ensure() -> bool:
	if _comp_vp != null:
		return true
	# AR viewport: the shared 3D world from the head POV on a transparent background, holograms only.
	_ar_vp = SubViewport.new()
	_ar_vp.size = Vector2i(W, H)
	_ar_vp.transparent_bg = true
	_ar_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	_ar_vp.world_3d = get_tree().root.world_3d
	add_child(_ar_vp)
	_ar_cam = Camera3D.new()
	_ar_cam.current = true
	_ar_vp.add_child(_ar_cam)
	# Composite viewport: blends the AR viewport over the camera.
	_comp_vp = SubViewport.new()
	_comp_vp.size = Vector2i(W, H)
	_comp_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	add_child(_comp_vp)
	_comp_mat = ShaderMaterial.new()
	_comp_mat.shader = load("res://addons/godot_xreal/shaders/xreal_blend_2d.gdshader")
	var rect := ColorRect.new()
	rect.size = Vector2(W, H)
	rect.material = _comp_mat
	_comp_vp.add_child(rect)
	return true

## Drive the AR camera from the RGB camera's real geometry: intrinsics give the vertical FOV, and
## the pose relative to the head gives a small forward offset. The holograms then match the camera
## image instead of a default guess. It is static per device, so it is applied once.
func _apply_rgb_geometry() -> void:
	if _rgb_geom_done or _ar_cam == null:
		return
	_rgb_offset = XrealShared.apply_rgb_camera_geometry(_system, _ar_cam)
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
	var tracker := XrealShared.find_head_tracker(get_tree())
	if tracker and _ar_cam:
		# Sit the AR camera at the RGB camera's pose, the head plus its small forward offset, rather than
		# at the head alone, so the holograms line up with the camera image and the parallax is right.
		_ar_cam.global_transform = tracker.global_transform.translated_local(_rgb_offset)
	_comp_mat.set_shader_parameter(&"y_texture", tex[0])
	_comp_mat.set_shader_parameter(&"cbcr_texture", tex[1])
	_comp_mat.set_shader_parameter(&"ar_texture", _ar_vp.get_texture())
	# Let both viewports render this frame before reading the composite back.
	await RenderingServer.frame_post_draw
	var img := _comp_vp.get_texture().get_image()
	if img == null:
		_fail("[xreal-blend] readback failed")
		return ""
	img.flip_y()  # SubViewport read-back is bottom-up (GL origin), so flip to upright before saving
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
