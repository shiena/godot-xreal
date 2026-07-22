# FPV stream receiving server (verification)

Receives the Godot app's **first-person-view stream** — H.264 video *and* AAC audio over RTP (the
ported `libmedia_codec` HW encoder, codecType 2 = RTP).

Everything here is open source: python 3 (stdlib only) plus ffmpeg. No vendor SDK, no
StreamingReceiver.exe. Dev/verification tooling — two twins do the same thing: `receive.ps1`
(Windows / PowerShell) and `receive.sh` (mac / Linux).

## Requirements
- `python` 3 — answers the app's discovery broadcast; no `pip install`.
- `ffmpeg` (bundles `ffplay`). Windows: `scoop install ffmpeg`. mac: `brew install ffmpeg`. Linux: `apt install ffmpeg`.

## Two receivers

| | |
|---|---|
| `fpv_server.py` | **Watch it in a browser.** RTP in, FLV over WebSocket out; the browser decodes. No codec library is linked, so no codec copyright or patent licence attaches to the server. |
| `receive.ps1` / `receive.sh` | **Watch or record locally** with ffplay/ffmpeg. |

### Browser: `fpv_server.py`

```
python scripts/stream_server/fpv_server.py     # then open http://localhost:8080
```

Pairing runs in-process, so this is the only thing to start. Open the page, hit **Stream** in the
app, and the video appears. Viewers may join and leave at any time — each one is served from the
next keyframe, with its own timeline rebased to zero.

