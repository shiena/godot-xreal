# Android / XREAL launch setup

**Symptom this addresses:** the app starts like a normal Android app and the glasses only show
a gray (mirrored) screen — it never enters the XREAL glasses display mode.

This is verified against a **real working Unity+XREAL APK**
(`com.kadinche.layeredclient.xreal`, `aapt dump xmltree`) and the SDK's build scripts
(`Editor/Android/XREALManifestProvider.cs`, `XREALBuildProcessor.cs`, `XREALProjectValidator.cs`)
plus disassembly of the `nractivitylife` aar.

---

## ⚠️ Critical finding: the Unity manifest cannot be copied 1:1

A working XREAL APK's **launcher is `ai.nreal.activitylife.NRXRActivity`** (not the engine's
activity). It sets up the glasses, then presents the content. **But that launcher is
Unity-specific:** `NRXRApp` instantiates `com.unity3d.player.UnityPlayer` directly and presents
*its* surface on the glasses display (`startUnityPlayer(UnityPlayer)`, `findNrealDisplay()`).

So for Godot you **must NOT include `nractivitylife`** (NRXRActivity/NRXRApp) — it would try to
create a Unity player that does not exist. Godot keeps its own `GodotApp` as the launcher, and
**our GDExtension must drive the glasses display itself** (Phase 2, below).

| Reference APK component | Reuse for Godot? |
|---|---|
| `nreal_sdk` / `com.nreal.supportDevices` / `autoLog` meta-data | ✅ add to Godot manifest |
| `GlassesInitProvider` (glasses detection) | ✅ engine-agnostic (from `GlassesDisplayPlugEvent.aar`) |
| `nr_loader` / `nr_common` / `nr_api` `.so`s + `uses-native-library` | ✅ |
| `libXREALNativeSessionManager.so` / `libXREALXRPlugin.so` | ✅ (we dlopen these) |
| `com.xreal.sdk.display.GlassesDisplay` (`nrdisplay.jar`) | ❓ not found in the Unity APK/package inspected so far |
| **`nractivitylife` → NRXRActivity / NRXRApp / UnityPlayerActivity** | ❌ Unity-only launcher, do not include |
| `MediaProjectionService` / `AutoLogProvider` | ⛔ optional (recording / logcat) |

> **Necessary but not sufficient.** Even with the manifest right, rendering your scene onto the
> glasses needs the display path (Phase 2: XREAL secondary display bootstrap →
> `InitializeRendering`/`CreateProjectionRigLayer`/`CreateFrame` or direct `NRRendering*`),
> which this repo has not
> implemented. Manifest-only gets you head tracking + XREAL mode, **not yet a 3D image on the
> glasses**. The gray screen has both causes. See `docs/port-plan.md`.

---

## 1. `AndroidManifest.xml` additions (verified from the reference APK)

`<application>`-level meta-data — exact names/values from the working APK:

```xml
<meta-data android:name="nreal_sdk" android:value="true" />               <!-- THE key flag -->
<meta-data android:name="com.nreal.supportDevices" android:value="1|XrealLight|2|XrealAir" />
<meta-data android:name="autoLog" android:value="0" />
```

The glasses-detection provider (from `GlassesDisplayPlugEvent.aar`; comes via aar merge, shown
here for reference):

```xml
<provider
    android:name="com.xreal.glassesdisplayplugevent.provider.GlassesInitProvider"
    android:authorities="${applicationId}.glassesdisplayplugevent.provider"
    android:directBootAware="true" android:exported="false" android:initOrder="100" />
```

`<manifest>`-level permissions / features (subset the SDK uses; the reference APK had all of
these):

```xml
<!-- INTERNET is REQUIRED (device-confirmed): the NRSDK "Leopard MCU" plugin opens an
     AF_INET/UDP socket to the glasses (which appear as the eth0 net device). Without it,
     socket() fails EPERM and CreateSession returns false → no head tracking. -->
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
<uses-permission android:name="android.permission.HIGH_SAMPLING_RATE_SENSORS" />
<uses-permission android:name="android.permission.SYSTEM_ALERT_WINDOW" />
<uses-permission android:name="android.permission.REORDER_TASKS" />
<uses-permission android:name="android.permission.ACTIVITY_EMBEDDING" />
<uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
<uses-feature android:glEsVersion="0x00030000" />
```

