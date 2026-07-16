@tool
extends EditorExportPlugin

## Android export plugin for the Godot XREAL addon.
##
## Contributes everything the XREAL glasses need into the exported APK WITHOUT hand-editing the
## Gradle Android build template (which Godot wipes on regeneration):
##   - manifest permissions (INTERNET etc.) and <application> markers (nreal_sdk, supportDevices…)
##   - the XREAL companion / NRFakeActivity declarations
##   - the XREAL/NR runtime .aar libraries
##   - the XrealBridge/XrealCompanionActivity Java sources, copied into the gradle build
##     template's src/ for the export's Gradle run to compile (and removed again afterwards,
##     so the template stays pristine) — no pre-built .jar, no local javac needed
##
## The XREAL/NR native .so are still packaged via godot_xreal.gdextension's [dependencies].
## Activity registration + System.loadLibrary happen at runtime from GDScript (see demo/main.gd's
## JavaClassWrapper path), so the launcher Activity (GodotApp) needs no patching.

const ANDROID_DIR := "res://addons/godot_xreal/android/"
## Java sources staged as a whole tree — package dirs (com/godot/game, ai/nreal/activitylife)
## are preserved relative to this root.
const JAVA_SRC_ROOT := ANDROID_DIR + "src/"
## The template's main source set root (its own GodotApp.java lives under it, standard AGP
## layout). Matches the default gradle_build/gradle_build_directory ("").
const GRADLE_JAVA_ROOT := "res://android/build/src/main/java/"
## nr_plugins.json must land in the APK's `assets/` (Android StreamingAssets) for the NR perception
## loader to find + load the image-tracking backend .so. Staged into the gradle template's assets
## source set for the export's Gradle run (and removed again in _export_end), like the Java sources.
const NR_PLUGINS_JSON := ANDROID_DIR + "nr_plugins.json"
const GRADLE_ASSETS_ROOT := "res://android/build/src/main/assets/"

func _get_name() -> String:
	return "godot_xreal_android"

func _supports_platform(platform: EditorExportPlatform) -> bool:
	return platform is EditorExportPlatformAndroid

## <manifest>-level: XREAL permissions (glEsVersion / supports-screens are already in the template).
func _get_android_manifest_element_contents(_platform: EditorExportPlatform, _debug: bool) -> String:
	return """<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
<uses-permission android:name="android.permission.HIGH_SAMPLING_RATE_SENSORS" />
<uses-permission android:name="android.permission.SYSTEM_ALERT_WINDOW" />
<uses-permission android:name="android.permission.REORDER_TASKS" />
<uses-permission android:name="android.permission.ACTIVITY_EMBEDDING" tools:ignore="ProtectedPermissions" />
<uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
<uses-permission android:name="android.permission.CAMERA" />
<uses-permission android:name="android.permission.RECORD_AUDIO" />
<uses-permission android:name="android.permission.VIBRATE" />"""

## <application>-level: XREAL markers + the companion / NRFakeActivity declarations. Uses the
## fully-qualified com.godot.game.XrealCompanionActivity (matches the .jar class).
func _get_android_manifest_application_element_contents(_platform: EditorExportPlatform, _debug: bool) -> String:
	return """<meta-data android:name="nreal_sdk" android:value="true" />
<meta-data android:name="com.nreal.supportDevices" android:value="1|XrealLight|2|XrealAir" />
<meta-data android:name="nr_features" android:value="multiResume" />
<meta-data android:name="autoLog" android:value="0" />
<activity android:name="com.godot.game.XrealCompanionActivity" android:autoRemoveFromRecents="true" android:excludeFromRecents="true" android:exported="false" android:hardwareAccelerated="true" android:launchMode="singleTask" android:resizeableActivity="true" android:screenOrientation="reverseLandscape" android:theme="@android:style/Theme.Black.NoTitleBar.Fullscreen" android:configChanges="layoutDirection|locale|orientation|keyboardHidden|screenSize|smallestScreenSize|density|keyboard|navigation|screenLayout|uiMode" />
<activity android:name="ai.nreal.activitylife.NRFakeActivity" android:autoRemoveFromRecents="true" android:excludeFromRecents="true" android:exported="false" android:hardwareAccelerated="false" android:launchMode="singleTask" android:resizeableActivity="true" android:screenOrientation="reverseLandscape" android:configChanges="mcc|mnc|locale|touchscreen|keyboard|keyboardHidden|navigation|orientation|screenLayout|uiMode|screenSize|smallestScreenSize|fontScale|layoutDirection|density" />"""

