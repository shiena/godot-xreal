# scripts/

Local dev pipeline for the godot-xreal GDExtension. The XREAL native libraries ship only for
Android arm64-v8a, so on-device testing is always:

```
cargo ndk build  ->  Godot APK export  ->  adb install  ->  launch on the glasses
```

`build.ps1` (Windows / PowerShell) and `build.sh` (Git Bash) wrap all four stages and the two
workarounds that bite every time — the Godot export hang, and the force-stop-before-launch
requirement (relaunching a not-fully-dead instance leaves the glasses black).

## Prerequisites (assumed installed and on PATH)

- **Rust + cargo-ndk** — `cargo install cargo-ndk`; `ANDROID_NDK_HOME` set (NDK r27).
- **adb** — use scrcpy's adb (v37). Mixing adb versions kills the server and drops the Wi-Fi link.
- **Godot 4.7-stable** (console binary) — template match; 4.8.dev fails with a version mismatch.
  The scripts call `godot` by default; override with `-Godot` / `$env:GODOT` (PS) or `GODOT=…` (sh)
  if it isn't on PATH under that name.

## Usage

```powershell
# Windows
.\scripts\build.ps1                       # build only (cargo ndk, release)
.\scripts\build.ps1 -All                  # build + export + install + run
.\scripts\build.ps1 -All -StereoMode 0 -TrackingType 0
.\scripts\build.ps1 -Export -Install -Run # reuse the current .so
.\scripts\build.ps1 -Run -Logcat          # relaunch and stream [xreal] logs
```

```bash
# Git Bash
./scripts/build.sh                        # build only
./scripts/build.sh --all                  # build + export + install + run
./scripts/build.sh --all --stereo 0 --tracking 0
./scripts/build.sh --export --install --run
./scripts/build.sh --run --logcat
```

Stages run in order when combined: **build → export → install → run → logcat**. With no stage flag,
only *build* runs. `--all` / `-All` = build + export + install + run.

## Options

| PowerShell | Bash | Meaning |
|---|---|---|
| `-Build` `-Export` `-Install` `-Run` `-Logcat` | `--build` `--export` `--install` `--run` `--logcat` | pick stages |
| `-All` | `--all` | build + export + install + run |
| `-StereoMode <n>` | `--stereo <n>` | set `debug.xreal.stereo_mode` before launch (0 = Multipass, 2 = Multiview) |
| `-TrackingType <n>` | `--tracking <n>` | set `debug.xreal.tracking_type` before launch (0 = 6DoF, 1 = 3DoF, 2 = 0DoF) |
| `-ReleaseApk` | `--release-apk` | export with the release keystore (default: debug keystore) |
| `-CargoDebug` | `--cargo-debug` | cargo debug profile (default: release) |
| `-Clippy` | `--clippy` | run `cargo clippy` before building (off by default) |
| `-Device <ip:port>` | `--device <ip:port>` | override the Wi-Fi device (default `192.168.0.4:5555`) |

Env overrides: `GODOT`, `ADB`, `XREAL_DEVICE`, `APK_OUT`, `EXPORT_PRESET`.

## Notes

- The APK exports to `../godot-build/godot-xreal.apk` (matches the export preset).
- The export runs headless and is **polled to completion** (fresh mtime + stable size + a valid ZIP
  EOCD) before the Godot process is killed — killing mid-write corrupts the APK.
- The recommended runtime config is **6DoF + Multipass**: `-All -StereoMode 0 -TrackingType 0`.
