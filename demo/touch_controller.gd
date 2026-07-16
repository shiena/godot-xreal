extends Control
## On-screen touch controller for the phone screen — the Godot analog of the XREAL SDK's
## `XREALVirtualController` prefab (a customizable phone-side controller).
##
## It draws a touchpad + buttons and turns finger input into signals, with phone-vibration
## haptics. It lives on the phone's root viewport only, so it never reaches the glasses eye
## SubViewports — the glasses show the 3D world while the phone shows this controller.
##
## Pure GDScript / touch input — no native interop. Customize the layout by editing the
## `_buttons` list and the rects computed in `_layout()` (the prefab-editing equivalent).

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
## Camera preview toggle flipped (true = on). Independent of the plane toggle.
signal camera_toggled(on: bool)
## Plane-detection toggle flipped (true = on). Independent of the camera toggle.
signal plane_toggled(on: bool)
## Spatial-anchor mode toggle flipped (true = on).
signal anchor_toggled(on: bool)
## Momentary "配置" button — place a spatial anchor at the hand fingertip now.
signal place_pressed()

## Backdrop fill. Opaque by default so the phone shows only the controller (the glasses-bound
## 3D preview behind it is hidden); set a translucent alpha to let the 3D show through instead.
@export var background_color := Color(0.05, 0.06, 0.09, 1.0)

# Momentary buttons (name -> label). Add/remove/rename here to customize the controller.
const _buttons := {
	"trigger": "TRIGGER",
	"grip": "GRIP",
	"menu": "MENU",
	"hand_l": "◀ 左手",
	"hand_r": "右手 ▶",
	"place": "配置",
}

# Toggle buttons (name -> label). Unlike the momentary buttons above they hold an on/off state
# (highlighted while on) and fire once on press. Used for the camera / plane / anchor switches;
# main.gd drives the actual XrealSystem calls and can push state back via set_toggle().
const _toggles := {
	"camera": "カメラ",
	"plane": "平面検出",
	"anchor": "アンカー",
}

# Layout, filled by _layout() from the current size.
var _pad_rect: Rect2
var _button_rects := {}

# Live state.
var _finger_widget := {}      # touch index -> widget name ("touchpad" / button name)
var _pressed := {}            # widget name -> bool (momentary press highlight)
var _toggle_on := {}          # toggle name -> bool (persistent on/off)
var _pad_value := Vector2.ZERO

func _ready() -> void:
	# The app renders portrait natively (project.godot display/window/handheld/orientation), so the
	# full-rect control is already tall — `_layout` picks the portrait arrangement from the aspect.
	set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	# Node-level _input handles the multitouch; don't intercept GUI focus/mouse from other UI.
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	resized.connect(_layout)
	_layout()

func _layout() -> void:
	var s := size
	# Momentary buttons first, then the toggles, in one stacked column.
	var names: Array = _buttons.keys() + _toggles.keys()
	var n := names.size()
	_button_rects.clear()
	if s.y > s.x:
		# Portrait: touchpad on top, buttons stacked below.
		var pad := s.x * 0.9
		_pad_rect = Rect2((s.x - pad) * 0.5, s.y * 0.04, pad, pad)
		var gap := s.y * 0.018
		var top := _pad_rect.end.y + s.y * 0.03
		var bottom := s.y * 0.97
		var bh := (bottom - top - gap * (n - 1)) / n
		var bw := s.x * 0.82
		var bx := (s.x - bw) * 0.5
		var by := top
		for name in names:
			_button_rects[name] = Rect2(bx, by, bw, bh)
			by += bh + gap
	else:
		# Landscape: touchpad left, buttons stacked right.
		var pad := minf(s.x * 0.42, s.y * 0.72)
		_pad_rect = Rect2(s.x * 0.05, (s.y - pad) * 0.5, pad, pad)
		var bw := s.x * 0.24
		var bx := s.x - bw - s.x * 0.05
		var gap := s.y * 0.04
		var bh := minf(s.y * 0.16, (s.y * 0.94 - gap * (n - 1)) / n)
		var by := (s.y - (bh * n + gap * (n - 1))) * 0.5
		for name in names:
			_button_rects[name] = Rect2(bx, by, bw, bh)
			by += bh + gap
	queue_redraw()

func _widget_at(pos: Vector2) -> String:
	if _pad_rect.has_point(pos):
		return "touchpad"
	for name in _button_rects:
		if (_button_rects[name] as Rect2).has_point(pos):
			return name
	return ""

func _input(event: InputEvent) -> void:
	if event is InputEventScreenTouch:
		if event.pressed:
			var w := _widget_at(event.position)
			if w != "":
				_finger_widget[event.index] = w
				_press(w, event.position)
				get_viewport().set_input_as_handled()
		elif _finger_widget.has(event.index):
			_release(_finger_widget[event.index])
			_finger_widget.erase(event.index)
			get_viewport().set_input_as_handled()
	elif event is InputEventScreenDrag:
		if _finger_widget.get(event.index, "") == "touchpad":
			_update_pad(event.position)
			get_viewport().set_input_as_handled()

