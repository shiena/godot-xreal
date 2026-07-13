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

The XREAL native libraries are **not** in this repo. Get them from the **XREAL SDK for Unity** — the
`com.xreal.xr` package, distributed as a tgz (`com.xreal.xr.tar.gz`) — and place **8 `.so` into
`jniLibs/arm64-v8a/`**. `godot_xreal.gdextension` lists all 8 under `[dependencies] android.arm64`, so
Godot's Android export packs them next to the extension. The extension `dlopen`s them at startup.

1. Extract `com.xreal.xr.tar.gz` → a `package/` directory.

2. **3 core libs** live loose at `package/Runtime/Plugins/Android/arm64-v8a/`; the script copies them:

   ```powershell
   pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage "C:\path\to\package"
   #   -> jniLibs/arm64-v8a/libXREALNativeSessionManager.so
   #   -> jniLibs/arm64-v8a/libXREALXRPlugin.so
   #   -> jniLibs/arm64-v8a/libVulkanSupport.so
   ```

3. **5 NR libs** live inside the package's `.aar` files (each `.aar` is a zip; extract
   `jni/arm64-v8a/<lib>` into `jniLibs/arm64-v8a/`):

   | .aar (`package/Runtime/Plugins/Android/`) | libraries |
   |---|---|
   | `nr_api.aar`    | `libnr_api.so`, `libnr_plugin_6dof.so`, `libnr_rgb_camera.so` |
   | `nr_loader.aar` | `libnr_loader.so` |
   | `nr_common.aar` | `libnr_libusb.so` (also holds many QNN/SNPE libs — those are **not** needed) |

   e.g. `unzip -j path/to/nr_api.aar 'jni/arm64-v8a/libnr_api.so' -d jniLibs/arm64-v8a/` (repeat per lib).

`scripts/build.ps1` / `scripts/build.sh` verify all 8 before an export and stop with these exact
instructions if any is missing (no download/extract is automated).

> The XREAL `.aar` files also carry a Java/JNI layer (`GlassesDisplay`, activity lifecycle) added as a
> Godot Android plugin — see `docs/port-plan.md`. Only the 8 native `.so` above go into `jniLibs/`.

## Lint

```bash
cargo fmt --all
cargo clippy --all-targets
```
