#!/usr/bin/env bash
# Receiving server for the XREAL first-person-view H.264/RTP stream (the Godot app's streaming cast
# sends to rtp://<this-PC>:<port>). Listens on the UDP port and plays the live stream with ffplay
# (or records it to an .mp4 with ffmpeg when --record is given). Uses stream.sdp next to this script.
#
# The app must be pointed at THIS PC's IP + port (printed below). ffmpeg/ffplay must be installed
# (e.g. `brew install ffmpeg` / `apt install ffmpeg`).
#
# Usage:
#   scripts/stream_server/receive.sh                 # live-play on UDP 5555
#   scripts/stream_server/receive.sh --port 5555 --record   # record to an .mp4 in this folder
#
# The .ps1 twin (receive.ps1) is the Windows version; this .sh is for mac/Linux. Keep them in sync.
set -euo pipefail

port=5555
record=0
while [ $# -gt 0 ]; do
    case "$1" in
        --port) port="$2"; shift 2 ;;
        --record) record=1; shift ;;
        -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sdp="$here/stream.sdp"

# Rewrite the SDP's port so --port takes effect (portable across BSD/GNU sed).
sed "s/m=video [0-9]* /m=video $port /" "$sdp" > "$sdp.tmp" && mv "$sdp.tmp" "$sdp"

# Show this host's LAN IPv4 addresses so you know what to configure in the app.
echo "Point the app's stream target at one of these  rtp://<IP>:$port :"
{
    if command -v ip >/dev/null 2>&1; then
        ip -4 -o addr show 2>/dev/null | awk '{print $4}' | cut -d/ -f1
    elif command -v ifconfig >/dev/null 2>&1; then
        ifconfig 2>/dev/null | awk '/inet /{print $2}'
    fi
} | grep -vE '^(127\.|169\.254\.)' | sort -u | while read -r ip; do
    echo "  rtp://$ip:$port"
done
echo ""

common=(-protocol_whitelist file,udp,rtp -fflags nobuffer -flags low_delay -analyzeduration 1000000 -i "$sdp")

if [ "$record" -eq 1 ]; then
    command -v ffmpeg >/dev/null 2>&1 || { echo "ffmpeg not found. Install it (e.g. 'brew install ffmpeg')." >&2; exit 1; }
    out="$here/fpv_$RANDOM.mp4"
    echo "Recording to $out  (Ctrl+C to stop) ..."
    ffmpeg "${common[@]}" -c copy "$out"
else
    command -v ffplay >/dev/null 2>&1 || { echo "ffplay not found. Install ffmpeg (e.g. 'brew install ffmpeg'), which bundles ffplay." >&2; exit 1; }
    echo "Listening on UDP $port — start streaming in the app (Ctrl+C to stop) ..."
    ffplay "${common[@]}"
fi
