#!/usr/bin/env python3
"""FPV receive server: RTP in, FLV over WebSocket out, browser plays it.

The server never decodes anything. It reframes bytes: RTP -> access units -> FLV tags -> WebSocket,
and the browser's own H.264/AAC decoders do the work. That is the whole point of the design — no
codec library is linked, so no codec copyright licence and no patent licence attaches to this
server. Only the browser decodes, and it already ships decoders.

Pipeline:

    app --RTP 5555 (H.264, RFC 6184 FU-A)--> depacketize -+
    app --RTP 5557 (AAC,   RFC 3016 LATM )--> depacketize -+-> FLV tags -> WebSocket -> mpegts.js

Both wire formats were confirmed on device by decoding them; see scripts/stream_server/README.md
and docs/archive/codex-rtp-receive-analysis.md.

Pairing is mandatory: the app broadcasts FIND-SERVER and streams only to whoever answers, so this
runs pair_server.run() in a thread rather than making you start a second process.

Usage:
    python scripts/stream_server/fpv_server.py          # then open http://localhost:8080
    python scripts/stream_server/fpv_server.py --http-port 9000 --audio-rate 16000

Standard library only - no pip install. Keep every print() ASCII: a stray em dash raises
UnicodeEncodeError on a cp932 console and kills the thread that printed it.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.server
import os
import queue
import socket
import struct
import sys
import threading
import time

import pair_server

HERE = os.path.dirname(os.path.abspath(__file__))
PLAYER_HTML = os.path.join(HERE, "fpv_player.html")

VIDEO_CLOCK = 90000  # RTP clock for H.264, fixed by RFC 6184


# --------------------------------------------------------------------------------------------
# RTP
# --------------------------------------------------------------------------------------------

def rtp_payload(pkt: bytes) -> "tuple[int, int, bool, bytes] | None":
    """-> (payload_type, timestamp, marker, payload), or None if this is not RTP v2."""
    if len(pkt) < 12 or (pkt[0] >> 6) != 2:
        return None
    csrc = pkt[0] & 0x0F
    has_ext = (pkt[0] >> 4) & 1
    marker = bool(pkt[1] >> 7)
    ptype = pkt[1] & 0x7F
    ts = struct.unpack_from("!I", pkt, 4)[0]
    off = 12 + csrc * 4
    if has_ext:
        if len(pkt) < off + 4:
            return None
        ext_words = struct.unpack_from("!H", pkt, off + 2)[0]
        off += 4 + ext_words * 4
    if off > len(pkt):
        return None
    return ptype, ts, marker, pkt[off:]


class Timeline:
    """Turns each stream's RTP clock into one shared millisecond timeline.

    The video and audio streams start from unrelated random RTP timestamps and no RTCP SR ever
    correlates them (measured), so the arrival time of each stream's first packet is what anchors
    them to each other. Within a stream the RTP clock still drives the timing, which keeps playback
    smooth instead of inheriting network jitter.
    """

    def __init__(self) -> None:
        self._origin: float | None = None
        self._base: dict[str, tuple[int, float]] = {}
        self._lock = threading.Lock()

    def stamp(self, stream: str, rtp_ts: int, clock: int) -> int:
        now = time.monotonic()
        with self._lock:
            if self._origin is None:
                self._origin = now
            base = self._base.get(stream)
            if base is None:
                base = (rtp_ts, now)
                self._base[stream] = base
            origin = self._origin
        base_ts, base_wall = base
        # RTP timestamps are 32-bit and wrap; take the shortest signed distance.
        delta = (rtp_ts - base_ts) & 0xFFFFFFFF
        if delta > 0x7FFFFFFF:
            delta -= 0x100000000
        return int(((base_wall - origin) + delta / clock) * 1000)


# --------------------------------------------------------------------------------------------
# Depacketizers
# --------------------------------------------------------------------------------------------

class H264Depacketizer:
    """RFC 6184 -> access units. Handles single NAL, STAP-A (24) and FU-A (28).

    SPS/PPS are pulled out rather than forwarded: FLV carries them once, in the
    AVCDecoderConfigurationRecord, not per frame.
    """

    def __init__(self) -> None:
        self.sps: bytes | None = None
        self.pps: bytes | None = None
        self._nals: list[bytes] = []
        self._ts: int | None = None
        self._fu: bytearray | None = None

    def push(self, ts: int, marker: bool, payload: bytes) -> "list[tuple[int, list[bytes]]]":
        out: list[tuple[int, list[bytes]]] = []
        if not payload:
            return out
        if self._ts is not None and ts != self._ts and self._nals:
            out.append((self._ts, self._nals))
            self._nals = []
        self._ts = ts

        kind = payload[0] & 0x1F
        if kind == 28:  # FU-A
            if len(payload) < 2:
                return out
            head = payload[1]
            start, end, nal_type = head & 0x80, head & 0x40, head & 0x1F
            if start:
                self._fu = bytearray([(payload[0] & 0xE0) | nal_type])
            if self._fu is not None:
                self._fu += payload[2:]
                if end:
                    self._take(bytes(self._fu))
                    self._fu = None
        elif kind == 24:  # STAP-A
            i = 1
            while i + 2 <= len(payload):
                size = struct.unpack_from("!H", payload, i)[0]
                i += 2
                if size == 0 or i + size > len(payload):
                    break
                self._take(payload[i:i + size])
                i += size
        elif 1 <= kind <= 23:
            self._take(payload)

        if marker and self._nals:
            out.append((ts, self._nals))
            self._nals = []
        return out

    def _take(self, nal: bytes) -> None:
        kind = nal[0] & 0x1F
        if kind == 7:
            self.sps = nal
        elif kind == 8:
            self.pps = nal
        elif kind == 9:  # access unit delimiter - noise once the AU boundary is known
            pass
        else:
            self._nals.append(nal)


def latm_aus(payload: bytes) -> "list[bytes]":
    """RFC 3016 AudioMuxElement with cpresent=0 -> raw AAC access units.

    PayloadLengthInfo is a run of 0xff bytes each adding 255, terminated by a byte < 255. This is
    what `ff ff ff 03` (= 768) is; reading it as a magic number is what made this format look
    proprietary for a while.
    """
    aus: list[bytes] = []
    pos = 0
    while pos < len(payload):
        size = 0
        while pos < len(payload) and payload[pos] == 0xFF:
            size += 255
            pos += 1
        if pos >= len(payload):
            break
        size += payload[pos]
        pos += 1
        if size == 0 or pos + size > len(payload):
            break
        aus.append(payload[pos:pos + size])
        pos += size
    return aus


# --------------------------------------------------------------------------------------------
# FLV
# --------------------------------------------------------------------------------------------

FLV_HEADER = b"FLV\x01\x05\x00\x00\x00\x09" + b"\x00\x00\x00\x00"  # flags 5 = audio + video

AAC_SAMPLE_RATES = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050,
                    16000, 12000, 11025, 8000, 7350]


def flv_tag(tag_type: int, timestamp: int, body: bytes) -> bytes:
    ts = max(0, timestamp)
    return b"".join([
        bytes([tag_type]),
        len(body).to_bytes(3, "big"),
        (ts & 0xFFFFFF).to_bytes(3, "big"),
        bytes([(ts >> 24) & 0xFF]),
        b"\x00\x00\x00",  # stream id
        body,
        (11 + len(body)).to_bytes(4, "big"),
    ])


def _amf_string(s: str) -> bytes:
    raw = s.encode("utf-8")
    return b"\x02" + len(raw).to_bytes(2, "big") + raw


def _amf_prop(key: str, value: float) -> bytes:
    raw = key.encode("utf-8")
    return len(raw).to_bytes(2, "big") + raw + b"\x00" + struct.pack("!d", value)


def script_tag() -> bytes:
    """onMetaData. Dimensions are left to the SPS; this just declares the codecs and 'live'."""
    props = [_amf_prop("duration", 0.0), _amf_prop("videocodecid", 7.0), _amf_prop("audiocodecid", 10.0)]
    body = _amf_string("onMetaData") + b"\x08" + len(props).to_bytes(4, "big") + b"".join(props) + b"\x00\x00\x09"
    return flv_tag(18, 0, body)


def avc_sequence_header(sps: bytes, pps: bytes) -> bytes:
    record = b"".join([
        b"\x01", sps[1:4],          # version, profile, compat, level - straight out of the SPS
        b"\xff",                    # 6 reserved bits + lengthSizeMinusOne = 3 (4-byte NAL lengths)
        b"\xe1", len(sps).to_bytes(2, "big"), sps,
        b"\x01", len(pps).to_bytes(2, "big"), pps,
    ])
    return flv_tag(9, 0, b"\x17\x00\x00\x00\x00" + record)


def avc_frame(nals: "list[bytes]", timestamp: int, keyframe: bool) -> bytes:
    body = b"".join(len(n).to_bytes(4, "big") + n for n in nals)
    # Composition time is 0: this encoder emits no B-frames, and RTP alone carries no DTS/PTS split.
    head = b"\x17\x01\x00\x00\x00" if keyframe else b"\x27\x01\x00\x00\x00"
    return flv_tag(9, timestamp, head + body)


def aac_sequence_header(sample_rate: int, channels: int) -> bytes:
    idx = AAC_SAMPLE_RATES.index(sample_rate)
    # AudioSpecificConfig: AOT 2 (AAC-LC), sampling frequency index, channel config, then zeroes.
    asc = bytes([(2 << 3) | (idx >> 1), ((idx & 1) << 7) | (channels << 3)])
    return flv_tag(8, 0, b"\xaf\x00" + asc)


def aac_frame(au: bytes, timestamp: int) -> bytes:
    # 0xaf: AAC, and the rate/size/type nibbles FLV wants for it are fixed - players read the
    # AudioSpecificConfig above instead.
    return flv_tag(8, timestamp, b"\xaf\x01" + au)


# --------------------------------------------------------------------------------------------
# WebSocket fan-out
# --------------------------------------------------------------------------------------------

WS_GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def ws_frame(payload: bytes) -> bytes:
    """Server-to-client binary frame, unmasked (RFC 6455 forbids the server masking)."""
    n = len(payload)
    if n < 126:
        head = bytes([0x82, n])
    elif n <= 0xFFFF:
        head = bytes([0x82, 126]) + n.to_bytes(2, "big")
    else:
        head = bytes([0x82, 127]) + n.to_bytes(8, "big")
    return head + payload


class Client:
    """One browser. Tags are queued so a slow client cannot stall the RTP threads."""

    def __init__(self, conn: socket.socket, addr: str) -> None:
        self.conn = conn
        self.addr = addr
        self.q: queue.Queue = queue.Queue(maxsize=512)
        self.alive = True
        # Each client's timeline is rebased to its own first tag, so joining an hour in still
        # starts at ~0 instead of handing MSE a one-hour gap.
        self.t0: int | None = None
        self.armed = False  # forward nothing until a keyframe gives the decoder a starting point

    def send(self, data: bytes) -> None:
        try:
            self.q.put_nowait(data)
        except queue.Full:
            self.alive = False  # hopelessly behind; let it reconnect

    def pump(self) -> None:
        try:
            while self.alive:
                data = self.q.get()
                if data is None:
                    break
                self.conn.sendall(ws_frame(data))
        except OSError:
            pass
        finally:
            self.alive = False
            try:
                self.conn.close()
            except OSError:
                pass


class Hub:
    def __init__(self, audio_rate: int, channels: int) -> None:
        self.clients: list[Client] = []
        self.lock = threading.Lock()
        self.avc_header: bytes | None = None
        self.aac_header = aac_sequence_header(audio_rate, channels)

    def add(self, client: Client) -> None:
        client.send(FLV_HEADER)
        client.send(script_tag())
        if self.avc_header:
            client.send(self.avc_header)
        client.send(self.aac_header)
        with self.lock:
            self.clients.append(client)
        print(f"[web] viewer connected: {client.addr} ({len(self.clients)} total)", flush=True)

    def set_avc_header(self, tag: bytes) -> None:
        if tag == self.avc_header:
            return
        self.avc_header = tag
        with self.lock:
            for c in self.clients:
                c.send(tag)

    def broadcast(self, tag: bytes, timestamp: int, keyframe: bool, is_video: bool) -> None:
        with self.lock:
            live = [c for c in self.clients if c.alive]
            dropped = len(self.clients) - len(live)
            self.clients = live
        if dropped:
            print(f"[web] dropped {dropped} viewer(s) that fell behind", flush=True)
        for c in live:
            if not c.armed:
                if not (is_video and keyframe):
                    continue
                c.armed = True
            if c.t0 is None:
                c.t0 = timestamp
            rel = timestamp - c.t0
            if rel < 0:
                # Audio buffered slightly ahead of the keyframe we started this client on. Clamping
                # it to 0 instead would emit several tags sharing timestamp 0, which is enough to
                # make a downstream muxer report non-monotonic DTS. Dropping is right: these frames
                # precede the first picture this client will ever see.
                continue
            c.send(_retime(tag, rel))


def _retime(tag: bytes, timestamp: int) -> bytes:
    """Rewrite a built tag's timestamp in place - cheaper than rebuilding it per client."""
    ts = max(0, timestamp)
    out = bytearray(tag)
    out[4:7] = (ts & 0xFFFFFF).to_bytes(3, "big")
    out[7] = (ts >> 24) & 0xFF
    return bytes(out)


