#!/usr/bin/env python3
"""Minimal FIND-SERVER responder — the pairing half of an OSS receiver.

The app does not stream to an address you type in: it discovers its peer, the way XREAL's own
StreamingReceiver expects. So a receiver built from ffmpeg alone never sees a packet. This answers
that handshake and then gets out of the way — ffplay/ffmpeg does the actual receiving, driven by
stream.sdp.

Protocol (mirrors addons/godot_xreal/features/xreal_stream_pairing.gd, which was validated against
the real StreamingReceiver.exe):

  1. The app UDP-broadcasts the ASCII "FIND-SERVER" to 255.255.255.255:6001.
     We reply "<our-ip>:<tcp-port>" to the sender. Note the discovered port is the *control* port —
     RTP is hard-coded to 5555 (video) / 5557 (audio) regardless.
  2. The app opens TCP to that port and sends EnterRoom(4). We echo EnterRoom(4) as the ack.
  3. The app sends MsgSync(7) carrying [u64 msgid le]{"useAudio":bool}. We reply MsgSync(7) with the
     same msgid and {"success":true}.
  4. The app then starts streaming and sends HeartBeat(3) every second. Dropping the TCP link stops
     the stream, so stay connected for as long as you want frames.

Ordering matters: with an SDP input ffmpeg starts probing immediately and gives up long before a
hand-driven app gets around to sending, so the receiver must not start until the app is streaming.
`--then` automates exactly that — it runs the rest of the command line once pairing succeeds.

  Framing is little-endian [u16 length][u16 type][payload], length including the 4-byte header.

Usage:
    python scripts/stream_server/pair_server.py                # answer discovery, print the RTP URL
    python scripts/stream_server/pair_server.py --ip 192.168.0.2   # force the advertised address
    python scripts/stream_server/pair_server.py --then ffplay -i stream.sdp   # launch on pairing

Standard library only — no pip install.
"""

from __future__ import annotations

import argparse
import json
import socket
import struct
import subprocess
import sys
import threading

DISCOVERY_PORT = 6001
FIND_MSG = b"FIND-SERVER"

# Set by --then: the receiver to launch once the app is actually streaming.
_then_cmd: list[str] = []
_then_proc: "subprocess.Popen | None" = None


def _launch_then() -> None:
    """Start the --then command, once. Re-pairing must not stack a second receiver on the port."""
    global _then_proc
    if not _then_cmd or (_then_proc and _then_proc.poll() is None):
        return
    print("[pair] launching: %s" % " ".join(_then_cmd), flush=True)
    try:
        _then_proc = subprocess.Popen(_then_cmd)
    except OSError as e:
        print("[pair] cannot launch receiver: %s" % e, file=sys.stderr, flush=True)

# MessageType, from the SDK sample's enum.
NONE, CONNECTED, DISCONNECT, HEARTBEAT, ENTER_ROOM, EXIT_ROOM, UPDATE_CAMERA, MSG_SYNC = range(8)
TYPE_NAMES = {
    HEARTBEAT: "HeartBeat",
    ENTER_ROOM: "EnterRoom",
    EXIT_ROOM: "ExitRoom",
    UPDATE_CAMERA: "UpdateCameraParam",
    MSG_SYNC: "MsgSync",
}


def frame(msg_type: int, payload: bytes = b"") -> bytes:
    """[u16 length][u16 type][payload], length includes the header."""
    return struct.pack("<HH", 4 + len(payload), msg_type) + payload