func _press(widget: String, pos: Vector2) -> void:
	_pressed[widget] = true
	_vibrate(20)
	if _toggles.has(widget):
		# Toggle: flip persistent state on press, fire the matching signal.
		var on := not bool(_toggle_on.get(widget, false))
		_toggle_on[widget] = on
		match widget:
			"camera":
				camera_toggled.emit(on)
			"plane":
				plane_toggled.emit(on)
			"anchor":
				anchor_toggled.emit(on)
		queue_redraw()
		return
	match widget:
		"touchpad":
			_update_pad(pos)
		"trigger":
			trigger_changed.emit(true)
		"grip":
			grip_changed.emit(true)
		"menu":
			menu_pressed.emit()
		"hand_l":
			hand_selected.emit(false)
		"hand_r":
			hand_selected.emit(true)
		"place":
			place_pressed.emit()
	queue_redraw()

func _release(widget: String) -> void:
	_pressed.erase(widget)
	match widget:
		"touchpad":
			_pad_value = Vector2.ZERO
			touchpad_released.emit()
		"trigger":
			trigger_changed.emit(false)
		"grip":
			grip_changed.emit(false)
	queue_redraw()

func _update_pad(pos: Vector2) -> void:
	# Normalize to [-1, 1] within the pad, +y up (screen y is down), clamped to the circle.
	var n := (pos - _pad_rect.position) / _pad_rect.size * 2.0 - Vector2.ONE
	n.y = -n.y
	if n.length() > 1.0:
		n = n.normalized()
	_pad_value = n
	touchpad_moved.emit(n)
	queue_redraw()

func _vibrate(ms: int) -> void:
	if OS.has_feature("android"):
		Input.vibrate_handheld(ms)

func _draw() -> void:
	var font := get_theme_default_font()
	var font_size := int(maxf(24.0, minf(size.x, size.y) * 0.045))

	# Backdrop: opaque by default, so the phone shows only the controller and the 3D preview
	# behind it is hidden (set a translucent background_color to let the 3D show through).
	draw_rect(Rect2(Vector2.ZERO, size), background_color)

	# Touchpad: filled panel + border, center crosshair, and the live thumb dot.
	var pad_on := _pressed.has("touchpad")
	draw_rect(_pad_rect, Color(0.25, 0.55, 0.9, 0.18 if pad_on else 0.10))
	draw_rect(_pad_rect, Color(0.5, 0.75, 1.0, 0.9), false, 3.0)
	var center := _pad_rect.position + _pad_rect.size * 0.5
	draw_line(center - Vector2(12, 0), center + Vector2(12, 0), Color(1, 1, 1, 0.25), 2.0)
	draw_line(center - Vector2(0, 12), center + Vector2(0, 12), Color(1, 1, 1, 0.25), 2.0)
	var dot := center + Vector2(_pad_value.x, -_pad_value.y) * _pad_rect.size * 0.5 * 0.92
	draw_circle(dot, _pad_rect.size.x * 0.09, Color(0.4, 0.85, 1.0, 0.95))
	_draw_label(font, font_size, _pad_rect, "TOUCHPAD", Color(1, 1, 1, 0.5),
		_pad_rect.position.y + _pad_rect.size.y - font_size * 1.2)

	# Momentary buttons.
	for name in _buttons:
		var r: Rect2 = _button_rects[name]
		var on := _pressed.has(name)
		draw_rect(r, Color(0.9, 0.9, 0.9, 0.28) if on else Color(0.5, 0.5, 0.5, 0.16))
		draw_rect(r, Color(1, 1, 1, 0.85), false, 3.0)
		_draw_label(font, font_size, r, _buttons[name], Color.WHITE,
			r.position.y + (r.size.y + font_size) * 0.5 - font_size * 0.3)

	# Toggle buttons: green while on, gray while off; a lighter flash while the finger is down.
	for name in _toggles:
		var r: Rect2 = _button_rects[name]
		var on := bool(_toggle_on.get(name, false))
		var fill := Color(0.2, 0.7, 0.4, 0.45) if on else Color(0.5, 0.5, 0.5, 0.16)
		if _pressed.has(name):
			fill.a += 0.15
		draw_rect(r, fill)
		draw_rect(r, Color(0.5, 1.0, 0.7, 0.95) if on else Color(1, 1, 1, 0.6), false, 3.0)
		_draw_label(font, font_size, r, "%s: %s" % [_toggles[name], "ON" if on else "OFF"],
			Color.WHITE, r.position.y + (r.size.y + font_size) * 0.5 - font_size * 0.3)

func _draw_label(font: Font, font_size: int, r: Rect2, text: String, color: Color, y: float) -> void:
	draw_string(font, Vector2(r.position.x, y), text, HORIZONTAL_ALIGNMENT_CENTER,
		r.size.x, font_size, color)

## Programmatically set a toggle's on/off state without emitting its signal — keeps the UI in
## sync when the app changes it (e.g. reflecting a camera start that failed, or a plane mode the
## device rejected). No-op for unknown names.
func set_toggle(name: String, on: bool) -> void:
	if _toggles.has(name):
		_toggle_on[name] = on
		queue_redraw()
