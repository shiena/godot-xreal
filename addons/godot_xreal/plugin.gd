@tool
extends EditorPlugin

## Godot XREAL addon.
##
## The runtime classes are provided by the `godot_xreal` GDExtension and are always
## available once the extension is loaded (regardless of this plugin's enabled state):
##
##   - XrealHeadTracker (Node3D) — drives its rotation from the native 3DoF head pose.
##     Parent a Camera3D under it (see addons/godot_xreal/xreal_rig.tscn).
##   - XrealSystem (RefCounted) — read-only SDK info (availability, session, version,
##     device type).
##
## This EditorPlugin exists so the addon can be toggled from
## Project > Project Settings > Plugins and to host future editor integration.

const ExportPluginScript := preload("res://addons/godot_xreal/export_plugin.gd")
const ImageDbDockScript := preload("res://addons/godot_xreal/editor/image_db_dock.gd")
var _export_plugin: EditorExportPlugin
var _image_db_dock: Control

func _enter_tree() -> void:
	# Contribute the XREAL Android manifest/library requirements at export time so the Gradle
	# build template needs no hand-edits (they survive template regeneration).
	_export_plugin = ExportPluginScript.new()
	add_export_plugin(_export_plugin)

	# Image-tracking DB builder dock (runs the vendored trackableImageTools to compile the blob —
	# the Godot analog of Unity's XREALImageLibraryBuildProcessor).
	_image_db_dock = ImageDbDockScript.new()
	_image_db_dock.name = "XREAL 画像DB"
	add_control_to_dock(EditorPlugin.DOCK_SLOT_LEFT_UR, _image_db_dock)

func _exit_tree() -> void:
	if _export_plugin:
		remove_export_plugin(_export_plugin)
		_export_plugin = null
	if _image_db_dock:
		remove_control_from_docks(_image_db_dock)
		_image_db_dock.free()
		_image_db_dock = null
