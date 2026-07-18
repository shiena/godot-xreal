#requires -Version 7
<#
.SYNOPSIS
    Receiving server for the XREAL first-person-view H.264/RTP stream (the Godot app's streaming cast
    sends to rtp://<this-PC>:<port>). Listens on the UDP port and plays the live stream with ffplay
    (or records it to an .mp4 with ffmpeg when -Record is given). Uses stream.sdp next to this script.

    The app must be pointed at THIS PC's IP + port (printed below). ffmpeg/ffplay must be installed
    (e.g. `scoop install ffmpeg`).

.PARAMETER Port
    UDP port to listen on (default 5555 — must match the app's stream target).

.PARAMETER Record
    Record to an .mp4 in this folder instead of live-playing.

.EXAMPLE
    pwsh scripts/stream_server/receive.ps1
.EXAMPLE
    pwsh scripts/stream_server/receive.ps1 -Port 5555 -Record
#>
param(
    [int]$Port = 5555,
    [switch]$Record
)

$ErrorActionPreference = 'Stop'
$here = Split-Path $PSCommandPath -Parent
$sdp = Join-Path $here 'stream.sdp'

# Rewrite the SDP's port so -Port takes effect.
(Get-Content $sdp -Raw) -replace 'm=video \d+ ', "m=video $Port " | Set-Content $sdp -NoNewline

# Show this PC's LAN IPv4 addresses so you know what to configure in the app.
Write-Host "Point the app's stream target at one of these  rtp://<IP>:$Port :" -ForegroundColor Cyan
Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue |
    Where-Object { $_.IPAddress -notmatch '^(127\.|169\.254\.)' } |
    ForEach-Object { Write-Host ("  rtp://{0}:{1}" -f $_.IPAddress, $Port) -ForegroundColor Green }
Write-Host ""

function Find-Tool([string]$name) {
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $scoop = Join-Path $env:USERPROFILE "scoop\apps\ffmpeg\current\bin\$name.exe"
    if (Test-Path $scoop) { return $scoop }
    return $null
}

$common = @(
    '-protocol_whitelist', 'file,udp,rtp',
    '-fflags', 'nobuffer', '-flags', 'low_delay', '-analyzeduration', '1000000',
    '-i', $sdp
)

if ($Record) {
    $ffmpeg = Find-Tool 'ffmpeg'
    if (-not $ffmpeg) { throw "ffmpeg not found. Install it (e.g. 'scoop install ffmpeg')." }
    $out = Join-Path $here ("fpv_{0}.mp4" -f (Get-Random))
    Write-Host "Recording to $out  (Ctrl+C to stop) ..." -ForegroundColor Yellow
    & $ffmpeg @common -c copy $out
} else {
    $ffplay = Find-Tool 'ffplay'
    if (-not $ffplay) { throw "ffplay not found. Install ffmpeg (e.g. 'scoop install ffmpeg'), which bundles ffplay." }
    Write-Host "Listening on UDP $Port — start streaming in the app (Ctrl+C to stop) ..." -ForegroundColor Yellow
    & $ffplay @common
}
