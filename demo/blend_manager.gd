extends Node
## Frame blending / mixed-reality capture — the Godot analog of the SDK's FrameBlender. Renders the AR
## scene from the head POV into a transparent-background SubViewport, then a composite SubViewport
## blends it OVER the RGB camera YCbCr feed (xreal_blend_2d.gdshader). "合成撮影" (カメラ tab) saves the
## composite as a JPG — what a bystander would see (camera + the virtual content overlaid).
## RGB-camera / Eyes feature — One Series only (guarded like the camera).

const W := 1280
const H := 720

var _system: Object                 # XrealSystem (is_camera_supported gate)
var _feed: Object                   # XrealCameraFeed (Y/CbCr), injected by main.gd
var _tracker: Node3D                # head tracker (AR camera follows it)
var _ar_vp: SubViewport
var _ar_cam: Camera3D
var _comp_vp: SubViewport
var _comp_mat: ShaderMaterial

func setup(system: Object, tracker: Node3D) -> void:
	_system = system
	_tracker = tracker

## Injected by main.gd once the RGB camera feed is created.
func set_feed(feed: Object) -> void:
	_feed = feed

func _ensure() -> bool:
	if _comp_vp != null:
		return true
	if _feed == null:
		return false
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
	_comp_mat.shader = load("res://demo/xreal_blend_2d.gdshader")
	var rect := ColorRect.new()
	rect.size = Vector2(W, H)
	rect.material = _comp_mat
	_comp_vp.add_child(rect)
	return true

## Capture the blended (camera + AR) composite to a JPG. Returns the path ("" on failure).
func capture_blended() -> String:
	if _system and _system.has_method(&"is_camera_supported") and not _system.is_camera_supported():
		push_warning("[blend] this device has no RGB camera (One Series only)")
		return ""
	if not _ensure():
		push_warning("[blend] camera feed not ready (enable the camera first)")
		return ""
	var yt = _feed.get_y_texture()
	var ct = _feed.get_cbcr_texture()
	if yt == null or ct == null:
		push_warning("[blend] no camera frame yet")
		return ""
	if _tracker and _ar_cam:
		_ar_cam.global_transform = _tracker.global_transform
	_comp_mat.set_shader_parameter(&"y_texture", yt)
	_comp_mat.set_shader_parameter(&"cbcr_texture", ct)
	_comp_mat.set_shader_parameter(&"ar_texture", _ar_vp.get_texture())
	# Let both viewports render this frame before reading the composite back.
	await RenderingServer.frame_post_draw
	var img := _comp_vp.get_texture().get_image()
	if img == null:
		push_warning("[blend] readback failed")
		return ""
	var path := OS.get_user_data_dir().path_join("blend_%d.jpg" % Time.get_ticks_msec())
	var err := img.save_jpg(path)
	if err != OK:
		push_warning("[blend] save_jpg failed (err %d)" % err)
		return ""
	print("[blend] composite saved -> %s" % path)
	return path
