extends Node
## Frame blending / mixed-reality capture as a drop-in feature component — the Godot analog of the
## SDK's FrameBlender. Renders the AR scene from the head POV into a transparent-background
## SubViewport, then a composite SubViewport blends it OVER the RGB camera YCbCr feed
## (xreal_blend_2d.gdshader). capture_blended() saves the composite as a JPG — what a bystander
## would see (camera + the virtual content overlaid). RGB-camera / Eyes feature — One Series only.
##
## Needs the camera running (xreal_camera.tscn enabled) and the head rig in the tree — both are
## discovered at capture time (XrealShared.find_camera_feed / find_head_tracker), no wiring needed.

## Emitted when an operation fails or the feature is unavailable, so the load site can react
## (show UI, log, flip a toggle). Carries the same human-readable text also pushed as a warning.
signal error(message: String)


const W := 1280
const H := 720

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _ar_vp: SubViewport
var _ar_cam: Camera3D
var _comp_vp: SubViewport
var _comp_mat: ShaderMaterial
var _rgb_offset := Vector3.ZERO    # RGB camera offset from the head (Godot space), for parallax
var _rgb_geom_done := false        # RGB geometry (FOV + offset) applied once — it's static per device

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert

func _ensure() -> bool:
	if _comp_vp != null:
		return true
	# AR viewport: the shared 3D world from the head POV, transparent background (only the holograms).
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

## Drive the AR camera from the RGB camera's real geometry (intrinsics -> vertical FOV,
## pose-from-head -> a small forward offset) so the holograms match the camera image instead of a
## default guess. Static per device, so applied once.
func _apply_rgb_geometry() -> void:
	if _rgb_geom_done or _ar_cam == null:
		return
	_rgb_offset = XrealShared.apply_rgb_camera_geometry(_system, _ar_cam)
	_rgb_geom_done = true

## The live camera feed's Y/CbCr textures as [yt, ct], or an empty array when the camera isn't
## ready (off-device / unsupported device / feed off / no frame yet — each case warns).
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
		# Sit the AR camera at the RGB camera's pose (head + its small forward offset), not just the
		# head, so the holograms line up with the camera image (parallax).
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
	img.flip_y()  # SubViewport read-back is bottom-up (GL origin) — flip to upright before saving
	var path := OS.get_user_data_dir().path_join("blend_%d.jpg" % Time.get_ticks_msec())
	var err := img.save_jpg(path)
	if err != OK:
		_fail("[xreal-blend] save_jpg failed (err %d)" % err)
		return ""
	print("[xreal-blend] composite saved -> %s" % path)
	XrealShared.save_to_gallery(path, "image/jpeg", false)  # also into the phone gallery (optional helper)
	return path

## Push a warning AND emit `error` so the load site can detect the failure (not just see the log).
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
