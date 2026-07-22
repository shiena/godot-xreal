@tool
extends VBoxContainer
## Editor dock: vendor the XREAL runtime libraries out of the Unity `com.xreal.xr` package — the
## in-editor analog of scripts/vendor_xreal_libs.*. Pick `com.xreal.xr(.tgz|.tar.gz)` (or an already
## extracted package root) and it extracts (via the system `tar`) and copies the .so / .aar / host
## tool to the gitignored destinations the Android export needs:
##
##   3 core .so + libmedia_codec.so -> jniLibs/arm64-v8a/        (dlopen'd; packed via .gdextension)
##   7 .aar                         -> addons/godot_xreal/android/ (shipped by export_plugin.gd)
##   trackableImageTools            -> addons/godot_xreal/tools/   (host image-DB build tool)
##   Marker~/InterMarker.bin        -> demo/image_tracking/markers.bin (AR-marker demo set)
##
## This only vendors XREAL's proprietary libs. The addon's own libgodot_xreal.so comes from the Rust
## build (cargo-ndk) or a prebuilt release — see docs/guides/build-and-release.md. All destinations are
## gitignored (SDK-derived; not redistributable). nractivitylife*.aar is deliberately skipped (its
## launcher is Unity-only). Keep the package version pinned to the one the Rust internal-call offsets
## were RE'd against (hand tracking / depth mesh / signal_guard) — a different version can crash.

# Minimum com.xreal.xr version accepted. The Rust internal-call offsets (hand tracking / depth mesh /
# signal_guard patches) were RE'd against this package; an older one can crash, so vendoring is refused
# below it. Read from the package's package.json (Unity UPM manifest).
const MIN_VERSION := "3.1.0"

# arm64-v8a core .so in Runtime/Plugins/Android/arm64-v8a/ -> jniLibs/arm64-v8a/.
const CORE_SO := ["libXREALNativeSessionManager.so", "libXREALXRPlugin.so", "libVulkanSupport.so"]
# The FPV HW encoder lives under the Camera Features plugin path.
const MEDIA_CODEC_REL := "Runtime/Scripts/Android/Camera Features/Plugins/Android/arm64/libmedia_codec.so"
# .aar in Runtime/Plugins/Android/ -> addons/godot_xreal/android/ (names hardcoded in export_plugin.gd).
const AARS := [
	"nr_loader.aar", "nr_api.aar", "nr_common.aar", "nr_spatial_anchor.aar",
	"nr_image_tracking.aar", "GlassesDisplayPlugEvent-2.4.2.aar", "Log-Control-1.2.aar",
]

var _status: RichTextLabel
var _file_dialog: EditorFileDialog

func _ready() -> void:
	_build_ui()

func _build_ui() -> void:
	add_theme_constant_override(&"separation", 6)

	var title := Label.new()
	title.text = "XREAL SDK Import (vendoring)"
	title.add_theme_font_size_override(&"font_size", 15)
	add_child(title)

	var help := Label.new()
	help.text = "Pick com.xreal.xr (.tgz / .tar.gz, or an extracted package folder) to stage the .so / .aar / tool."
	help.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	# Cap the wrapped height. An autowrap Label reports its *wrapped* height as its minimum, and a dock
	# tab that has never been the active one was never laid out (~17px wide), so it wraps to hundreds
	# of lines and asks for thousands of px. The editor's dock slot sizes hidden tabs too
	# (use_hidden_tabs_for_min_size), so that minimum pushes the dock below it (FileSystem) off-screen
	# until this tab is first opened.
	help.max_lines_visible = 3
	help.tooltip_text = "Extracts (via the system tar) and copies the required .so / .aar / host tool out of the Unity package — the in-editor analog of scripts/vendor_xreal_libs."
	help.modulate = Color(1, 1, 1, 0.75)
	add_child(help)

	var pick := Button.new()
	pick.text = "Select package…"
	pick.pressed.connect(_on_pick_pressed)
	add_child(pick)

	_status = RichTextLabel.new()
	_status.bbcode_enabled = true
	_status.selection_enabled = true
	# Floor only, and deliberately no fit_content: fit_content makes the minimum height the *content*
	# height measured at the current width, which in a never-shown dock tab is thousands of px — same
	# trap as the help label above. The import log scrolls inside the label instead, and EXPAND_FILL
	# still hands it every pixel the dock actually has.
	_status.custom_minimum_size = Vector2(0, 48)
	_status.size_flags_vertical = Control.SIZE_EXPAND_FILL
	add_child(_status)

	# Browse the whole filesystem — the package usually lives outside the project.
	_file_dialog = EditorFileDialog.new()
	_file_dialog.file_mode = EditorFileDialog.FILE_MODE_OPEN_ANY  # file (.tgz) or a package dir
	_file_dialog.access = EditorFileDialog.ACCESS_FILESYSTEM
	_file_dialog.add_filter("*.tgz,*.tar.gz", "XREAL package archive")
	_file_dialog.file_selected.connect(_on_selected)
	_file_dialog.dir_selected.connect(_on_selected)
	add_child(_file_dialog)

