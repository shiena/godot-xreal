#!/usr/bin/env bash
# Vendor everything the Android export needs out of the Unity `com.xreal.xr` package, in one
# go (all destinations are git-ignored). POSIX twin of vendor_xreal_libs.ps1, used by build.sh
# on non-Windows platforms:
#
#   - 3 core .so       -> jniLibs/arm64-v8a/           (copied; dlopen'd by the GDExtension,
#                                                        packed via godot_xreal.gdextension)
#   - 5 .aar           -> addons/godot_xreal/android/  (shipped into the APK by export_plugin.gd:
#                                                        Java/JNI layer + manifest merge; Gradle
#                                                        also merges each .aar's jni/arm64-v8a/*.so
#                                                        — the NR libs — into the APK, so they are
#                                                        NOT extracted separately)
#   - xreal_bridge.jar -> addons/godot_xreal/android/  (compiled from the committed Java source;
#                                                        needs a JDK `javac` + an Android SDK
#                                                        platform `android.jar`)
#
# Nothing is downloaded — you supply a local copy of the package. nractivitylife*.aar is
# DELIBERATELY EXCLUDED: its NRXRActivity/NRXRApp launcher is Unity-specific (instantiates
# com.unity3d.player.UnityPlayer) and must not ship in a Godot app. See docs/android-setup.md.
#
# Usage:
#   ./scripts/vendor_xreal_libs.sh <package-root-or-com.xreal.xr.tar.gz> [--skip-jar]
#
#   <package>   either the Unity package root (the folder containing Runtime/Plugins/Android)
#               or the com.xreal.xr.tar.gz archive itself — the archive is extracted to a temp
#               dir (removed afterwards) and its `package/` root is used
#   --skip-jar  skip compiling xreal_bridge.jar (e.g. no JDK; an already-built jar is kept)

set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

warn() { echo -e "\033[33mWARNING: $*\033[0m" >&2; }
die()  { echo -e "\033[31m$*\033[0m" >&2; exit 1; }

pkg=""; skip_jar=0
while [ $# -gt 0 ]; do
    case "$1" in
        --skip-jar) skip_jar=1 ;;
        -h|--help)  sed -n '2,28p' "$0"; exit 0 ;;
        *)          [ -n "$pkg" ] && die "Unexpected argument: $1"; pkg="$1" ;;
    esac
    shift
done
[ -n "$pkg" ] || die "Usage: $0 <package-root-or-com.xreal.xr.tar.gz> [--skip-jar]"

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

# --- 2) 5 .aar -> addons/godot_xreal/android (shipped by export_plugin.gd _get_android_libraries;
#        the exact file names are hardcoded there). Besides the Java/JNI layer + manifest merge,
#        the aars carry the NR native libs at jni/arm64-v8a/ (nr_api.aar: libnr_api.so /
#        libnr_plugin_6dof.so / libnr_rgb_camera.so, nr_loader.aar: libnr_loader.so,
#        nr_common.aar: libnr_libusb.so) — Gradle merges those into the APK.
#
# Log-Control is REQUIRED whenever GlassesDisplayPlugEvent ships: its GlassesInitProvider
# (a ContentProvider that auto-runs at app startup) references com.xreal.logcontrol.LogControl,
# and without it the app crashes before Godot starts with
# NoClassDefFoundError: com/xreal/logcontrol/LogControl. (Device-confirmed 2026-06-15.)
aars=(nr_loader.aar nr_api.aar nr_common.aar GlassesDisplayPlugEvent-2.4.2.aar Log-Control-1.2.aar)
for aar in "${aars[@]}"; do
    if [ ! -f "$src_android/$aar" ]; then warn "Missing in package: $aar"; continue; fi
    cp -f "$src_android/$aar" "$addon_dir/$aar"
    echo "aar  $aar"
done