# --------------------------------------------------------------------------------------------
# Receiving threads
# --------------------------------------------------------------------------------------------

def receive_video(port: int, hub: Hub, timeline: Timeline) -> None:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", port))
    depack = H264Depacketizer()
    seen = False
    last_ms = -1
    while True:
        parsed = rtp_payload(sock.recv(65535))
        if not parsed:
            continue
        _pt, ts, marker, payload = parsed
        for au_ts, nals in depack.push(ts, marker, payload):
            if depack.sps and depack.pps:
                hub.set_avc_header(avc_sequence_header(depack.sps, depack.pps))
            if not nals or hub.avc_header is None:
                continue
            keyframe = any((n[0] & 0x1F) == 5 for n in nals)
            if not seen:
                print(f"[rtp] video: first access unit ({len(nals)} NAL(s), keyframe={keyframe})", flush=True)
                seen = True
            # The app pushes frames at the glasses' refresh rate rather than the configured 30 fps,
            # so neighbouring frames sometimes land in the same millisecond. FLV timestamps are
            # milliseconds, so nudge duplicates forward rather than handing players a non-monotonic
            # DTS - the visible timing error is under a frame.
            ms = timeline.stamp("v", au_ts, VIDEO_CLOCK)
            if ms <= last_ms:
                ms = last_ms + 1
            last_ms = ms
            hub.broadcast(avc_frame(nals, 0, keyframe), ms, keyframe, True)


