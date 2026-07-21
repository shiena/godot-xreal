extends Node3D
## Demo-side head-locked live preview of the XREAL RGB camera. The addon's xreal_camera feature
## only owns the feed (CameraServer registration); whether to SHOW it is the app's choice, so the
## demo renders it here — the same path the photo/blend/stream features take: discover the live feed
## via XrealShared.find_camera_feed() and sample its Y/CbCr ImageTextures DIRECTLY (a CameraTexture
## on a script-fed feed only shows Godot's placeholder). Shader = the addon's spatial YCbCr->RGB one.
##
## The quad is reparented under the head tracker (xreal_head_tracker group) once it exists, so it
## follows the gaze (head-locked) and is drawn by the eye SubViewports (shared world). Inert off
## device / while the camera is off: the panel stays hidden (a not-yet-fed shader would show pink).

## Show the head-locked preview quad. Turn off to keep the shared camera feed running with no preview.
@export var show_preview := true

var _panel: MeshInstance3D

func _ready() -> void:
	_panel = $PreviewPanel

func _process(_delta: float) -> void:
	if _panel == null:
		return
	if not show_preview:
		if _panel.visible:
			_panel.visible = false
		return
	# Discovered per frame (like the photo/blend/stream features), so camera on/off just toggles the
	# preview with no wiring. Off device this is always null -> the panel stays hidden.
	var feed := XrealShared.find_camera_feed(get_tree())
	var live := feed != null and is_instance_valid(feed) and feed.has_method(&"get_y_texture")
	var yt = feed.get_y_texture() if live else null
	var ct = feed.get_cbcr_texture() if live else null
	if yt == null or ct == null:
		# Camera off / not started / no frame yet — keep the unset-sampler (pink) panel hidden.
		if _panel.visible:
			_panel.visible = false
		return
	# Head-lock: reparent the quad under the tracker (spawned at runtime) so it follows the gaze.
	var tracker := XrealShared.find_head_tracker(get_tree())
	if tracker and _panel.get_parent() != tracker:
		_panel.reparent(tracker, false)
	# The XrealCameraFeed keeps these ImageTextures updated in place; re-set them each frame so a
	# camera off->on (a fresh feed with new textures) rewires cleanly.
	var mat: ShaderMaterial = _panel.material_override
	if mat:
		mat.set_shader_parameter(&"y_texture", yt)
		mat.set_shader_parameter(&"cbcr_texture", ct)
		_panel.visible = true

func _exit_tree() -> void:
	# The panel lives under the tracker once live — take it down with us.
	if _panel and is_instance_valid(_panel) and _panel.get_parent() != self:
		_panel.queue_free()
