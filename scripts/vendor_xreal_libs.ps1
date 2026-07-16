#requires -Version 7
<#
.SYNOPSIS
    Vendor the XREAL runtime libraries the Android export needs out of the Unity `com.xreal.xr`
    package, in one go (all destinations are git-ignored):

      - 3 core .so       -> jniLibs/arm64-v8a/           (copied; dlopen'd by the GDExtension,
                                                          packed via godot_xreal.gdextension)
      - 7 .aar           -> addons/godot_xreal/android/  (shipped into the APK by export_plugin.gd:
                                                          Java/JNI layer + manifest merge; Gradle
                                                          also merges each .aar's jni/arm64-v8a/*.so
                                                          — the NR libs — into the APK, so they are
                                                          NOT extracted separately)
      - trackableImageTools -> addons/godot_xreal/tools/ (host build tool, NOT in the APK: generates
                                                          the image-tracking DB blob from PNGs)

    Extraction only — the XrealBridge Java sources are compiled by the export's gradle build
    (export_plugin.gd stages them into the build template), not here.

    Nothing is downloaded — you supply a local copy of the package. nractivitylife*.aar is
    DELIBERATELY EXCLUDED: its NRXRActivity/NRXRApp launcher is Unity-specific (instantiates
    com.unity3d.player.UnityPlayer) and must not ship in a Godot app. See docs/guides/android-setup.md.

.PARAMETER XrealPackage
    Either the Unity package root (the folder containing Runtime/Plugins/Android) or the
    `com.xreal.xr.tar.gz` archive itself — the archive is extracted to a temp dir (removed
    afterwards) and its `package/` root is used.

.EXAMPLE
    pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage "C:\Users\shien\dev\tmp_xreal\com.xreal.xr\package"

.EXAMPLE
    pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage "C:\path\to\com.xreal.xr.tar.gz"
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$XrealPackage
)

$ErrorActionPreference = 'Stop'

# --- 0) Accept the tar.gz directly: extract to a temp dir (cleaned up in finally) and use
#        its `package/` root. Windows 10+ ships bsdtar as tar.exe.
$tempExtract = $null
if ((Test-Path $XrealPackage -PathType Leaf)) {
    if ($XrealPackage -notmatch '\.(tar\.gz|tgz)$') {
        throw "-XrealPackage must be the package root directory or a .tar.gz archive: $XrealPackage"
    }
    $tempExtract = Join-Path ([System.IO.Path]::GetTempPath()) "xreal_pkg_$([guid]::NewGuid().ToString('n'))"
    New-Item -ItemType Directory -Path $tempExtract | Out-Null
    Write-Host "extracting $XrealPackage ..."
    # Use Windows' System32 tar (bsdtar) explicitly: when invoked from Git Bash / MSYS, PATH
    # resolves `tar` to GNU tar, which reads the `C:` in a Windows path as a remote host name.
    $tarExe = Join-Path $env:SystemRoot 'System32\tar.exe'
    if (-not (Test-Path $tarExe)) { $tarExe = 'tar' }
    & $tarExe -xzf $XrealPackage -C $tempExtract
    if ($LASTEXITCODE -ne 0) {
        Remove-Item -Recurse -Force $tempExtract -ErrorAction SilentlyContinue
        throw "tar -xzf failed (exit $LASTEXITCODE): $XrealPackage"
    }
    $XrealPackage = Join-Path $tempExtract 'package'
    if (-not (Test-Path $XrealPackage)) {
        # No `package/` root — fall back to whichever top-level dir holds Runtime/Plugins/Android.
        $XrealPackage = Get-ChildItem $tempExtract -Directory |
            Where-Object { Test-Path (Join-Path $_.FullName 'Runtime/Plugins/Android') } |
            Select-Object -First 1 -ExpandProperty FullName
        if (-not $XrealPackage) {
            Remove-Item -Recurse -Force $tempExtract -ErrorAction SilentlyContinue
            throw "No package root with Runtime/Plugins/Android found inside the archive."
        }
    }
}

