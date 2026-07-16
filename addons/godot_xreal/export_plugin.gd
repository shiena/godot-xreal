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
const JAVA_SRC_DIR := ANDROID_DIR + "src/com/godot/game/"
## Same package dir as the template's own GodotApp.java (standard AGP main source set).
## Matches the default gradle_build/gradle_build_directory ("").
const GRADLE_JAVA_DIR := "res://android/build/src/main/java/com/godot/game/"

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

## Local .aar the plugin ships (XREAL/NR runtime archives).
func _get_android_libraries(_platform: EditorExportPlatform, _debug: bool) -> PackedStringArray:
	return PackedStringArray([
		ANDROID_DIR + "nr_loader.aar",
		ANDROID_DIR + "nr_api.aar",
		ANDROID_DIR + "nr_common.aar",
		ANDROID_DIR + "GlassesDisplayPlugEvent-2.4.2.aar",
		ANDROID_DIR + "Log-Control-1.2.aar",
	])

var _copied_java := PackedStringArray()

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
	var err := DirAccess.make_dir_recursive_absolute(GRADLE_JAVA_DIR)
	if err != OK:
		push_error("godot_xreal: cannot create %s (error %d)" % [GRADLE_JAVA_DIR, err])
		return
	for f in DirAccess.get_files_at(JAVA_SRC_DIR):
		if f.get_extension() != "java":
			continue
		err = DirAccess.copy_absolute(JAVA_SRC_DIR + f, GRADLE_JAVA_DIR + f)
		if err != OK:
			push_error("godot_xreal: failed to copy %s into the gradle build template (error %d)" % [f, err])
			continue
		_copied_java.append(GRADLE_JAVA_DIR + f)

## Remove the staged sources again — the build template must stay pristine (Godot wipes it on
## regeneration, and stale copies would shadow renamed/deleted addon sources).
func _export_end() -> void:
	for f in _copied_java:
		DirAccess.remove_absolute(f)
	_copied_java.clear()
