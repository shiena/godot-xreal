extends Control
## On-screen touch controller for the phone screen — the Godot analog of the XREAL SDK's
## `XREALVirtualController` prefab (a customizable phone-side controller).
##
## Built from standard Control nodes (Button / containers), created in _ready from the
## `_buttons` / `_toggles` / `_tabs` data tables below (the prefab-editing equivalent: edit the
## lists to customize). Multitouch needs no custom routing: since Godot 4.7 each BaseButton
## tracks its own touch index and the Viewport routes each touch's drags to the Control that
## claimed it, so holding TRIGGER while dragging the touchpad just works. Only the touchpad is
## a custom widget (the `Touchpad` class at the bottom) — there is no built-in analog-pad Control.
##
## It lives on the phone's root viewport only, so it never reaches the glasses eye
## SubViewports — the glasses show the 3D world while the phone shows this controller.
## Portrait-only layout (the project locks the handheld orientation to portrait).

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
## Yes/No confirmation overlay; this fires only on Yes.) A phone-menu exit for glasses without physical
## keys (the Air 2 Ultra has only an EC-dimming button).
signal exit_confirmed()

## Backdrop fill. Opaque by default so the phone shows only the controller (the glasses-bound
## 3D preview behind it is hidden); set a translucent alpha to let the 3D show through instead.
@export var background_color := Color(0.05, 0.06, 0.09, 1.0):
	set(value):
		background_color = value
		queue_redraw()

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

var _theme: Theme
var _controls := {}                    # button/toggle name -> its Button node
var _pages: Array[VBoxContainer] = []  # one per tab, only the active one visible
var _tab_buttons: Array[Button] = []   # the tab-bar radio Buttons
var _pair_boxes: Array[HBoxContainer] = []
var _active_tab := 0
# trigger/grip currently held down. Needed because hiding a pressed Button (tab switch) resets its
# state WITHOUT emitting button_up — _show_page releases held ones explicitly so the consumer never
# sees a stuck-held trigger.
var _held := {}
var _margin: MarginContainer
var _column: VBoxContainer
var _pad: Touchpad
var _tab_row: HBoxContainer
var _overlay: Control                  # modal Exit confirmation (full-rect, swallows all touches)
var _dialog: PanelContainer
var _yes_button: Button
var _no_button: Button

func _ready() -> void:
	set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	# The root is the opaque backdrop: STOP so touches that miss every widget are consumed here.
	# The $UI debug CanvasLayer (layer 1) is processed before this layer 0, so it is unaffected.
	mouse_filter = Control.MOUSE_FILTER_STOP
	_theme = _build_theme()
	theme = _theme
	_build_ui()
	_build_exit_overlay()
	resized.connect(_apply_metrics)
	_apply_metrics()
	_show_page(0)

## Backdrop only — everything else is real Control nodes drawn by themselves.
func _draw() -> void:
	draw_rect(Rect2(Vector2.ZERO, size), background_color)

# ---------------------------------------------------------------- UI construction ---

