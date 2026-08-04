@tool
extends EditorPlugin

## Godot XREAL addon.
##
## The `godot_xreal` GDExtension provides the runtime classes, and they are available once the
## extension is loaded, whatever this plugin's enabled state:
##
##   - XrealHeadTracker (Node3D) drives its transform from the native 6DoF head pose, rotation
##     and position, with 3DoF and 0DoF selectable through the xreal/tracking_type setting.
##     Parent a Camera3D under it; see addons/godot_xreal/xreal_rig.tscn.
##   - XrealSystem (RefCounted) is the facade over the native plugin: query availability,
##     version and tracking state, switch the tracking mode, and drive the AR subsystems,
##     render metrics, capture and FPV streaming that the feature sub-scenes build on.
##   - XrealAR (Node) polls the AR change streams each frame and re-emits them as plane,
##     spatial-anchor, tracked-image and glasses-event signals.
##   - XrealHandTracker (Node) publishes XREAL hand tracking to XRServer as two XRHandTrackers
##     (Air 2 Ultra only).
##   - XrealCameraFeed (CameraFeed) publishes the RGB camera frames.
##
## The drop-in feature sub-scenes (camera, plane detection, spatial anchors, image tracking,
## depth mesh, hand tracking, photo and blend capture, FPV streaming) live under
## addons/godot_xreal/features/.
##
## This EditorPlugin exists so the addon can be toggled from Project > Project Settings >
## Plugins, and so it can host the editor docks: the SDK import and the image-tracking DB
## builder.

const ExportPluginScript := preload("res://addons/godot_xreal/export_plugin.gd")
const ImageDbDockScript := preload("res://addons/godot_xreal/editor/image_db_dock.gd")
const VendorImportDockScript := preload("res://addons/godot_xreal/editor/vendor_import_dock.gd")
const MeshSnapshotDockScript := preload("res://addons/godot_xreal/editor/mesh_snapshot_dock.gd")
var _export_plugin: EditorExportPlugin
var _image_db_dock: Control
var _vendor_import_dock: Control
var _mesh_snapshot_dock: Control