def local_ip_towards(peer: str) -> str:
    """The address this host would use to reach `peer` — picks the right NIC on multi-homed hosts."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect((peer, 9))
        return s.getsockname()[0]
    finally:
        s.close()


def serve_control(sock: socket.socket) -> None:
    """Accept control connections forever — one per Stream toggle, so repeated runs keep working."""
    while True:
        conn, addr = sock.accept()
        print(f"[pair] control TCP from {addr[0]}:{addr[1]}", flush=True)
        try:
            _serve_one(conn)
        except OSError as e:
            print(f"[pair] control link error: {e}", flush=True)
        finally:
            conn.close()


def _serve_one(conn: socket.socket) -> None:
    # Keep every print() in this file ASCII-only: this runs on a plain console, and on a Japanese
    # Windows one (cp932) a stray em dash raises UnicodeEncodeError, which killed this thread
    # mid-handshake and left the app timing out with discovery still answering.
    buf = bytearray()
    while True:
        chunk = conn.recv(4096)
        if not chunk:
            print("[pair] control link closed by the app - streaming stops", flush=True)
            return
        buf += chunk
        while len(buf) >= 4:
            length, mtype = struct.unpack_from("<HH", buf, 0)
            if length < 4 or len(buf) < length:
                break
            payload = bytes(buf[4:length])
            del buf[:length]
            name = TYPE_NAMES.get(mtype, f"type{mtype}")
            if mtype == ENTER_ROOM:
                print("[pair] EnterRoom -> ack", flush=True)
                conn.sendall(frame(ENTER_ROOM))
            elif mtype == MSG_SYNC and len(payload) >= 8:
                msgid = struct.unpack_from("<Q", payload, 0)[0]
                body = payload[8:].decode("utf-8", "replace")
                print(f"[pair] MsgSync {body} -> success", flush=True)
                # Echo the msgid back: the app matches the reply to its request by it.
                reply = struct.pack("<Q", msgid) + json.dumps({"success": True}).encode()
                conn.sendall(frame(MSG_SYNC, reply))
                # The app starts sending RTP right after this ack, so this is the moment a
                # receiver can safely attach.
                _launch_then()
            elif mtype == HEARTBEAT:
                pass  # the app only needs the link to stay up
            else:
                print(f"[pair] {name} ({len(payload)} bytes) - ignored", flush=True)


def run(ip: "str | None" = None, tcp_port: int = 6002, hint: str = "") -> int:
    """Answer discovery forever. Importable so fpv_server.py can pair without shelling out."""
    # Deliberately no SO_REUSEADDR. On Windows it lets a second instance bind the same port, and
    # the two then split the app's discovery reply and control connection between them: pairing
    # times out while this one still cheerfully logs FIND-SERVER. Failing the bind is far kinder.
    tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        tcp.bind(("0.0.0.0", tcp_port))
        udp.bind(("0.0.0.0", DISCOVERY_PORT))
    except OSError as e:
        print(f"[pair] cannot bind ({e})", file=sys.stderr)
        print("[pair] another pair_server, or XREAL's StreamingReceiver, already holds "
              f"UDP {DISCOVERY_PORT} / TCP {tcp_port} - close it and retry.", file=sys.stderr)
        return 1
    tcp.listen(1)

    print(f"[pair] waiting for FIND-SERVER on UDP {DISCOVERY_PORT} (control TCP {tcp_port})", flush=True)
    if hint:
        print(hint, flush=True)

    threading.Thread(target=serve_control, args=(tcp,), daemon=True).start()

    while True:
        data, addr = udp.recvfrom(1024)
        if FIND_MSG not in data:
            continue
        reply_ip = ip or local_ip_towards(addr[0])
        reply = f"{reply_ip}:{tcp_port}".encode("ascii")
        udp.sendto(reply, addr)
        print(f"[pair] FIND-SERVER from {addr[0]} -> {reply.decode()}", flush=True)
        print(f"[pair] the app will stream to rtp://{reply_ip}:5555 (video) / :5557 (audio)", flush=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ip", help="address to advertise (default: whichever NIC faces the app)")
    ap.add_argument("--tcp-port", type=int, default=6002, help="control port to advertise (default 6002)")
    ap.add_argument("--then", nargs=argparse.REMAINDER, metavar="CMD",
                    help="command to run once the app starts streaming (must be the last option)")
    args = ap.parse_args()

    if args.then:
        _then_cmd.extend(args.then)

    return run(args.ip, args.tcp_port,
               hint="[pair] hit Stream in the app, then start ffplay/ffmpeg on stream.sdp.")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        pass
