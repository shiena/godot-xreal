#!/usr/bin/env bash
# godot-xreal local dev pipeline (Git Bash / MSYS on Windows).
#
# The XREAL native libraries ship only for Android arm64-v8a, so on-device testing means:
#   cargo ndk build  ->  Godot APK export  ->  adb install  ->  launch on the glasses.
# This wraps all four stages plus the two workarounds that bite every time: the Godot export
# hang, and the force-stop-before-launch requirement.
#
# Assumes the toolchain is installed and on PATH: cargo + cargo-ndk, adb (scrcpy's v37), and a
# Godot 4.7-stable console binary (see GODOT below), with ANDROID_NDK_HOME set for cargo-ndk.
#
# Usage:
#   ./scripts/build.sh                        # build only (cargo ndk, release)
#   ./scripts/build.sh --extract <com.xreal.xr.tar.gz>  # vendor the XREAL runtime libs from the SDK
#   ./scripts/build.sh --all                  # build + export + install + run
#   ./scripts/build.sh --all --stereo 0 --tracking 0   # + set device props first
#   ./scripts/build.sh --export --install --run        # reuse the current .so
#   ./scripts/build.sh --run --logcat         # relaunch and stream [xreal] logs
#   ./scripts/build.sh --install --run --release-apk
#
# Stages run in order when combined: extract -> build -> export -> install -> run -> logcat.
# With no stage flag, only build runs (--extract alone just vendors). --all = build+export+install+run.
# Env overrides: GODOT (default: godot), ADB (default: adb), XREAL_DEVICE, APK_OUT, EXPORT_PRESET.

set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

GODOT="${GODOT:-godot}"                       # a Godot 4.7-stable binary on PATH (name or full path)
ADB="${ADB:-adb}"                             # scrcpy's adb v37, on PATH
DEVICE="${XREAL_DEVICE:-192.168.0.4:5555}"    # Wi-Fi device; empty = whatever `adb` is attached to
APK_OUT="${APK_OUT:-$repo_root/../godot-build/godot-xreal.apk}"
PRESET="${EXPORT_PRESET:-Android}"
PKG="com.example.godotxreal"
ACTIVITY="$PKG/com.godot.game.GodotAppLauncher"

do_build=0; do_export=0; do_install=0; do_run=0; do_logcat=0
release_apk=0; cargo_debug=0; run_checks=0
stereo=-1; tracking=-1; extract=""

while [ $# -gt 0 ]; do
    case "$1" in
        --all)         do_build=1; do_export=1; do_install=1; do_run=1 ;;
        --build)       do_build=1 ;;
        --export)      do_export=1 ;;
        --install)     do_install=1 ;;
        --run)         do_run=1 ;;
        --logcat)      do_logcat=1 ;;
        --release-apk) release_apk=1 ;;
        --cargo-debug) cargo_debug=1 ;;
        --checks)      run_checks=1 ;;
        --extract)     extract="$2"; shift ;;
        --stereo)      stereo="$2"; shift ;;
        --tracking)    tracking="$2"; shift ;;
        --device)      DEVICE="$2"; shift ;;
        -h|--help)     sed -n '2,26p' "$0"; exit 0 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
    shift
done
[ $((do_build+do_export+do_install+do_run+do_logcat)) -eq 0 ] && [ -z "$extract" ] && do_build=1

say() { echo -e "\033[36m>> $*\033[0m"; }
ok()  { echo -e "\033[32m$*\033[0m"; }
die() { echo -e "\033[31m$*\033[0m" >&2; exit 1; }
# adb, optionally targeting a specific device (-s) when XREAL_DEVICE is set.
adbx() { if [ -n "$DEVICE" ]; then "$ADB" -s "$DEVICE" "$@"; else "$ADB" "$@"; fi; }

