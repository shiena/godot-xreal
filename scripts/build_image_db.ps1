#requires -Version 7
<#
.SYNOPSIS
    Build the image-tracking reference-image DB blob from a manifest, using the vendored
    trackableImageTools (addons/godot_xreal/tools/, placed by scripts/vendor_xreal_libs.ps1).

    Reads the manifest (default demo/image_tracking/reference.json), writes an image-list config
    (`<guid:N>|<image path>|<width_metres>` per image), runs the CLI, and produces the `blob` named
    in the manifest next to it. See docs/plans/ar-features-plan.md for the pipeline.

.PARAMETER Manifest
    Path (repo-relative or absolute) to the reference.json manifest.

.EXAMPLE
    pwsh scripts/build_image_db.ps1
#>
param(
    [string]$Manifest = 'demo/image_tracking/reference.json'
)

$ErrorActionPreference = 'Stop'
$repo = Resolve-Path (Join-Path $PSScriptRoot '..')

$manifestPath = if ([System.IO.Path]::IsPathRooted($Manifest)) { $Manifest } else { Join-Path $repo $Manifest }
if (-not (Test-Path $manifestPath)) { throw "Manifest not found: $manifestPath" }
$dir = Split-Path $manifestPath -Parent
$m = Get-Content $manifestPath -Raw | ConvertFrom-Json

$tool = Join-Path $repo 'addons/godot_xreal/tools/trackableImageTools.exe'
if (-not (Test-Path $tool)) {
    throw "trackableImageTools.exe not found at $tool — run scripts/vendor_xreal_libs.ps1 first."
}

# Image-list config: one `<guid:N>|<image path>|<width>` line per image (the CLI's --images_config_file).
$lines = foreach ($img in $m.images) {
    $imgPath = Join-Path $dir $img.image
    if (-not (Test-Path $imgPath)) { throw "Image not found: $imgPath (referenced by $($m.blob))" }
    '{0}|{1}|{2}' -f $img.guid, (Resolve-Path $imgPath).Path, $img.width
}
$listPath = Join-Path ([System.IO.Path]::GetTempPath()) "imglist_$([guid]::NewGuid().ToString('n')).txt"
($lines -join "`n") | Set-Content -NoNewline -Path $listPath -Encoding utf8

$blobPath = Join-Path $dir $m.blob
try {
    & $tool --images_config_file $listPath --save_path $blobPath
} finally {
    Remove-Item $listPath -ErrorAction SilentlyContinue
}

if ((Test-Path $blobPath) -and (Get-Item $blobPath).Length -gt 0) {
    Write-Host "Built $($m.blob) ($((Get-Item $blobPath).Length) bytes) from $($m.images.Count) image(s)." -ForegroundColor Green
} else {
    throw "Blob build failed (no output at $blobPath)."
}