try {
    $repo = Resolve-Path (Join-Path $PSScriptRoot '..')
    $srcAndroid = Join-Path $XrealPackage 'Runtime/Plugins/Android'
    $srcAbi = Join-Path $srcAndroid 'arm64-v8a'
    if (-not (Test-Path $srcAbi)) {
        throw "Not found: $srcAbi  (is -XrealPackage the com.xreal.xr/package root?)"
    }

    $jniDir = Join-Path $repo 'jniLibs/arm64-v8a'
    $addonDir = Join-Path $repo 'addons/godot_xreal/android'
    New-Item -ItemType Directory -Force -Path $jniDir, $addonDir | Out-Null

    # --- 1) 3 core .so -> jniLibs/arm64-v8a (dlopen'd; listed in godot_xreal.gdextension [dependencies])
    $coreLibs = @(
        'libXREALNativeSessionManager.so',
        'libXREALXRPlugin.so',
        'libVulkanSupport.so'
    )
    foreach ($lib in $coreLibs) {
        $src = Join-Path $srcAbi $lib
        if (-not (Test-Path $src)) { Write-Warning "Missing in package: $lib"; continue }
        Copy-Item -Path $src -Destination (Join-Path $jniDir $lib) -Force
        Write-Host "so   $lib"
    }

    # --- 2) 7 .aar -> addons/godot_xreal/android (shipped by export_plugin.gd _get_android_libraries;
    #        the exact file names are hardcoded there). Besides the Java/JNI layer + manifest merge,
    #        the aars carry the NR native libs at jni/arm64-v8a/ (nr_api.aar: libnr_api.so /
    #        libnr_plugin_6dof.so / libnr_rgb_camera.so, nr_loader.aar: libnr_loader.so,
    #        nr_common.aar: libnr_libusb.so, nr_spatial_anchor.aar: libnr_spatial_anchor.so, nr_image_tracking.aar: libnr_image_tracking.so) — Gradle merges those into the APK.
    #
    # Log-Control is REQUIRED whenever GlassesDisplayPlugEvent ships: its GlassesInitProvider
    # (a ContentProvider that auto-runs at app startup) references com.xreal.logcontrol.LogControl,
    # and without it the app crashes before Godot starts with
    # NoClassDefFoundError: com/xreal/logcontrol/LogControl. (Device-confirmed 2026-06-15.)
    $aars = @(
        'nr_loader.aar',
        'nr_api.aar',
        'nr_common.aar',
        'nr_spatial_anchor.aar',
        'nr_image_tracking.aar',
        'GlassesDisplayPlugEvent-2.4.2.aar',
        'Log-Control-1.2.aar'
    )
    foreach ($aar in $aars) {
        $src = Join-Path $srcAndroid $aar
        if (-not (Test-Path $src)) { Write-Warning "Missing in package: $aar"; continue }
        Copy-Item -Path $src -Destination (Join-Path $addonDir $aar) -Force
        Write-Host "aar  $aar"
    }

    # --- 3) Host build tool -> addons/godot_xreal/tools/ (NOT shipped in the APK): trackableImageTools
    #        generates the image-tracking reference-image DB blob from PNGs at build time (see
    #        docs/plans/ar-features-plan.md).
    $toolsDir = Join-Path $repo 'addons/godot_xreal/tools'
    New-Item -ItemType Directory -Force -Path $toolsDir | Out-Null
    $toolSrc = Join-Path $XrealPackage 'Tools~/Windows/trackableImageTools.exe'
    if (Test-Path $toolSrc) {
        Copy-Item -Path $toolSrc -Destination (Join-Path $toolsDir 'trackableImageTools.exe') -Force
        Write-Host "tool trackableImageTools.exe"
    } else {
        Write-Warning "Missing in package: Tools~/Windows/trackableImageTools.exe"
    }

    # --- Final verification: everything the export needs (build.ps1/build.sh check the same lists).
    $missing = @()
    foreach ($lib in $coreLibs) {
        if (-not (Test-Path (Join-Path $jniDir $lib))) { $missing += "jniLibs/arm64-v8a/$lib" }
    }
    foreach ($f in $aars) {
        if (-not (Test-Path (Join-Path $addonDir $f))) { $missing += "addons/godot_xreal/android/$f" }
    }

    Write-Host ""
    if ($missing) {
        Write-Host "INCOMPLETE — still missing:" -ForegroundColor Red
        $missing | ForEach-Object { Write-Host "  - $_" }
        exit 1
    }
    Write-Host "Done: 3 core .so -> jniLibs/arm64-v8a, 7 .aar -> addons/godot_xreal/android, trackableImageTools -> addons/godot_xreal/tools." -ForegroundColor Green
    Write-Host "(NR .so ship via the .aar; nractivitylife*.aar deliberately excluded — Unity-only launcher.)"
}
finally {
    if ($tempExtract) { Remove-Item -Recurse -Force $tempExtract -ErrorAction SilentlyContinue }
}
