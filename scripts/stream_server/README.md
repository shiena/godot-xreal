# FPV stream receiving server (verification)

Receives the Godot app's **first-person-view H.264/RTP stream** (the ported
`libmedia_codec` HW encoder streams to `rtp://<this-PC>:<port>`, codecType 2 = RTP).

Dev/verification tooling. Two twins do the same thing: `receive.ps1` (Windows / PowerShell) and
`receive.sh` (mac / Linux).

## Requirements
- `ffmpeg` (bundles `ffplay`). Windows: `scoop install ffmpeg`. mac: `brew install ffmpeg`. Linux: `apt install ffmpeg`.

## Use
1. Run the server (prints this PC's `rtp://IP:5555` addresses):
   ```
   pwsh scripts/stream_server/receive.ps1          # Windows
   scripts/stream_server/receive.sh                # mac / Linux
   ```
   or record to an mp4 instead of live-playing:
   ```
   pwsh scripts/stream_server/receive.ps1 -Record  # Windows
   scripts/stream_server/receive.sh --record       # mac / Linux
   ```
2. In the app, set the stream target to one of the printed `rtp://<IP>:5555` URLs and start
   streaming (phone-menu toggle). The live view appears in the ffplay window.

## Notes
- The port (default 5555) must match the app's target; override with `-Port`.
- `stream.sdp` describes the RTP stream (H.264, payload type 96, packetization-mode 1); the
  script rewrites its port from `-Port`.
- The PC and glasses must be on the same LAN; allow ffplay/ffmpeg through the firewall on the
  chosen UDP port.
- For a first bring-up you can skip the network entirely: stream **codecType 0 (local mp4)** in
  the app (records on-device), pull the file with `adb pull`, and play it — this validates the
  encode pipeline before RTP.
