# Android customizations → EditorExportPlugin migration

Status: **DONE (device-confirmed 2026-07-02).** `addons/godot_xreal/export_plugin.gd` injects the
XREAL manifest entries + ships the `.aar`/`.jar` at export time; the Gradle template needs no
hand-edits. Verified on XREAL One Pro: manifest markers present in the APK, `XrealBridge` (from the
compiled `xreal_bridge.jar`) registers the Activity, `session_started=true`, stereo renders.

## Implemented (Option B)
- `addons/godot_xreal/export_plugin.gd` (`EditorExportPlugin`): `_get_android_manifest_element_contents`
  (XREAL permissions), `_get_android_manifest_application_element_contents` (nreal_sdk / supportDevices /
  nr_features / autoLog meta-data + `com.godot.game.XrealCompanionActivity` + `ai.nreal.activitylife.NRFakeActivity`),
  `_get_android_libraries` (the bridge `.jar` + `nr_loader/nr_api/nr_common/GlassesDisplayPlugEvent/Log-Control` `.aar`).
- Registered from `addons/godot_xreal/plugin.gd` (`add_export_plugin` in `_enter_tree`).
- `XrealBridge.java` + `XrealCompanionActivity.java` moved to `addons/godot_xreal/android/src/com/godot/game/`
  (kept in package `com.godot.game` so the JNI symbol `Java_com_godot_game_XrealBridge_nativeRegisterActivity`
  is unchanged). Compile to `addons/godot_xreal/android/xreal_bridge.jar`:
  ```bash
  javac -encoding UTF-8 -source 11 -target 11 -classpath "<sdk>/platforms/android-35/android.jar" \
    -d out addons/godot_xreal/android/src/com/godot/game/*.java
  jar cf addons/godot_xreal/android/xreal_bridge.jar -C out .
  ```
- Registration/`System.loadLibrary` run from GDScript (`demo/main.gd` JavaClassWrapper path), so the
  Gradle template's `GodotApp` needs no patch. `.so` still packaged via `godot_xreal.gdextension`.
- The `.aar`/`.jar` under `addons/godot_xreal/android/` are gitignored (vendored/built locally, like
  `jniLibs/*.so`); the Java **source** is committed.

Remaining polish (non-blocking): revert the working template's `GodotApp.java` to stock (its
`XrealBridge.register` call is now redundant with the GDScript path).
~~Fold the AAR-copy + JAR-build into `scripts/vendor_xreal_libs.ps1`~~ — **done (2026-07-14)**:
the script now stages the 3 core `.so`, copies the 5 `.aar` into `addons/godot_xreal/android/`,
and compiles `xreal_bridge.jar`; `build.ps1`/`build.sh` check all of it before an export.
**Superseded (2026-07-15):** the pre-compiled `xreal_bridge.jar` is gone — `export_plugin.gd`
now stages the Java sources into the gradle build template at `_export_begin` and the export's
Gradle run compiles them (removed again at `_export_end`). The vendor scripts only extract the
`.so`/`.aar` from the SDK package; no local JDK/javac step remains.
The 5 NR `.so` are no longer extracted from the `.aar` nor listed in `godot_xreal.gdextension`
`[dependencies]` — Gradle merges each shipped `.aar`'s `jni/arm64-v8a/*.so` into the APK anyway
(APK-verified 2026-07-14: with the NR libs absent from `jniLibs/`, they still land in
`lib/arm64-v8a/` via the aars, no duplicates).

---

## Original investigation (kept for context)

Goal: move the hand-edits in `android/build/`
(which Godot wipes whenever the Android build template is reinstalled/regenerated — see
`docs/guides/android-setup.md` §4) into a Godot **Android export plugin** so they survive regeneration
and live with the addon.

## What actually needs to survive template regen

Template regen wipes `android/build/`. The XREAL-specific pieces currently living there:

| Piece | Where now | Wiped on regen? |
|---|---|---|
| Manifest permissions / meta-data / activities / `uses-native-library` | `android/build/src/main/AndroidManifest.xml` | ✅ yes |
| Java: `XrealBridge.java`, `XrealCompanionActivity.java` | `android/build/src/main/java/com/godot/game/` | ✅ yes |
| `GodotApp.java` patch (`XrealBridge.register` + window flags) | `android/build/src/main/java/.../GodotApp.java` | ✅ yes |
| AARs: `nr_api`, `nr_common`, `nr_loader`, `GlassesDisplayPlugEvent`, `Log-Control` | `android/build/libs/{debug,release}/*.aar` | ✅ yes |
| XREAL/NR `.so` ×9 | `android/build/libs/.../arm64-v8a/*.so` | ✅ yes, **but irrelevant for export** |