## Build the node tree (portrait): Margin > Column [ Touchpad / tab row / one page per tab ].
func _build_ui() -> void:
	_margin = MarginContainer.new()
	_margin.name = "Margin"
	_margin.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	_margin.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(_margin)
	_column = VBoxContainer.new()
	_column.name = "Column"
	_column.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_margin.add_child(_column)

	_pad = Touchpad.new()
	_pad.name = "Pad"
	_pad.size_flags_horizontal = Control.SIZE_SHRINK_CENTER
	_pad.pad_pressed.connect(_vibrate.bind(20))
	_pad.moved.connect(touchpad_moved.emit)
	_pad.released.connect(touchpad_released.emit)
	_column.add_child(_pad)

	# Tab bar: a row of equal-width radio Buttons (a shared ButtonGroup). TabBar is not used —
	# it cannot stretch its tabs evenly, which the 1/3-width touch targets need.
	_tab_row = HBoxContainer.new()
	_tab_row.name = "TabRow"
	_tab_row.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_column.add_child(_tab_row)
	var group := ButtonGroup.new()
	for i in _tabs.size():
		var tab := Button.new()
		tab.name = "Tab%d" % i
		tab.theme_type_variation = &"TabButton"
		tab.toggle_mode = true
		tab.button_group = group
		tab.action_mode = BaseButton.ACTION_MODE_BUTTON_PRESS
		tab.focus_mode = Control.FOCUS_NONE
		tab.text = _tabs[i]["label"]
		tab.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		tab.toggled.connect(_on_tab_toggled.bind(i))
		_tab_row.add_child(tab)
		_tab_buttons.append(tab)
	_tab_buttons[0].set_pressed_no_signal(true)

	# One page (button column) per tab; `_paired_rows` pairs share one 2-column row.
	for tab_def in _tabs:
		var page := VBoxContainer.new()
		page.name = "Page%s" % tab_def["label"]
		page.mouse_filter = Control.MOUSE_FILTER_IGNORE
		var items: Array = tab_def["items"]
		var i := 0
		while i < items.size():
			if _pair_at(items, i):
				var row := HBoxContainer.new()
				row.mouse_filter = Control.MOUSE_FILTER_IGNORE
				for pair_name in [items[i], items[i + 1]]:
					var paired := _make_button(pair_name)
					paired.size_flags_horizontal = Control.SIZE_EXPAND_FILL
					row.add_child(paired)
				_pair_boxes.append(row)
				page.add_child(row)
				i += 2
			else:
				page.add_child(_make_button(items[i]))
				i += 1
		_column.add_child(page)
		_pages.append(page)

## One control Button (momentary or toggle) wired to its signal. Kept in `_controls` by name.
func _make_button(control_name: String) -> Button:
	var btn := Button.new()
	btn.name = control_name
	btn.focus_mode = Control.FOCUS_NONE
	btn.clip_text = true
	btn.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	btn.button_down.connect(_vibrate.bind(20))
	_controls[control_name] = btn
	if _toggles.has(control_name):
		btn.theme_type_variation = &"ToggleButton"
		btn.toggle_mode = true
		# Flip on finger-down (the drawn version's behavior), not on release.
		# NB: this relies on input_devices/pointing/emulate_mouse_from_touch=false (project.godot).
		# With it on (the default), Godot 4.7.1's BaseButton processes BOTH the real touch and the
		# synthesized mouse event for one tap, so a PRESS-mode toggle flips twice (on->off) per tap.
		# (master fixed it with an early `device == EMULATION` return in BaseButton::gui_input.)
		btn.action_mode = BaseButton.ACTION_MODE_BUTTON_PRESS
		btn.toggled.connect(_on_toggled.bind(control_name))
		_update_toggle_label(control_name)
	else:
		btn.theme_type_variation = &"MomentaryButton"
		btn.text = _buttons[control_name]
		btn.button_down.connect(_on_momentary_down.bind(control_name))
		if control_name == "trigger" or control_name == "grip":
			btn.button_up.connect(_on_hold_up.bind(control_name))
	return btn