def receive_audio(port: int, hub: Hub, timeline: Timeline, clock: int) -> None:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", port))
    seen = False
    while True:
        parsed = rtp_payload(sock.recv(65535))
        if not parsed:
            continue
        _pt, ts, _marker, payload = parsed
        aus = latm_aus(payload)
        if aus and not seen:
            print(f"[rtp] audio: first access unit ({len(aus[0])} bytes)", flush=True)
            seen = True
        stamp = timeline.stamp("a", ts, clock)
        for i, au in enumerate(aus):
            # Sub-frames within one payload are 1024 samples apart; there is normally just one.
            hub.broadcast(aac_frame(au, 0), stamp + i * 1024 * 1000 // clock, False, False)


# --------------------------------------------------------------------------------------------
# HTTP + WebSocket server
# --------------------------------------------------------------------------------------------

def make_handler(hub: Hub):
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, fmt, *args):  # noqa: A003 - quiet; we do our own logging
            pass

        def do_GET(self):  # noqa: N802 - name fixed by BaseHTTPRequestHandler
            if self.headers.get("Upgrade", "").lower() == "websocket":
                self._upgrade()
            elif self.path in ("/", "/index.html"):
                self._serve_player()
            else:
                self.send_error(404)

        def _serve_player(self):
            try:
                with open(PLAYER_HTML, "rb") as f:
                    body = f.read()
            except OSError as e:
                self.send_error(500, f"cannot read player page: {e}")
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def _upgrade(self):
            key = self.headers.get("Sec-WebSocket-Key", "")
            accept = base64.b64encode(hashlib.sha1(key.encode() + WS_GUID).digest()).decode()
            self.wfile.write(
                b"HTTP/1.1 101 Switching Protocols\r\n"
                b"Upgrade: websocket\r\nConnection: Upgrade\r\n"
                b"Sec-WebSocket-Accept: " + accept.encode() + b"\r\n\r\n")
            self.wfile.flush()
            client = Client(self.connection, f"{self.client_address[0]}:{self.client_address[1]}")
            hub.add(client)
            client.pump()  # owns the socket from here; returning would close it
            self.close_connection = True

    return Handler


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--video-port", type=int, default=5555, help="RTP video port (default 5555)")
    ap.add_argument("--audio-port", type=int, default=0, help="RTP audio port (default: video + 2)")
    ap.add_argument("--audio-rate", type=int, default=16000,
                    help="AAC sample rate the encoder was configured with (default 16000)")
    ap.add_argument("--audio-channels", type=int, default=1, help="AAC channel count (default 1)")
    ap.add_argument("--http-port", type=int, default=8080, help="web port (default 8080)")
    ap.add_argument("--control-port", type=int, default=pair_server.CONTROL_PORT,
                    help=f"loopback shutdown port (default {pair_server.CONTROL_PORT})")
    ap.add_argument("--ip", help="address to advertise to the app (default: the NIC facing it)")
    args = ap.parse_args()

    audio_port = args.audio_port or args.video_port + 2
    if args.audio_rate not in AAC_SAMPLE_RATES:
        print(f"[fpv] --audio-rate {args.audio_rate} is not an AAC sample rate", file=sys.stderr)
        return 2

    hub = Hub(args.audio_rate, args.audio_channels)
    timeline = Timeline()

    try:
        http_server = http.server.ThreadingHTTPServer(("0.0.0.0", args.http_port), make_handler(hub))
    except OSError as e:
        print(f"[fpv] cannot bind HTTP {args.http_port}: {e}", file=sys.stderr)
        return 1

    threading.Thread(target=receive_video, args=(args.video_port, hub, timeline), daemon=True).start()
    threading.Thread(target=receive_audio,
                     args=(audio_port, hub, timeline, args.audio_rate), daemon=True).start()
    threading.Thread(target=http_server.serve_forever, daemon=True).start()

    print(f"[fpv] RTP video {args.video_port}, audio {audio_port} "
          f"(AAC {args.audio_rate} Hz x{args.audio_channels})", flush=True)
    print(f"[fpv] open http://localhost:{args.http_port} in a browser, then hit Stream in the app",
          flush=True)
    return pair_server.run(args.ip, hint="[pair] pairing runs in-process; no second server needed.",
                           control_port=args.control_port)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("[fpv] stopped", flush=True)
