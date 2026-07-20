# godot-xreal local dev pipeline (Windows / PowerShell).
#
# The XREAL native libraries ship only for Android arm64-v8a, so on-device testing means:
#   cargo ndk build  ->  Godot APK export  ->  adb install  ->  launch on the glasses.
# This wraps all four stages (and the two workarounds that bite every time: the Godot export
# hang, and the force-stop-before-launch requirement).
#
# Assumes the toolchain is installed and on PATH: cargo + cargo-ndk, adb (scrcpy's v37), and a
# Godot 4.7-stable console binary (see -Godot), with ANDROID_NDK_HOME set for cargo-ndk.
#
# Usage:
#   .\scripts\build.ps1                       # build only (cargo ndk, release)
#   .\scripts\build.ps1 -Extract <com.xreal.xr.tar.gz>  # vendor the XREAL runtime libs from the SDK
#   .\scripts\build.ps1 -All                  # build + export + install + run
#   .\scripts\build.ps1 -All -StereoMode 0 -TrackingType 0   # + set device props first
#   .\scripts\build.ps1 -Export -Install -Run # reuse the current .so
#   .\scripts\build.ps1 -Run -Logcat          # relaunch and stream [xreal] logs
#   .\scripts\build.ps1 -Install -Run -ReleaseApk
#
# Stages run in order when combined: Extract -> Build -> Export -> Install -> Run -> Logcat.
# With no stage switch, only -Build runs (-Extract alone just vendors). -All = Build+Export+Install+Run.

