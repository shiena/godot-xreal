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

func _enter_tree() -> void:
	pass

func _exit_tree() -> void:
	pass