# --- 3) xreal_bridge.jar -> addons/godot_xreal/android, compiled from the committed Java source.
#        Needs javac/jar (a JDK) and an Android SDK platform android.jar for the classpath.
jar_out="$addon_dir/xreal_bridge.jar"
if [ "$skip_jar" -eq 0 ]; then
    javac_bin="$(command -v javac || true)"
    android_jar=""
    for sdk in "${ANDROID_HOME:-}" "${ANDROID_SDK_ROOT:-}" "$HOME/Android/Sdk" "$HOME/Library/Android/sdk"; do
        [ -n "$sdk" ] && [ -d "$sdk/platforms" ] || continue
        # Highest numbered platforms/android-NN that actually contains android.jar.
        android_jar="$(for d in "$sdk"/platforms/android-*/; do
                [ -f "${d}android.jar" ] || continue
                n="$(basename "$d" | tr -cd '0-9')"
                printf '%s %s\n' "${n:-0}" "${d}android.jar"
            done | sort -rn | head -1 | cut -d' ' -f2-)"
        [ -n "$android_jar" ] && break
    done
    if [ -z "$javac_bin" ] || [ -z "$android_jar" ]; then
        why="javac (JDK) not on PATH"
        [ -n "$javac_bin" ] && why="no android.jar under ANDROID_HOME/ANDROID_SDK_ROOT/~/Android/Sdk/~/Library/Android/sdk"
        if [ -f "$jar_out" ]; then
            warn "xreal_bridge.jar NOT rebuilt ($why) — keeping the existing jar."
        else
            warn "Cannot build xreal_bridge.jar ($why). Build it manually:
  javac -encoding UTF-8 -source 8 -target 8 -classpath <sdk>/platforms/android-NN/android.jar \\
    -d out addons/godot_xreal/android/src/com/godot/game/*.java
  jar cf addons/godot_xreal/android/xreal_bridge.jar -C out ."
        fi
    else
        jar_tool="$(dirname "$javac_bin")/jar"
        [ -x "$jar_tool" ] || jar_tool="jar"
        # The sources are Java-8-compatible; a JDK 8 javac rejects `-source 11` outright, so
        # match the language level to the installed JDK (8 on JDK 8, 11 on JDK 11+).
        javac_ver="$("$javac_bin" -version 2>&1 | awk '{print $2}')"   # e.g. 1.8.0_492 / 17.0.9 / 21
        major="${javac_ver%%.*}"
        [ "$major" = "1" ] && major="$(printf '%s' "$javac_ver" | cut -d. -f2)"
        lang=11
        [ "${major:-11}" -lt 11 ] 2>/dev/null && lang=8
        classes_dir="$(mktemp -d "${TMPDIR:-/tmp}/xreal_bridge_XXXXXXXX")"
        (
            set -e
            "$javac_bin" -encoding UTF-8 -source "$lang" -target "$lang" -nowarn \
                -classpath "$android_jar" -d "$classes_dir" \
                "$addon_dir"/src/com/godot/game/*.java
            "$jar_tool" cf "$jar_out" -C "$classes_dir" .
        )
        rc=$?
        rm -rf "$classes_dir"
        [ "$rc" -eq 0 ] || die "building xreal_bridge.jar failed (exit $rc)"
        echo "jar  xreal_bridge.jar  (classpath $android_jar)"
    fi
fi

# --- Final verification: everything the export needs (build.ps1/build.sh check the same lists).
missing=()
for lib in "${core_libs[@]}"; do
    [ -f "$jni_dir/$lib" ] || missing+=("jniLibs/arm64-v8a/$lib")
done
for f in "${aars[@]}" xreal_bridge.jar; do
    [ -f "$addon_dir/$f" ] || missing+=("addons/godot_xreal/android/$f")
done

echo ""
if [ ${#missing[@]} -gt 0 ]; then
    echo -e "\033[31mINCOMPLETE — still missing:\033[0m"
    printf '  - %s\n' "${missing[@]}"
    exit 1
fi
echo -e "\033[32mDone: 3 core .so -> jniLibs/arm64-v8a, 5 .aar + xreal_bridge.jar -> addons/godot_xreal/android.\033[0m"
echo "(NR .so ship via the .aar; nractivitylife*.aar deliberately excluded — Unity-only launcher.)"