## Local .aar the plugin ships (XREAL/NR runtime archives). nr_spatial_anchor / nr_image_tracking
## carry the libnr_spatial_anchor.so / libnr_image_tracking.so backends for the anchor / image-tracking
## C ABIs (see docs/plans/ar-features-plan.md).
func _get_android_libraries(_platform: EditorExportPlatform, _debug: bool) -> PackedStringArray:
	return PackedStringArray([
		ANDROID_DIR + "nr_loader.aar",
		ANDROID_DIR + "nr_api.aar",
		ANDROID_DIR + "nr_common.aar",
		ANDROID_DIR + "nr_spatial_anchor.aar",
		ANDROID_DIR + "nr_image_tracking.aar",
		ANDROID_DIR + "GlassesDisplayPlugEvent-2.4.2.aar",
		ANDROID_DIR + "Log-Control-1.2.aar",
	])

var _copied_java := PackedStringArray()
var _copied_assets := PackedStringArray()

## .java files under root, as paths relative to root (recursive, package dirs preserved).
func _collect_java_sources(root: String, rel: String, out: PackedStringArray) -> void:
	for f in DirAccess.get_files_at(root + rel):
		if f.get_extension() == "java":
			out.append(rel + f)
	for d in DirAccess.get_directories_at(root + rel):
		_collect_java_sources(root, rel + d + "/", out)

## Stage the bridge Java sources into the gradle build template so the export's Gradle run
## compiles them (the gradle build is mandatory for this addon: _get_android_libraries and the
## manifest hooks above only apply to gradle exports).
func _export_begin(features: PackedStringArray, _is_debug: bool, _path: String, _flags: int) -> void:
	if not features.has("android"):
		return
	_copied_java.clear()
	if not DirAccess.dir_exists_absolute("res://android/build"):
		push_error("godot_xreal: gradle build template not found at res://android/build — " +
			"install it via Project > Install Android Build Template (the XrealBridge Java " +
			"sources are compiled by the export's gradle build).")
		return
	var sources := PackedStringArray()
	_collect_java_sources(JAVA_SRC_ROOT, "", sources)
	for rel in sources:
		var dst := GRADLE_JAVA_ROOT + rel
		var err := DirAccess.make_dir_recursive_absolute(dst.get_base_dir())
		if err == OK:
			err = DirAccess.copy_absolute(JAVA_SRC_ROOT + rel, dst)
		if err != OK:
			push_error("godot_xreal: failed to stage %s into the gradle build template (error %d)" % [rel, err])
			continue
		_copied_java.append(dst)

	# Stage nr_plugins.json into the gradle assets so it ends up at the APK's assets/nr_plugins.json
	# (the NR perception loader reads it to load the image-tracking backend). Optional: only if present.
	if FileAccess.file_exists(NR_PLUGINS_JSON):
		var asset_dst := GRADLE_ASSETS_ROOT + "nr_plugins.json"
		var aerr := DirAccess.make_dir_recursive_absolute(asset_dst.get_base_dir())
		if aerr == OK:
			aerr = DirAccess.copy_absolute(NR_PLUGINS_JSON, asset_dst)
		if aerr == OK:
			_copied_assets.append(asset_dst)
		else:
			push_error("godot_xreal: failed to stage nr_plugins.json into the gradle assets (error %d)" % aerr)

## Remove the staged sources again — the build template must stay pristine (Godot wipes it on
## regeneration, and stale copies would shadow renamed/deleted addon sources). Package dirs we
## introduced are pruned too (remove fails harmlessly on non-empty dirs like com/godot/game).
func _export_end() -> void:
	for f in _copied_java:
		DirAccess.remove_absolute(f)
		var dir := f.get_base_dir()
		while dir.length() > GRADLE_JAVA_ROOT.length() and DirAccess.remove_absolute(dir) == OK:
			dir = dir.get_base_dir()
	_copied_java.clear()
	for f in _copied_assets:
		DirAccess.remove_absolute(f)
	_copied_assets.clear()
