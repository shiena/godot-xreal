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

```bash
rustup target add aarch64-linux-android
cargo install cargo-ndk
# ANDROID_NDK_HOME must point at an installed NDK
cargo ndk -t arm64-v8a -o ./jniLibs build --release
#   -> jniLibs/arm64-v8a/libgodot_xreal.so
#   -> target/aarch64-linux-android/release/libgodot_xreal.so  (referenced by the .gdextension)
```

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
