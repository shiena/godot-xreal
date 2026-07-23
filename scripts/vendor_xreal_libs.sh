#!/usr/bin/env bash
# Vendor the XREAL runtime libraries the Android export needs out of the Unity `com.xreal.xr`
# package, in one go (all destinations are git-ignored). POSIX twin of vendor_xreal_libs.ps1,
# used by build.sh on non-Windows platforms:
#
#   - 3 core .so       -> jniLibs/arm64-v8a/           (copied; dlopen'd by the GDExtension,
#                                                        packed via godot_xreal.gdextension)
#   - 7 .aar           -> addons/godot_xreal/android/  (shipped into the APK by export_plugin.gd:
#                                                        Java/JNI layer + manifest merge; Gradle
#                                                        also merges each .aar's jni/arm64-v8a/*.so
#                                                        — the NR libs — into the APK, so they are
#                                                        NOT extracted separately)
#   - nr_plugins.json  -> addons/godot_xreal/android/  (NR perception manifest; makes the loader
#                                                        load libnr_image_tracking.so — staged into
#                                                        the APK's assets/ by export_plugin.gd)
#   - trackableImageTools -> addons/godot_xreal/tools/ (host build tool, NOT in the APK: generates
#                                                        the image-tracking DB blob from PNGs)
#
# Extraction only — the XrealBridge Java sources are compiled by the export's gradle build
# (export_plugin.gd stages them into the build template), not here.
#
# Nothing is downloaded — you supply a local copy of the package. nractivitylife*.aar is
# DELIBERATELY EXCLUDED: its NRXRActivity/NRXRApp launcher is Unity-specific (instantiates
# com.unity3d.player.UnityPlayer) and must not ship in a Godot app. See docs/guides/android-setup.md.
#
# Usage:
#   ./scripts/vendor_xreal_libs.sh <package-root-or-com.xreal.xr.tar.gz>
#
#   <package>   either the Unity package root (the folder containing Runtime/Plugins/Android)
#               or the com.xreal.xr.tar.gz archive itself — the archive is extracted to a temp
#               dir (removed afterwards) and its `package/` root is used

set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

warn() { echo -e "\033[33mWARNING: $*\033[0m" >&2; }
die()  { echo -e "\033[31m$*\033[0m" >&2; exit 1; }

pkg=""
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)  sed -n '2,26p' "$0"; exit 0 ;;
        *)          [ -n "$pkg" ] && die "Unexpected argument: $1"; pkg="$1" ;;
    esac
    shift
done
[ -n "$pkg" ] || die "Usage: $0 <package-root-or-com.xreal.xr.tar.gz>"

# --- 0) Accept the tar.gz directly: extract to a temp dir (cleaned up on exit) and use
#        its `package/` root.
temp_extract=""
trap '[ -n "$temp_extract" ] && rm -rf "$temp_extract"' EXIT