func _on_pick_pressed() -> void:
	_file_dialog.popup_file_dialog()

# --- import ---------------------------------------------------------------------------------------

func _on_selected(path: String) -> void:
	_status.text = "Importing …"
	# Defer so the "Importing …" paint lands before the (blocking) tar/copy work.
	call_deferred("_import", path)

func _import(path: String) -> void:
	var log := PackedStringArray()
	var temp := ""
	var pkg := ""

	if DirAccess.dir_exists_absolute(path):
		# An already-extracted package root (contains Runtime/Plugins/Android) or its parent.
		pkg = _find_package_root(path)
		if pkg.is_empty():
			_fail("Runtime/Plugins/Android not found in the selected folder: %s" % path)
			return
	else:
		if not (path.ends_with(".tgz") or path.ends_with(".tar.gz")):
			_fail("Pick a .tgz / .tar.gz or a package folder: %s" % path)
			return
		temp = OS.get_cache_dir().path_join("xreal_pkg_%d" % Time.get_ticks_usec())
		DirAccess.make_dir_recursive_absolute(temp)
		var terr := _extract(path, temp, log)
		if terr != OK:
			_rmtree(temp)
			_fail("Extraction failed:\n%s" % "\n".join(log))
			return
		pkg = _find_package_root(temp)
		if pkg.is_empty():
			_rmtree(temp)
			_fail("No package containing Runtime/Plugins/Android found in the archive.")
			return

	var src_android := pkg.path_join("Runtime/Plugins/Android")
	var src_abi := src_android.path_join("arm64-v8a")
	if not DirAccess.dir_exists_absolute(src_abi):
		if temp: _rmtree(temp)
		_fail("Doesn't look like a com.xreal.xr package (%s missing)" % src_abi)
		return

	# Version gate: the native offsets were RE'd against MIN_VERSION+, so refuse older packages
	# (crash risk) before copying anything.
	var verr := _version_error(pkg)
	if not verr.is_empty():
		if temp: _rmtree(temp)
		_fail(verr)
		return

	# Destinations (all gitignored).
	var jni := ProjectSettings.globalize_path("res://jniLibs/arm64-v8a")
	var addon := ProjectSettings.globalize_path("res://addons/godot_xreal/android")
	var tools := ProjectSettings.globalize_path("res://addons/godot_xreal/tools")
	var demo := ProjectSettings.globalize_path("res://demo/image_tracking")
	for d in [jni, addon, tools, demo]:
		DirAccess.make_dir_recursive_absolute(d)

	var missing := PackedStringArray()

	# 1) core .so + libmedia_codec.so -> jniLibs/arm64-v8a
	for so in CORE_SO:
		_copy(src_abi.path_join(so), jni.path_join(so), "so   " + so, log, missing)
	_copy(pkg.path_join(MEDIA_CODEC_REL), jni.path_join("libmedia_codec.so"), "so   libmedia_codec.so", log, missing)

	# 2) 7 .aar -> addons/godot_xreal/android
	for aar in AARS:
		_copy(src_android.path_join(aar), addon.path_join(aar), "aar  " + aar, log, missing)

	# 3) host image-DB tool -> addons/godot_xreal/tools (OS-specific; Linux has no prebuilt tool)
	var tool_rel := ""
	var tool_dst := ""
	match OS.get_name():
		"Windows":
			tool_rel = "Tools~/Windows/trackableImageTools.exe"
			tool_dst = tools.path_join("trackableImageTools.exe")
		"macOS":
			tool_rel = "Tools~/MacOS/trackableImageTools"
			tool_dst = tools.path_join("trackableImageTools")
	if tool_rel.is_empty():
		log.append("skip trackableImageTools (no tool bundled for this OS)")
	elif _copy(pkg.path_join(tool_rel), tool_dst, "tool " + tool_dst.get_file(), log, missing) and OS.get_name() != "Windows":
		OS.execute("chmod", ["+x", tool_dst])  # copy drops the exec bit on Unix

	# 4) AR-marker demo set (optional).
	var marker_src := pkg.path_join("Marker~/InterMarker.bin")
	if FileAccess.file_exists(marker_src):
		_copy(marker_src, demo.path_join("markers.bin"), "asset markers.bin (AR-marker DB)", log, [])

	if temp:
		_rmtree(temp)
	EditorInterface.get_resource_filesystem().scan()

	# Report.
	var body := "\n".join(log)
	if missing.is_empty():
		_status.text = "[color=green]Done[/color] — everything staged.\n[code]%s[/code]\n[color=gray]note: the addon's own libgodot_xreal.so comes separately (Rust build or prebuilt release)[/color]" % body
	else:
		_status.text = "[color=orange]Partly missing[/color] (possible package-version mismatch):\n[code]%s[/code]\n[color=orange]missing:\n  - %s[/color]" % [body, "\n  - ".join(missing)]

