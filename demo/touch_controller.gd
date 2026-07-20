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
## FPV mp4-recording toggle flipped (true = on). Camera ON records the camera+AR blend; OFF the AR view.
signal record_toggled(on: bool)
## First-person-view streaming toggle flipped (true = on).
signal stream_toggled(on: bool)
## Momentary "配置" button — place a spatial anchor at the hand fingertip now.
signal place_pressed()
## Momentary "画像切替" button — cycle the active image-tracking set.
signal image_cycle_pressed()
## Momentary "撮影" button — capture a photo from the RGB camera.
signal capture_pressed()
## Momentary "合成撮影" button — capture a blended camera+AR (mixed-reality) photo.
signal blend_pressed()
## The user confirmed "Yes" on the Exit dialog — the app should quit. (The "Exit" button first shows a
## drawn Yes/No confirmation; this fires only on Yes.) A phone-menu exit for glasses without physical
## keys (the Air 2 Ultra has only an EC-dimming button).
signal exit_confirmed()

## Backdrop fill. Opaque by default so the phone shows only the controller (the glasses-bound
## 3D preview behind it is hidden); set a translucent alpha to let the 3D show through instead.
@export var background_color := Color(0.05, 0.06, 0.09, 1.0)

# Momentary buttons (name -> label). Add/remove/rename here to customize the controller.
const _buttons := {
	"trigger": "TRIGGER",
	"grip": "GRIP",
	"menu": "MENU",
	"hand_l": "◀ L Hand",
	"hand_r": "R Hand ▶",
	"place": "Place",
	"image_cycle": "Cycle Image",
	"capture": "Photo",
	"blend": "Blend Photo",
	"exit": "Exit",
}

# Toggle buttons (name -> label). Unlike the momentary buttons above they hold an on/off state
# (highlighted while on) and fire once on press. Used for the camera / plane / anchor switches;
# main.gd drives the actual XrealSystem calls and can push state back via set_toggle().
const _toggles := {
	"camera": "Camera",
	"plane": "Plane",
	"anchor": "Anchor",
	"image": "Image",
	"mesh": "Mesh",
	"record": "Record",
	"stream": "Stream",
}

# Tabs, grouped by which glasses support each feature (per XREAL's Feature Compatibility table) so the
# right controls are easy to find per device. The touchpad stays visible above; each tab shows only its
# own buttons. Each item must be a key of `_buttons` or `_toggles`.
#   Control — the virtual controller (3DoF): ALL glasses (One Series / Air·Air 2·Air 2 Pro / Air 2 Ultra).
#   Camera  — the RGB camera: XREAL One Series only (Air/Air 2/Air 2 Pro and Air 2 Ultra have no RGB cam).
#   AR      — perception (plane / spatial anchor / image tracking / depth mesh): Air 2 Ultra only.
# Streaming (FPV) and Record (mp4 -> gallery) live in the Camera tab because they cast/record the
# camera+AR blend when the camera is on, but they do NOT require the camera: they feed on our own
# SubViewport (the AR-only view with no camera), so they work on the camera-less Air 2 Ultra too.
const _tabs := [
	{"label": "Control", "items": ["trigger", "grip", "menu", "hand_l", "hand_r", "exit"]},
	{"label": "Camera", "items": ["capture", "blend", "camera", "record", "stream"]},
	{"label": "AR", "items": ["plane", "anchor", "place", "image", "image_cycle", "mesh"]},
]

# Adjacent item pairs (left name first) laid out as one 2-column row instead of two stacked rows, for
# tightly-related controls: hand_l/hand_r (pointer-origin hand), anchor/place (anchor mode + drop-at-
# fingertip), and image/image_cycle (image-tracking mode + set cycle). Both names of a pair must sit
# next to each other, in this order, in the tab's `items`.
const _paired_rows := [["hand_l", "hand_r"], ["anchor", "place"], ["image", "image_cycle"]]

# Layout, filled by _layout() from the current size.
var _pad_rect: Rect2
var _button_rects := {}        # only the active tab's items
var _tab_rects: Array = []     # tab index -> Rect2 (the tab-bar cells)

