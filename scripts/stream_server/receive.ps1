#requires -Version 7
<#
.SYNOPSIS
    Receiving server for the XREAL first-person-view stream (H.264 + AAC over RTP). Answers the
    app's FIND-SERVER discovery, and once the app starts streaming, plays it live with ffplay (or
    records it with ffmpeg when -Record is given). Uses stream.sdp and pair_server.py next to this
    script.

    You do NOT type an address into the app: it discovers this PC itself. Start this script first,
    then hit Stream in the app — the receiver is launched automatically at the right moment, because
    ffmpeg gives up probing long before a hand-driven app gets around to sending.

    Needs python 3 (stdlib only) and ffmpeg/ffplay (e.g. `scoop install ffmpeg`).

.PARAMETER Port
    Video RTP port (default 5555). Audio is always this + 2; both are fixed in the app's encoder, so
    change this only if the app changes.

.PARAMETER Record
    Record to an .mp4 in this folder instead of live-playing.

.EXAMPLE
    pwsh scripts/stream_server/receive.ps1
.EXAMPLE
    pwsh scripts/stream_server/receive.ps1 -Record
#>
param(
    [int]$Port = 5555,
    [switch]$Record
)

$ErrorActionPreference = 'Stop'
$here = Split-Path $PSCommandPath -Parent
$sdp = Join-Path $here 'stream.sdp'
$pair = Join-Path $here 'pair_server.py'

# Rewrite both media ports so -Port takes effect. The encoder puts audio on video+2.
$audioPort = $Port + 2
$text = (Get-Content $sdp -Raw) -replace 'm=video \d+ ', "m=video $Port " -replace 'm=audio \d+ ', "m=audio $audioPort "
Set-Content $sdp $text -NoNewline

function Find-Tool([string]$name) {
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $scoop = Join-Path $env:USERPROFILE "scoop\apps\ffmpeg\current\bin\$name.exe"
    if (Test-Path $scoop) { return $scoop }
    return $null
}

$python = (Find-Tool 'python') ?? (Find-Tool 'python3')
if (-not $python) { throw "python 3 not found — pair_server.py answers the app's discovery broadcast." }

# -analyzeduration must stay generous: cut it short and ffmpeg starts before the first SPS/PPS,
# leaving the H.264 decoder with 'non-existing PPS 0 referenced'.
$common = @('-protocol_whitelist', 'file,udp,rtp', '-analyzeduration', '1000000')

if ($Record) {
    $ffmpeg = Find-Tool 'ffmpeg'
    if (-not $ffmpeg) { throw "ffmpeg not found. Install it (e.g. 'scoop install ffmpeg')." }
    # Matroska, not mp4: RTP AAC arrives without a frame size, which the mp4 muxer refuses
    # ("track 1: codec frame size is not set") and then drops the whole audio track.
    $out = Join-Path $here ("fpv_{0}.mkv" -f (Get-Date -Format 'yyyyMMdd_HHmmss'))
    # Audio is re-encoded, not copied. Depacketized LATM packets reach the muxer without usable
    # timestamps, and both mp4 and mkv then write an audio track header with zero packets in it —
    # a file that looks right until you decode it. Copying the same AAC from an ADTS file muxes
    # fine, so this is the RTP path, not the container. Video copies through untouched.
    # The two RTP streams also start from unrelated random timestamps and carry no RTCP sync, so
    # ffmpeg's own clock cuts the audio short; stamping on arrival keeps video and audio whole.
    $recv = @($ffmpeg) + $common + @('-use_wallclock_as_timestamps', '1', '-i', $sdp,
                                     '-c:v', 'copy', '-c:a', 'aac', '-b:a', '96k', $out)
    Write-Host "Will record to $out once the app starts streaming." -ForegroundColor Yellow
} else {
    $ffplay = Find-Tool 'ffplay'
    if (-not $ffplay) { throw "ffplay not found. Install ffmpeg (e.g. 'scoop install ffmpeg'), which bundles ffplay." }
    $recv = @($ffplay) + $common + @('-fflags', 'nobuffer', '-flags', 'low_delay', '-i', $sdp)
    Write-Host "Will open a live window once the app starts streaming." -ForegroundColor Yellow
}

Write-Host "Waiting for the app's FIND-SERVER broadcast — hit Stream in the app (Ctrl+C to stop) ..." -ForegroundColor Cyan
& $python $pair --then @recv