param(
    [string]$Extract,        # path to com.xreal.xr.tar.gz (or the extracted package/ dir):
                             # runs vendor_xreal_libs.ps1 first to stage the XREAL runtime libs
    [switch]$Build,
    [switch]$Export,
    [switch]$Install,
    [switch]$Run,
    [switch]$Logcat,
    [switch]$All,
    [switch]$ReleaseApk,     # export a release-keystore APK (default: debug keystore)
    [switch]$CargoDebug,     # cargo debug profile (default: release)
    [switch]$Checks,         # run cargo fmt --check + clippy before building (off by default)
    [int]$StereoMode = -1,   # -1 = leave device prop; 0 = Multipass, 2 = Multiview
    [int]$TrackingType = -1, # -1 = leave device prop; 0 = 6DoF, 1 = 3DoF, 2 = 0DoF
    [string]$Device  = $(if ($env:XREAL_DEVICE) { $env:XREAL_DEVICE } else { '192.168.0.4:5555' }),
    [string]$Godot   = $(if ($env:GODOT) { $env:GODOT } else { 'godot' }),  # a Godot 4.7-stable binary on PATH
    [string]$Adb     = $(if ($env:ADB)   { $env:ADB }   else { 'adb' }),    # scrcpy's adb v37, on PATH
    [string]$ApkOut,
    [string]$Preset  = 'Android'
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

if (-not $ApkOut) { $ApkOut = Join-Path (Split-Path -Parent $repoRoot) 'godot-build\godot-xreal.apk' }

$Pkg      = 'com.example.godotxreal'
$Activity = "$Pkg/com.godot.game.GodotAppLauncher"
$profile_ = if ($CargoDebug) { 'debug' } else { 'release' }

if ($All) { $Build = $Export = $Install = $Run = $true }
if (-not ($Build -or $Export -or $Install -or $Run -or $Logcat -or $Extract)) { $Build = $true }

function Say ([string]$m) { Write-Host ">> $m" -ForegroundColor Cyan }
function Ok  ([string]$m) { Write-Host $m -ForegroundColor Green }
function Die ([string]$m) { Write-Error $m }
function Adbx { if ($Device) { & $Adb -s $Device @args } else { & $Adb @args } }

# The XREAL runtime pieces the APK must bundle. They are NOT in this repo — vendor them from the
# XREAL SDK for Unity (see README / the guide below):
#   - 3 core .so in jniLibs/arm64-v8a (packed via godot_xreal.gdextension [dependencies])
#   - 5 .aar in addons/godot_xreal/android (shipped into the APK by the addon's export_plugin.gd:
#     Java/JNI layer + manifest merge; the aars also carry the NR native libs, which Gradle
#     merges into the APK)
# The XrealBridge Java sources are compiled by the export's gradle build (export_plugin.gd
# stages them into the build template) — nothing to vendor for those.
# This checks both before an export and stops with instructions if anything is missing; it never
# downloads anything.
$RequiredLibs = @(
    'libXREALNativeSessionManager.so', 'libXREALXRPlugin.so', 'libVulkanSupport.so', 'libmedia_codec.so'
)
$RequiredAddonFiles = @(
    'nr_loader.aar', 'nr_api.aar', 'nr_common.aar', 'nr_spatial_anchor.aar', 'nr_image_tracking.aar',
    'GlassesDisplayPlugEvent-2.4.2.aar', 'Log-Control-1.2.aar'
)
function Require-VendoredLibs {
    $jniDir = Join-Path $repoRoot 'jniLibs\arm64-v8a'
    $addonDir = Join-Path $repoRoot 'addons\godot_xreal\android'
    $missing = @($RequiredLibs | Where-Object { -not (Test-Path (Join-Path $jniDir $_)) } |
            ForEach-Object { "jniLibs/arm64-v8a/$_" }) +
        @($RequiredAddonFiles | Where-Object { -not (Test-Path (Join-Path $addonDir $_)) } |
            ForEach-Object { "addons/godot_xreal/android/$_" })
    if (-not $missing) { return }
    Write-Host "Missing vendored XREAL runtime pieces:" -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "  - $_" }
    Write-Host @'

These ship with the XREAL SDK for Unity (com.xreal.xr) and are NOT included in this repo.
Vendor them once from a local copy of the package (nothing is downloaded):
  1. Obtain the XREAL SDK for Unity package `com.xreal.xr.tar.gz` and extract it (-> a `package/` dir).
  2. Run  pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <...>\package
     which stages everything:
       - 3 core .so -> jniLibs/arm64-v8a/      (dlopen'd by the GDExtension)
       - 5 .aar -> addons/godot_xreal/android/ (shipped by the addon's export plugin; they also
         carry the NR native libs, which Gradle merges into the APK)
     (The XrealBridge Java sources are compiled by the export's gradle build — no JDK step here.)
See the README "Prerequisite: vendor the XREAL runtime libraries" and docs/guides/build-and-release.md.
'@
    exit 1
}

# --------------------------------------------------- Extract (vendor XREAL runtime libs) ---
if ($Extract) {
    Say "vendor XREAL runtime libs from $Extract"
    & (Join-Path $PSScriptRoot 'vendor_xreal_libs.ps1') -XrealPackage $Extract
    if ($LASTEXITCODE -ne 0) { Die "vendoring failed (exit $LASTEXITCODE)" }
}

# Fail fast (before a long build) if an export is requested but the XREAL runtime libs aren't vendored.
if ($Export) { Require-VendoredLibs }

# ---------------------------------------------------------------- Build (cargo ndk) ---
if ($Build) {
    if ($Checks) {
        Say 'cargo fmt --check'
        cargo fmt --check; if ($LASTEXITCODE -ne 0) { Die 'cargo fmt --check failed (run `cargo fmt`)' }
        Say 'cargo clippy --release'
        cargo clippy --release; if ($LASTEXITCODE -ne 0) { Die 'cargo clippy failed' }
    }
    Say "cargo ndk -t arm64-v8a build ($profile_)"
    $cargoArgs = @('ndk', '-t', 'arm64-v8a', 'build')
    if (-not $CargoDebug) { $cargoArgs += '--release' }
    cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { Die "cargo ndk build failed (exit $LASTEXITCODE)" }
    $profileDir = if ($CargoDebug) { 'debug' } else { 'release' }
    $built = Join-Path $repoRoot "target\aarch64-linux-android\$profileDir\libgodot_xreal.so"
    if (-not (Test-Path $built)) { Die "Build artifact not found: $built" }
    # Place it where the .gdextension expects it (addons/godot_xreal/bin/android/, packed on export).
    $so = Join-Path $repoRoot 'addons\godot_xreal\bin\android\libgodot_xreal.so'
    New-Item -ItemType Directory -Force (Split-Path -Parent $so) | Out-Null
    Copy-Item -Force $built $so
    Ok "Built: $so"
}

# ------------------------------------------------------- Export APK (non-console binary) ---
if ($Export) {
    $ver = (& $Godot --version 2>$null | Select-Object -First 1)
    if ($ver -notmatch '^4\.7') { Die "Godot must be 4.7-stable (template match); '$Godot --version' = '$ver'. Set -Godot / `$env:GODOT to a 4.7 binary." }

    $outDir = Split-Path -Parent $ApkOut
    New-Item -ItemType Directory -Force $outDir | Out-Null
    $exportFlag = if ($ReleaseApk) { '--export-release' } else { '--export-debug' }

    # Use the NON-console Godot binary here (Godot_..._win64.exe, not *_console.exe). The console
    # wrapper waits for every process in its Job Object to exit, and an Android *Gradle* build leaves
    # a resident Gradle daemon behind, so the console binary hangs after writing the APK. The
    # non-console binary exits normally (~20-30s). See godot-android-export-hang-repro.
    Say "Godot export ($exportFlag `"$Preset`") -> $ApkOut"
    $proc = Start-Process -FilePath $Godot -PassThru -WindowStyle Hidden -ArgumentList @(
        '--headless', '--path', $repoRoot, $exportFlag, $Preset, $ApkOut
    )
    if (-not $proc.WaitForExit(180000)) {
        Stop-Process -Id $proc.Id -Force
        Die "APK export did not finish in 180s. A *console* Godot binary hangs here after a Gradle export (Gradle daemon stuck in its Job Object) — use the non-console binary."
    }
    if (-not (Test-Path $ApkOut) -or (Get-Item $ApkOut).Length -lt 1MB) {
        Die "APK export finished (exit $($proc.ExitCode)) but $ApkOut is missing or too small. Check the export preset / keystore."
    }
    Ok ("Exported: {0} ({1:N0} bytes)" -f $ApkOut, (Get-Item $ApkOut).Length)
}

# ---------------------------------------------------------------- Install (adb) ---
if ($Install) {
    if (-not (Test-Path $ApkOut)) { Die "APK not found: $ApkOut (run with -Export first)" }
    if ($Device) { & $Adb connect $Device | Out-Null }
    Say "adb install -r $ApkOut"
    # Join: adb output spans multiple lines, and -notmatch on an array filters instead of testing.
    $out = (Adbx install -r $ApkOut 2>&1) -join "`n"
    if ($out -notmatch 'Success') { Die "install failed: $out" }
    Ok 'Installed.'
}

# ---------------------------------------------------------------- Run (launch) ---
if ($Run) {
    if ($Device) { & $Adb connect $Device | Out-Null }
    if ($StereoMode   -ge 0) { Adbx shell setprop debug.xreal.stereo_mode   $StereoMode;   Say "setprop debug.xreal.stereo_mode $StereoMode" }
    if ($TrackingType -ge 0) { Adbx shell setprop debug.xreal.tracking_type $TrackingType; Say "setprop debug.xreal.tracking_type $TrackingType" }
    # Force-stop first: relaunching a not-fully-dead instance leaves the XR display registration
    # stuck ("graphics-thread callbacks registered as null"), so the glasses stay black.
    Adbx shell am force-stop $Pkg | Out-Null
    Start-Sleep -Milliseconds 800
    Say "am start $Activity"
    Adbx shell am start -n $Activity | Out-Null
    Ok 'Launched (put on the glasses).'
}

# ---------------------------------------------------------------- Logcat ([xreal]) ---
if ($Logcat) {
    if ($Device) { & $Adb connect $Device | Out-Null }
    Say 'streaming [xreal] logs (Ctrl-C to stop)'
    if ($Device) { & $Adb -s $Device logcat -v time --regex '\[xreal\]' }
    else { & $Adb logcat -v time --regex '\[xreal\]' }
}