# Live state.
var _active_tab := 0
var _finger_widget := {}      # touch index -> widget name ("touchpad" / "tab:N" / button name)
var _pressed := {}            # widget name -> bool (momentary press highlight)
var _toggle_on := {}          # toggle name -> bool (persistent on/off)
var _disabled := {}           # button/toggle name -> true (unsupported: drawn greyed, not tappable)
var _label_override := {}     # button name -> label text (overrides the static _buttons label)
var _pad_value := Vector2.ZERO
# Exit confirmation: while true, a drawn Yes/No dialog covers the controller and only its two buttons
# are tappable (see _draw / _input). Their hit-rects are recomputed each _draw.
var _confirm_exit := false
var _yes_rect := Rect2()
var _no_rect := Rect2()

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

## Whether items[i] and items[i+1] form a `_paired_rows` pair (so they share one 2-column row).
func _pair_at(items: Array, i: int) -> bool:
	if i + 1 >= items.size():
		return false
	for pair in _paired_rows:
		if items[i] == pair[0] and items[i + 1] == pair[1]:
			return true
	return false

## Number of stacked rows for `items` — each `_paired_rows` pair collapses two items into one row.
func _row_count(items: Array) -> int:
	var rows := 0
	var i := 0
	while i < items.size():
		i += 2 if _pair_at(items, i) else 1
		rows += 1
	return rows

## Fill `_button_rects` for `items` stacked at column [x, x+w] from `top`, row height `bh`, gap `gap`.
## A `_paired_rows` pair shares one row split into two columns (left name in the left column) so the
## positions match their meaning (e.g. 左手/右手, or Anchor mode + Place).
func _layout_items(items: Array, x: float, top: float, w: float, bh: float, gap: float) -> void:
	var by := top
	var i := 0
	while i < items.size():
		if _pair_at(items, i):
			var g := w * 0.03
			var hw := (w - g) * 0.5
			_button_rects[items[i]] = Rect2(x, by, hw, bh)
			_button_rects[items[i + 1]] = Rect2(x + hw + g, by, hw, bh)
			i += 2
		else:
			_button_rects[items[i]] = Rect2(x, by, w, bh)
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
		if _disabled.has(name):
			continue  # a disabled button is not tappable — treat as not there
		if (_button_rects[name] as Rect2).has_point(pos):
			return name
	return ""

