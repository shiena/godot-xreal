#requires -Version 7
<#
.SYNOPSIS
    Copy the XREAL native runtime libraries out of the Unity `com.xreal.xr` package
    into ./jniLibs/arm64-v8a so the GDExtension can dlopen them and the Godot
    Android export can pack them into the APK.

.PARAMETER XrealPackage
    Path to the Unity package root (the folder containing Runtime/Plugins/Android).

.EXAMPLE
    pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage "C:\Users\shien\dev\tmp_xreal\com.xreal.xr\package"
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$XrealPackage
)

$ErrorActionPreference = 'Stop'

$srcDir = Join-Path $XrealPackage 'Runtime/Plugins/Android/arm64-v8a'
if (-not (Test-Path $srcDir)) {
    throw "Not found: $srcDir  (is -XrealPackage the com.xreal.xr/package root?)"
}

$destDir = Join-Path $PSScriptRoot '../jniLibs/arm64-v8a'
New-Item -ItemType Directory -Force -Path $destDir | Out-Null

# The libraries the GDExtension dlopen()s (see godot_xreal.gdextension / src/native.rs).
$libs = @(
    'libXREALNativeSessionManager.so',
    'libXREALXRPlugin.so',
    'libVulkanSupport.so'
)

foreach ($lib in $libs) {
    $src = Join-Path $srcDir $lib
    if (-not (Test-Path $src)) {
        Write-Warning "Missing in package: $lib"
        continue
    }
    Copy-Item -Path $src -Destination (Join-Path $destDir $lib) -Force
    Write-Host "vendored $lib"
}

Write-Host ""
Write-Host "Done (3 core libs) -> $((Resolve-Path $destDir).Path)"
Write-Host "Still needed by godot_xreal.gdextension: the 5 NR libs, which live inside the package's .aar"
Write-Host "  (each .aar is a zip; extract jni/arm64-v8a/<lib> into jniLibs/arm64-v8a/):"
Write-Host "    nr_api.aar    -> libnr_api.so, libnr_plugin_6dof.so, libnr_rgb_camera.so"
Write-Host "    nr_loader.aar -> libnr_loader.so"
Write-Host "    nr_common.aar -> libnr_libusb.so"
Write-Host "See docs/build-and-release.md (Vendor the XREAL runtime libraries)."