`uses-native-library` (inside `<application>`, from `nr_loader`):

```xml
<uses-native-library android:name="libnr_api.xreal.so" android:required="false" />
```

Keep `GodotApp` as the `MAIN`/`LAUNCHER` activity. Do **not** add `NRXRActivity`.

## 2. Required build settings (`XREALProjectValidator.cs` + reference APK badging)

| Setting | Required | Godot equivalent |
|---|---|---|
| Min SDK | **29** | export *Min SDK* = 29 |
| Target arch | **arm64-v8a** only | export *Architectures* → only `arm64-v8a` |
| Graphics API | **OpenGLES3** (REQUIRED) | project renderer → **Compatibility** (GLES3), not Mobile/Vulkan |
| App entry | classic `Activity` | Godot's `GodotApp` is an `Activity` subclass — OK |

> **GLES3 is required, not just recommended.** Verified on device: with the **Mobile (Vulkan)**
> renderer the app crashes during engine init — `Vulkan ... Forward Mobile`, then
> `GraphicBuffer ... failed (Unknown error -5)` (the XREAL display layer can't satisfy the Vulkan
> swapchain / buffer formats), then `Fatal signal 11 (SIGSEGV) ... in tid (VkThread)` right after
> `OnGodotSetupCompleted` — i.e. *before* the scene even loads. The XREAL display/compositor path
> on Android is built around GLES3. Set the project renderer to **Compatibility**
> (`rendering/renderer/rendering_method = "gl_compatibility"` and `...method.mobile =
> "gl_compatibility"`). This repo's `project.godot` is already set to Compatibility.

## 3. How to apply in Godot

This repo already ships the wiring — the steps below are what produced the current state:

1. **Editor → Project → Install Android Build Template** (creates `res://android/build/`).
2. The XREAL meta-data + permissions are already in
   `res://android/build/src/main/AndroidManifest.xml` (§1). **Re-apply them if you reinstall the
   build template** (that overwrites the manifest).
3. Build the Rust extension for Android and stage the XREAL libs:
   ```bash
   $env:ANDROID_NDK_HOME = "<sdk>/ndk/<version>"
   cargo ndk -t arm64-v8a -o ./jniLibs build --release      # -> libgodot_xreal.so
   pwsh tools/setup_android_build.ps1 -XrealPackage "<...>/com.xreal.xr/package"
   ```
   `setup_android_build.ps1` copies the 3 XREAL `.so` into `jniLibs/arm64-v8a/` and the
   engine-agnostic aars (`nr_loader`, `nr_common`, `nr_api`, `GlassesDisplayPlugEvent`,
   **`Log-Control`**) into `android/build/libs/{debug,release}/`. It **excludes `nractivitylife`**.
   (Add `nrdisplay` / `chameleon` jars later for Phase 2.)

   > **`Log-Control-1.2.aar` is mandatory whenever `GlassesDisplayPlugEvent` ships.** The latter's
   > `GlassesInitProvider` (a ContentProvider that auto-runs at app startup) references
   > `com.xreal.logcontrol.LogControl`; without it the app crashes *before Godot starts* with
   > `NoClassDefFoundError: com/xreal/logcontrol/LogControl` (device-confirmed).
4. In the **Android export preset**: *Use Gradle Build* = **ON** (required — otherwise the custom
   `android/build/` manifest edits, the `XrealBridge` Java, and the `libs/*.aar` are all ignored
   and only the GDExtension `.so` ships), *Min SDK* = **29**, *Architectures* = `arm64-v8a` only.
5. Project renderer → **Compatibility** (GLES3) — **required** (Vulkan crashes on device, see §2).
   Already set in `project.godot`.

## 4. Re-apply after reinstalling the Android build template

Godot overwrites `android/build/` when the Android build template is reinstalled or regenerated.
After doing that, re-apply the project-specific XREAL wiring below.