## Modal Exit confirmation: a full-rect STOP overlay (all touches land here while visible, so the
## controller behind it is inoperable) with a centered Yes/No panel of real Buttons.
func _build_exit_overlay() -> void:
	_overlay = Control.new()
	_overlay.name = "ExitOverlay"
	_overlay.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	_overlay.mouse_filter = Control.MOUSE_FILTER_STOP
	_overlay.visible = false
	add_child(_overlay)
	var dim := ColorRect.new()
	dim.color = Color(0, 0, 0, 0.65)
	dim.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	dim.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_overlay.add_child(dim)
	var center := CenterContainer.new()
	center.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	center.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_overlay.add_child(center)
	_dialog = PanelContainer.new()
	_dialog.theme_type_variation = &"DialogPanel"
	_dialog.mouse_filter = Control.MOUSE_FILTER_STOP
	center.add_child(_dialog)
	var inner := MarginContainer.new()
	inner.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_dialog.add_child(inner)
	var vbox := VBoxContainer.new()
	vbox.mouse_filter = Control.MOUSE_FILTER_IGNORE
	inner.add_child(vbox)
	var label := Label.new()
	label.text = "Exit the app?"
	label.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	label.vertical_alignment = VERTICAL_ALIGNMENT_CENTER
	label.size_flags_vertical = Control.SIZE_EXPAND_FILL
	label.mouse_filter = Control.MOUSE_FILTER_IGNORE
	vbox.add_child(label)
	var row := HBoxContainer.new()
	row.mouse_filter = Control.MOUSE_FILTER_IGNORE
	row.alignment = BoxContainer.ALIGNMENT_CENTER
	vbox.add_child(row)
	_no_button = _make_dialog_button("No", &"MomentaryButton")
	_no_button.button_down.connect(func() -> void:
		_overlay.hide()
		_vibrate(15))
	row.add_child(_no_button)
	_yes_button = _make_dialog_button("Yes", &"DangerButton")
	_yes_button.button_down.connect(func() -> void:
		_overlay.hide()
		_vibrate(20)
		exit_confirmed.emit())
	row.add_child(_yes_button)

func _make_dialog_button(text: String, variation: StringName) -> Button:
	var btn := Button.new()
	btn.text = text
	btn.theme_type_variation = variation
	btn.focus_mode = Control.FOCUS_NONE
	btn.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	return btn

# ---------------------------------------------------------------- theme / metrics ---

func _flat(fill: Color, border: Color, border_width: int) -> StyleBoxFlat:
	var sb := StyleBoxFlat.new()
	sb.bg_color = fill
	sb.border_color = border
	sb.set_border_width_all(border_width)
	return sb

## The controller's look (the drawn version's colors), as Button theme variations:
## "MomentaryButton" (gray, flash while down), "ToggleButton" (gray OFF / green ON),
## "TabButton" (dim / active blue), "DangerButton" (the red Yes) + "DialogPanel".
func _build_theme() -> Theme:
	var t := Theme.new()
	var disabled_style := _flat(Color(0.15, 0.15, 0.17, 0.5), Color(1, 1, 1, 0.12), 2)
	for variation in [&"MomentaryButton", &"ToggleButton", &"TabButton", &"DangerButton"]:
		t.set_type_variation(variation, &"Button")
		t.set_stylebox(&"focus", variation, StyleBoxEmpty.new())
		t.set_stylebox(&"disabled", variation, disabled_style)
		for color_name in [&"font_color", &"font_hover_color", &"font_pressed_color",
				&"font_hover_pressed_color", &"font_focus_color"]:
			t.set_color(color_name, variation, Color.WHITE)
		t.set_color(&"font_disabled_color", variation, Color(1, 1, 1, 0.3))
	var momentary := _flat(Color(0.5, 0.5, 0.5, 0.16), Color(1, 1, 1, 0.85), 3)
	var momentary_down := _flat(Color(0.9, 0.9, 0.9, 0.28), Color(1, 1, 1, 0.85), 3)
	t.set_stylebox(&"normal", &"MomentaryButton", momentary)
	t.set_stylebox(&"hover", &"MomentaryButton", momentary)
	t.set_stylebox(&"pressed", &"MomentaryButton", momentary_down)
	t.set_stylebox(&"hover_pressed", &"MomentaryButton", momentary_down)
	var toggle_off := _flat(Color(0.5, 0.5, 0.5, 0.16), Color(1, 1, 1, 0.6), 3)
	var toggle_on := _flat(Color(0.2, 0.7, 0.4, 0.45), Color(0.5, 1.0, 0.7, 0.95), 3)
	# hover_pressed slightly brighter: the drawn version's press flash while the finger is down.
	var toggle_on_down := _flat(Color(0.2, 0.7, 0.4, 0.6), Color(0.5, 1.0, 0.7, 0.95), 3)
	t.set_stylebox(&"normal", &"ToggleButton", toggle_off)
	t.set_stylebox(&"hover", &"ToggleButton", toggle_off)
	t.set_stylebox(&"pressed", &"ToggleButton", toggle_on)
	t.set_stylebox(&"hover_pressed", &"ToggleButton", toggle_on_down)
	var tab_idle := _flat(Color(0.4, 0.4, 0.45, 0.18), Color(1, 1, 1, 0.35), 3)
	var tab_active := _flat(Color(0.3, 0.55, 0.9, 0.5), Color(0.6, 0.8, 1.0, 0.95), 3)
	t.set_stylebox(&"normal", &"TabButton", tab_idle)
	t.set_stylebox(&"hover", &"TabButton", tab_idle)
	t.set_stylebox(&"pressed", &"TabButton", tab_active)
	t.set_stylebox(&"hover_pressed", &"TabButton", tab_active)
	t.set_color(&"font_color", &"TabButton", Color(1, 1, 1, 0.6))
	t.set_color(&"font_hover_color", &"TabButton", Color(1, 1, 1, 0.6))
	var danger := _flat(Color(0.7, 0.25, 0.25, 0.45), Color(1, 0.6, 0.6, 0.95), 2)
	var danger_down := _flat(Color(0.7, 0.25, 0.25, 0.6), Color(1, 0.6, 0.6, 0.95), 2)
	t.set_stylebox(&"normal", &"DangerButton", danger)
	t.set_stylebox(&"hover", &"DangerButton", danger)
	t.set_stylebox(&"pressed", &"DangerButton", danger_down)
	t.set_stylebox(&"hover_pressed", &"DangerButton", danger_down)
	t.set_type_variation(&"DialogPanel", &"PanelContainer")
	t.set_stylebox(&"panel", &"DialogPanel", _flat(Color(0.1, 0.12, 0.16, 0.98), Color(0.6, 0.8, 1.0, 0.9), 3))
	t.set_color(&"font_color", &"Label", Color.WHITE)
	return t

