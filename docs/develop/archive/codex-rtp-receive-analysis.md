# XREAL FPV RTP receive format analysis

Date: 2026-07-22. Primary binary: `jniLibs/arm64-v8a/libmedia_codec.so`
(9,121,256 bytes). Corroborating receiver binary:
`StreamingReceiver_Data/Plugins/media_enc.dll` from StreamingReceiver v1.2.0. The Android binary
contains a statically linked FFmpeg 3.4-era build (`libavformat 57.83.100`); the Windows binary imports
the equivalent FFmpeg 4.x APIs. Addresses below are image-relative virtual addresses unless a full
Windows VA beginning with `0x180...` is shown.

## Verdict

**CONFIRMED — an OSS-only receiver can be built, and no vendor depacketizer is required.** The audio
is ordinary **AAC-LC, 16 kHz, mono**, transported as **RTP MP4A-LATM per RFC 3016 in out-of-band
configuration mode**. The previously unexplained four bytes are not vendor magic:

```text
ff ff ff 03 = 255 + 255 + 255 + 3 = 768
```

They are FFmpeg's standard MP4A-LATM `PayloadLengthInfo()` encoding. The following 768 bytes are one
raw AAC access unit: no ADTS header, no LOAS sync word, no encryption, and no second vendor header.
At 16,000 samples/s and 1,024 samples/access unit this is 64 ms of audio. A 768-byte access unit every
64 ms is exactly 96 kbit/s, consistent with the receiver SDP's `b=AS:96`, despite the sender JSON's
`audioBitRate:128000`.

The minimum receiver is FFmpeg plus this SDP (substitute the desired local addresses only if the host
requires it):

```sdp
v=0
o=- 0 0 IN IP4 127.0.0.1
s=No Name
c=IN IP4 0.0.0.0
t=0 0
m=video 5555 RTP/AVP 96
b=AS:4096
a=rtpmap:96 H264/90000
a=fmtp:96 packetization-mode=1
m=audio 5557 RTP/AVP 97
c=IN IP4 0.0.0.0
b=AS:96
a=rtpmap:97 MP4A-LATM/16000/1
a=fmtp:97 profile-level-id=40;cpresent=0;config=400028103fc0
```

For example:

```sh
ffplay -protocol_whitelist file,udp,rtp xreal.sdp
ffmpeg -protocol_whitelist file,udp,rtp -i xreal.sdp -map 0:a:0 audio.wav
```

This is materially the SDP embedded in the official receiver, not a reconstructed guess.

## The audio format, byte by byte

### RTP and payload layout

**CONFIRMED — each observed audio datagram carries one complete access unit.** After the normal RTP
header (PT 97), the observed payload is:

```text
offset  size  value / meaning
0       1     ff    length accumulator += 255; continue
1       1     ff    length accumulator += 255; continue
2       1     ff    length accumulator += 255; continue
3       1     03    length accumulator += 3; stop (total 768)
4       768   one raw AAC-LC access unit
```

