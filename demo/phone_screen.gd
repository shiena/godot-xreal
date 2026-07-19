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
## Camera preview toggle flipped (true = on).
signal camera_toggled(on: bool)
## Plane-detection toggle flipped (true = on).
signal plane_toggled(on: bool)
## Spatial-anchor mode toggle flipped (true = on).
signal anchor_toggled(on: bool)
## Image-tracking mode toggle flipped (true = on).
signal image_toggled(on: bool)
## Depth-mesh mode toggle flipped (true = on).
signal mesh_toggled(on: bool)
## First-person-view streaming toggle flipped (true = on).
signal stream_toggled(on: bool)
## Momentary "配置" button — place a spatial anchor at the hand fingertip.
signal place_pressed()
## Momentary "画像切替" button — cycle the active image-tracking set.
signal image_cycle_pressed()
## Momentary "撮影" button — capture a photo from the RGB camera.
signal capture_pressed()
## Momentary "合成撮影" button — capture a blended camera+AR (mixed-reality) photo.
signal blend_pressed()
## The user confirmed "Yes" on the phone-menu Exit dialog — quit the app.
signal exit_confirmed()

func _ready() -> void:
	var c: Control = $TouchController
	c.trigger_changed.connect(trigger_changed.emit)
	c.grip_changed.connect(grip_changed.emit)
	c.menu_pressed.connect(menu_pressed.emit)
	c.touchpad_moved.connect(touchpad_moved.emit)
	c.touchpad_released.connect(touchpad_released.emit)
	c.hand_selected.connect(hand_selected.emit)
	c.camera_toggled.connect(camera_toggled.emit)
	c.plane_toggled.connect(plane_toggled.emit)
	c.anchor_toggled.connect(anchor_toggled.emit)
	c.image_toggled.connect(image_toggled.emit)
	c.mesh_toggled.connect(mesh_toggled.emit)
	c.stream_toggled.connect(stream_toggled.emit)
	c.place_pressed.connect(place_pressed.emit)
	c.image_cycle_pressed.connect(image_cycle_pressed.emit)
	c.capture_pressed.connect(capture_pressed.emit)
	c.blend_pressed.connect(blend_pressed.emit)
	c.exit_confirmed.connect(exit_confirmed.emit)

## Forward a programmatic toggle-state change to the touch controller (see its set_toggle).
func set_toggle(name: String, on: bool) -> void:
	($TouchController as Control).set_toggle(name, on)

## Forward an enable/disable to the touch controller (greys out unsupported controls; see set_disabled).
func set_disabled(name: String, disabled: bool) -> void:
	($TouchController as Control).set_disabled(name, disabled)

## Forward a runtime button-label override to the touch controller (see set_button_label).
func set_button_label(name: String, text: String) -> void:
	($TouchController as Control).set_button_label(name, text)
