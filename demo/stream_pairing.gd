extends Node
## Pairs the FPV stream with XREAL's "StreamingReceiver" PC app via its LAN-discovery protocol,
## reverse-engineered from the SDK sample (Samples~/Camera Features/FirstPersonStreammingCast) and
## validated against the real receiver. See docs/plans/fpv-streaming-plan.md.
##
## Handshake (all little-endian):
##   1. DISCOVERY : UDP-broadcast the ASCII "FIND-SERVER" to 255.255.255.255:6001; the receiver replies
##                  with an ASCII "<ip>:<tcpPort>" string.
##   2. CONTROL   : TCP-connect to <ip>:<tcpPort>. Frames are [u16 length][u16 type][payload] where
##                  length includes the 4-byte header and `type` is a MsgType. Send EnterRoom (4) and
##                  wait for its ack, then send MsgSync (7) carrying [u64 msgid][{"useAudio":bool}] and
##                  wait for the matching [u64 msgid][{"success":true}]. A HeartBeat (3) every second
##                  keeps the link alive (the receiver echoes it).
##   3. The caller then streams to rtp://<ip>:5555 (video 5555 / audio 5557); the port is hard-coded in
##      the SDK — the discovered port is only the TCP control port.

## Pairing finished; caller should stream to rtp://<server_ip>:5555.
signal paired(server_ip: String)
## Pairing could not complete (discovery/connect/handshake failed or timed out).
signal failed(reason: String)
## The control link dropped after pairing (caller should stop streaming).
signal lost()

const DISCOVERY_ADDR := "255.255.255.255"
const DISCOVERY_PORT := 6001
const FIND_MSG := "FIND-SERVER"
const HEARTBEAT_S := 1.0
const DISCOVERY_TIMEOUT_S := 4.0
const CONNECT_TIMEOUT_S := 5.0
const HANDSHAKE_TIMEOUT_S := 5.0

enum MsgType { NONE = 0, CONNECTED = 1, DISCONNECT = 2, HEARTBEAT = 3, ENTER_ROOM = 4, EXIT_ROOM = 5, UPDATE_CAMERA = 6, MSG_SYNC = 7 }
enum State { IDLE, DISCOVERING, CONNECTING, ENTER_ROOM, NEGOTIATE, ACTIVE }

var _udp: PacketPeerUDP
var _tcp: StreamPeerTCP
var _state := State.IDLE
var _server_ip := ""
var _tcp_port := 0
var _recv := PackedByteArray()   # accumulated TCP bytes, sliced into frames
var _timer := 0.0                # per-state timeout accumulator
var _hb := 0.0                   # heartbeat accumulator (ACTIVE)
var _with_audio := true
var _msgid := 0

func _ready() -> void:
	set_process(false)

## Begin pairing. `with_audio` is announced to the receiver as `useAudio` (match what the encoder sends).
func start(with_audio: bool) -> void:
	stop()
	_with_audio = with_audio
	_udp = PacketPeerUDP.new()
	_udp.set_broadcast_enabled(true)
	var err := _udp.bind(0)  # ephemeral local port; the receiver unicasts its reply back to it
	if err != OK:
		_fail("UDP bind failed (%d)" % err)
		return
	_udp.set_dest_address(DISCOVERY_ADDR, DISCOVERY_PORT)
	_udp.put_packet(FIND_MSG.to_ascii_buffer())
	_state = State.DISCOVERING
	_timer = 0.0
	set_process(true)
	print("[pairing] discovering (broadcast FIND-SERVER -> %s:%d)" % [DISCOVERY_ADDR, DISCOVERY_PORT])

## Stop pairing / tear down the control link (best-effort ExitRoom first). Safe to call any time.
func stop() -> void:
	if _tcp and _tcp.get_status() == StreamPeerTCP.STATUS_CONNECTED:
		_send(MsgType.EXIT_ROOM)
		_tcp.poll()
	_tcp = null
	if _udp:
		_udp.close()
		_udp = null
	_recv = PackedByteArray()
	_state = State.IDLE
	set_process(false)

func is_active() -> bool:
	return _state == State.ACTIVE

func _process(delta: float) -> void:
	match _state:
		State.DISCOVERING:
			_timer += delta
			if _udp.get_available_packet_count() > 0:
				_on_discovery_reply(_udp.get_packet().get_string_from_ascii())
			elif _timer > DISCOVERY_TIMEOUT_S:
				_fail("discovery timeout — is StreamingReceiver.exe running on this LAN?")
		State.CONNECTING:
			_timer += delta
			_tcp.poll()
			var st := _tcp.get_status()
			if st == StreamPeerTCP.STATUS_CONNECTED:
				print("[pairing] TCP connected -> EnterRoom")
				_send(MsgType.ENTER_ROOM)
				_state = State.ENTER_ROOM
				_timer = 0.0
			elif st == StreamPeerTCP.STATUS_ERROR:
				_fail("TCP connect error")
			elif _timer > CONNECT_TIMEOUT_S:
				_fail("TCP connect timeout")
		State.ENTER_ROOM, State.NEGOTIATE, State.ACTIVE:
			_pump_tcp(delta)

