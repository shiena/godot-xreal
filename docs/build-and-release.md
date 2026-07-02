# Building

The GDExtension is plain godot-rust. The only project-specific step is vendoring the XREAL
native libraries before an Android export.

## Desktop (editor iteration)

```powershell
cargo build            # -> target/debug/godot_xreal.dll   (Windows)
cargo build --release  # -> target/release/godot_xreal.dll
```

The native XREAL `.so` files are Android-only, so on desktop `XrealHeadTracker` logs a warning
and stays at identity. This is intended — it lets you open the project and edit scenes on PC.

Crate output names (`crate-type = ["cdylib"]`): `godot_xreal.dll` (Windows),
`libgodot_xreal.so` (Linux/Android), `libgodot_xreal.dylib` (macOS).

## Android (XREAL device = arm64)

Run these commands from the repository root (`C:\Users\shien\dev\godot-xreal`). The `cargo ndk`
output directory and the Gradle Android template library directory are not the same directory.

```powershell
rustup target add aarch64-linux-android
cargo install cargo-ndk
# ANDROID_NDK_HOME must point at an installed NDK
cargo ndk -t arm64-v8a -o ./jniLibs build --release
#   -> jniLibs/arm64-v8a/libgodot_xreal.so
#   -> target/aarch64-linux-android/release/libgodot_xreal.so  (referenced by the .gdextension)
```

When building the Android template directly with Gradle, copy the freshly built `.so` from
`jniLibs` into the template paths that Gradle actually packages:

```powershell
# Must be run from repo root, not from android/build.
Copy-Item -LiteralPath .\jniLibs\arm64-v8a\libgodot_xreal.so `
  -Destination .\android\build\libs\debug\arm64-v8a\libgodot_xreal.so -Force
Copy-Item -LiteralPath .\jniLibs\arm64-v8a\libgodot_xreal.so `
  -Destination .\android\build\libs\release\arm64-v8a\libgodot_xreal.so -Force
```

Then run Gradle with the package name explicitly passed. Without this property, a direct Gradle
build falls back to `com.godot.game` even if `export_presets.cfg` says `com.example.godotxreal`.

```powershell
Push-Location .\android\build
.\gradlew.bat assembleStandardDebug '-Pexport_package_name=com.example.godotxreal'
Pop-Location
```

The debug APK produced by the template may need manual signing before `adb install` accepts it:

```powershell
$bt = 'C:\Users\shien\AppData\Local\Android\Sdk\build-tools\36.1.0'
$unsigned = 'android\build\build\outputs\apk\standard\debug\android_debug.apk'
$aligned = 'android\build\build\outputs\apk\standard\debug\android_debug-aligned.apk'
$signed = 'android\build\build\outputs\apk\standard\debug\android_debug-signed.apk'

Remove-Item -LiteralPath $aligned, $signed -Force -ErrorAction SilentlyContinue
& "$bt\zipalign.exe" -f -p 4 $unsigned $aligned
& "$bt\apksigner.bat" sign `
  --ks "$env:USERPROFILE\.android\debug.keystore" `
  --ks-pass pass:android `
  --key-pass pass:android `
  --ks-key-alias androiddebugkey `
  --out $signed $aligned
& "$bt\apksigner.bat" verify --verbose $signed

adb install --no-incremental $signed
adb shell am start -n com.example.godotxreal/com.godot.game.GodotAppLauncher
```

Use `--no-incremental` during RE iterations. Incremental install can keep stale native libraries
around, which makes the log look like an old `libgodot_xreal.so` is still running.

### Vendor the XREAL runtime libraries (once)

The extension `dlopen`s `libXREALNativeSessionManager.so` / `libXREALXRPlugin.so` /
`libVulkanSupport.so` at startup, so they must be packed into the APK. Copy them out of the Unity
package:

```powershell
pwsh tools/vendor_xreal_libs.ps1 -XrealPackage "C:\path\to\com.xreal.xr\package"
#   -> jniLibs/arm64-v8a/libXREALNativeSessionManager.so
#   -> jniLibs/arm64-v8a/libXREALXRPlugin.so
#   -> jniLibs/arm64-v8a/libVulkanSupport.so
```

`godot_xreal.gdextension` already lists these under `[dependencies] android.arm64`, so Godot's
Android export packs them next to the extension.

> **Not yet wired:** the XREAL `.aar` Java/JNI layer (`GlassesDisplay`, activity lifecycle,
> `nr_*` perception backends). Phase 1 (head pose) may run on an already-initialised XREAL host;
> session bootstrap + display (Phase 2) require these `.aar`s to be added as a Godot Android
> plugin. See `docs/port-plan.md`.

## Lint

```bash
cargo fmt --all
cargo clippy --all-targets
```