if [ -f "$pkg" ]; then
    case "$pkg" in
        *.tar.gz|*.tgz) ;;
        *) die "package argument must be the package root directory or a .tar.gz archive: $pkg" ;;
    esac
    temp_extract="$(mktemp -d "${TMPDIR:-/tmp}/xreal_pkg_XXXXXXXX")"
    echo "extracting $pkg ..."
    tar -xzf "$pkg" -C "$temp_extract" || die "tar -xzf failed: $pkg"
    if [ -d "$temp_extract/package" ]; then
        pkg="$temp_extract/package"
    else
        # No `package/` root — fall back to whichever top-level dir holds Runtime/Plugins/Android.
        pkg=""
        for d in "$temp_extract"/*/; do
            if [ -d "${d}Runtime/Plugins/Android" ]; then pkg="${d%/}"; break; fi
        done
        [ -n "$pkg" ] || die "No package root with Runtime/Plugins/Android found inside the archive."
    fi
fi

src_android="$pkg/Runtime/Plugins/Android"
src_abi="$src_android/arm64-v8a"
[ -d "$src_abi" ] || die "Not found: $src_abi  (is the argument the com.xreal.xr/package root?)"

# --- Version gate: refuse packages older than MIN_VERSION. The Rust internal-call offsets
#     (hand tracking / depth mesh / signal_guard patches) were RE'd against this version; an older
#     package can crash. Read from the Unity UPM manifest (package.json).
MIN_VERSION="3.1.0"
pkg_json="$pkg/package.json"
[ -f "$pkg_json" ] || die "package.json not found in $pkg (is this a com.xreal.xr package?)"
pkg_ver="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$pkg_json" | head -1)"
[ -n "$pkg_ver" ] || die "Could not read \"version\" from $pkg_json"
# Compare the numeric core (strip any -pre / +build suffix) via sort -V.
ver_core="${pkg_ver%%[-+]*}"
if [ "$ver_core" != "$MIN_VERSION" ] && \
   [ "$(printf '%s\n%s\n' "$ver_core" "$MIN_VERSION" | sort -V | head -1)" = "$ver_core" ]; then
    die "com.xreal.xr $pkg_ver is too old — this addon needs $MIN_VERSION or newer (the native offsets were RE'd against $MIN_VERSION; an older package can crash). Nothing was vendored."
fi
echo "com.xreal.xr version $pkg_ver (>= $MIN_VERSION) — ok"

jni_dir="$repo_root/jniLibs/arm64-v8a"
addon_dir="$repo_root/addons/godot_xreal/android"
mkdir -p "$jni_dir" "$addon_dir"

# --- 1) 3 core .so -> jniLibs/arm64-v8a (dlopen'd; listed in godot_xreal.gdextension [dependencies])
core_libs=(libXREALNativeSessionManager.so libXREALXRPlugin.so libVulkanSupport.so)
for lib in "${core_libs[@]}"; do
    if [ ! -f "$src_abi/$lib" ]; then warn "Missing in package: $lib"; continue; fi
    cp -f "$src_abi/$lib" "$jni_dir/$lib"
    echo "so   $lib"
done

# libmedia_codec.so lives under the Camera Features plugin path (the FPV HW encoder — see
# docs/plans/fpv-streaming-plan.md). Copy it into jniLibs too (listed in godot_xreal.gdextension).
media_codec_src="$pkg/Runtime/Scripts/Android/Camera Features/Plugins/Android/arm64/libmedia_codec.so"
if [ -f "$media_codec_src" ]; then
    cp -f "$media_codec_src" "$jni_dir/libmedia_codec.so"
    echo "so   libmedia_codec.so"
else
    warn "Missing in package: ${media_codec_src#"$pkg/"}"
fi

# --- 2) 7 .aar -> addons/godot_xreal/android (shipped by export_plugin.gd _get_android_libraries;
#        the exact file names are hardcoded there). Besides the Java/JNI layer + manifest merge,
#        the aars carry the NR native libs at jni/arm64-v8a/ (nr_api.aar: libnr_api.so /
#        libnr_plugin_6dof.so / libnr_rgb_camera.so, nr_loader.aar: libnr_loader.so,
#        nr_common.aar: libnr_libusb.so, nr_spatial_anchor.aar: libnr_spatial_anchor.so, nr_image_tracking.aar: libnr_image_tracking.so) — Gradle
#        merges those into the APK.
#
# Log-Control is REQUIRED whenever GlassesDisplayPlugEvent ships: its GlassesInitProvider
# (a ContentProvider that auto-runs at app startup) references com.xreal.logcontrol.LogControl,
# and without it the app crashes before Godot starts with
# NoClassDefFoundError: com/xreal/logcontrol/LogControl. (Device-confirmed 2026-06-15.)
aars=(nr_loader.aar nr_api.aar nr_common.aar nr_spatial_anchor.aar nr_image_tracking.aar GlassesDisplayPlugEvent-2.4.2.aar Log-Control-1.2.aar)
for aar in "${aars[@]}"; do
    if [ ! -f "$src_android/$aar" ]; then warn "Missing in package: $aar"; continue; fi
    cp -f "$src_android/$aar" "$addon_dir/$aar"
    echo "aar  $aar"
done

# nr_plugins.json (NR perception manifest) -> addons/godot_xreal/android. Lives under Marker~/ in the
# package but is the image-tracking backend manifest (loads libnr_image_tracking.so); staged into the
# APK's assets/ by export_plugin.gd. SDK-derived + may change across SDK versions, so vendor it (do NOT
# commit) alongside the .aar.
nr_plugins_src="$pkg/Marker~/nr_plugins.json"
if [ -f "$nr_plugins_src" ]; then
    cp -f "$nr_plugins_src" "$addon_dir/nr_plugins.json"
    echo "json nr_plugins.json"
else
    warn "Missing in package: Marker~/nr_plugins.json"
fi

# --- 3) Host build tool -> addons/godot_xreal/tools/ (NOT shipped in the APK): trackableImageTools
#        generates the image-tracking reference-image DB blob from PNGs at build time (see
#        docs/plans/ar-features-plan.md). Only a MacOS binary exists in the POSIX package layout; on
#        Linux there is no prebuilt tool (build the blob on a Mac/Windows host, or skip image tracking).
tools_dir="$repo_root/addons/godot_xreal/tools"
mkdir -p "$tools_dir"
case "$(uname -s)" in
    Darwin) tool_src="$pkg/Tools~/MacOS/trackableImageTools" ;;
    *)      tool_src=""; warn "trackableImageTools: no prebuilt tool for $(uname -s) (Windows/MacOS only)" ;;
esac
if [ -n "$tool_src" ]; then
    if [ -f "$tool_src" ]; then
        cp -f "$tool_src" "$tools_dir/trackableImageTools"
        chmod +x "$tools_dir/trackableImageTools"
        echo "tool trackableImageTools"
    else
        warn "Missing in package: ${tool_src#"$pkg/"}"
    fi
fi

# --- Final verification: everything the export needs (build.ps1/build.sh check the same lists).
missing=()
for lib in "${core_libs[@]}"; do
    [ -f "$jni_dir/$lib" ] || missing+=("jniLibs/arm64-v8a/$lib")
done
for f in "${aars[@]}"; do
    [ -f "$addon_dir/$f" ] || missing+=("addons/godot_xreal/android/$f")
done
[ -f "$addon_dir/nr_plugins.json" ] || missing+=("addons/godot_xreal/android/nr_plugins.json")

echo ""
if [ ${#missing[@]} -gt 0 ]; then
    echo -e "\033[31mINCOMPLETE — still missing:\033[0m"
    printf '  - %s\n' "${missing[@]}"
    exit 1
fi
echo -e "\033[32mDone: 3 core .so -> jniLibs/arm64-v8a, 7 .aar + nr_plugins.json -> addons/godot_xreal/android, trackableImageTools -> addons/godot_xreal/tools.\033[0m"
echo "(NR .so ship via the .aar; nractivitylife*.aar deliberately excluded — Unity-only launcher.)"
