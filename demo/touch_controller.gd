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
## Image-tracking mode toggle flipped (true = on).
signal image_toggled(on: bool)
## Depth-mesh mode toggle flipped (true = on).
signal mesh_toggled(on: bool)
## First-person-view streaming toggle flipped (true = on).
signal stream_toggled(on: bool)
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
	"image": "画像",
	"mesh": "メッシュ",
	"stream": "配信",
}

# Tabs, grouped by which glasses support each feature (per XREAL's Feature Compatibility table) so the
# right controls are easy to find per device. The touchpad stays visible above; each tab shows only its
# own buttons. Each item must be a key of `_buttons` or `_toggles`.
#   操作  — the virtual controller (3DoF): ALL glasses (One Series / Air·Air 2·Air 2 Pro / Air 2 Ultra).
#   カメラ — the RGB camera: XREAL One Series only (Air/Air 2/Air 2 Pro and Air 2 Ultra have no RGB cam).
#   Air2U — perception (plane / spatial anchor / image tracking / depth mesh): Air 2 Ultra only.
# 配信 (FPV streaming) streams the rendered view, so it works on all glasses → the 操作 tab.
const _tabs := [
	{"label": "操作", "items": ["trigger", "grip", "menu", "hand_l", "hand_r", "stream"]},
	{"label": "カメラ", "items": ["camera"]},
	{"label": "Air2U", "items": ["place", "plane", "anchor", "image", "mesh"]},
]

# Layout, filled by _layout() from the current size.
var _pad_rect: Rect2
var _button_rects := {}        # only the active tab's items
var _tab_rects: Array = []     # tab index -> Rect2 (the tab-bar cells)

# Live state.
var _active_tab := 0
var _finger_widget := {}      # touch index -> widget name ("touchpad" / "tab:N" / button name)
var _pressed := {}            # widget name -> bool (momentary press highlight)
var _toggle_on := {}          # toggle name -> bool (persistent on/off)
var _pad_value := Vector2.ZERO

## The button names shown on the active tab.
func _active_items() -> Array:
	return _tabs[_active_tab]["items"]

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
	var items := _active_items()
	var rows := _row_count(items)
	_button_rects.clear()
	_tab_rects.clear()
	if s.y > s.x:
		# Portrait: touchpad on top, tab bar, then the active tab's buttons stacked below.
		var pad := s.x * 0.86
		_pad_rect = Rect2((s.x - pad) * 0.5, s.y * 0.03, pad, pad)
		var col_x := (s.x - pad) * 0.5
		var tab_top := _pad_rect.end.y + s.y * 0.02
		var tab_h := s.y * 0.055
		_layout_tab_bar(col_x, tab_top, pad, tab_h)
		var gap := s.y * 0.016
		var top := tab_top + tab_h + s.y * 0.02
		var bottom := s.y * 0.98
		# Cap the height so a tab with few buttons keeps normal-sized (top-aligned) buttons instead of
		# one giant one filling the whole area.
		var bh := minf((bottom - top - gap * (rows - 1)) / rows, s.y * 0.09)
		_layout_items(items, col_x, top, pad, bh, gap)
	else:
		# Landscape: touchpad left; tab bar + the active tab's buttons stacked on the right.
		var pad := minf(s.x * 0.42, s.y * 0.72)
		_pad_rect = Rect2(s.x * 0.05, (s.y - pad) * 0.5, pad, pad)
		var bw := s.x * 0.42
		var bx := s.x - bw - s.x * 0.05
		var tab_h := s.y * 0.1
		var tab_top := s.y * 0.05
		_layout_tab_bar(bx, tab_top, bw, tab_h)
		var gap := s.y * 0.035
		var top := tab_top + tab_h + s.y * 0.03
		var bottom := s.y * 0.95
		var bh := minf(s.y * 0.16, (bottom - top - gap * (rows - 1)) / rows)
		_layout_items(items, bx, top, bw, bh, gap)
	queue_redraw()

## Number of stacked rows for `items` — hand_l + hand_r (if both present) share one 2-column row.
func _row_count(items: Array) -> int:
	var rows := items.size()
	if items.has("hand_l") and items.has("hand_r"):
		rows -= 1
	return rows

