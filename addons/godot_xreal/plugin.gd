@tool
extends EditorPlugin

## Godot XREAL addon.
##
## The runtime classes are provided by the `godot_xreal` GDExtension and are always
## available once the extension is loaded (regardless of this plugin's enabled state):
##
##   - XrealHeadTracker (Node3D) — drives its transform from the native 6DoF head pose
##     (rotation + position; 3DoF/0DoF selectable via the xreal/tracking_type setting).
##     Parent a Camera3D under it (see addons/godot_xreal/xreal_rig.tscn).
##   - XrealSystem (RefCounted) — façade over the native plugin: query availability /
##     version / tracking state, switch the tracking mode, and drive the AR subsystems,
##     render metrics, capture and FPV streaming that the feature sub-scenes build on.
##   - XrealAR (Node) — polls the AR change streams each frame and re-emits them as
##     plane / spatial-anchor / tracked-image / glasses-event signals.
##   - XrealHandTracker (Node) — publishes XREAL hand tracking to XRServer as two
##     XRHandTrackers (Air 2 Ultra only).
##   - XrealCameraFeed (CameraFeed) — publishes the RGB camera frames.
##
## Drop-in feature sub-scenes (camera, plane detection, spatial anchors, image tracking,
## depth mesh, hand tracking, photo/blend capture, FPV streaming) live under
## addons/godot_xreal/features/.
##
## This EditorPlugin exists so the addon can be toggled from Project > Project Settings >
## Plugins and to host the editor docks (SDK import + image-tracking DB builder).

const ExportPluginScript := preload("res://addons/godot_xreal/export_plugin.gd")
const ImageDbDockScript := preload("res://addons/godot_xreal/editor/image_db_dock.gd")
const VendorImportDockScript := preload("res://addons/godot_xreal/editor/vendor_import_dock.gd")
var _export_plugin: EditorExportPlugin
var _image_db_dock: Control
var _vendor_import_dock: Control

## The `xreal/*` project settings consumed at runtime (demo/main.gd reads them with these same
## inline defaults, so a project works with or without them persisted). Registered here so they
## show up in Project > Project Settings with proper types/hints; only values changed from the
## default are written to project.godot. Left in place on plugin disable (removing them would
## drop user-chosen values).
const PROJECT_SETTINGS: Array[Dictionary] = [
	{
		# Head-tracking mode applied at boot. "SDK Default" (-1) leaves the native default /
		# `debug.xreal.tracking_type` system property in charge.
		"name": "xreal/tracking_type",
		"type": TYPE_INT,
		"hint": PROPERTY_HINT_ENUM,
		"hint_string": "SDK Default:-1,6DoF:0,3DoF:1,0DoF:2",
		"default": -1,
	},
	{
		# Stereo rendering mode applied at boot. "SDK Default" (-1) leaves the
		# `debug.xreal.stereo_mode` system property / native default (Multipass) in charge.
		# Multiview is single-pass-instanced but buys no GPU on this rig — Multipass recommended.
		"name": "xreal/stereo_mode",
		"type": TYPE_INT,
		"hint": PROPERTY_HINT_ENUM,
		"hint_string": "SDK Default:-1,Multipass:0,Multiview:2",
		"default": -1,
	},
	{
		# Keep the glasses display on while the headset is not worn (bypass the proximity sensor's
		# auto-off). On by default; turn off to let the display sleep when the glasses are taken off.
		"name": "xreal/display_bypass_psensor",
		"type": TYPE_BOOL,
		"default": true,
	},
]

func _register_project_settings() -> void:
	for s in PROJECT_SETTINGS:
		var setting_name: String = s["name"]
		if not ProjectSettings.has_setting(setting_name):
			ProjectSettings.set_setting(setting_name, s["default"])
		ProjectSettings.set_initial_value(setting_name, s["default"])
		ProjectSettings.add_property_info({
			"name": setting_name,
			"type": s["type"],
			"hint": s.get("hint", PROPERTY_HINT_NONE),
			"hint_string": s.get("hint_string", ""),
		})
		ProjectSettings.set_as_basic(setting_name, true)

func _enter_tree() -> void:
	_register_project_settings()
	# Contribute the XREAL Android manifest/library requirements at export time so the Gradle
	# build template needs no hand-edits (they survive template regeneration).
	_export_plugin = ExportPluginScript.new()
	add_export_plugin(_export_plugin)

	# SDK vendoring dock: pick the com.xreal.xr package (.tgz/.tar.gz or an extracted folder) and copy
	# the .so/.aar/tool into place — the in-editor analog of scripts/vendor_xreal_libs.*.
	_vendor_import_dock = VendorImportDockScript.new()
	_vendor_import_dock.name = "XREAL Import"
	add_control_to_dock(EditorPlugin.DOCK_SLOT_LEFT_UR, _vendor_import_dock)

	# Image-tracking DB builder dock (runs the vendored trackableImageTools to compile the blob —
	# the Godot analog of Unity's XREALImageLibraryBuildProcessor).
	_image_db_dock = ImageDbDockScript.new()
	_image_db_dock.name = "XREAL Image DB"
	add_control_to_dock(EditorPlugin.DOCK_SLOT_LEFT_UR, _image_db_dock)

func _exit_tree() -> void:
	if _export_plugin:
		remove_export_plugin(_export_plugin)
		_export_plugin = null
	if _image_db_dock:
		remove_control_from_docks(_image_db_dock)
		_image_db_dock.free()
		_image_db_dock = null
	if _vendor_import_dock:
		remove_control_from_docks(_vendor_import_dock)
		_vendor_import_dock.free()
		_vendor_import_dock = null
