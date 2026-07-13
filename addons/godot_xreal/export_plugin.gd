@tool
extends EditorExportPlugin

## Android export plugin for the Godot XREAL addon.
##
## Contributes everything the XREAL glasses need into the exported APK WITHOUT hand-editing the
## Gradle Android build template (which Godot wipes on regeneration):
##   - manifest permissions (INTERNET etc.) and <application> markers (nreal_sdk, supportDevices…)
##   - the XREAL companion / NRFakeActivity declarations
##   - the XREAL/NR runtime .aar libraries + the compiled XrealBridge/XrealCompanionActivity .jar
##
## The XREAL/NR native .so are still packaged via godot_xreal.gdextension's [dependencies].
## Activity registration + System.loadLibrary happen at runtime from GDScript (see demo/main.gd's
## JavaClassWrapper path), so the launcher Activity (GodotApp) needs no patching.

const ANDROID_DIR := "res://addons/godot_xreal/android/"

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

## Local .jar/.aar the plugin ships (XrealBridge classes + XREAL/NR runtime archives).
func _get_android_libraries(_platform: EditorExportPlatform, _debug: bool) -> PackedStringArray:
	return PackedStringArray([
		ANDROID_DIR + "xreal_bridge.jar",
		ANDROID_DIR + "nr_loader.aar",
		ANDROID_DIR + "nr_api.aar",
		ANDROID_DIR + "nr_common.aar",
		ANDROID_DIR + "GlassesDisplayPlugEvent-2.4.2.aar",
		ANDROID_DIR + "Log-Control-1.2.aar",
	])