func _on_discovery_reply(reply: String) -> void:
	print("[pairing] discovery reply: %s" % reply)
	var parts := reply.split(":")
	if parts.size() != 2 or not parts[1].is_valid_int():
		_fail("bad discovery reply: %s" % reply)
		return
	_server_ip = parts[0]
	_tcp_port = int(parts[1])
	_tcp = StreamPeerTCP.new()
	if _tcp.connect_to_host(_server_ip, _tcp_port) != OK:
		_fail("TCP connect_to_host %s:%d failed" % [_server_ip, _tcp_port])
		return
	_state = State.CONNECTING
	_timer = 0.0
	print("[pairing] connecting control TCP %s:%d" % [_server_ip, _tcp_port])

func _pump_tcp(delta: float) -> void:
	_tcp.poll()
	if _tcp.get_status() != StreamPeerTCP.STATUS_CONNECTED:
		if _state == State.ACTIVE:
			_lost()
		else:
			_fail("control link dropped during handshake")
		return
	var avail := _tcp.get_available_bytes()
	if avail > 0:
		var res: Array = _tcp.get_data(avail)  # [err, PackedByteArray]
		if res[0] == OK:
			_recv.append_array(res[1])
			_process_frames()
	if _state == State.ACTIVE:
		_hb += delta
		if _hb >= HEARTBEAT_S:
			_hb = 0.0
			_send(MsgType.HEARTBEAT)
	else:
		_timer += delta
		if _timer > HANDSHAKE_TIMEOUT_S:
			_fail("handshake timeout in state %d" % _state)

## Slice `_recv` into complete [u16 length][u16 type][payload] frames and dispatch each.
func _process_frames() -> void:
	while _recv.size() >= 4:
		var length := _recv[0] | (_recv[1] << 8)
		var mtype := _recv[2] | (_recv[3] << 8)
		if length < 4 or _recv.size() < length:
			break
		var payload := _recv.slice(4, length)
		_recv = _recv.slice(length)
		_on_frame(mtype, payload)

func _on_frame(mtype: int, payload: PackedByteArray) -> void:
	match mtype:
		MsgType.ENTER_ROOM:
			if _state == State.ENTER_ROOM:
				print("[pairing] EnterRoom ack -> negotiate useAudio=%s" % _with_audio)
				_msgid = int(Time.get_unix_time_from_system() * 1000.0)
				var body := _u64le(_msgid)
				body.append_array(('{"useAudio":%s}' % ("true" if _with_audio else "false")).to_utf8_buffer())
				_send(MsgType.MSG_SYNC, body)
				_state = State.NEGOTIATE
				_timer = 0.0
		MsgType.MSG_SYNC:
			if _state == State.NEGOTIATE and payload.size() >= 8:
				var json_str := payload.slice(8).get_string_from_utf8()
				var data: Variant = JSON.parse_string(json_str)
				var ok: bool = data is Dictionary and bool(data.get("success", false))
				print("[pairing] useAudio response: %s (ok=%s)" % [json_str, ok])
				if ok:
					_state = State.ACTIVE
					_hb = 0.0
					paired.emit(_server_ip)
				else:
					_fail("server rejected useAudio")
		_:
			pass  # HeartBeat echo / others — ignore

## Send a framed message: [u16 length][u16 type][payload] (little-endian; length includes the header).
func _send(mtype: int, payload := PackedByteArray()) -> void:
	if not _tcp or _tcp.get_status() != StreamPeerTCP.STATUS_CONNECTED:
		return
	var length := 4 + payload.size()
	var buf := PackedByteArray()
	buf.append(length & 0xFF)
	buf.append((length >> 8) & 0xFF)
	buf.append(mtype & 0xFF)
	buf.append((mtype >> 8) & 0xFF)
	buf.append_array(payload)
	_tcp.put_data(buf)

func _u64le(v: int) -> PackedByteArray:
	var b := PackedByteArray()
	for i in 8:
		b.append((v >> (i * 8)) & 0xFF)
	return b

func _fail(reason: String) -> void:
	push_warning("[pairing] failed: %s" % reason)
	stop()
	failed.emit(reason)

func _lost() -> void:
	push_warning("[pairing] control link lost while streaming")
	stop()
	lost.emit()