The server only reframes bytes: RTP → access units → FLV tags → WebSocket. Depacketizing is ~150
lines (RFC 6184 FU-A/STAP-A for video, RFC 3016 LATM for audio) and FLV muxing is a tag header plus
an `AVCDecoderConfigurationRecord` / `AudioSpecificConfig` — the browser's own H.264 and AAC decoders
do the rest, via [mpegts.js](https://github.com/xqq/mpegts.js/) (Apache-2.0) and Media Source
Extensions. `fpv_player.html` loads mpegts.js from a CDN; to run offline, vendor it next to the page.

Options: `--http-port`, `--video-port` (audio is `+2`), `--audio-rate` / `--audio-channels` (must
match the encoder's `audioSampleRate`, default 16000 mono), `--ip` to force the advertised address,
`--control-port` (see [Stopping a receiver](#stopping-a-receiver)).

### ffplay / ffmpeg: `receive.ps1` / `receive.sh`

1. Start the receiver **first** and leave it running:
   ```
   pwsh scripts/stream_server/receive.ps1          # Windows
   scripts/stream_server/receive.sh                # mac / Linux
   ```
   or record to an .mkv instead of live-playing:
   ```
   pwsh scripts/stream_server/receive.ps1 -Record  # Windows
   scripts/stream_server/receive.sh --record       # mac / Linux
   ```
2. Hit **Stream** in the app (Camera tab). That is all — you never type an address into the app.

The ordering is not cosmetic. The app finds this PC itself, and ffmpeg fed an SDP starts probing
immediately and gives up long before a hand-driven app gets around to sending. So `pair_server.py`
holds the handshake and launches ffplay/ffmpeg (`--then`) at the moment the app actually starts
sending.

## Stopping a receiver

Ctrl+C in its own window works. From anywhere else:

```
pwsh scripts/stream_server/receive.ps1 -Stop     # Windows
scripts/stream_server/receive.sh --stop          # mac / Linux
```

That stops either receiver — `fpv_server.py` too, since both share the same code. Stop one before
starting another: two servers can bind the same port and then split the app's discovery reply
between them, which looks like the app timing out for no reason.

It **asks** the server to stop rather than killing it, and that distinction is the whole reason it
exists. The server owns the ffmpeg it launched, so it can hand it a Ctrl+Break and wait — ffmpeg
then finalises the file (`Exiting normally, received signal 2`). Nothing outside that process can do
the same on Windows, which has no way to deliver a console interrupt to another process's group; a
receiver torn down with `Stop-Process` mid-record leaves an unplayable file. Measured: 0 bytes when
killed, a complete 12.09 s recording when stopped this way.

The server listens for that request on **loopback only** (`127.0.0.1:6004`, `--control-port` to
change). It is deliberately not reachable from the LAN — whoever stops a receiver is sitting at the
machine running it, and an unauthenticated stop endpoint open to the network would be a gift. By
hand:

```
curl -X POST http://127.0.0.1:6004/shutdown      # stop
curl http://127.0.0.1:6004/                      # "running", if one is
```

## How the app finds you

There is no "stream to this IP" field: the app broadcasts and expects a peer to answer, the way
XREAL's own StreamingReceiver does. `pair_server.py` reimplements just enough of that, mirroring
`addons/godot_xreal/features/xreal_stream_pairing.gd`:

1. UDP broadcast `FIND-SERVER` to `255.255.255.255:6001` → we reply `"<our-ip>:<tcp-port>"`.
2. TCP to that port, `EnterRoom` → we echo it as the ack.
3. `MsgSync` `[u64 msgid]{"useAudio":bool}` → we reply with the same msgid and `{"success":true}`.
4. Streaming starts; `HeartBeat` every second. **Dropping the TCP link stops the stream**, so
   `pair_server.py` must stay up for as long as you want frames.

The discovered port is the *control* port. RTP itself is hard-coded: **5555 video, 5557 audio**
(RTCP on 5556 / 5558). `-Port`/`--port` sets the video port and derives audio as `+2`; change it
only if the app changes.

## Wire format

Both streams are plain standards — verified on device by decoding them, not assumed:

| | |
|---|---|
| Video | RFC 6184 H.264, PT 96, FU-A fragmentation, in-band SPS/PPS, 1280×720 @ ~29.8 fps |
| Audio | RFC 3016 **MP4A-LATM**, PT 97, AAC-LC 16 kHz mono, 1024 samples/frame |

Each audio RTP payload is one LATM `AudioMuxElement` with `cpresent=0`: a `PayloadLengthInfo` of
`0xff` bytes (each +255) terminated by a byte < 255, then exactly that many bytes of one raw AAC
access unit. So `ff ff ff 03` = 255·3+3 = 768, and the payload is 772 bytes. Unwrapping those AUs
by hand and re-wrapping them in ADTS decodes with **zero** errors — 60 AUs → 61,440 samples.

`stream.sdp`'s `config=400028103fc0` is the `StreamMuxConfig`; the AudioSpecificConfig inside it is
AAC-LC / 16 kHz / mono / 1024-sample frames, which is what ffmpeg reports back.

## Notes
- The PC and phone must be on the same LAN; allow python and ffmpeg/ffplay through the firewall on
  UDP 6001 and 5555–5558, and TCP 6002.
- `-analyzeduration` stays at 1 s on purpose. Shortening it makes ffmpeg start before the first
  SPS/PPS and the decoder reports `non-existing PPS 0 referenced`.
- Recording passes `-use_wallclock_as_timestamps 1`: the two RTP streams start from unrelated random
  timestamps and carry no RTCP sync, so ffmpeg's own clock otherwise cuts the audio short (measured:
  3.9 s of audio kept out of an 8 s capture; with the flag, 7.99 s).
- Audio is **microphone only** — Godot's Android audio driver offers no loopback of the app's own
  output. See the streaming section of the top-level README.
- **A silent audio track in a quiet room is expected, not a bug.** The encoder opens the mic as
  `VOICE_COMMUNICATION` with an Acoustic Echo Canceler and Noise Suppression attached
  (`adb shell dumpsys audio` shows this under `RecordActivityMonitor`), and that gate floors ambient
  noise to exact zeros — every sample identical, ffmpeg reporting mean == max == -91 dB. Measured:
  dead silence with the room quiet, then -40.9 dB mean / -25.3 dB peak the moment a tone played
  nearby. Make a noise before concluding the audio path is broken.
- Keep every `print()` in `pair_server.py` ASCII. On a Japanese Windows console (cp932) a stray
  em dash raises `UnicodeEncodeError`, which kills the control thread mid-session: discovery keeps
  answering while the app times out on the handshake, which is a thoroughly misleading symptom.
- For a first bring-up you can skip the network entirely: stream **codecType 0 (local mp4)** in the
  app (records on-device), pull the file with `adb pull`, and play it — this validates the encode
  pipeline before RTP.
