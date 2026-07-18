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
var _rgb_offset := Vector3.ZERO    # RGB camera offset from the head (Godot space), for parallax
var _rgb_geom_done := false        # RGB geometry (FOV + offset) applied once — it's static per device

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

## Drive the AR camera from the RGB camera's real geometry (intrinsics -> vertical FOV, pose-from-head
## -> a small forward offset) so the holograms match the camera image instead of a default guess. Static
## per device, so applied once. See docs/plans/coordinate-systems-notes.md.
func _apply_rgb_geometry() -> void:
	if _rgb_geom_done or _ar_cam == null or _system == null or not _system.has_method(&"get_camera_intrinsics"):
		return
	var comp := 2  # XREALComponent.RGB_CAMERA
	var res: Vector2i = _system.get_device_resolution(comp)
	var intr: PackedFloat32Array = _system.get_camera_intrinsics(comp)  # [fx, fy, cx, cy]
	if res.y > 0 and intr.size() == 4 and intr[1] > 0.0:
		_ar_cam.fov = rad_to_deg(2.0 * atan((res.y * 0.5) / intr[1]))  # vertical FOV from fy (KEEP_HEIGHT)
		print("[blend] RGB-matched AR FOV=%.1f deg" % _ar_cam.fov)
	var pose: PackedFloat32Array = _system.get_device_pose_from_head(comp)  # [px,py,pz, qx,qy,qz,qw] Unity
	if pose.size() == 7:
		_rgb_offset = Vector3(pose[0], -pose[1], -pose[2])  # Unity->Godot (this port's (x,-y,-z)); ~cm parallax
		print("[blend] RGB offset from head=%s m" % _rgb_offset)
	_rgb_geom_done = true

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
	_apply_rgb_geometry()
	if _tracker and _ar_cam:
		# Sit the AR camera at the RGB camera's pose (head + its small forward offset), not just the head,
		# so the holograms line up with the camera image (parallax).
		_ar_cam.global_transform = _tracker.global_transform.translated_local(_rgb_offset)
	_comp_mat.set_shader_parameter(&"y_texture", yt)
	_comp_mat.set_shader_parameter(&"cbcr_texture", ct)
	_comp_mat.set_shader_parameter(&"ar_texture", _ar_vp.get_texture())
	# Let both viewports render this frame before reading the composite back.
	await RenderingServer.frame_post_draw
	var img := _comp_vp.get_texture().get_image()
	if img == null:
		push_warning("[blend] readback failed")
		return ""
	img.flip_y()  # SubViewport read-back is bottom-up (GL origin) — flip to upright before saving
	var path := OS.get_user_data_dir().path_join("blend_%d.jpg" % Time.get_ticks_msec())
	var err := img.save_jpg(path)
	if err != OK:
		push_warning("[blend] save_jpg failed (err %d)" % err)
		return ""
	print("[blend] composite saved -> %s" % path)
	return path