**Already solved for the Godot *export* path:** the `.so` are packed from
`godot_xreal.gdextension` (`[dependencies] android.arm64` lists all 8 XREAL/NR libs; the extension
`.so` itself from `android.*.arm64`). The confirmed exported APK contained
`libgodot_xreal.so` + `libXREALXRPlugin.so` + `libnr_loader.so` etc. The `android/build/libs/.../*.so`
copies are only needed for a **direct `gradlew` build**, not `godot --export`. So the migration does
**not** need to handle `.so` — leave the `.gdextension` as-is.

`GodotApp.java` today: `tryRedirectToXrealDisplay()` (route B) is **defined but not called** in
`onCreate` — it is dormant. The only live GodotApp customization is `XrealBridge.register(this)`
(after `super.onCreate`) plus dev-only window flags (`setShowWhenLocked` / `setTurnScreenOn` /
`KEEP_SCREEN_ON`). So the display-routing patch is **not** a migration blocker right now.

## The EditorExportPlugin Android API (Godot 4.7, verified in engine source)

`EditorExportPlugin` exposes these Android hooks (`editor/export/editor_export_plugin.cpp`):

| Method | Injects into |
|---|---|
| `_get_android_manifest_element_contents(platform, debug)` | `<manifest>` children — `uses-permission`, `uses-feature`, `queries` |
| `_get_android_manifest_application_element_contents(platform, debug)` | `<application>` children — `meta-data`, `activity`, `provider`, `service`, `uses-native-library` |
| `_get_android_manifest_activity_element_contents(platform, debug)` | inside the main `<activity>` (GodotApp) — `intent-filter`, per-activity `meta-data` |
| `_get_android_libraries(platform, debug)` | local `.aar`/`.jar` paths (PackedStringArray) |
| `_get_android_dependencies(platform, debug)` | Maven coordinates (PackedStringArray) |
| `_get_android_dependencies_maven_repos(platform, debug)` | extra Maven repo URLs |
| `_update_android_prebuilt_manifest(platform, manifest_data)` | rewrite the whole compiled manifest (advanced, avoid) |
| `_supports_platform(platform)` | gate the plugin to Android |

Registered from an `@tool` `EditorPlugin` in `addons/godot_xreal/` via
`add_export_plugin()` / `remove_export_plugin()`.

## Migration mapping (what goes where)

- **Permissions + `uses-feature`** (`INTERNET`, `ACCESS_NETWORK_STATE`, `HIGH_SAMPLING_RATE_SENSORS`,
  `SYSTEM_ALERT_WINDOW`, `REORDER_TASKS`, `ACTIVITY_EMBEDDING`, `FOREGROUND_SERVICE`,
  `glEsVersion 0x30000`) → `_get_android_manifest_element_contents`.
- **`<application>` meta-data** (`nreal_sdk=true`, `com.nreal.supportDevices`, `nr_features=multiResume`,
  `autoLog=0`), **`uses-native-library libnr_api.xreal.so`**, **`<activity>` for
  `XrealCompanionActivity`** (+ `NRFakeActivity` if kept), and the `GlassesInitProvider` (comes via the
  AAR merge, no manual entry needed) → `_get_android_manifest_application_element_contents`.
- **AARs** (`nr_api`, `nr_common`, `nr_loader`, `GlassesDisplayPlugEvent-2.4.2`, `Log-Control-1.2`) →
  `_get_android_libraries` returning `res://` paths to the vendored AARs stored under the addon.
- **`.so`** → unchanged (`.gdextension`).
- **Custom Java + register** → the decision below.

## The one real decision: custom Java (`XrealBridge` / `XrealCompanionActivity`)

`XrealBridge` does more than pass the Activity — investigation found it is load-bearing:

- `loadLibraries()`: `System.loadLibrary` for `nr_loader`, `nr_api`, `XREALNativeSessionManager`,
  `XREALXRPlugin`, `godot_xreal`. **Required** — the XREAL natives' `JNI_OnLoad` / `FindClass`
  (e.g. `GlassesInitSetting`) need the app-classloader namespace that `System.loadLibrary` gives;
  a bare `dlopen` from Rust is not equivalent.
- `register(Activity)`: `nativeRegisterActivity(activity)` (JNI → Rust → `ndk_context::initialize_android_context`)
  + `registerDisplayListener` (reacts to the XREAL display appearing) + first-run companion launch.
- `findXrealDisplay(Context)`, `startCompanionOnXrealDisplayIfNeeded(Activity)`.
- `XrealCompanionActivity`: a real `Activity` subclass (needed only if the companion/route-B path is used).

### Option A — eliminate custom Java (aggressive)
Move registration to Rust `#[func]` + GDScript, drop `XrealCompanionActivity` and route B.
- GDScript: `JavaClassWrapper.wrap("java.lang.System").loadLibrary("nr_loader")` ×5, then get the
  Activity from the `AndroidRuntime` singleton and pass it to a new Rust `#[func]` that extracts the
  `jobject` and calls the existing `ndk_context` init.