## All screen-proportional metrics in one place, re-applied on every resize: font sizes (via the
## Theme, so one call restyles every Button), the square touchpad, margins/gaps, and the row
## height clamp so the tallest tab still fits (no stretch settings — the app runs at native
## resolution, so everything scales off the control's size like the drawn version did).
func _apply_metrics() -> void:
	var s := size
	if s.x <= 0.0 or s.y <= 0.0:
		return
	var base := int(maxf(24.0, minf(s.x, s.y) * 0.045))
	_theme.set_font_size(&"font_size", &"Button", base)
	_theme.set_font_size(&"font_size", &"TabButton", int(maxf(20.0, base * 0.9)))
	_theme.set_font_size(&"font_size", &"Label", base)
	var pad := s.x * 0.86
	_pad.custom_minimum_size = Vector2(pad, pad)
	_pad.label_font_size = base
	_margin.add_theme_constant_override(&"margin_left", int((s.x - pad) * 0.5))
	_margin.add_theme_constant_override(&"margin_right", int((s.x - pad) * 0.5))
	_margin.add_theme_constant_override(&"margin_top", int(s.y * 0.03))
	_margin.add_theme_constant_override(&"margin_bottom", int(s.y * 0.02))
	_column.add_theme_constant_override(&"separation", int(s.y * 0.02))
	var gap := int(s.y * 0.016)
	for page in _pages:
		page.add_theme_constant_override(&"separation", gap)
	for row in _pair_boxes:
		row.add_theme_constant_override(&"separation", int(pad * 0.03))
	var tab_h := s.y * 0.055
	for tab in _tab_buttons:
		tab.custom_minimum_size = Vector2(0, tab_h)
	# Row height: fit the tallest page into the space under the tab bar, capped at 9% of the
	# screen so a sparse tab keeps normal-sized buttons instead of giant ones (the drawn
	# version's clamp — needed on tall 20:9 phones where 5 fixed-height rows would overflow).
	var rows := 0
	for tab_def in _tabs:
		rows = maxi(rows, _row_count(tab_def["items"]))
	var avail := s.y * 0.98 - (s.y * 0.03 + pad + s.y * 0.02 + tab_h + s.y * 0.02)
	var bh := maxf(minf((avail - gap * (rows - 1)) / rows, s.y * 0.09), 0.0)
	for control_name in _controls:
		(_controls[control_name] as Button).custom_minimum_size = Vector2(0, bh)
	var dw := s.x * 0.82
	var dh := minf(s.y * 0.24, s.x * 0.55)
	_dialog.custom_minimum_size = Vector2(dw, dh)
	for btn in [_no_button, _yes_button]:
		btn.custom_minimum_size = Vector2(dw * 0.4, dh * 0.3)

