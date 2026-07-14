extends CanvasLayer
## Root script of the phone-screen scene (demo/phone_screen.tscn): hosts the on-screen touch
## controller (touch_controller.gd) and re-emits its signals, so consumers connect to this
## scene's root and never reach into its internals. The layer renders only on the phone's root
## viewport — the glasses eye SubViewports never render CanvasLayers — so the glasses show the
## 3D world while the phone shows the controller. layer 0 keeps it below the $UI debug layer.

## Emitted when the trigger button goes down (true) / up (false).
signal trigger_changed(pressed: bool)
## Emitted when the grip button goes down (true) / up (false).
signal grip_changed(pressed: bool)
## Emitted once when the (momentary) menu button is pressed.
signal menu_pressed()
## Touchpad position while a finger drags it, normalized to [-1, 1] with +y up.
signal touchpad_moved(value: Vector2)
## Emitted when the touchpad finger lifts (value returns to zero).
signal touchpad_released()
## Right/left hand toggle for the 3D pointer beam origin (true = right hand).
signal hand_selected(is_right: bool)

func _ready() -> void:
	var c: Control = $TouchController
	c.trigger_changed.connect(trigger_changed.emit)
	c.grip_changed.connect(grip_changed.emit)
	c.menu_pressed.connect(menu_pressed.emit)
	c.touchpad_moved.connect(touchpad_moved.emit)
	c.touchpad_released.connect(touchpad_released.emit)
	c.hand_selected.connect(hand_selected.emit)