func _input(event: InputEvent) -> void:
	# While the Exit dialog is up it is modal: only its Yes/No buttons respond; all other touches (and
	# drags) are swallowed so the controller behind it can't be operated.
	if _confirm_exit:
		if event is InputEventScreenTouch and event.pressed:
			if _yes_rect.has_point(event.position):
				_confirm_exit = false
				_vibrate(20)
				exit_confirmed.emit()
			elif _no_rect.has_point(event.position):
				_confirm_exit = false
				_vibrate(15)
			queue_redraw()
		if event is InputEventScreenTouch or event is InputEventScreenDrag:
			get_viewport().set_input_as_handled()
		return
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
			"record":
				record_toggled.emit(on)
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
		"image_cycle":
			image_cycle_pressed.emit()
		"capture":
			capture_pressed.emit()
		"blend":
			blend_pressed.emit()
		"exit":
			_confirm_exit = true  # show the Yes/No dialog; actual quit fires on Yes (see _input/_draw)
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
		if _disabled.has(name):
			# Unsupported on this device: flat dark fill, faint border, dim label — reads as inert.
			draw_rect(r, Color(0.15, 0.15, 0.17, 0.5))
			draw_rect(r, Color(1, 1, 1, 0.12), false, 2.0)
			var label := "%s: —" % _toggles[name] if _toggles.has(name) else _button_label(name)
			_draw_label(font, font_size, r, label, Color(1, 1, 1, 0.3),
				r.position.y + (r.size.y + font_size) * 0.5 - font_size * 0.3)
			continue
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
			_draw_label(font, font_size, r, _button_label(name), Color.WHITE,
				r.position.y + (r.size.y + font_size) * 0.5 - font_size * 0.3)

	# Modal "Exit the app?" confirmation, drawn on top of everything (its Yes/No are the only tappable
	# widgets while up — see _input). Recompute the button hit-rects here each frame.
	if _confirm_exit:
		draw_rect(Rect2(Vector2.ZERO, size), Color(0, 0, 0, 0.65))  # dim the controller behind it
		var dw := size.x * 0.82
		var dh := minf(size.y * 0.24, size.x * 0.55)
		var dx := (size.x - dw) * 0.5
		var dy := (size.y - dh) * 0.5
		var dlg := Rect2(dx, dy, dw, dh)
		draw_rect(dlg, Color(0.1, 0.12, 0.16, 0.98))
		draw_rect(dlg, Color(0.6, 0.8, 1.0, 0.9), false, 3.0)
		_draw_label(font, font_size, dlg, "Exit the app?", Color.WHITE, dy + dh * 0.34)
		var bw := dw * 0.4
		var bh := dh * 0.3
		var by := dy + dh - bh - dh * 0.1
		_no_rect = Rect2(dx + dw * 0.06, by, bw, bh)
		_yes_rect = Rect2(dx + dw - bw - dw * 0.06, by, bw, bh)
		draw_rect(_no_rect, Color(0.4, 0.4, 0.45, 0.35))
		draw_rect(_no_rect, Color(1, 1, 1, 0.7), false, 2.0)
		draw_rect(_yes_rect, Color(0.7, 0.25, 0.25, 0.45))
		draw_rect(_yes_rect, Color(1, 0.6, 0.6, 0.95), false, 2.0)
		_draw_label(font, font_size, _no_rect, "No", Color.WHITE,
			_no_rect.position.y + (_no_rect.size.y + font_size) * 0.5 - font_size * 0.3)
		_draw_label(font, font_size, _yes_rect, "Yes", Color.WHITE,
			_yes_rect.position.y + (_yes_rect.size.y + font_size) * 0.5 - font_size * 0.3)

func _draw_label(font: Font, font_size: int, r: Rect2, text: String, color: Color, y: float) -> void:
	# Trim with an ellipsis if the label is wider than the button (draw_string only aligns, it does
	# not clip), so a long image-set name on the narrow "Cycle" button can't spill over its neighbour.
	var t := _fit_text(font, font_size, text, r.size.x - font_size * 0.6)
	draw_string(font, Vector2(r.position.x, y), t, HORIZONTAL_ALIGNMENT_CENTER,
		r.size.x, font_size, color)

## Shorten `text` with a trailing "…" until it fits `max_width` at `font_size` (returns it unchanged
## when it already fits).
func _fit_text(font: Font, font_size: int, text: String, max_width: float) -> String:
	if font.get_string_size(text, HORIZONTAL_ALIGNMENT_LEFT, -1, font_size).x <= max_width:
		return text
	var t := text
	while t.length() > 1 and font.get_string_size(t + "…", HORIZONTAL_ALIGNMENT_LEFT, -1, font_size).x > max_width:
		t = t.substr(0, t.length() - 1)
	return t.strip_edges() + "…"

## Programmatically set a toggle's on/off state without emitting its signal — keeps the UI in
## sync when the app changes it (e.g. reflecting a camera start that failed, or a plane mode the
## device rejected). No-op for unknown names.
func set_toggle(name: String, on: bool) -> void:
	if _toggles.has(name):
		_toggle_on[name] = on
		queue_redraw()

## The displayed label for a momentary button: a runtime override if set, else the static one.
func _button_label(name: String) -> String:
	return str(_label_override.get(name, _buttons.get(name, name)))

## Override a momentary button's label (e.g. show the active image set on the "Cycle Image" button).
## Pass "" to clear the override and restore the static label.
func set_button_label(name: String, text: String) -> void:
	if text.is_empty():
		_label_override.erase(name)
	else:
		_label_override[name] = text
	queue_redraw()

## Enable/disable a button or toggle by name. A disabled control is drawn greyed and inert (taps do
## nothing) — used to reflect a capability the device lacks (no RGB camera, no plane detection, …).
func set_disabled(name: String, disabled: bool) -> void:
	if disabled:
		_disabled[name] = true
	else:
		_disabled.erase(name)
	queue_redraw()