# The XREAL runtime pieces the APK must bundle. They are NOT in this repo — you vendor them from
# the XREAL SDK for Unity (see README / the guide printed below):
#   - 3 core .so in jniLibs/arm64-v8a (packed via godot_xreal.gdextension [dependencies])
#   - 5 .aar in addons/godot_xreal/android (shipped into the APK by the addon's export_plugin.gd:
#     Java/JNI layer + manifest merge; the aars also carry the NR native libs, which Gradle
#     merges into the APK)
# The XrealBridge Java sources are compiled by the export's gradle build (export_plugin.gd
# stages them into the build template) — nothing to vendor for those.
# This checks both before an export and stops with instructions if anything is missing; it never
# downloads anything.
REQUIRED_LIBS=(libXREALNativeSessionManager.so libXREALXRPlugin.so libVulkanSupport.so libmedia_codec.so)
REQUIRED_ADDON_FILES=(nr_loader.aar nr_api.aar nr_common.aar nr_spatial_anchor.aar nr_image_tracking.aar \
                      GlassesDisplayPlugEvent-2.4.2.aar Log-Control-1.2.aar)
require_vendored_libs() {
    local jni_dir="$repo_root/jniLibs/arm64-v8a" addon_dir="$repo_root/addons/godot_xreal/android" missing=()
    local f
    for f in "${REQUIRED_LIBS[@]}"; do [ -f "$jni_dir/$f" ] || missing+=("jniLibs/arm64-v8a/$f"); done
    for f in "${REQUIRED_ADDON_FILES[@]}"; do [ -f "$addon_dir/$f" ] || missing+=("addons/godot_xreal/android/$f"); done
    [ ${#missing[@]} -eq 0 ] && return 0
    {
        echo -e "\033[31mMissing vendored XREAL runtime pieces:\033[0m"
        printf '  - %s\n' "${missing[@]}"
        cat <<'GUIDE'

These ship with the XREAL SDK for Unity (com.xreal.xr) and are NOT included in this repo.
Vendor them once from a local copy of the package (nothing is downloaded):
  1. Obtain the XREAL SDK for Unity package `com.xreal.xr.tar.gz` and extract it (-> a `package/` dir).
  2. Run  scripts/vendor_xreal_libs.sh <...>/package
     (on Windows: pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <...>) which stages everything:
       - 3 core .so -> jniLibs/arm64-v8a/      (dlopen'd by the GDExtension)
       - 5 .aar -> addons/godot_xreal/android/ (shipped by the addon's export plugin; they also
         carry the NR native libs, which Gradle merges into the APK)
     (The XrealBridge Java sources are compiled by the export's gradle build — no JDK step here.)
See the README "Prerequisite: vendor the XREAL runtime libraries" and docs/guides/build-and-release.md.
GUIDE
    } >&2
    exit 1
}

profile=release; [ "$cargo_debug" -eq 1 ] && profile=debug

# --------------------------------------------------- extract (vendor XREAL runtime libs) ---
if [ -n "$extract" ]; then
    say "vendor XREAL runtime libs from $extract"
    bash "$repo_root/scripts/vendor_xreal_libs.sh" "$extract" || die "vendoring failed"
fi

# Fail fast (before a long build) if an export is requested but the XREAL runtime libs aren't vendored.
[ "$do_export" -eq 1 ] && require_vendored_libs

# ---------------------------------------------------------------- build (cargo ndk) ---
if [ "$do_build" -eq 1 ]; then
    if [ "$run_checks" -eq 1 ]; then
        say "cargo fmt --check"; cargo fmt --check || die "cargo fmt --check failed (run: cargo fmt)"
        say "cargo clippy --release"; cargo clippy --release || die "cargo clippy failed"
    fi
    say "cargo ndk -t arm64-v8a build ($profile)"
    ndk_args=(ndk -t arm64-v8a -o ./jniLibs build)
    [ "$cargo_debug" -eq 0 ] && ndk_args+=(--release)
    cargo "${ndk_args[@]}" || die "cargo ndk build failed"
    so="$repo_root/jniLibs/arm64-v8a/libgodot_xreal.so"
    [ -f "$so" ] || die "Build artifact not found: $so"
    ok "Built: $so"
fi

# ------------------------------------------------------- export APK (with hang poll) ---
if [ "$do_export" -eq 1 ]; then
    ver="$("$GODOT" --version 2>/dev/null | head -1)"
    case "$ver" in 4.7*) ;; *) die "Godot must be 4.7-stable (template match); \`$GODOT --version\` = '$ver'. Set GODOT to a 4.7 binary." ;; esac
    mkdir -p "$(dirname "$APK_OUT")"
    flag="--export-debug"; [ "$release_apk" -eq 1 ] && flag="--export-release"

    # Godot's Android export writes the APK then HANGS instead of exiting: run it detached and poll
    # for a completed APK (fresh mtime + stable size + a valid ZIP EOCD 50 4B 05 06), then kill it.
    # Killing mid-write corrupts the APK (INSTALL_PARSE_FAILED_NOT_APK).
    start=$(date +%s)
    win_root="$(cygpath -w "$repo_root" 2>/dev/null || echo "$repo_root")"
    win_out="$(cygpath -w "$APK_OUT" 2>/dev/null || echo "$APK_OUT")"
    say "Godot export ($flag \"$PRESET\") -> $APK_OUT"
    "$GODOT" --headless --path "$win_root" "$flag" "$PRESET" "$win_out" >/dev/null 2>&1 &
    gpid=$!
    prev=-1; stable=0; done=0
    for _ in $(seq 1 60); do
        sleep 4
        [ -f "$APK_OUT" ] || continue
        mtime=$(stat -c %Y "$APK_OUT" 2>/dev/null || echo 0)
        size=$(stat -c %s "$APK_OUT" 2>/dev/null || echo 0)
        [ "$mtime" -lt "$start" ] && continue
        if [ "$size" -eq "$prev" ] && [ "$size" -gt 104857600 ]; then stable=$((stable+1)); else stable=0; fi
        prev=$size
        if [ "$stable" -ge 2 ] && tail -c 100000 "$APK_OUT" | xxd | grep -q "504b 0506"; then done=1; break; fi
    done
    kill "$gpid" 2>/dev/null
    # Godot's export process double-forks; make sure the headless editor is gone.
    if command -v powershell.exe >/dev/null 2>&1; then
        powershell.exe -NoProfile -Command "Get-Process | Where-Object {\$_.Path -like '*$(basename "$GODOT")*' -or \$_.Name -like 'Godot*'} | Stop-Process -Force" 2>/dev/null
    fi
    [ "$done" -eq 1 ] || die "APK export did not complete (no fresh, valid APK at $APK_OUT). Check the preset / keystore."
    ok "Exported: $APK_OUT ($(stat -c %s "$APK_OUT") bytes)"
fi

# ---------------------------------------------------------------- install (adb) ---
if [ "$do_install" -eq 1 ]; then
    [ -f "$APK_OUT" ] || die "APK not found: $APK_OUT (run with --export first)"
    [ -n "$DEVICE" ] && "$ADB" connect "$DEVICE" >/dev/null 2>&1
    say "adb install -r $APK_OUT"
    out="$(adbx install -r "$APK_OUT" 2>&1)"
    echo "$out" | grep -q "Success" || die "install failed: $out"
    ok "Installed."
fi

# ---------------------------------------------------------------- run (launch) ---
if [ "$do_run" -eq 1 ]; then
    [ -n "$DEVICE" ] && "$ADB" connect "$DEVICE" >/dev/null 2>&1
    [ "$stereo" -ge 0 ]   && { adbx shell setprop debug.xreal.stereo_mode "$stereo";     say "setprop debug.xreal.stereo_mode $stereo"; }
    [ "$tracking" -ge 0 ] && { adbx shell setprop debug.xreal.tracking_type "$tracking"; say "setprop debug.xreal.tracking_type $tracking"; }
    # Force-stop first: relaunching a not-fully-dead instance leaves the XR display registration
    # stuck ("graphics-thread callbacks registered as null") and the glasses stay black.
    adbx shell am force-stop "$PKG" >/dev/null 2>&1
    sleep 1
    say "am start $ACTIVITY"
    adbx shell am start -n "$ACTIVITY" >/dev/null 2>&1
    ok "Launched (put on the glasses)."
fi

# ---------------------------------------------------------------- logcat ([xreal]) ---
if [ "$do_logcat" -eq 1 ]; then
    [ -n "$DEVICE" ] && "$ADB" connect "$DEVICE" >/dev/null 2>&1
    say "streaming [xreal] logs (Ctrl-C to stop)"
    adbx logcat -v time | grep --line-buffered -E "\[xreal\]"
fi