# ---------------------------------------------------------------- behavior ---

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

## Tab selection changed. Driven by `toggled`, NOT `button_down`: a PRESS-mode toggle clears
## touch_index when it flips (base_button.cpp), so the touch release no longer matches and
## `pressed_down_with_focus` is never reset — meaning `button_down` fires only on a tab's FIRST
## press. `toggled` fires on every selection change (it's what already moved the tab highlight),
## so the page follows the highlight instead of sticking after the first switch.
func _on_tab_toggled(on: bool, idx: int) -> void:
	# The deselected tab also emits toggled(false); act only on the newly-selected one.
	if not on or idx == _active_tab:
		return
	_vibrate(15)
	_show_page(idx)

## Show one tab's page (BoxContainer skips hidden children, so visibility is the whole switch).
func _show_page(idx: int) -> void:
	# Hiding a pressed Button resets its state without emitting button_up — release held
	# trigger/grip explicitly first so the consumer never sees them stuck held.
	_release_held()
	_active_tab = idx
	for i in _pages.size():
		_pages[i].visible = i == idx

## Synthesize the release signals for a held trigger/grip (see _show_page).
func _release_held() -> void:
	if _held.get("trigger", false):
		_held["trigger"] = false
		trigger_changed.emit(false)
	if _held.get("grip", false):
		_held["grip"] = false
		grip_changed.emit(false)

func _on_toggled(on: bool, control_name: String) -> void:
	_update_toggle_label(control_name)
	match control_name:
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

func _on_momentary_down(control_name: String) -> void:
	match control_name:
		"trigger":
			_held["trigger"] = true
			trigger_changed.emit(true)
		"grip":
			_held["grip"] = true
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
			_overlay.show()  # actual quit fires on the overlay's Yes

func _on_hold_up(control_name: String) -> void:
	if not _held.get(control_name, false):
		return  # already released synthetically (tab switch)
	_held[control_name] = false
	if control_name == "trigger":
		trigger_changed.emit(false)
	else:
		grip_changed.emit(false)

## A toggle's label carries its state ("Camera: ON" / "Camera: OFF", "Camera: —" while disabled).
func _update_toggle_label(control_name: String) -> void:
	var btn: Button = _controls[control_name]
	if btn.disabled:
		btn.text = "%s: —" % _toggles[control_name]
	else:
		btn.text = "%s: %s" % [_toggles[control_name], "ON" if btn.button_pressed else "OFF"]

func _vibrate(ms: int) -> void:
	if OS.has_feature("android"):
		Input.vibrate_handheld(ms)

# ---------------------------------------------------------------- public API ---

## Programmatically set a toggle's on/off state without emitting its signal — keeps the UI in
## sync when the app changes it (e.g. reflecting a camera start that failed, or a plane mode the
## device rejected). No-op for unknown names.
func set_toggle(name: String, on: bool) -> void:
	if _toggles.has(name):
		(_controls[name] as Button).set_pressed_no_signal(on)
		_update_toggle_label(name)

