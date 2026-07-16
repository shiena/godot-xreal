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
#   .\scripts\build.ps1 -All                  # build + export + install + run
#   .\scripts\build.ps1 -All -StereoMode 0 -TrackingType 0   # + set device props first
#   .\scripts\build.ps1 -Export -Install -Run # reuse the current .so
#   .\scripts\build.ps1 -Run -Logcat          # relaunch and stream [xreal] logs
#   .\scripts\build.ps1 -Install -Run -ReleaseApk
#
# Stages run in order when combined: Build -> Export -> Install -> Run -> Logcat.
# With no stage switch, only -Build runs. -All = Build+Export+Install+Run.

param(
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
if (-not ($Build -or $Export -or $Install -or $Run -or $Logcat)) { $Build = $true }

function Say ([string]$m) { Write-Host ">> $m" -ForegroundColor Cyan }
function Ok  ([string]$m) { Write-Host $m -ForegroundColor Green }
function Die ([string]$m) { Write-Error $m }
function Adbx { if ($Device) { & $Adb -s $Device @args } else { & $Adb @args } }

# The XREAL runtime libraries the APK must bundle (see godot_xreal.gdextension [dependencies]).
# They are NOT in this repo — vendor them from the XREAL SDK for Unity (see README / the guide below).
# This checks jniLibs before an export and stops with instructions if any is missing; it never
# downloads or extracts anything.
$RequiredLibs = @(
    'libXREALNativeSessionManager.so', 'libXREALXRPlugin.so', 'libVulkanSupport.so',
    'libnr_api.so', 'libnr_libusb.so', 'libnr_loader.so', 'libnr_plugin_6dof.so', 'libnr_rgb_camera.so'
)
function Require-VendoredLibs {
    $dir = Join-Path $repoRoot 'jniLibs\arm64-v8a'
    $missing = $RequiredLibs | Where-Object { -not (Test-Path (Join-Path $dir $_)) }
    if (-not $missing) { return }
    Write-Host "Missing XREAL runtime libraries in jniLibs/arm64-v8a:" -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "  - $_" }
    Write-Host @'

These ship with the XREAL SDK for Unity (com.xreal.xr) and are NOT included in this repo.
Vendor them once (no download/extract is automated):
  1. Obtain the XREAL SDK for Unity package `com.xreal.xr.tar.gz` and extract it (-> a `package/` dir).
  2. Copy the 3 core libs from package/Runtime/Plugins/Android/arm64-v8a/ into jniLibs/arm64-v8a/,
     e.g.  pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <...>\package
       libXREALNativeSessionManager.so, libXREALXRPlugin.so, libVulkanSupport.so
  3. Extract the 5 NR libs from the package's .aar (each .aar is a zip; take jni/arm64-v8a/<lib>)
     into jniLibs/arm64-v8a/:
       nr_api.aar    -> libnr_api.so, libnr_plugin_6dof.so, libnr_rgb_camera.so
       nr_loader.aar -> libnr_loader.so
       nr_common.aar -> libnr_libusb.so
See the README "Vendoring the XREAL runtime libraries" section and docs/build-and-release.md.
'@
    exit 1
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
    $cargoArgs = @('ndk', '-t', 'arm64-v8a', '-o', './jniLibs', 'build')
    if (-not $CargoDebug) { $cargoArgs += '--release' }
    cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { Die "cargo ndk build failed (exit $LASTEXITCODE)" }
    $so = Join-Path $repoRoot 'jniLibs\arm64-v8a\libgodot_xreal.so'
    if (-not (Test-Path $so)) { Die "Build artifact not found: $so" }
    Ok "Built: $so"
}

# ------------------------------------------------------- Export APK (with hang poll) ---
if ($Export) {
    $ver = (& $Godot --version 2>$null | Select-Object -First 1)
    if ($ver -notmatch '^4\.7') { Die "Godot must be 4.7-stable (template match); '$Godot --version' = '$ver'. Set -Godot / `$env:GODOT to a 4.7 binary." }

    $outDir = Split-Path -Parent $ApkOut
    New-Item -ItemType Directory -Force $outDir | Out-Null
    $exportFlag = if ($ReleaseApk) { '--export-release' } else { '--export-debug' }

    # The Godot Android export writes the APK then HANGS instead of exiting, so run it detached and
    # poll for a completed APK (fresh mtime + stable size + a valid ZIP end-of-central-directory),
    # then kill it. Killing mid-write corrupts the APK (INSTALL_PARSE_FAILED_NOT_APK).
    $start = Get-Date
    Say "Godot export ($exportFlag `"$Preset`") -> $ApkOut"
    $proc = Start-Process -FilePath $Godot -PassThru -WindowStyle Hidden -ArgumentList @(
        '--headless', '--path', $repoRoot, $exportFlag, $Preset, $ApkOut
    )

    $done = $false; $prev = -1L; $stable = 0
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Seconds 4
        if (-not (Test-Path $ApkOut)) { continue }
        $fi = Get-Item $ApkOut
        if ($fi.LastWriteTime -lt $start) { continue }        # stale APK from a previous run
        $size = $fi.Length
        if ($size -eq $prev -and $size -gt 100MB) { $stable++ } else { $stable = 0 }
        $prev = $size
        if ($stable -ge 2) {
            $bytes = [System.IO.File]::ReadAllBytes($ApkOut)
            $eocd = $false
            for ($j = $bytes.Length - 22; $j -ge [Math]::Max(0, $bytes.Length - 65558); $j--) {
                if ($bytes[$j] -eq 0x50 -and $bytes[$j+1] -eq 0x4B -and $bytes[$j+2] -eq 0x05 -and $bytes[$j+3] -eq 0x06) { $eocd = $true; break }
            }
            if ($eocd) { $done = $true; break }
        }
    }
    if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force; Start-Sleep -Milliseconds 500 }
    Get-Process | Where-Object { $_.Name -like 'Godot*' } | Stop-Process -Force -ErrorAction SilentlyContinue
    if (-not $done) { Die "APK export did not complete (no fresh, valid APK at $ApkOut). Check the export preset / keystore." }
    Ok ("Exported: {0} ({1:N0} bytes)" -f $ApkOut, (Get-Item $ApkOut).Length)
}

# ---------------------------------------------------------------- Install (adb) ---
if ($Install) {
    if (-not (Test-Path $ApkOut)) { Die "APK not found: $ApkOut (run with -Export first)" }
    if ($Device) { & $Adb connect $Device | Out-Null }
    Say "adb install -r $ApkOut"
    $out = Adbx install -r $ApkOut 2>&1
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
