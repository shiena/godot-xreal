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
var _export_plugin: EditorExportPlugin

func _enter_tree() -> void:
	# Contribute the XREAL Android manifest/library requirements at export time so the Gradle
	# build template needs no hand-edits (they survive template regeneration).
	_export_plugin = ExportPluginScript.new()
	add_export_plugin(_export_plugin)

func _exit_tree() -> void:
	if _export_plugin:
		remove_export_plugin(_export_plugin)
		_export_plugin = null
