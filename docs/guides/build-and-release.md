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
`com.xreal.xr` package, distributed as a tgz (`com.xreal.xr.tar.gz`) — extract it (→ a `package/`
directory) and run:

```powershell
pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage "C:\path\to\package"
```

The script stages **everything** the Android export needs (all git-ignored; nothing is downloaded —
you supply the package):

1. **3 core `.so` → `jniLibs/arm64-v8a/`** — copied from
   `package/Runtime/Plugins/Android/arm64-v8a/`: `libXREALNativeSessionManager.so`,
   `libXREALXRPlugin.so`, `libVulkanSupport.so`. `godot_xreal.gdextension` lists them under
   `[dependencies] android.arm64`, so Godot's Android export packs them next to the extension; the
   extension `dlopen`s them at startup.

2. **5 `.aar` → `addons/godot_xreal/android/`** — `nr_loader.aar`, `nr_api.aar`, `nr_common.aar`,
   `GlassesDisplayPlugEvent-2.4.2.aar`, `Log-Control-1.2.aar`, copied from
   `package/Runtime/Plugins/Android/`. The addon's export plugin
   (`addons/godot_xreal/export_plugin.gd`) ships them into the APK: they carry the Java/JNI layer +
   the merged manifest entries (glasses-detection `GlassesInitProvider` etc.), **and** the NR
   native libs at `jni/arm64-v8a/`, which Gradle merges into the APK — so those `.so` are *not*
   extracted into `jniLibs/` (APK-verified 2026-07-14):

   | .aar (`package/Runtime/Plugins/Android/`) | native libs delivered into the APK |
   |---|---|
   | `nr_api.aar`    | `libnr_api.so`, `libnr_plugin_6dof.so`, `libnr_rgb_camera.so` |
   | `nr_loader.aar` | `libnr_loader.so` |
   | `nr_common.aar` | `libnr_libusb.so` (plus many QNN/SNPE libs — unused, but they ride along) |

   `Log-Control` is **required** whenever `GlassesDisplayPlugEvent` ships (its provider references
   `com.xreal.logcontrol.LogControl`; missing it crashes the app before Godot starts).
   `nractivitylife*.aar` is deliberately excluded — its launcher is Unity-only.

3. **XrealBridge Java sources — no vendoring, no pre-compiled jar (since 2026-07-15).** The
   committed sources (`addons/godot_xreal/android/src/com/godot/game/*.java`) are staged into the
   gradle build template (`android/build/src/main/java/com/godot/game/`) by `export_plugin.gd`'s
   `_export_begin` and compiled by the export's Gradle run; `_export_end` removes them again so
   the template stays pristine. Requires the Android build template to be installed
   (Project > Install Android Build Template) — but the gradle export needs that anyway.

`scripts/build.ps1` / `scripts/build.sh` verify all of the above (the 3 core `.so` **and** the
addon's `.aar`) before an export and stop with these instructions if anything is missing.

## Lint

```bash
cargo fmt --all
cargo clippy --all-targets
```

## CI / automated release

Two GitHub Actions workflows (`.github/workflows/`):

- **`ci.yaml`** (push / PR to `main`) — `cargo fmt --check` + `cargo clippy -D warnings` (host), then
  builds the addon exactly as a release would: the Android arm64 GDExtension via `cargo ndk` (needs no
  vendored XREAL libs — they are `dlopen`'d at runtime) and the 6 desktop dummy stubs via `clang`+`lld`.
- **`release.yaml`** (`workflow_dispatch` with a `version` input) — bumps `plugin.cfg` + `Cargo.toml`,
  builds the same artifacts, tags, and publishes a GitHub Release. The release attaches
  `libgodot_xreal.so` plus a drop-in zip (`godot_xreal.gdextension` + `dummy/` + the built `.so` +
  `addons/godot_xreal/`, with an `INSTALL.md`). Users unzip it and vendor the XREAL libs — no Rust /
  cargo-ndk needed. Changelog is generated by git-cliff (`.github/cliff.toml`) from Conventional Commits.

Build targets = one Android `release` `.so` (both `android.debug`/`android.release` `.gdextension` slots
point at it, so even `--export-debug` deploys ship the optimized extension) + the editor-only desktop
stubs. The `.so` build is proprietary-free, so the whole pipeline runs on stock CI runners.
