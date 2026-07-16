#requires -Version 7
<#
.SYNOPSIS
    Vendor everything the Android export needs out of the Unity `com.xreal.xr` package,
    in one go (all destinations are git-ignored):

      - 3 core .so       -> jniLibs/arm64-v8a/           (copied; dlopen'd by the GDExtension,
                                                          packed via godot_xreal.gdextension)
      - 5 .aar           -> addons/godot_xreal/android/  (shipped into the APK by export_plugin.gd:
                                                          Java/JNI layer + manifest merge; Gradle
                                                          also merges each .aar's jni/arm64-v8a/*.so
                                                          — the NR libs — into the APK, so they are
                                                          NOT extracted separately)
      - xreal_bridge.jar -> addons/godot_xreal/android/  (compiled from the committed Java source;
                                                          needs a JDK `javac` + an Android SDK
                                                          platform `android.jar`)

    Nothing is downloaded — you supply a local copy of the package. nractivitylife*.aar is
    DELIBERATELY EXCLUDED: its NRXRActivity/NRXRApp launcher is Unity-specific (instantiates
    com.unity3d.player.UnityPlayer) and must not ship in a Godot app. See docs/android-setup.md.

.PARAMETER XrealPackage
    Path to the Unity package root (the folder containing Runtime/Plugins/Android).

.PARAMETER SkipJar
    Skip compiling xreal_bridge.jar (e.g. no JDK on this machine; an already-built jar is kept).

.EXAMPLE
    pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage "C:\Users\shien\dev\tmp_xreal\com.xreal.xr\package"
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$XrealPackage,
    [switch]$SkipJar
)

$ErrorActionPreference = 'Stop'

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
$aars = @(
    'nr_loader.aar',
    'nr_api.aar',
    'nr_common.aar',
    'GlassesDisplayPlugEvent-2.4.2.aar',
    'Log-Control-1.2.aar'
)
foreach ($aar in $aars) {
    $src = Join-Path $srcAndroid $aar
    if (-not (Test-Path $src)) { Write-Warning "Missing in package: $aar"; continue }
    Copy-Item -Path $src -Destination (Join-Path $addonDir $aar) -Force
    Write-Host "aar  $aar"
}

# --- 3) xreal_bridge.jar -> addons/godot_xreal/android, compiled from the committed Java source.
#        Needs javac/jar (a JDK) and an Android SDK platform android.jar for the classpath.
$jarOut = Join-Path $addonDir 'xreal_bridge.jar'
if (-not $SkipJar) {
    $javac = Get-Command javac -ErrorAction SilentlyContinue
    $androidJar = $null
    foreach ($sdk in @($env:ANDROID_HOME, $env:ANDROID_SDK_ROOT, (Join-Path $env:LOCALAPPDATA 'Android\Sdk'))) {
        if (-not $sdk -or -not (Test-Path (Join-Path $sdk 'platforms'))) { continue }
        $androidJar = Get-ChildItem (Join-Path $sdk 'platforms') -Directory -Filter 'android-*' |
            Sort-Object { [int]('0' + ($_.Name -replace '\D', '')) } -Descending |
            ForEach-Object { Join-Path $_.FullName 'android.jar' } |
            Where-Object { Test-Path $_ } |
            Select-Object -First 1
        if ($androidJar) { break }
    }
    if (-not $javac -or -not $androidJar) {
        $why = if (-not $javac) { 'javac (JDK) not on PATH' } else { "no android.jar under ANDROID_HOME/ANDROID_SDK_ROOT/%LOCALAPPDATA%\Android\Sdk" }
        if (Test-Path $jarOut) {
            Write-Warning "xreal_bridge.jar NOT rebuilt ($why) — keeping the existing jar."
        }
        else {
            Write-Warning ("Cannot build xreal_bridge.jar ($why). Build it manually:`n" +
                "  javac -encoding UTF-8 -source 11 -target 11 -classpath <sdk>/platforms/android-NN/android.jar ``
    -d out addons/godot_xreal/android/src/com/godot/game/*.java`n" +
                "  jar cf addons/godot_xreal/android/xreal_bridge.jar -C out .")
        }
    }
    else {
        $jarTool = Join-Path (Split-Path $javac.Source) 'jar.exe'
        if (-not (Test-Path $jarTool)) { $jarTool = 'jar' }
        $classesDir = Join-Path ([System.IO.Path]::GetTempPath()) "xreal_bridge_$([guid]::NewGuid().ToString('n'))"
        New-Item -ItemType Directory -Path $classesDir | Out-Null
        try {
            $javaSrc = Get-ChildItem (Join-Path $addonDir 'src/com/godot/game') -Filter '*.java' | ForEach-Object FullName
            & $javac.Source -encoding UTF-8 -source 11 -target 11 -nowarn -classpath $androidJar -d $classesDir @javaSrc
            if ($LASTEXITCODE -ne 0) { throw "javac failed (exit $LASTEXITCODE)" }
            & $jarTool cf $jarOut -C $classesDir .
            if ($LASTEXITCODE -ne 0) { throw "jar failed (exit $LASTEXITCODE)" }
            Write-Host "jar  xreal_bridge.jar  (classpath $androidJar)"
        }
        finally { Remove-Item -Recurse -Force $classesDir -ErrorAction SilentlyContinue }
    }
}

# --- Final verification: everything the export needs (build.ps1/build.sh check the same lists).
$missing = @()
foreach ($lib in $coreLibs) {
    if (-not (Test-Path (Join-Path $jniDir $lib))) { $missing += "jniLibs/arm64-v8a/$lib" }
}
foreach ($f in ($aars + 'xreal_bridge.jar')) {
    if (-not (Test-Path (Join-Path $addonDir $f))) { $missing += "addons/godot_xreal/android/$f" }
}

Write-Host ""
if ($missing) {
    Write-Host "INCOMPLETE — still missing:" -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "  - $_" }
    exit 1
}
Write-Host "Done: 3 core .so -> jniLibs/arm64-v8a, 5 .aar + xreal_bridge.jar -> addons/godot_xreal/android." -ForegroundColor Green
Write-Host "(NR .so ship via the .aar; nractivitylife*.aar deliberately excluded — Unity-only launcher.)"
