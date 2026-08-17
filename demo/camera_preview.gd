extends Node3D
## Demo-side head-locked live preview of the XREAL RGB camera. The addon's xreal_camera feature
## owns only the feed (CameraServer registration), and showing it is the app's choice, so the demo
## renders it here. It takes the same path as the photo, blend and stream features: discover the
## live feed with XrealShared.find_camera_feed() and sample its Y/CbCr ImageTextures DIRECTLY,
## because a CameraTexture on a script-fed feed only shows Godot's placeholder. The shader is the
## addon's spatial YCbCr->RGB one.
##
## The quad is reparented under the common XRCamera3D once it exists, so it follows the gaze
## (head-locked) on every XR backend. Legacy rigs remain supported through XrealShared's fallback.
## It stays inert off device and while the camera is off: the panel keeps hidden, since a
## not-yet-fed shader would show pink.

## Show the head-locked preview quad. Turn off to keep the shared camera feed running with no preview.
@export var show_preview: bool = true

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
	# preview with no wiring. Off device this is always null, so the panel stays hidden.
	var feed: Object = XrealShared.find_camera_feed(get_tree())
	var live: bool = feed != null and is_instance_valid(feed) and feed.has_method(&"get_y_texture")
	var yt: Texture2D = feed.get_y_texture() if live else null
	var ct: Texture2D = feed.get_cbcr_texture() if live else null
	if yt == null or ct == null:
		# Camera off, not started, or no frame yet: keep the unset-sampler (pink) panel hidden.
		if _panel.visible:
			_panel.visible = false
		return
	# Head-lock to the common XR camera, including any XROrigin3D world-space adjustment.
	var head: Node3D = XrealShared.find_tracking_head(get_tree())
	if head and _panel.get_parent() != head:
		_panel.reparent(head, false)
	# The XrealCameraFeed keeps these ImageTextures updated in place; re-set them each frame so a
	# camera off->on (a fresh feed with new textures) rewires cleanly.
	var mat: ShaderMaterial = _panel.material_override
	if mat:
		mat.set_shader_parameter(&"y_texture", yt)
		mat.set_shader_parameter(&"cbcr_texture", ct)
		# The RD renderers (Forward+/Mobile, i.e. Vulkan) skip the extra ALBEDO srgb_to_linear that
		# Compatibility applies, so tell the YCbCr shader to linearize twice there; see the shader.
		mat.set_shader_parameter(&"double_srgb", RenderingServer.get_rendering_device() != null)
		_panel.visible = true

func _exit_tree() -> void:
	# The panel lives under the tracker once live, so take it down with us.
	if _panel and is_instance_valid(_panel) and _panel.get_parent() != self:
		_panel.queue_free()