# --- helpers --------------------------------------------------------------------------------------

## Validate the package version against MIN_VERSION. Returns "" when ok, or a user-facing error
## message (unreadable manifest / too old) — kept as one function so `_import` stays under
## gdlint's max-returns.
func _version_error(pkg: String) -> String:
	var ver := _package_version(pkg)
	if ver.is_empty():
		return "Could not read \"version\" from package.json — is this a com.xreal.xr package?"
	if _version_lt(ver, MIN_VERSION):
		return "com.xreal.xr %s is too old — this addon needs %s or newer (the native hand-tracking / depth-mesh / signal_guard offsets were reverse-engineered against %s; an older package can crash). Nothing was imported." % [ver, MIN_VERSION, MIN_VERSION]
	return ""

## Read the package version from the Unity UPM manifest (`<pkg>/package.json`), or "" if unreadable.
func _package_version(pkg: String) -> String:
	var pj := pkg.path_join("package.json")
	if not FileAccess.file_exists(pj):
		return ""
	var f := FileAccess.open(pj, FileAccess.READ)
	if f == null:
		return ""
	var data = JSON.parse_string(f.get_as_text())
	f.close()
	return str(data.get("version", "")) if typeof(data) == TYPE_DICTIONARY else ""

## True if semver `a` is older than `b`, comparing the dot-separated numeric core (any -pre / +build
## suffix is ignored). Missing components read as 0 (so "3.1" == "3.1.0").
func _version_lt(a: String, b: String) -> bool:
	var pa := a.split("-")[0].split("+")[0].split(".")
	var pb := b.split("-")[0].split("+")[0].split(".")
	for i in 3:
		var na := int(pa[i]) if i < pa.size() else 0
		var nb := int(pb[i]) if i < pb.size() else 0
		if na != nb:
			return na < nb
	return false

## Extract a .tar.gz into `dest` using the system tar (bsdtar on Win10+, native tar on macOS/Linux).
func _extract(archive: String, dest: String, log: PackedStringArray) -> int:
	var tar := "tar"
	if OS.get_name() == "Windows":
		# Use System32\tar.exe explicitly: a GNU tar on PATH reads the `C:` in a Windows path as a host.
		var sysroot := OS.get_environment("SystemRoot")
		if sysroot.is_empty():
			sysroot = "C:/Windows"
		var win_tar := sysroot.path_join("System32/tar.exe")
		if FileAccess.file_exists(win_tar):
			tar = win_tar
	var out := []
	var code := OS.execute(tar, ["-xzf", archive, "-C", dest], out, true)
	if code != 0:
		log.append("tar exit %d: %s" % [code, "\n".join(out)])
	return OK if code == 0 else FAILED

## Return the package root under `base`: `base` itself if it holds Runtime/Plugins/Android, else a
## `package/` child, else the first child dir that holds it. Empty string if none.
func _find_package_root(base: String) -> String:
	if DirAccess.dir_exists_absolute(base.path_join("Runtime/Plugins/Android")):
		return base
	var pkg := base.path_join("package")
	if DirAccess.dir_exists_absolute(pkg.path_join("Runtime/Plugins/Android")):
		return pkg
	for sub in DirAccess.get_directories_at(base):
		var cand := base.path_join(sub)
		if DirAccess.dir_exists_absolute(cand.path_join("Runtime/Plugins/Android")):
			return cand
	return ""

## Copy one file if present; append a log line, or record it as missing. Returns whether it copied.
func _copy(src: String, dst: String, label: String, log: PackedStringArray, missing) -> bool:
	if not FileAccess.file_exists(src):
		log.append("MISS " + label.strip_edges())
		if missing is PackedStringArray or missing is Array:
			missing.append(label.strip_edges())
		return false
	var err := DirAccess.copy_absolute(src, dst)
	if err != OK:
		log.append("FAIL " + label.strip_edges() + " (err %d)" % err)
		if missing is PackedStringArray or missing is Array:
			missing.append(label.strip_edges())
		return false
	log.append(label)
	return true

## Recursively delete a directory (Godot has no built-in recursive remove).
func _rmtree(path: String) -> void:
	var d := DirAccess.open(path)
	if d == null:
		return
	for f in d.get_files():
		DirAccess.remove_absolute(path.path_join(f))
	for sub in d.get_directories():
		_rmtree(path.path_join(sub))
	DirAccess.remove_absolute(path)

func _fail(msg: String) -> void:
	_status.text = "[color=red]%s[/color]" % msg
