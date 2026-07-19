extends Node3D
## Root script of the static AR scene (demo/ar_scene.tscn): environment, sun and the ring of
## colored boxes need no code at all; this script only exposes the two nodes that main.gd
## drives at runtime, so the consumer never reaches into the scene's internals by path:
##   - cursor        head-locked touch-controller cursor — reparented under the head tracker
##   - phone_pointer phone-IMU 3D pointer — revealed once the NRController starts
## (The camera preview quad now lives in the addon: addons/godot_xreal/features/xreal_camera.tscn.)

@onready var cursor: MeshInstance3D = $Cursor
@onready var phone_pointer: Node3D = $PhonePointer

func _ready() -> void:
	_color_room_boxes()

## The ring box colors depend on the box COUNT (evenly spread hues), so they can't be baked
## into the .tscn: add or remove boxes under $Room in the editor and the ring recolors itself.
## Each box gets its OWN material — phone_pointer.gd mutates it per box (hover emission,
## select recolor), so sharing one material would highlight the whole ring at once.
func _color_room_boxes() -> void:
	var boxes := $Room.get_children()
	for i in boxes.size():
		var box := boxes[i] as MeshInstance3D
		if box == null:
			continue
		var material := StandardMaterial3D.new()
		material.albedo_color = Color.from_hsv(float(i) / float(boxes.size()), 0.7, 0.9)
		box.material_override = material
