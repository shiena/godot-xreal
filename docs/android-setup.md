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
| `com.xreal.sdk.display.GlassesDisplay` (`nrdisplay.jar`) | ✅ this is the Phase 2 presentation primitive |
| **`nractivitylife` → NRXRActivity / NRXRApp / UnityPlayerActivity** | ❌ Unity-only launcher, do not include |
| `MediaProjectionService` / `AutoLogProvider` | ⛔ optional (recording / logcat) |

> **Necessary but not sufficient.** Even with the manifest right, rendering your scene onto the
> glasses needs the display path (Phase 2: `CreateSession` → `GlassesDisplay` surface →
> `InitializeRendering`/`CreateProjectionRigLayer`/`CreateFrame`), which this repo has not
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

## 4. Debugging on device

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

## 5. Inspect the reference APK yourself

```bash
AAPT="$ANDROID_SDK/build-tools/36.1.0/aapt.exe"
"$AAPT" dump xmltree LayeredClientXREAL.apk AndroidManifest.xml   # full manifest tree
"$AAPT" dump badging  LayeredClientXREAL.apk | grep -E "launchable-activity|uses-permission|sdkVersion"
```