FFmpeg 3.4's `ff_rtp_send_latm()` computes `header_size = size / 0xff + 1`, fills all but the last
length byte with `0xff`, writes `size % 0xff` as the final byte, and then copies the encoded access
unit after it. Its matching `latm_parse_packet()` sums bytes until a byte other than `0xff`, allocates
an `AVPacket` of that size, and copies exactly those bytes into it. See the upstream
[`rtpenc_latm.c`](https://ffmpeg.org/doxygen/3.4/rtpenc__latm_8c_source.html) and
[`rtpdec_latm.c`](https://ffmpeg.org/doxygen/3.4/rtpdec__latm_8c_source.html), which match the version
family and strings compiled into the Android library.

**CONFIRMED — the XREAL sender selects this FFmpeg path.** At `0x21329c..0x2132b0`, the Android
encoder calls an FFmpeg option setter with these arguments:

```text
0x21329c  adrp x1, 0xf2000
0x2132a4  add  x1, x1, #0x33f       ; "rtpflags" @ 0xf233f
0x2132a8  add  x2, x2, #0xf2b       ; "latm" @ 0x12ef2b
0x2132ac  mov  w3, wzr              ; search_flags = 0
0x2132b0  bl   0x44f340             ; av_opt_set-like option setter
```

The next instructions at `0x2132b4..0x2132c4` set bit `0x40` in the owning format context's flags.
The same binary contains FFmpeg's option description `Use MP4A-LATM packetization instead of
MPEG4-GENERIC for AAC` at `0xf5f00`. This connects XREAL's mux setup to `ff_rtp_send_latm()`, which
constructs the four length bytes seen on the wire.

**CONFIRMED — fragmentation is standard and not used by the captured 768-byte access units.** If an
access unit exceeds FFmpeg's maximum RTP payload, `ff_rtp_send_latm()` puts the length field only in
the first RTP packet, continues the access unit in subsequent packets with the same timestamp, and
sets the RTP marker only on the last packet. `latm_parse_packet()` buffers packets until the marker.
The observed 772-byte payload fits in one packet, so the current stream needs no multi-packet audio
reassembly beyond normal loss/reordering handling. A robust general implementation should still
honour the marker and sequence numbers.

### Configuration and decoder parameters

**CONFIRMED — the official receiver supplies the missing configuration out of band.** Its complete
audio SDP template resides at Windows VAs `0x18000ac00..0x18000ac8a`:

```text
m=audio %d RTP/AVP 97
c=IN IP4 0.0.0.0
b=AS:96
a=rtpmap:97 MP4A-LATM/16000/1
a=fmtp:97 profile-level-id=40;cpresent=0;config=400028103fc0
```

The video template immediately preceding it is at `0x18000ab40..0x18000abf4`. The receive setup at
`0x1800033fd..0x18000340b` calls `av_find_input_format("sdp")` and then `avformat_open_input()` on
`"memory.sdp"`; `av_dump_format()` and `avformat_find_stream_info()` follow at
`0x180003476..0x18000348d`. Thus this exact SDP is consumed by libavformat in the official receiver.

`cpresent=0` is important: the `StreamMuxConfig` is in SDP and is not repeated in the RTP payload.
The six-byte hexadecimal value decodes as follows:

```text
StreamMuxConfig 400028103fc0 (MSB first)
  audioMuxVersion              0
  allStreamsSameTimeFraming    1
  numSubFrames                 0
  numProgram                   0
  numLayer                     0
  AudioSpecificConfig:
    audioObjectType            2       AAC Low Complexity (AAC-LC)
    samplingFrequencyIndex     8       16000 Hz
    channelConfiguration       1       mono
    frameLengthFlag            0       1024 samples
    dependsOnCoreCoder         0
    extensionFlag              0
  frameLengthType              0
  latmBufferFullness           0xff
  otherDataPresent             0
  crcCheckPresent              0
  final four bits                      zero padding
```

The standalone AAC `AudioSpecificConfig` an OSS decoder needs is **`14 08`**. FFmpeg 3.4's RTP LATM
SDP parser skips the first 15 StreamMuxConfig bits and passes the remainder through codec parameters.
The Windows receiver's audio-decoder creation passes codec ID `0x15002` (`AV_CODEC_ID_AAC` in that
ABI) to `avcodec_find_decoder()` at `0x18000119c`, calls `avcodec_alloc_context3()` at
`0x1800011a7`, `avcodec_parameters_to_context()` at `0x1800011b6`, and `avcodec_open2()` at
`0x180001234`. Its decode loop calls `avcodec_send_packet()` at `0x18000162f` and
`avcodec_receive_frame()` at `0x180001650`. This proves that the depacketized bytes are decoded as
AAC, not PCM or a private codec.

**CONFIRMED — the effective captured wire rate is 96 kbit/s.** `768 * 8 * 16000 / 1024 = 96000`.
The fixed-size payload is therefore natural for a constant-bit-rate AAC encoder. **UNRESOLVED — why
the requested JSON value `audioBitRate:128000` becomes 96 kbit/s.** It may be overridden by the RTP
path or clamped by the hardware codec. This discrepancy does not affect decoder configuration.

## The video format

**CONFIRMED — port 5555 is standard RTP H.264, packetization mode 1.** The official receiver's SDP at
`0x18000ab40..0x18000abf4` declares PT 96 as `H264/90000` with
`a=fmtp:96 packetization-mode=1`. The live capture already established ordinary H.264 NAL units and
FU-A fragmentation, and a plain FFmpeg SDP decodes the stream. No vendor header or payload transform
is present.

**CONFIRMED — SPS/PPS are available in band for the tested stream.** The official SDP has no
`sprop-parameter-sets`, while the existing plain-SDP receive test starts and decodes successfully.
Consequently the sender provides the parameter sets in RTP. This conclusion is empirical for the
tested encoder session; a receiver should retain the latest SPS/PPS and begin display at an IDR in
the usual way.

**CONFIRMED — receiver engineering can focus on audio configuration, not a custom video format.** A
normal RFC 6184 H.264 depacketizer supporting single-NAL packets, aggregation as offered by its RTP
stack, and FU-A is sufficient. No vendor deviation was found in the captured traffic or embedded
receiver SDP.

## Per-question findings

1. **Audio payload — CONFIRMED.** AAC-LC, 16 kHz, mono, 1,024 samples/access unit. The first four
   bytes encode the access-unit length 768 using RFC 3016/FFmpeg `PayloadLengthInfo`; the remaining
   bytes are the raw AAC access unit. SDP StreamMuxConfig is `400028103fc0`; minimal AAC ASC is
   `1408`.
2. **Minimum OSS recipe — CONFIRMED.** Use the SDP in the Verdict with FFmpeg. For a hand-written
   path, reassemble through the marker if necessary, consume the base-255 length field, and submit
   the resulting byte range as one AAC packet to an AAC decoder configured with ASC `14 08`. There
   is no interleaving or private depacketization in the observed stream.
3. **Video — CONFIRMED.** Standard RFC 6184 H.264, PT 96, 90 kHz clock, packetization mode 1, FU-A,
   with SPS/PPS in band in the tested stream.
4. **Other required traffic — CONFIRMED for the tested direct stream.** Nothing else is required to
   decode or to keep the sender transmitting. Ports 5556 and 5558 carry the normal RTCP companions;
   a passive receiver may ignore sender reports if it does not need wall-clock A/V synchronization.
   RTCP receiver reports are good RTP citizenship but no dependency on them was found. The fact that
   the earlier plain-SDP receiver continuously received video is direct evidence that no proprietary
   receive-side acknowledgement is required.

The reported RTCP “PT 72” is the low seven bits of the RTCP packet-type byte (`200 & 0x7f = 72`) when
viewed through an RTP payload-type interpretation; it is not a fifth media codec.

**CONFIRMED — FIND-SERVER/TCP is control-plane discovery, not media framing.** The official workflow
uses it to discover a receiver, enter a room, tell the Windows UI whether audio will follow, and only
then make the sender start. If the Android app is already configured to send to
`rtp://<receiver>:5555`, a bare OSS receiver does not need to implement or answer that handshake.
Conversely, a clone intended to trigger the unmodified official sample automatically would still need
the documented UDP/TCP control protocol; that requirement is outside RTP decoding.

## What an OSS receiver must implement

- Bind RTP video on UDP 5555 and audio on UDP 5557. Bind 5556/5558 too only if RTCP timing or reports
  are desired.
- Feed the exact SDP above to libavformat/FFmpeg. This is the shortest and best-tested path.
- For a custom audio depacketizer:
  - validate RTP version/PT/SSRC and order packets by sequence number;
  - collect fragments with the same timestamp until marker if the access unit is ever fragmented;
  - from the first byte, add each length octet to a total and continue while it equals `0xff`;
  - take exactly that many following bytes as one AAC access unit (currently 768);
  - pass one access unit per decoder packet with AAC ASC `14 08`;
  - advance presentation time by 1,024 samples (64 ms at 16 kHz), using the RTP timestamp directly;
  - drop an incomplete access unit after sequence loss rather than feeding partial AAC.
- If using a decoder interface that accepts only ADTS, synthesize a seven-byte ADTS header for each
  access unit using MPEG-4, AAC-LC (`profile=1` in ADTS), sampling-frequency index 8, mono, and frame
  length `7 + access_unit_size`. A libavcodec API using extradata `14 08` avoids this conversion.
- Use a standard RFC 6184 H.264 depacketizer on PT 96 and wait for SPS, PPS, and an IDR before showing
  video after a mid-stream join.
- Use RTCP sender reports if accurate cross-stream wall-clock synchronization matters; otherwise RTP
  timestamps are sufficient for independent audio/video pacing.
- Implement FIND-SERVER/TCP only if the receiver must cause an otherwise idle official sender sample
  to begin. It is not needed when the destination URL is supplied directly.

## Unresolved / what a capture experiment would settle

- **UNRESOLVED — bitrate override location.** The wire and official SDP say 96 kbit/s while the JSON
  requests 128 kbit/s. Hooking/logging `AMediaFormat_setInt32` around the AAC encoder setup at
  `0x20cf34..0x20cfbc`, including resolved key names and values, would show whether XREAL supplies
  96,000 or MediaCodec changes it.
- **UNRESOLVED — behavior when configured with 48 kHz.** The official receiver SDP is hard-coded to
  16 kHz mono and the examined repo currently sends 16,000. A fresh 48 kHz capture plus the actual
  receiver-generated SDP would show whether the RTP path truly supports that JSON value or silently
  retains 16 kHz. Do not reuse ASC `14 08` for a genuinely 48 kHz stream; AAC-LC 48 kHz mono would
  require ASC `11 88` and a matching StreamMuxConfig.
- **UNRESOLVED — optional RTP header extensions and exact RTCP SDES text.** The supplied capture
  summary is sufficient to determine media payloads but not to enumerate every RTP X-bit or SDES
  item. A saved pcap would settle these immediately. Standard RTP libraries skip header extensions,
  and neither item is required by the official FFmpeg decode path.
- **LIKELY — one access unit per audio RTP timestamp remains stable.** It is true for every supplied
  observation and is what this FFmpeg sender normally emits. A long capture under loss and a forced
  small MTU would exercise the standard marker-based fragmentation branch and verify that XREAL has
  not constrained it elsewhere.

The decisive capture check is simple: save one PT 97 payload, sum its initial `0xff` continuation
bytes, remove the resulting length field, and submit the remaining 768 bytes to libavcodec as one AAC
packet with extradata `14 08`. It should yield 1,024 signed-PCM samples. The full SDP recipe already
performs exactly those steps inside libavformat and libavcodec.