1. Patch `android/build/src/main/AndroidManifest.xml`.

   Add the XREAL permissions at the manifest level:

   ```xml
   <uses-permission android:name="android.permission.INTERNET" />
   <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
   <uses-permission android:name="android.permission.HIGH_SAMPLING_RATE_SENSORS" />
   <uses-permission android:name="android.permission.SYSTEM_ALERT_WINDOW" />
   <uses-permission android:name="android.permission.REORDER_TASKS" />
   <uses-permission
       android:name="android.permission.ACTIVITY_EMBEDDING"
       tools:ignore="ProtectedPermissions" />
   <uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
   ```

   Add the XREAL app markers inside `<application>`:

   ```xml
   <meta-data android:name="nreal_sdk" android:value="true" />
   <meta-data android:name="com.nreal.supportDevices" android:value="1|XrealLight|2|XrealAir" />
   <meta-data android:name="autoLog" android:value="0" />
   ```

2. Re-add the XREAL Android bridge classes.

   Required files:

   ```text
   android/build/src/main/java/com/godot/game/XrealBridge.java
   android/build/src/main/java/com/godot/game/XrealCompanionActivity.java
   ```

   `XrealBridge` must:

   - find the XREAL Android secondary display
   - start `XrealCompanionActivity` on that display, mirroring the display-selection part of
     Unity's `NRFakeActivity` without referencing `UnityPlayer`
   - `System.loadLibrary("nr_loader")`
   - `System.loadLibrary("nr_api")`
   - `System.loadLibrary("XREALNativeSessionManager")`
   - `System.loadLibrary("XREALXRPlugin")`
   - `System.loadLibrary("godot_xreal")`
   - call the native `nativeRegisterActivity(Activity)` bridge

3. Patch `android/build/src/main/java/com/godot/game/GodotApp.java`.

   Immediately after `super.onCreate(savedInstanceState);`, call:

   ```java
   XrealBridge.register(this);
   XrealBridge.startCompanionOnXrealDisplayIfNeeded(this);
   ```

   This publishes the Godot Activity before scene nodes call `ready()`, then starts a small black
   companion Activity on the XREAL display. The companion also calls `XrealBridge.register(this)`
   when created/resumed, so the native session can see an Activity whose default display is the
   glasses.

   Godot 4.7 also exposes `JavaClassWrapper` and the `AndroidRuntime` singleton to GDScript. The
   demo scene uses them as an additional runtime fallback:

   ```gdscript
   var android_runtime = Engine.get_singleton("AndroidRuntime")
   var activity = android_runtime.getActivity()
   var bridge = JavaClassWrapper.wrap("com.godot.game.XrealBridge")
   activity.runOnUiThread(android_runtime.createRunnableFromGodotCallable(func():
       bridge.register(activity)
       bridge.startCompanionOnXrealDisplayIfNeeded(activity)
   ))
   ```

   This fallback is useful for detecting template drift after regeneration, but it is not a full
   replacement for the Java template patch because the template call publishes the Activity earlier
   in startup.

4. Register the companion Activity in `AndroidManifest.xml`.

   ```xml
   <activity
       android:name=".XrealCompanionActivity"
       android:autoRemoveFromRecents="true"
       android:excludeFromRecents="true"
       android:exported="false"
       android:hardwareAccelerated="false"
       android:launchMode="singleTask"
       android:resizeableActivity="true"
       android:screenOrientation="reverseLandscape"
       android:theme="@android:style/Theme.Black.NoTitleBar.Fullscreen"
       android:configChanges="layoutDirection|locale|orientation|keyboardHidden|screenSize|smallestScreenSize|density|keyboard|navigation|screenLayout|uiMode" />
   ```

5. Restore native libraries into the template.

   The Gradle template packages native libraries from:

   - `android/build/libs/debug/arm64-v8a/`
   - `android/build/libs/release/arm64-v8a/`

   Copy the current `jniLibs/arm64-v8a/*.so` into both directories. Required files:

   ```text
   libgodot_xreal.so
   libVulkanSupport.so
   libXREALNativeSessionManager.so
   libXREALXRPlugin.so
   libnr_api.so
   libnr_libusb.so
   libnr_loader.so
   libnr_plugin_6dof.so
   libnr_rgb_camera.so
   ```

   PowerShell:

   ```powershell
   New-Item -ItemType Directory -Force `
     android\build\libs\debug\arm64-v8a, `
     android\build\libs\release\arm64-v8a | Out-Null

   Copy-Item -Path jniLibs\arm64-v8a\*.so -Destination android\build\libs\debug\arm64-v8a -Force
   Copy-Item -Path jniLibs\arm64-v8a\*.so -Destination android\build\libs\release\arm64-v8a -Force
   ```

