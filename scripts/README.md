# scripts/

Local dev pipeline for the godot-xreal GDExtension. The XREAL native libraries ship only for
Android arm64-v8a, so on-device testing always runs:

```
cargo ndk build  ->  Godot APK export  ->  adb install  ->  launch on the glasses
```

`build.ps1` (Windows / PowerShell) and `build.sh` (Git Bash) wrap all four stages and the two
workarounds this always needs: the Godot export hang, and the force-stop-before-launch requirement.
Relaunching an instance that has not fully stopped leaves the glasses black.

`vendor_xreal_libs.ps1` / `vendor_xreal_libs.sh` is the one-time prerequisite. It stages every
XREAL runtime piece (four `.so` → `addons/godot_xreal/jniLibs/arm64-v8a/`, seven `.aar` → `addons/godot_xreal/android/`,
with the aars also carrying the NR native libs into the APK) out of a local copy of the SDK
package, either the extracted `package/` dir or the `com.xreal.xr.tar.gz` archive itself, which it
extracts to a temp dir. The build scripts wrap it as `-Extract` / `--extract`. The export's gradle
build compiles the XrealBridge Java sources, so they need no vendoring step.

> No terminal? The addon ships an in-editor equivalent. The **XREAL Import** dock
> (`addons/godot_xreal/editor/vendor_import_dock.gd`) runs the same vendoring from a file dialog:
> pick `com.xreal.xr(.tgz|.tar.gz)` (or an extracted `package/` folder) and it extracts with the
> system `tar` and copies the same `.so`, `.aar`, and tool into place. The developer docs
> (indexed at `docs/develop/README.md`) cover the background.

`build_dummy_libs.ps1` / `build_dummy_libs.sh` builds the desktop stub libraries into `dummy/`.
These GDExtension stubs register empty Node-derived placeholder classes, so a desktop editor
neither errors on this Android-only extension nor warns on scenes placing those classes. They stay
out of the repo, so run this once after cloning. It cross-compiles all six desktop targets from any
host with just clang and lld; rerun it only when the dummy sources or the `entry_symbol` change.

`gen_stub_classes.ps1` / `gen_stub_classes.sh` regenerates the placeholder class list,
`dummy/stub_classes.inc`, from the `#[class(base = ...)]` declarations in `src/`. The matching
`build_dummy_libs` script runs it automatically, and `-Check` / `--check` verifies the committed
file. Keep the two scripts' output byte-identical when editing either one.

The two documentation generators take the doc comments as their single source of truth, and both
support `-Check` / `--check` to verify that the committed output is in sync:

- `gen_docs.ps1` / `gen_docs.sh` → `dummy/stub_docs.inc` + `dummy/stub_members.inc`, the **editor F1
  help** for the native classes (from the `///` comments through gdext's `register-docs`). Rust
  only, so CI can run it.
- `gen_api_docs.ps1` / `gen_api_docs.sh` → `docs/user/api/*.md`, the **web class reference** covering both
  the native classes and the GDScript feature components (whose `##` comments arrive through
  `godot --doctool --gdscript-docs`). This one needs a Godot 4.7 binary, so it runs locally and the
  pages are committed. The binary is a variable, resolved command line → environment → `godot` on
  PATH: `-Godot <path>` or `godot=<path>` (PS), `godot=<path>` or `--godot <path>` (sh), `GODOT=…`
  either way.

## Prerequisites (assumed installed and on PATH)

- **Rust + cargo-ndk**: `cargo install cargo-ndk`; `ANDROID_NDK_HOME` set (NDK r27).
- **adb**: use scrcpy's adb (v37). Mixing adb versions shuts down the server and drops the Wi-Fi link.
- **Godot 4.7-stable** (console binary): it must match the templates, and 4.8.dev fails with a
  version mismatch. The scripts call `godot` by default; override with `-Godot` / `$env:GODOT` (PS)
  or `GODOT=…` (sh) if it isn't on PATH under that name.
- **XREAL runtime pieces vendored**: the four `.so` in `addons/godot_xreal/jniLibs/arm64-v8a/` plus the seven `.aar`
  in `addons/godot_xreal/android/`, none of which are in the repo.
  `vendor_xreal_libs.ps1 -XrealPackage <…>/package` (or `-XrealPackage <…>/com.xreal.xr.tar.gz`,
  or the build scripts' `-Extract` / `--extract <tar.gz>`) stages all of them from a local copy of
  the XREAL SDK for Unity. The `-Export` / `--export` stage checks for them and prints the
  acquisition steps if anything is missing. See the main
  [README](../README.md#prerequisite-vendor-the-xreal-runtime-libraries).

## Usage

```powershell
# Windows
.\scripts\build.ps1 -Extract <…>\com.xreal.xr.tar.gz   # vendor the XREAL runtime libs (once)
.\scripts\build.ps1                       # build only (cargo ndk, release)
.\scripts\build.ps1 -All                  # build + export + install + run
.\scripts\build.ps1 -All -TrackingType 0
.\scripts\build.ps1 -Export -Install -Run # reuse the current .so
.\scripts\build.ps1 -Run -Logcat          # relaunch and stream [xreal] logs
```

```bash
# Git Bash
./scripts/build.sh --extract <…>/com.xreal.xr.tar.gz  # vendor the XREAL runtime libs (once)
./scripts/build.sh                        # build only
./scripts/build.sh --all                  # build + export + install + run
./scripts/build.sh --all --tracking 0
./scripts/build.sh --export --install --run
./scripts/build.sh --run --logcat
```

Stages run in order when combined: **extract → build → export → install → run → logcat**. With no
stage flag, only *build* runs (`--extract` alone just vendors). `--all` / `-All` = build + export +
install + run.

## Options

| PowerShell | Bash | Meaning |
|---|---|---|
| `-Build` `-Export` `-Install` `-Run` `-Logcat` | `--build` `--export` `--install` `--run` `--logcat` | pick stages |
| `-Extract <path>` | `--extract <path>` | vendor the XREAL runtime libs from `com.xreal.xr.tar.gz` (or the extracted `package/` dir) through `vendor_xreal_libs.ps1` |
| `-All` | `--all` | build + export + install + run |
| `-TrackingType <n>` | `--tracking <n>` | set `debug.xreal.tracking_type` before launch (0 = 6DoF, 1 = 3DoF, 2 = 0DoF) |
| `-ReleaseApk` | `--release-apk` | export with the release keystore (default: debug keystore) |
| `-CargoDebug` | `--cargo-debug` | cargo debug profile (default: release) |
| `-Checks` | `--checks` | run `cargo fmt --check` + `cargo clippy` before building (off by default) |
| `-Device <ip:port>` | `--device <ip:port>` | override the Wi-Fi device (default `192.168.0.4:5555`) |

Env overrides: `GODOT`, `ADB`, `XREAL_DEVICE`, `APK_OUT`, `EXPORT_PRESET`.

## Notes

- The APK exports to `../godot-build/godot-xreal.apk` (matches the export preset).
- The export runs headless and is **polled to completion** (fresh mtime + stable size + a valid ZIP
  EOCD) before the Godot process is stopped, because stopping it mid-write corrupts the APK.
- The recommended runtime config is **6DoF**: `-All -TrackingType 0`. Stereo is always Multipass.
