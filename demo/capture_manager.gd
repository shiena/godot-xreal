extends Node
## Photo capture — the Godot analog of the SDK's XREALPhotoCapture. Like the SDK (which reads the
## camera RenderTexture back and EncodeToJPG), this renders the XREAL RGB camera YCbCr feed into an
## offscreen SubViewport and saves the read-back image as a JPG. Driven by the phone-menu "撮影"
## button (カメラ tab). RGB-camera / Eyes feature — One Series only (guarded like the camera).

const PHOTO_W := 1280
const PHOTO_H := 720

var _system: Object                 # XrealSystem (for the is_camera_supported gate)
var _feed: Object                   # XrealCameraFeed (Y/CbCr textures), injected by main.gd
var _viewport: SubViewport
var _rect: ColorRect
var _mat: ShaderMaterial

func setup(system: Object) -> void:
	_system = system

## Injected by main.gd once the RGB camera feed is created.
func set_feed(feed: Object) -> void:
	_feed = feed

func _ensure_viewport() -> void:
	if _viewport != null:
		return
	_viewport = SubViewport.new()
	_viewport.size = Vector2i(PHOTO_W, PHOTO_H)
	_viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	add_child(_viewport)
	_mat = ShaderMaterial.new()
	_mat.shader = load("res://demo/xreal_ycbcr_2d.gdshader")
	_rect = ColorRect.new()
	_rect.size = Vector2(PHOTO_W, PHOTO_H)
	_rect.material = _mat
	_viewport.add_child(_rect)

## Capture the current camera view to a JPG in the user data dir. Returns the path ("" on failure).
func capture_photo() -> String:
	if _system and _system.has_method(&"is_camera_supported") and not _system.is_camera_supported():
		push_warning("[capture] this device has no RGB camera (One Series only)")
		return ""
	if _feed == null or not _feed.has_method(&"get_y_texture"):
		push_warning("[capture] camera feed not ready (enable the camera first)")
		return ""
	var yt = _feed.get_y_texture()
	var ct = _feed.get_cbcr_texture()
	if yt == null or ct == null:
		push_warning("[capture] no camera frame yet")
		return ""
	_ensure_viewport()
	_mat.set_shader_parameter(&"y_texture", yt)
	_mat.set_shader_parameter(&"cbcr_texture", ct)
	# Let the offscreen viewport render this frame with the textures before reading it back.
	await RenderingServer.frame_post_draw
	var img := _viewport.get_texture().get_image()
	if img == null:
		push_warning("[capture] readback failed")
		return ""
	img.flip_y()  # SubViewport read-back is bottom-up (GL origin) — flip to upright before saving
	var path := OS.get_user_data_dir().path_join("photo_%d.jpg" % Time.get_ticks_msec())
	var err := img.save_jpg(path)
	if err != OK:
		push_warning("[capture] save_jpg failed (err %d)" % err)
		return ""
	print("[capture] photo saved -> %s" % path)
	return path