6. Restore XREAL/NR Android AAR dependencies into the template.

   The Gradle template compiles Java/Kotlin classes and merges manifest entries from:

   - `android/build/libs/debug/*.aar`
   - `android/build/libs/release/*.aar`

   Required files:

   ```text
   GlassesDisplayPlugEvent-2.4.2.aar
   Log-Control-1.2.aar
   nr_api.aar
   nr_common.aar
   nr_loader.aar
   ```

   `GlassesDisplayPlugEvent-2.4.2.aar` provides
   `com.xreal.glassesdisplayplugevent.GlassesInitSetting`. If it is missing,
   `libXREALNativeSessionManager.so` aborts during `JNI_OnLoad` with
   `ClassNotFoundException: com.xreal.glassesdisplayplugevent.GlassesInitSetting`.

   PowerShell, when `android_old` is available:

   ```powershell
   Copy-Item -Path android_old\build\libs\debug\GlassesDisplayPlugEvent-2.4.2.aar, `
     android_old\build\libs\debug\Log-Control-1.2.aar, `
     android_old\build\libs\debug\nr_api.aar, `
     android_old\build\libs\debug\nr_common.aar, `
     android_old\build\libs\debug\nr_loader.aar `
     -Destination android\build\libs\debug -Force

   Copy-Item -Path android_old\build\libs\release\GlassesDisplayPlugEvent-2.4.2.aar, `
     android_old\build\libs\release\Log-Control-1.2.aar, `
     android_old\build\libs\release\nr_api.aar, `
     android_old\build\libs\release\nr_common.aar, `
     android_old\build\libs\release\nr_loader.aar `
     -Destination android\build\libs\release -Force
   ```

   Or rerun:

   ```powershell
   pwsh tools\setup_android_build.ps1 -XrealPackage "<...>\com.xreal.xr\package"
   ```

7. Ensure `godot_xreal.gdextension` lists all Android native dependencies.

   The `[dependencies] android.arm64` block must include the XREAL and NR `.so` files above.
   Without this, Godot export may omit the lower NR libraries even if the Rust extension builds.

8. Ensure the Android template default min SDK is 29.

   The Godot export preset should pass `export_version_min_sdk=29`, but running Gradle directly
   from `android/build` falls back to `android/build/config.gradle`. Set:

   ```groovy
   minSdk             : 29,
   ```

   `GlassesDisplayPlugEvent-2.4.2.aar` declares `minSdkVersion 28`, and this project uses 29 per
   the XREAL SDK requirements.

9. Ensure Gradle can find the Android SDK.

   If `android/build/local.properties` is absent, add:

   ```properties
   sdk.dir=C\:\\Users\\shien\\AppData\\Local\\Android\\Sdk
   ```

   Adjust the path for other machines. Do not commit machine-specific paths unless this is only
   for a local working tree.

10. Verify the template after patching.

   ```powershell
   cd android\build
   .\gradlew.bat assembleStandardDebug
   ```

   Expected APK:

   ```text
   android/build/build/outputs/apk/standard/debug/android_debug.apk
   ```

## 5. Debugging on device

```bash
adb logcat | grep -iE "xreal|nreal|godot"
```

Check, in order:
- `Initialize godot-rust ...` — the GDExtension loaded.
- `[xreal] native head tracking started` (good) vs
  `[xreal] head tracking disabled: dlopen libXREALNativeSessionManager.so ...` — the `.so`s are
  **not in the APK** (vendor them, §3) or were stripped.
- XREAL/nreal logs about glasses detection / `GlassesInitProvider` — confirms the manifest markers
  took effect.

## 6. Inspect the reference APK yourself

```bash
AAPT="$ANDROID_SDK/build-tools/36.1.0/aapt.exe"
"$AAPT" dump xmltree LayeredClientXREAL.apk AndroidManifest.xml   # full manifest tree
"$AAPT" dump badging  LayeredClientXREAL.apk | grep -E "launchable-activity|uses-permission|sdkVersion"
```
