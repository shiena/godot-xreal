extends Node
## Photo capture as a drop-in feature component — the Godot analog of the SDK's XREALPhotoCapture.
## Like the SDK (which reads the camera RenderTexture back and EncodeToJPG), this renders the XREAL
## RGB camera YCbCr feed into an offscreen SubViewport and saves the read-back image as a JPG (in
## the user data dir + the phone gallery). RGB-camera / Eyes feature — One Series only.
##
## Needs the camera running: drop in xreal_camera.tscn too and enable it — the live feed is
## discovered via XrealShared.find_camera_feed() at capture time, no wiring needed.

## Emitted when an operation fails or the feature is unavailable, so the load site can react
## (show UI, log, flip a toggle). Carries the same human-readable text also pushed as a warning.
signal error(message: String)


const PHOTO_W := 1280
const PHOTO_H := 720

var _system: Object                 # XrealSystem (this feature's own stateless instance)
var _viewport: SubViewport
var _rect: ColorRect
var _mat: ShaderMaterial

func _ready() -> void:
	_system = XrealShared.make_system()  # null off-device -> inert

func _ensure_viewport() -> void:
	if _viewport != null:
		return
	_viewport = SubViewport.new()
	_viewport.size = Vector2i(PHOTO_W, PHOTO_H)
	_viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	add_child(_viewport)
	_mat = ShaderMaterial.new()
	_mat.shader = load("res://addons/godot_xreal/shaders/xreal_ycbcr_2d.gdshader")
	_rect = ColorRect.new()
	_rect.size = Vector2(PHOTO_W, PHOTO_H)
	_rect.material = _mat
	_viewport.add_child(_rect)

## The live camera feed's Y/CbCr textures as [yt, ct], or an empty array when the camera isn't
## ready (off-device / unsupported device / feed off / no frame yet — each case warns).
func _feed_textures() -> Array:
	if _system == null:
		return []
	if _system.has_method(&"is_camera_supported") and not _system.is_camera_supported():
		_fail("[xreal-capture] this device has no RGB camera (One Series only)")
		return []
	var feed := XrealShared.find_camera_feed(get_tree())
	if feed == null or not feed.has_method(&"get_y_texture"):
		_fail("[xreal-capture] camera feed not ready (enable the camera first)")
		return []
	var yt = feed.get_y_texture()
	var ct = feed.get_cbcr_texture()
	if yt == null or ct == null:
		_fail("[xreal-capture] no camera frame yet")
		return []
	return [yt, ct]

## Capture the current camera view to a JPG in the user data dir. Returns the path ("" on failure).
func capture_photo() -> String:
	var tex := _feed_textures()
	if tex.is_empty():
		return ""
	_ensure_viewport()
	_mat.set_shader_parameter(&"y_texture", tex[0])
	_mat.set_shader_parameter(&"cbcr_texture", tex[1])
	# Let the offscreen viewport render this frame with the textures before reading it back.
	await RenderingServer.frame_post_draw
	var img := _viewport.get_texture().get_image()
	if img == null:
		_fail("[xreal-capture] readback failed")
		return ""
	img.flip_y()  # SubViewport read-back is bottom-up (GL origin) — flip to upright before saving
	var path := OS.get_user_data_dir().path_join("photo_%d.jpg" % Time.get_ticks_msec())
	var err := img.save_jpg(path)
	if err != OK:
		_fail("[xreal-capture] save_jpg failed (err %d)" % err)
		return ""
	print("[xreal-capture] photo saved -> %s" % path)
	XrealShared.save_to_gallery(path, "image/jpeg", false)  # also into the phone gallery (optional helper)
	return path

## Push a warning AND emit `error` so the load site can detect the failure (not just see the log).
func _fail(msg: String) -> void:
	push_warning(msg)
	error.emit(msg)
