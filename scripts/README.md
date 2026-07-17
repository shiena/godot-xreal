# scripts/

Local dev pipeline for the godot-xreal GDExtension. The XREAL native libraries ship only for
Android arm64-v8a, so on-device testing is always:

```
cargo ndk build  ->  Godot APK export  ->  adb install  ->  launch on the glasses
```

`build.ps1` (Windows / PowerShell) and `build.sh` (Git Bash) wrap all four stages and the two
workarounds that bite every time — the Godot export hang, and the force-stop-before-launch
requirement (relaunching a not-fully-dead instance leaves the glasses black).

`vendor_xreal_libs.ps1` / `vendor_xreal_libs.sh` is the one-time prerequisite: it stages every
XREAL runtime piece (4 `.so` → `jniLibs/arm64-v8a/`, 7 `.aar` →
`addons/godot_xreal/android/`; the aars also carry the NR native libs into the APK) out of a
local copy of the SDK package — either the extracted `package/` dir or the `com.xreal.xr.tar.gz`
archive itself (auto-extracted to a temp dir). The build scripts wrap it as `-Extract` / `--extract`.
(The XrealBridge Java sources are compiled by the export's gradle build — no vendoring step.)

> No terminal? The addon ships an in-editor equivalent: the **`XREAL Import`** dock
> (`addons/godot_xreal/editor/vendor_import_dock.gd`) runs the same vendoring from a file dialog —
> pick `com.xreal.xr(.tgz|.tar.gz)` (or an extracted `package/` folder) and it extracts (system `tar`)
> and copies the same `.so`/`.aar`/tool into place. See `docs/guides/android-setup.md` §3.

`build_dummy_libs.ps1` / `build_dummy_libs.sh` builds the desktop stub libraries into `dummy/`
(GDExtension stubs that register empty Node-derived placeholder classes so a desktop editor
neither errors on this Android-only extension nor warns on scenes placing those classes). Not
committed — run once after cloning. Cross-compiles all six desktop targets from any host with
just clang + lld; rerun only if the dummy sources or the `entry_symbol` change.

`gen_stub_classes.ps1` / `gen_stub_classes.sh` regenerates `dummy/stub_classes.inc` — the
placeholder class list — from the `#[class(base = ...)]` declarations in `src/` (run
automatically by the matching `build_dummy_libs` script; `-Check` / `--check` verifies the
committed file). Keep the two scripts' output byte-identical when editing either.

## Prerequisites (assumed installed and on PATH)

- **Rust + cargo-ndk** — `cargo install cargo-ndk`; `ANDROID_NDK_HOME` set (NDK r27).
- **adb** — use scrcpy's adb (v37). Mixing adb versions kills the server and drops the Wi-Fi link.
- **Godot 4.7-stable** (console binary) — template match; 4.8.dev fails with a version mismatch.
  The scripts call `godot` by default; override with `-Godot` / `$env:GODOT` (PS) or `GODOT=…` (sh)
  if it isn't on PATH under that name.
- **XREAL runtime pieces vendored** — the 4 `.so` in `jniLibs/arm64-v8a/` plus the 7 `.aar`
  in `addons/godot_xreal/android/`; none are in the repo.
  `vendor_xreal_libs.ps1 -XrealPackage <…>/package` (or `-XrealPackage <…>/com.xreal.xr.tar.gz`,
  or the build scripts' `-Extract` / `--extract <tar.gz>`) stages all of them from a local copy of
  the XREAL SDK for Unity. The `-Export` / `--export` stage checks for them and
  prints the acquisition steps if anything is missing. See the main
  [README](../README.md#prerequisite-vendor-the-xreal-runtime-libraries).

## Usage

```powershell
# Windows
.\scripts\build.ps1 -Extract <…>\com.xreal.xr.tar.gz   # vendor the XREAL runtime libs (once)
.\scripts\build.ps1                       # build only (cargo ndk, release)
.\scripts\build.ps1 -All                  # build + export + install + run
.\scripts\build.ps1 -All -StereoMode 0 -TrackingType 0
.\scripts\build.ps1 -Export -Install -Run # reuse the current .so
.\scripts\build.ps1 -Run -Logcat          # relaunch and stream [xreal] logs
```

```bash
# Git Bash
./scripts/build.sh --extract <…>/com.xreal.xr.tar.gz  # vendor the XREAL runtime libs (once)
./scripts/build.sh                        # build only
./scripts/build.sh --all                  # build + export + install + run
./scripts/build.sh --all --stereo 0 --tracking 0
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
| `-Extract <path>` | `--extract <path>` | vendor the XREAL runtime libs from `com.xreal.xr.tar.gz` (or the extracted `package/` dir) via `vendor_xreal_libs.ps1` |
| `-All` | `--all` | build + export + install + run |
| `-StereoMode <n>` | `--stereo <n>` | set `debug.xreal.stereo_mode` before launch (0 = Multipass, 2 = Multiview) |
| `-TrackingType <n>` | `--tracking <n>` | set `debug.xreal.tracking_type` before launch (0 = 6DoF, 1 = 3DoF, 2 = 0DoF) |
| `-ReleaseApk` | `--release-apk` | export with the release keystore (default: debug keystore) |
| `-CargoDebug` | `--cargo-debug` | cargo debug profile (default: release) |
| `-Checks` | `--checks` | run `cargo fmt --check` + `cargo clippy` before building (off by default) |
| `-Device <ip:port>` | `--device <ip:port>` | override the Wi-Fi device (default `192.168.0.4:5555`) |

Env overrides: `GODOT`, `ADB`, `XREAL_DEVICE`, `APK_OUT`, `EXPORT_PRESET`.

## Notes

- The APK exports to `../godot-build/godot-xreal.apk` (matches the export preset).
- The export runs headless and is **polled to completion** (fresh mtime + stable size + a valid ZIP
  EOCD) before the Godot process is killed — killing mid-write corrupts the APK.
- The recommended runtime config is **6DoF + Multipass**: `-All -StereoMode 0 -TrackingType 0`.