- **Wrinkles found:** (1) passing a live Android `jobject` from GDScript into a godot-rust `#[func]`
  and extracting it needs godot-rust Android/JNI interop that is fiddly and version-sensitive; (2) the
  `DisplayListener` (react to XREAL display hotplug) would have to be reimplemented in Rust JNI or
  dropped; (3) `System.loadLibrary` order/namespace must still be exactly right.
- **Pro:** no custom Java, no gradle sub-project to maintain. **Con:** real rewrite + JNI-interop risk;
  loses the display hotplug listener unless reimplemented.

### Option B — AAR-wrap the existing Java (conservative, recommended)
Compile the current `XrealBridge` + `XrealCompanionActivity` into a small `.aar` once (a minimal
`android/plugins/xreal-bridge/` gradle project, or reuse the existing template build), store the AAR
in the addon, and add it via `_get_android_libraries`.
- Registration: call `XrealBridge.register(activity)` + `loadLibraries()` from GDScript at startup via
  `JavaClassWrapper` (the fallback that already exists in `demo/main.gd`), so **the `GodotApp.java`
  patch is dropped entirely** — nothing edits the template.
- **Pro:** behavior-preserving (keeps `System.loadLibrary`, `ndk_context` init, display listener,
  companion), zero JNI-interop risk, fully template-regen-safe. **Con:** maintain a tiny prebuilt AAR
  (rebuild only when `XrealBridge`/`CompanionActivity` change — rare).

### Recommendation
**Option B**, with registration driven from GDScript. Rationale: the Java is small and stable but
genuinely load-bearing (`System.loadLibrary` namespace, display hotplug); wrapping it as an AAR keeps
all confirmed-working behavior, removes every `android/build/` hand-edit (manifest → plugin, Java →
AAR, register → GDScript), and survives template regen. Option A trades that safety for a JNI-interop
rewrite whose only payoff is not shipping one small AAR — not worth it while the display path is still
being brought up.

## Proposed structure (Option B)

```
addons/godot_xreal/
  plugin.cfg                     # existing
  export_plugin.gd               # NEW: EditorExportPlugin (manifest + _get_android_libraries)
  editor_plugin.gd               # NEW/￭ @tool EditorPlugin: add_export_plugin() on _enter_tree
  android/
    xreal_bridge.aar             # NEW: compiled XrealBridge + XrealCompanionActivity
    nr_api.aar  nr_common.aar  nr_loader.aar
    GlassesDisplayPlugEvent-2.4.2.aar  Log-Control-1.2.aar
src/android/                     # NEW: XrealBridge.java + XrealCompanionActivity.java + build.gradle
                                 #      (source of truth for the AAR; build once)
```

## Migration steps (when approved)
1. Add `export_plugin.gd` (the `EditorExportPlugin`) + register it from an `@tool` `EditorPlugin`.
2. Fill `_get_android_manifest_element_contents` / `_..._application_element_contents` with the exact
   entries above (copy from the current manifest verbatim).
3. Stand up `src/android/` gradle project for `XrealBridge`+`XrealCompanionActivity`, build
   `xreal_bridge.aar`, drop it (and the 5 vendored AARs) under `addons/godot_xreal/android/`; return
   them from `_get_android_libraries`.
4. Move `XrealBridge.register(activity)` + `loadLibraries()` into the addon's startup GDScript
   (autoload or the demo's existing `JavaClassWrapper` path); delete the `GodotApp.java` patch.
5. Regenerate the Android build template clean (no manual edits), export, and verify parity:
   `loaded libgodot_xreal.so`, `Activity registered`, `session_started=true`, and the manifest markers
   (`nreal_sdk`, `com.nreal.supportDevices`).

## Risks / open items
- **Startup timing of register:** the Java patch ran `register` inside `GodotApp.onCreate` (before
  scene `ready()`). From GDScript it runs at first `_ready`/autoload — verify the native session's
  retry (`session::shared()` is already retry-friendly) tolerates the slightly later Activity publish.
  Device log shows the JavaClassWrapper fallback already works, so this is low risk.
- **AAR build toolchain:** needs a one-time gradle setup for the bridge AAR (AGP + the Godot
  `godot-lib` as `compileOnly`). Pin versions.
- **`GlassesInitProvider`:** ships inside `GlassesDisplayPlugEvent.aar`; confirm it still auto-registers
  when the AAR comes via `_get_android_libraries` (it should — same manifest merge).
- This migration is **orthogonal to the black-screen frame-submission work**; do it when convenient,
  it does not unblock rendering.
```
