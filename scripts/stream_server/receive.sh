#!/usr/bin/env bash
# Receiving server for the XREAL first-person-view stream (H.264 + AAC over RTP). Answers the app's
# FIND-SERVER discovery, and once the app starts streaming, plays it live with ffplay (or records it
# with ffmpeg when --record is given). Uses stream.sdp and pair_server.py next to this script.
#
# You do NOT type an address into the app: it discovers this host itself. Start this script first,
# then hit Stream in the app — the receiver is launched automatically at the right moment, because
# ffmpeg gives up probing long before a hand-driven app gets around to sending.
#
# Needs python 3 (stdlib only) and ffmpeg/ffplay (e.g. `brew install ffmpeg` / `apt install ffmpeg`).
#
# Usage:
#   scripts/stream_server/receive.sh                 # live-play
#   scripts/stream_server/receive.sh --record        # record to an .mkv in this folder
#   scripts/stream_server/receive.sh --port 5555     # video port; audio is always this + 2
#   scripts/stream_server/receive.sh --stop          # stop a receiver started elsewhere, and exit
#
# --stop posts to the running server's loopback control port (default 6004, --control-port to
# change) and also stops fpv_server.py. It asks the server to stop rather than killing it, so an
# in-progress --record capture is finalised: the server interrupts ffmpeg and waits for it. Stop one
# receiver before starting another - two servers binding the same port split the app's discovery
# reply between them, which looks like the app timing out for no reason.
#
# The .ps1 twin (receive.ps1) is the Windows version; this .sh is for mac/Linux. Keep them in sync.
set -euo pipefail

port=5555
record=0
stop=0
control_port=6004
while [ $# -gt 0 ]; do
    case "$1" in
        --port) port="$2"; shift 2 ;;
        --record) record=1; shift ;;
        --stop) stop=1; shift ;;
        --control-port) control_port="$2"; shift 2 ;;
        -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sdp="$here/stream.sdp"
pair="$here/pair_server.py"

if [ "$stop" -eq 1 ]; then
    url="http://127.0.0.1:$control_port/shutdown"
    if command -v curl >/dev/null 2>&1; then
        ok=$(curl -fsS -m 10 -X POST "$url" 2>/dev/null || true)
    else
        # No curl: bash can speak just enough HTTP on its own. The server closes the connection as
        # it exits, so read what arrives and do not treat the close as an error.
        ok=$(exec 3<>"/dev/tcp/127.0.0.1/$control_port" 2>/dev/null && {
                printf 'POST /shutdown HTTP/1.0\r\n\r\n' >&3
                cat <&3 2>/dev/null | tail -1
             } || true)
    fi
    if [ -n "$ok" ]; then
        echo "Receiver stopped."
    else
        echo "No receiver answered on 127.0.0.1:$control_port." >&2
        echo "It may not be running, or was started with a different --control-port." >&2
    fi
    exit 0
fi

# Rewrite both media ports so --port takes effect. The encoder puts audio on video+2.
audio_port=$((port + 2))
sed -e "s/m=video [0-9]* /m=video $port /" -e "s/m=audio [0-9]* /m=audio $audio_port /" \
    "$sdp" > "$sdp.tmp" && mv "$sdp.tmp" "$sdp"

python=""
for c in python3 python; do
    if command -v "$c" >/dev/null 2>&1; then python="$c"; break; fi
done
[ -n "$python" ] || { echo "python 3 not found - pair_server.py answers the app's discovery broadcast." >&2; exit 1; }

# -analyzeduration must stay generous: cut it short and ffmpeg starts before the first SPS/PPS,
# leaving the H.264 decoder with 'non-existing PPS 0 referenced'.
common=(-protocol_whitelist file,udp,rtp -analyzeduration 1000000)

if [ "$record" -eq 1 ]; then
    command -v ffmpeg >/dev/null 2>&1 || { echo "ffmpeg not found. Install it (e.g. 'brew install ffmpeg')." >&2; exit 1; }
    # Matroska, not mp4: RTP AAC arrives without a frame size, which the mp4 muxer refuses
    # ("track 1: codec frame size is not set") and then drops the whole audio track.
    out="$here/fpv_$(date +%Y%m%d_%H%M%S).mkv"
    # Audio is re-encoded, not copied. Depacketized LATM packets reach the muxer without usable
    # timestamps, and both mp4 and mkv then write an audio track header with zero packets in it -
    # a file that looks right until you decode it. Copying the same AAC from an ADTS file muxes
    # fine, so this is the RTP path, not the container. Video copies through untouched.
    # The two RTP streams also start from unrelated random timestamps and carry no RTCP sync, so
    # ffmpeg's own clock cuts the audio short; stamping on arrival keeps video and audio whole.
    recv=(ffmpeg "${common[@]}" -use_wallclock_as_timestamps 1 -i "$sdp" \
          -c:v copy -c:a aac -b:a 96k "$out")
    echo "Will record to $out once the app starts streaming."
else
    command -v ffplay >/dev/null 2>&1 || { echo "ffplay not found. Install ffmpeg (e.g. 'brew install ffmpeg'), which bundles ffplay." >&2; exit 1; }
    recv=(ffplay "${common[@]}" -fflags nobuffer -flags low_delay -i "$sdp")
    echo "Will open a live window once the app starts streaming."
fi

echo "Waiting for the app's FIND-SERVER broadcast - hit Stream in the app."
echo "Stop with Ctrl+C here, or 'receive.sh --stop' from anywhere."
# --then must stay last: it swallows the rest of the command line.
exec "$python" "$pair" --control-port "$control_port" --then "${recv[@]}"