## Fill `_button_rects` for `items` stacked at column [x, x+w] from `top`, row height `bh`, gap `gap`.
## The adjacent hand_l/hand_r pair shares one row split into two columns — 左手 on the left, 右手 on the
## right — so the button positions match their meaning.
func _layout_items(items: Array, x: float, top: float, w: float, bh: float, gap: float) -> void:
	var by := top
	var i := 0
	while i < items.size():
		var name: String = items[i]
		if name == "hand_l" and i + 1 < items.size() and items[i + 1] == "hand_r":
			var g := w * 0.03
			var hw := (w - g) * 0.5
			_button_rects["hand_l"] = Rect2(x, by, hw, bh)
			_button_rects["hand_r"] = Rect2(x + hw + g, by, hw, bh)
			i += 2
		else:
			_button_rects[name] = Rect2(x, by, w, bh)
			i += 1
		by += bh + gap

## Lay out the tab-bar cells across [x, x+w] at (y, height).
func _layout_tab_bar(x: float, y: float, w: float, h: float) -> void:
	var count := _tabs.size()
	var cw := w / count
	for i in count:
		_tab_rects.append(Rect2(x + i * cw, y, cw, h))

func _widget_at(pos: Vector2) -> String:
	if _pad_rect.has_point(pos):
		return "touchpad"
	for i in _tab_rects.size():
		if (_tab_rects[i] as Rect2).has_point(pos):
			return "tab:%d" % i
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
	if widget.begins_with("tab:"):
		var idx := int(widget.substr(4))
		if idx != _active_tab:
			_active_tab = idx
			_vibrate(15)
			_layout()  # recompute the button rects for the newly-active tab
		return
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
			"image":
				image_toggled.emit(on)
			"mesh":
				mesh_toggled.emit(on)
			"stream":
				stream_toggled.emit(on)
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

	# Tab bar: the active tab is highlighted; the others read as dim.
	var tab_font_size := int(maxf(20.0, font_size * 0.9))
	for i in _tab_rects.size():
		var tr: Rect2 = _tab_rects[i]
		var active := i == _active_tab
		draw_rect(tr, Color(0.3, 0.55, 0.9, 0.5) if active else Color(0.4, 0.4, 0.45, 0.18))
		draw_rect(tr, Color(0.6, 0.8, 1.0, 0.95) if active else Color(1, 1, 1, 0.35), false, 3.0)
		_draw_label(font, tab_font_size, tr, _tabs[i]["label"],
			Color.WHITE if active else Color(1, 1, 1, 0.6),
			tr.position.y + (tr.size.y + tab_font_size) * 0.5 - tab_font_size * 0.3)

	# The active tab's buttons: momentary vs toggle drawn differently.
	for name in _active_items():
		var r: Rect2 = _button_rects[name]
		if _toggles.has(name):
			# Toggle: green while on, gray while off; a lighter flash while the finger is down.
			var on := bool(_toggle_on.get(name, false))
			var fill := Color(0.2, 0.7, 0.4, 0.45) if on else Color(0.5, 0.5, 0.5, 0.16)
			if _pressed.has(name):
				fill.a += 0.15
			draw_rect(r, fill)
			draw_rect(r, Color(0.5, 1.0, 0.7, 0.95) if on else Color(1, 1, 1, 0.6), false, 3.0)
			_draw_label(font, font_size, r, "%s: %s" % [_toggles[name], "ON" if on else "OFF"],
				Color.WHITE, r.position.y + (r.size.y + font_size) * 0.5 - font_size * 0.3)
		else:
			var on := _pressed.has(name)
			draw_rect(r, Color(0.9, 0.9, 0.9, 0.28) if on else Color(0.5, 0.5, 0.5, 0.16))
			draw_rect(r, Color(1, 1, 1, 0.85), false, 3.0)
			_draw_label(font, font_size, r, _buttons[name], Color.WHITE,
				r.position.y + (r.size.y + font_size) * 0.5 - font_size * 0.3)

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