## Enable/disable a button or toggle by name. A disabled Button is drawn greyed and inert (taps do
## nothing) — used to reflect a capability the device lacks (no RGB camera, no plane detection, …).
func set_disabled(name: String, disabled: bool) -> void:
	if not _controls.has(name):
		return
	(_controls[name] as Button).disabled = disabled
	if _toggles.has(name):
		_update_toggle_label(name)

## Override a momentary button's label (e.g. show the active image set on the "Cycle Image" button).
## Pass "" to clear the override and restore the static label.
func set_button_label(name: String, text: String) -> void:
	if _buttons.has(name) and _controls.has(name):
		(_controls[name] as Button).text = _buttons[name] if text.is_empty() else text

# ---------------------------------------------------------------- touchpad widget ---

## The analog touchpad — the one widget with no built-in Control equivalent. Claims one touch on
## press and follows that finger's drags (the Viewport keeps routing them here even outside the
## rect), emitting the normalized [-1, 1] position (+y up, clamped to the circle). One finger
## only: while claimed, other touches are ignored. Also handles a real mouse for desktop test
## runs (touch-emulated mouse events are ignored to avoid double handling on device).
class Touchpad extends Control:
	## The claiming touch went down (haptics hook).
	signal pad_pressed()
	## Normalized pad position while the finger drags.
	signal moved(value: Vector2)
	## The claiming touch lifted; the value snapped back to zero.
	signal released()

	## Caption font size, driven by the owner's _apply_metrics.
	var label_font_size := 24:
		set(value):
			label_font_size = value
			queue_redraw()
	var _touch := -1               # claiming touch index (-1 none, -2 mouse)
	var _value := Vector2.ZERO

	func _gui_input(event: InputEvent) -> void:
		if event is InputEventScreenTouch:
			if event.pressed and _touch == -1:
				_touch = event.index
				pad_pressed.emit()
				_update_value(event.position)
				accept_event()
			elif not event.pressed and event.index == _touch:
				_release_value()
				accept_event()
		elif event is InputEventScreenDrag:
			if event.index == _touch:
				_update_value(event.position)
				accept_event()
		elif event is InputEventMouseButton and event.device != InputEvent.DEVICE_ID_EMULATION:
			if event.button_index != MOUSE_BUTTON_LEFT:
				return
			if event.pressed and _touch == -1:
				_touch = -2
				pad_pressed.emit()
				_update_value(event.position)
				accept_event()
			elif not event.pressed and _touch == -2:
				_release_value()
				accept_event()
		elif event is InputEventMouseMotion and event.device != InputEvent.DEVICE_ID_EMULATION:
			if _touch == -2:
				_update_value(event.position)
				accept_event()

	func _update_value(pos: Vector2) -> void:
		var n := pos / size * 2.0 - Vector2.ONE
		n.y = -n.y
		if n.length() > 1.0:
			n = n.normalized()
		_value = n
		moved.emit(n)
		queue_redraw()

	func _release_value() -> void:
		_touch = -1
		_value = Vector2.ZERO
		released.emit()
		queue_redraw()

	func _draw() -> void:
		var active := _touch != -1
		draw_rect(Rect2(Vector2.ZERO, size), Color(0.25, 0.55, 0.9, 0.18 if active else 0.10))
		draw_rect(Rect2(Vector2.ZERO, size), Color(0.5, 0.75, 1.0, 0.9), false, 3.0)
		var center := size * 0.5
		draw_line(center - Vector2(12, 0), center + Vector2(12, 0), Color(1, 1, 1, 0.25), 2.0)
		draw_line(center - Vector2(0, 12), center + Vector2(0, 12), Color(1, 1, 1, 0.25), 2.0)
		var dot := center + Vector2(_value.x, -_value.y) * size * 0.5 * 0.92
		draw_circle(dot, size.x * 0.09, Color(0.4, 0.85, 1.0, 0.95))
		draw_string(get_theme_default_font(), Vector2(0, size.y - label_font_size * 1.2),
			"TOUCHPAD", HORIZONTAL_ALIGNMENT_CENTER, size.x, label_font_size, Color(1, 1, 1, 0.5))