## The `xreal/*` project settings. Most are consumed at runtime, and demo/main.gd reads them with
## these same inline defaults, so a project works with or without them persisted. The exceptions
## are `xreal/multi_resume` and `xreal/auto_log`, which export_plugin.gd reads at EXPORT time to
## shape the Android manifest. They are registered here so they show up in Project > Project
## Settings with proper types and hints, and only values changed from the default are written to
## project.godot. Disabling the plugin leaves them in place, since removing them would drop
## user-chosen values.
const PROJECT_SETTINGS: Array[Dictionary] = [
	{
		# Head-tracking mode applied at boot. "SDK Default" (-1) leaves the native default and the
		# `debug.xreal.tracking_type` system property in charge.
		"name": "xreal/tracking_type",
		"type": TYPE_INT,
		"hint": PROPERTY_HINT_ENUM,
		"hint_string": "SDK Default:-1,6DoF:0,3DoF:1,0DoF:2",
		"default": -1,
	},
	{
		# Stereo rendering mode applied at boot. "SDK Default" (-1) leaves the
		# `debug.xreal.stereo_mode` system property, and the native default of Multipass, in charge.
		# Multiview is single-pass-instanced but buys no GPU on this rig, so Multipass is recommended.
		"name": "xreal/stereo_mode",
		"type": TYPE_INT,
		"hint": PROPERTY_HINT_ENUM,
		"hint_string": "SDK Default:-1,Multipass:0,Multiview:2",
		"default": -1,
	},
	{
		# Per-eye internal 3D resolution. The compositor-facing texture remains 1968x1134, while
		# Godot renders 3D at this scale and bilinearly upscales it. 0.75 renders 44% fewer pixels.
		# This is also the quality ceiling when xreal/dynamic_render_scale is enabled.
		# `debug.xreal.render_scale=<50..100>` overrides it for device A/B measurements.
		"name": "xreal/render_scale",
		"type": TYPE_FLOAT,
		"hint": PROPERTY_HINT_RANGE,
		"hint_string": "0.5,1.0,0.05",
		"default": 1.0,
	},
	{
		# Adapt the per-eye render scale to sustained frame pressure. This setting is sampled when
		# the XREAL stereo rig is created; restart the scene after changing it.
		"name": "xreal/dynamic_render_scale",
		"type": TYPE_BOOL,
		"default": false,
	},
	{
		# Experimental Vulkan path: render the root XR viewport once with two views through an
		# XRInterfaceExtension, then copy its two array layers into the existing XREAL eye targets.
		# GLES keeps using the established two-SubViewport path.
		"name": "xreal/xr_multiview_poc",
		"type": TYPE_BOOL,
		"default": false,
	},
	{
		# The eye viewports already render the shared 3D world. Keep the host display 2D-only by
		# default to avoid a hidden third scene pass; disable this for a 3D phone mirror.
		"name": "xreal/disable_host_viewport_3d",
		"type": TYPE_BOOL,
		"default": true,
	},
	{
		# Which input sources InitUserDefinedSettings asks the SDK for. "SDK Default" (-1) leaves the
		# `debug.xreal.input_source` property, and the native default of Controller, in charge.
		#
		# Pick a value with Hands only if you actually use hand tracking. The Hands bit makes the SDK
		# call NativePerception::SetHandTrackingEnabled synchronously during input start, measured at
		# ~878 ms of cold start on an X4000 with a One Pro, and hand tracking is Air 2 Ultra only, so on
		# any other headset that is pure startup latency. See docs/plans/startup-latency.md.
		"name": "xreal/input_source",
		"type": TYPE_INT,
		"hint": PROPERTY_HINT_ENUM,
		"hint_string": "SDK Default:-1,Controller:1,Hands:2,Controller And Hands:3",
		"default": -1,
	},
	{
		# Keep the glasses display on while the headset is not worn, bypassing the proximity sensor's
		# auto-off. It is on by default; turn it off to let the display sleep when the glasses come off.
		"name": "xreal/display_bypass_psensor",
		"type": TYPE_BOOL,
		"default": true,
	},
	{
		# Multi-resume: keep the glasses app running live when the phone switches to another app, through
		# the Android manifest `nr_features=multiResume`. It is on by default, and turning it off drops
		# the marker so the app follows the normal Android lifecycle. export_plugin.gd reads it at EXPORT
		# time, not at runtime.
		"name": "xreal/multi_resume",
		"type": TYPE_BOOL,
		"default": true,
	},
	{
		# NRSDK verbose native logging, the Android manifest `autoLog`, emitted as 0 or 1. Off by default.
		# export_plugin.gd reads it at EXPORT time, like xreal/multi_resume.
		"name": "xreal/auto_log",
		"type": TYPE_BOOL,
		"default": false,
	},
	{
		# Periodic-IDR workaround for the FPV stream / mp4 recorder. libmedia_codec 3.1.0 emits only
		# one keyframe at start, so a viewer that joins or reloads mid-stream never gets a decodable
		# starting point; this asks the codec for a sync frame about once a second so late joiners
		# work. ON by default. It reaches the codec through a reverse-engineered object layout, so an
		# SDK update may need it re-checked (see docs/archive/codex-idr-analysis.md); turn it off to
		# fall back to the single-IDR behaviour. `debug.xreal.idr_hack` overrides this at runtime.
		"name": "xreal/idr_workaround",
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
	# Contribute the XREAL Android manifest and library requirements at export time, so the Gradle
	# build template needs no hand-edits and they survive template regeneration.
	_export_plugin = ExportPluginScript.new()
	add_export_plugin(_export_plugin)

	# SDK vendoring dock: pick the com.xreal.xr package, a .tgz, a .tar.gz or an extracted folder, and
	# copy the .so, .aar and host tool into place. It is the in-editor analog of
	# scripts/vendor_xreal_libs.*.
	_vendor_import_dock = VendorImportDockScript.new()
	_vendor_import_dock.name = "XREAL Import"
	add_control_to_dock(EditorPlugin.DOCK_SLOT_LEFT_UR, _vendor_import_dock)

	# Image-tracking DB builder dock. It runs the vendored trackableImageTools to compile the blob,
	# the Godot analog of Unity's XREALImageLibraryBuildProcessor.
	_image_db_dock = ImageDbDockScript.new()
	_image_db_dock.name = "XREAL Image DB"
	add_control_to_dock(EditorPlugin.DOCK_SLOT_LEFT_UR, _image_db_dock)

	# Mesh-snapshot converter dock: a depth-mesh scan saved on the glasses becomes an ArrayMesh or a
	# .glb here, so mesh-consuming work can iterate in the editor. The Godot answer to the SDK's
	# "Use Meshes in the Editor", which exports .obj and drops the semantic classification.
	_mesh_snapshot_dock = MeshSnapshotDockScript.new()
	_mesh_snapshot_dock.name = "XREAL Mesh Snapshot"
	add_control_to_dock(EditorPlugin.DOCK_SLOT_LEFT_UR, _mesh_snapshot_dock)

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
	if _mesh_snapshot_dock:
		remove_control_from_docks(_mesh_snapshot_dock)
		_mesh_snapshot_dock.free()
		_mesh_snapshot_dock = null
