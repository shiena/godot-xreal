# XREAL RGB-camera acquire path and zero-copy prospects

Date: 2026-07-21. Binaries examined: SDK 3.1.0 arm64-v8a
`libXREALXRPlugin.so`, `libnr_api.so`, and `libnr_rgb_camera.so`, using NDK 27.2
`llvm-objdump`, `llvm-nm`, and `llvm-readelf`. This is static analysis; timing and lifetime claims
that require a live device are called out explicitly.

The public capture-layer reference used in the follow-up pass is XREAL's
[`nreal-ai/uvc_android`](https://github.com/nreal-ai/uvc_android/tree/0e91d263eb57ca00c6247b27060b35f198389f63/uvctest/src/main/cpp)
at commit `0e91d263eb57ca00c6247b27060b35f198389f63`. Its `v4l2.c` is the same
REQBUFS/QUERYBUF/MMAP/DQBUF/QBUF design seen in this binary. Its `main.c` selects
`V4L2_PIX_FMT_HEVC` for the colour path and removes a trailing 128-byte metadata record before
publishing the HEVC payload. This source is corroborating evidence, not proof that the Beam Pro /
One Pro selects the same backend and format at runtime.

## Verdict

**There is no GPU-zero-copy route through the exported `libXREALXRPlugin.so` RGB-camera C ABI.**
The pointer returned by `TryGetRGBCameraDataPlane` is a pointer into a pooled
`std::vector<uint8_t>` owned by the Unity wrapper, not a dmabuf, `AHardwareBuffer`, or the camera
backend's mmap ring. Before either poll mode or callback mode can observe a frame,
`NativeRGBCamera::GetRGBCameraData` copies and vertically flips exactly `width * height * 3 / 2`
bytes (1,382,400 bytes at 1280x720) from a lower-layer raw pointer into that vector. The private
`libnr_api` raw-data accessor itself is lower-copy: it returns a pointer already stored in its image
object, and the core callback copies only a 24-byte descriptor. However, that image handle is not
exported by the Unity wrapper. Therefore the safe route available today is the already-tested
direct texture upload from the wrapper plane pointers; callback mode cannot remove the wrapper
copy. `TryGetRGBCameraFrame` is nevertheless useful as a destructive new-frame gate: with a 30 Hz
camera and 60 Hz polling it can avoid approximately half of the duplicate acquire/upload work
(roughly 1.25 ms averaged over each 60 Hz render interval if the observed 2.5 ms cost scales
linearly), although it does not suppress the SDK's once-per-camera-frame copy. A private-ABI adapter
or binary hook could potentially remove the 1.3824 MB wrapper copy, but
static analysis cannot honestly convert that saving to milliseconds, and it may remove little or
none of the measured 2.5-2.7 ms render-thread span: `TryAcquireLatestImage` and
`TryGetRGBCameraDataPlane` themselves contain no payload copy, wait, conversion, or IPC.

## The acquire path, traced

The complete resolved path is:

```text
libnr_rgb_camera camera backend
  V4L2 QUERYBUF + mmap capture buffers                 0x8fea0..0x90024
  backend delivery callback                            0x5a660
  YUY2 -> I420/RGB24 when transcoder mode selects it   0x60464..0x60568
  downstream callback after transform                  0x5a994..0x5a9b0
  possible HEVC -> YUV MediaCodec edge                 unresolved
    |
libnr_api camera image object
  copies a 24-byte descriptor, not the image payload   0xccbcb8..0xccbcc8
  invokes registered callback with image handle        0xccbcd8..0xccbce4
  NRRgbCameraImageGetRawData returns stored ptr/size    0xccb3d8..0xccb3f0
    |
libXREALXRPlugin NativeRGBCamera
  queries timestamp, resolution, raw ptr/size           0xa5c84..0xa5f7c
  resizes/reuses a pooled vector                         0xa60e0..0xa61dc
  row-copies/flips Y, U, and V into that vector          0xa61e0..0xa62dc
  destroys/releases the core image                       0xa62e0..0xa62f0
    |
SessionManager
  stores the completed shared_ptr as latest             0x86440..0x86470
  invokes Unity/client callbacks after the copy          0x86514..0x86565
  TryAcquire: map<int32, shared_ptr<frame>> insertion     0x86a80..0x86af0
  TryGetPlane: returns offsets within frame.vector        0x86c48..0x86ce4
```

The important boundary is `libnr_api` to `libXREALXRPlugin`. The internal core callback receives an
image handle. `NativeRGBCamera::GetRGBCameraData @ 0xa5c50` calls the core raw-data accessor at
`0xa5f64..0xa5f7c`, then materializes a different, wrapper-owned frame. Poll mode does not trigger
this work: it merely acquires another `shared_ptr` to work already completed by the capture
callback.

At the core boundary, the object-update function at `0xccbc7c` loads 16 bytes and then 8 bytes from
the incoming descriptor and stores them at image-object offsets `+0x30` and `+0x40`:

```text
0xccbcb8  ldr q0, [x19]
0xccbcc0  str q0, [x20,#0x30]
0xccbcc4  ldr x9, [x19,#0x10]
0xccbcc8  str x9, [x20,#0x40]
0xccbcdc  add x2, x20, #0x30       ; image handle passed to callback
0xccbce4  blr x8
```

`NRRgbCameraImageGetRawData`'s resolved implementation then validates that handle and directly
loads the pointer and byte count from the object:

```text
0xccb3c4  add x8, x20, #0x30
0xccb3c8  cmp x8, x19
0xccb3d8  ldr x8, [x20,#0x38]
0xccb3dc  str x8, [x21]            ; *data
0xccb3e8  ldr x8, [x20,#0x40]
0xccb3ec  str w8, [x22]            ; *size
```

This confirms that the core accessor is not the 1.4 MB copy. It does not, by itself, prove that the
backend did no earlier conversion or copy.

## Per-question findings

### 1. `TryGetRGBCameraDataPlane` and the frame handle

**CONFIRMED — the returned pointer addresses a wrapper-owned pooled vector.** The exported function
at `0x540ac` tail-calls the demangled
`SessionManager::TryGetRGBCameraDataPlane(int,int,void**,NRSize2i*) @ 0x86b58`. The binary signature
agrees with the existing declaration at `src/ffi.rs:199`:
`(i32, i32, void **, NRSize2i *)`. The prompt's earlier `int32_t *` description of the final
argument was only a paraphrase error; there is no codebase ABI bug. Plane 0 stores one 64-bit
width/height pair at `0x86c5c..0x86c64`, while chroma planes store two 32-bit dimensions at
`0x86c78..0x86c90` and `0x86cb4..0x86ccc`.

The lookup at `0x86b68..0x86c48` hashes the signed 32-bit handle and searches
`unordered_map<int, shared_ptr<RGBCameraDataFrame>>` at `SessionManager+0x198`. On success, plane 0
loads vector begin from frame offset `+0x10` (`0x86c68..0x86c70`). Plane 1 adds `width*height`
(`0x86cd0..0x86ce0`); plane 2 additionally adds `(width/2)*(height/2)`
(`0x86c94..0x86cac`). These offsets describe contiguous planar Y, U, V (I420). This agrees with the
current client: it reads planes 1 and 2 and interleaves them into the RG8 output texture. The
observed interleaved CbCr is therefore the client's output, not a contradiction in the SDK layout.

**CONFIRMED — the handle is a map key, not a ring index or native image handle.**
`TryAcquireLatestImage` reads and increments the integer at `SessionManager+0x144`
(`0x86aa4..0x86ac4`) and emplaces the latest frame's `shared_ptr` under that key at
`0x86ac8..0x86af0`. `DisposeRGBCameraDataHandle @ 0x86e08` erases the key at `0x86e10..0x86e18`.
`IsRGBCameraDataHandleValid @ 0x86cf0` performs the same hash-table lookup. None of these values is
usable by the backend.

**LIKELY — addresses will repeat, but they are not a fixed contractual ring.** The object pool at
`0x865a0` reuses released frame objects and their vector capacities; when empty it allocates a new
0x40-byte frame object at `0x86668`. Therefore prompt disposal with a constant resolution should
produce a small recurring set of vector addresses. Holding several handles causes the pool to grow,
and a vector resize/resolution change may move an allocation. The static binary contains no fixed
slot count or address-stability promise. A returned pointer remains valid while its map entry keeps
the frame `shared_ptr` alive; use after `DisposeRGBCameraDataHandle` is unsafe.

### 2. `TryAcquireLatestImage` and the 2.5 ms attribution

**CONFIRMED — `TryAcquireLatestImage` does not copy or wait for image data.** The export at `0x53f8c`
only initializes/loads the singleton and tail-calls `SessionManager::TryAcquireLatestImage @
0x86a5c`. That function:

- loads the already-completed latest frame at `SessionManager+0x188` (`0x86a74`);
- copies its timestamp and resolution to outputs (`0x86a80..0x86aa0`);
- generates an integer key (`0x86aa4..0x86ac4`); and
- inserts a `shared_ptr` into a hash table (`0x86ac8..0x86af0`).

There is no payload `memcpy`, mutex/condition-variable operation, decoder, binder call, or call into
`libnr_api` in this function. `TryGetRGBCameraDataPlane @ 0x86b58` is likewise a hash lookup plus
pointer arithmetic. Hash-node allocation is possible on acquire, but cannot explain a deterministic
1.4 MB-scale operation.

**CONFIRMED — the Unity wrapper copies 1,382,400 bytes per 1280x720 frame, before acquire.**
`NativeRGBCamera::GetRGBCameraData` obtains the raw size, grows its vector if needed at
`0xa60e0..0xa61dc`, and performs bottom-to-top row copies:

- Y: `height` calls of `memcpy(width)` at `0xa61e0..0xa624c`;
- U: `height/2` calls of `memcpy(width/2)` at `0xa6254..0xa6294`;
- V: `height/2` calls of `memcpy(width/2)` at `0xa6298..0xa62dc`.

That is 1,440 `memcpy` calls and `921,600 + 230,400 + 230,400 = 1,382,400` bytes per 1280x720
frame. The copy also performs a vertical flip by walking source rows in reverse. Capacity is reused,
so allocation/initialization should normally occur only at startup or a size change.

**LIKELY — this copy is meaningful background CPU load, but it is not the directly timed poll
floor.** It executes synchronously on the SDK's capture-callback path before the completed frame is
published at `0x86440..0x86470`. The direct-upload PoC's 2.5-2.7 ms render-thread interval also
contains `glTexSubImage2D` and any driver synchronization. Static code proves the earlier memo's
specific attribution (“the remaining time must be dominated by the two acquire getters”) false,
but cannot split the measured interval among GL upload, scheduling, and surrounding client code.

#### MediaCodec reachability

**CONFIRMED — `libnr_api.so` has exactly one decoder-creation site.** An exhaustive disassembly
search finds the sole `AMediaCodec_createDecoderByType` call at `0x1beb428`; its two configure calls
are `0x1beb684` and the fallback at `0x1beb748`. The same object feeds compressed bytes through
`AMediaCodec_getInputBuffer @ 0x1bebf3c` and retrieves CPU output with
`AMediaCodec_getOutputBuffer @ 0x1becc3c`. The adjacent encoder is separate:
`AMediaCodec_createEncoderByType @ 0x1befcac`, configured at `0x1bf2150`. These are all three
`AMediaCodec_configure` call sites in the library; `0x1bf2150` belongs to factory type 3's encoder
and cannot be the RGB decode operation.

The decoder MIME is assembled into a stack string at `0x1beb328..0x1beb370` from process globals
selected by codec enum `1` or `2`, then passed at `0x1beb410..0x1beb428`. Those globals are populated
through the binary's encrypted-string machinery, so the exact MIME (`video/hevc` versus another
codec) is **UNRESOLVED**; absence from `llvm-strings` is not evidence against it. The no-surface
path sets `AMEDIAFORMAT_KEY_COLOR_FORMAT` to 19 at `0x1beb658..0x1beb66c`, retries with 21 at
`0x1beb724..0x1beb748`, and therefore requests byte-buffer YUV rather than an
`AHardwareBuffer`/surface output.

**UNRESOLVED — this decoder cannot be proven reachable from NRRgbCamera statically.** The decoder
wrapper is constructed at `0x1be4088..0x1be40ac` by a generic factory for factory types 1 and 2;
type 3 constructs the adjacent encoder at `0x1be414c..0x1be4178`. The only direct caller of that
factory is `0x19a4b88`. No direct call or branch from the resolved NRRgbCamera implementations
`0xcca8ac..0xccb950` reaches the factory, decoder object, or the `0x19a4xxx` owner. A plugin callback
or RPC handler can hide an indirect edge, so this is not proof of non-reachability. XREAL's public
HEVC colour-camera example and the `NRRgbCameraInitSetCompression` API strings make a camera use
plausible, while the separate `libmedia_codec.so` does not settle it: that library exports
`RTPDecoder*` but imports MediaCodec only for its encoder, not
`AMediaCodec_createDecoderByType`. Static evidence cannot honestly classify the sole core decoder
as either RGB-camera or casting/RTP.

#### What `RgbTranscoder` does

**CONFIRMED — `RgbTranscoder` performs a full-frame CPU transform for two of its three modes, and
it is not an HEVC decoder.** It is allocated lazily at `0x57a7c..0x57acc` as a 0xbdd884-byte object
with three modes: input selector 1 maps to mode 0, selector 2 to mode 1, and all other values to mode
2 (`0x57a8c..0x57a9c`). Its synchronous processing routine is `0x60464`:

- modes 0 and 1 call `0x7c500` at `0x604a8..0x604d8`, supplying one packed source pointer/stride
  and separate destination Y, U, and V pointers/strides;
- mode 0 publishes the first work buffer with size `width * height * 3 / 2` at
  `0x604ec..0x60504`;
- mode 1 additionally calls `0x7c6e0` at `0x60508..0x60540`, publishes a second work buffer, and
  records size `width * height * 3` at `0x60544..0x60564`;
- mode 2 skips both transforms and carries the input pointer onward at `0x60484..0x604a4`.

**LIKELY — these bundled row converters are YUY2-to-I420 and I420-to-RGB24.** The argument shape
and arithmetic of `0x7c500` match a packed 4:2:2 source converted into planar 4:2:0, and the
`width*height*3/2` result confirms I420. The second routine has three planar source pointers, one
destination with `3*width` stride, and a `3*width*height` result, matching RGB24. This selects option
(b), YUY2 to I420 (and optionally RGB24), not HEVC decode or mere repackaging. `libnr_rgb_camera.so`
has no MediaCodec/JPEG/HEVC decoder import, and `RgbTranscoder` executes fixed width/height row
conversion rather than a variable-length bitstream decode. Exact private function names are
stripped, hence the converter names remain LIKELY rather than CONFIRMED.

**CONFIRMED — the transform is upstream of the polling thread.** Backend callback `0x5a660` obtains
the shared transcoder, copies the 128-byte frame descriptor/metadata at `0x5a8f8..0x5a924`, calls
the transform at `0x5a928..0x5a92c`, and only then invokes the downstream callback at
`0x5a994..0x5a9b0`. The callback is registered at `0x57cac..0x57cc0`, `0x57e0c..0x57e20`, or
`0x57ef4..0x57f08` depending on the configured camera mode. Thus any selected YUY2 conversion runs
inline on the backend delivery/dispatcher thread, not in `TryAcquireLatestImage`. The exact Linux
thread for the Beam Pro path is **UNRESOLVED**.

Whether the One Pro actually selects this YUY2 path or the public-reference V4L2/HEVC path is also
**UNRESOLVED**. If HEVC is active, `RgbTranscoder` is not its decoder; the transform between the
MMAP ring and the 24-byte core image descriptor must be the unresolved MediaCodec path or another
indirect component.

#### Metadata rows and the vertical flip

**CONFIRMED — the wrapper does not reserve or subtract metadata rows.** The lower layer's returned
byte count is used only to resize the destination vector at `0xa60e0..0xa61dc`. Visible-copy bounds
come exclusively from the reported width and height: the first Y copy reads
`source + width*(height-1)` at `0xa61e0..0xa6208`, and the loop continues through every one of the
`height` rows at `0xa6214..0xa624c`. No `height-1`, `height-2`, or 128-byte exclusion is applied.

Consequently, **if** the pointer returned to this wrapper still contains camera metadata embedded
in the final advertised Y row(s), those bytes become visible at the top after the flip: source row
`height-1` becomes destination row 0, and source row `height-2` becomes destination row 1. The U
and V loops independently start after `width*height` at `0xa6254..0xa62dc`. An extra record appended
after the I420 payload would not enter the visible planes, although the vector may retain the extra
tail bytes.

**LIKELY — HEVC-path metadata is removed before the wrapper receives decoded I420, but this is not
proved for the device.** XREAL's public `uvc_android/main.c::outputRgb` treats the final 128 bytes as
`UNIVERSAL_META_DATA`, sets `data_bytes = size - 128`, and copies only that shorter HEVC payload.
That is strong evidence that a matching HEVC decoder never sees the metadata record. Static analysis
has not connected that reference path to the Beam Pro binary's core raw pointer, so the actual
presence or absence of top-row corruption remains a device experiment.

### 3. `TryGetRGBCameraFrame`

**CONFIRMED — full signature: `bool TryGetRGBCameraFrame(uint64_t *timestamp)`.** The one-argument
export at `0x53e7c` calls the demangled function with that exact parameter type at `0x86a20`. The
implementation loads the latest frame timestamp and stores it through the sole pointer
(`0x86a30..0x86a3c`), reads and clears the new-frame byte at `SessionManager+0x140`
(`0x86a40..0x86a48`), and returns whether that byte was nonzero.

It does not return a frame object, plane description, fd, stride, format, native client buffer, or
`AHardwareBuffer*`. Its purpose is a cheap “new frame?” poll plus timestamp, not an alternate image
export.

**CONFIRMED — it is set once for each wrapper frame publication, and acquire/callback delivery does
not clear it.** `SessionManager::GetRGBCameraData` holds `SessionManager+0x1c0`'s mutex from
`0x863b4..0x863c4` through publication and callback delivery, stores the new latest `shared_ptr` at
`0x86440..0x86444`, then writes byte value 1 at `0x86468..0x8646c`. The duplicate store at
`0x864b4..0x864bc` is the alternate shared-pointer-release branch of the same publication, not a
second store on one execution path. Exhaustive references in the SessionManager region show clears
only in `TryGetRGBCameraFrame @ 0x86a44`, stop at `0x86930`, and object initialization at
`0x870a8`, `0x8721c`, and `0x87318`. `TryAcquireLatestImage @ 0x86a5c..0x86b20` never accesses
`+0x140`; the Unity callback loop at `0x86514..0x86565` only consumes the completed frame.

**CONFIRMED — the gate is destructive, single-consumer, and racy.** The writer uses a plain `strb`
at `0x8646c` while holding its mutex, but `TryGetRGBCameraFrame` takes no lock and uses plain
`ldrb`/`strb` at `0x86a40..0x86a44`; there is no atomic read-modify-write or acquire/release
operation. A publish between the reader's load and clear can therefore be lost, delaying the next
positive result until a later frame. Multiple callers can also consume each other's notification.
The latest frame object remains valid and the next publication sets the byte again, so this is
acceptable as a best-effort preview/upload gate with one polling consumer, but it is not a reliable
exactly-once frame event. The timestamp/latest-pointer loads at `0x86a30..0x86a3c` are similarly
unlocked.

**LIKELY — gating is a practical win at 30 Hz camera / 60 Hz render polling.** Call
`TryGetRGBCameraFrame` once from the sole poller and run acquire plus GPU upload only when it returns
true. This avoids re-acquiring and re-uploading the same latest frame on roughly every other render
tick. It does not avoid the wrapper copy, which already happens only when the SDK publishes a camera
frame. A rare lost gate race means a preview frame can be skipped, not that a stale pointer is
returned.

### 4. Callback mode

**CONFIRMED — callback type and payload.** `StartRGBCameraDataCapture @ 0x53c50` resolves to
`SessionManager::StartRGBCameraDataCapture(void (*)(RGBCameraDataFrameToUnity, void*), void*) @
0x86008`. It stores callback and user-data pairs at `0x86034..0x860f4`. The invocation at
`0x86540..0x86565` constructs the by-value frame from the completed wrapper object: timestamp and
resolution are loaded as the first 16 bytes, followed by the vector data pointer and length. The
effective layout is:

```c
struct RGBCameraDataFrameToUnity { // RE / unverified public declaration
    uint64_t timestamp;
    int32_t width;
    int32_t height;
    uint8_t *data;
    uint64_t size;
};
void callback(RGBCameraDataFrameToUnity frame, void *user_data);
```

The ABI shape is confirmed by register/stack construction; field names and public typedef are RE,
not backed by the supplied C# declaration.

**CONFIRMED — callback mode does not skip the copy.** The native callback adapter at `0x8725c`
forwards the core image handle to `SessionManager::GetRGBCameraData @ 0x863a8`. That function first
gets a pooled frame and calls `NativeRGBCamera::GetRGBCameraData` at `0x863d8..0x86400`; only after
the copy does it publish the frame and invoke registered client callbacks at `0x86514..0x86565`.
The callback's data pointer is the same wrapper vector used by polling, not the USB/V4L2 buffer.

**LIKELY — callback execution thread and lifetime.** The callback is invoked inline on whichever
thread `libnr_api` uses to deliver its registered capture callback; there is no dispatch to Unity's
main/render thread in `0x863a8..0x86565`. The backend imports `pthread_create` and runs libusb event
handling, so a backend worker is likely, but the precise X4000 thread could not be proven
statically. The pointer is valid during the callback and while a corresponding wrapper
`shared_ptr` remains alive; callback code must not retain it without an independently acquired
handle or a copy. Exact post-callback retention is not a public contract.

### 5. Hardware buffers, EGL, and the backend `mmap`

**CONFIRMED — no AHardwareBuffer/EGL image is on the resolved RGB path.** Exhaustive direct call-site
search in `libnr_api.so` finds `eglGetNativeClientBufferANDROID`/`eglCreateImageKHR` only at
`0xec7d9c`/`0xec7df4`, in a GL texture-import path, and AHardwareBuffer operations only at
`0xed2894`, `0xed2918`, `0xed349c`, and `0xed34b0`, in the separately named Android
`ImageListener`/Chameleon region. None is called by the NRRgbCamera resolver functions
`0xc01df4..0xc02540`, their internal implementations `0xcca8ac..0xccb950`, or the Unity wrapper
path. `libnr_rgb_camera.so` has no AHardwareBuffer or EGL dependency/import at all.

This establishes non-reachability from the traced RGB API, although it does not mean the large SDK
has no other camera-like Android subsystem using hardware buffers.

**CONFIRMED — `mmap` maps V4L2 capture buffers.** At `0x8fea0..0x8feb0`, the backend issues ioctl
request `0xc0585609`, Linux `VIDIOC_QUERYBUF`, for each buffer. It then calls:

```text
0x8ffe8  mov x0, xzr              ; addr = NULL
0x8fff0  mov w2, #3               ; PROT_READ | PROT_WRITE
0x8fff8  mov w3, #1               ; MAP_SHARED
0x8fff4  ldr w1, [sp,#0x98]       ; queried buffer length
0x90004  ldr w4, [x19,#0x640]     ; V4L2 device fd
0x90008  ldr w5, [sp,#0x90]       ; queried offset
0x9000c  bl mmap
0x90020  str x0, [ring + index*16]
```

Thus it is a conventional V4L2 MMAP capture ring, not evidence of an Android gralloc allocation or
tracking-process shared memory. XREAL's public `uvc_android/v4l2.c` uses the same sequence and is the
reference implementation for this capture-layer interpretation. The mapped addresses are retained
in an internal array, but the
plugin exports only `NRPluginLoad @ 0x793a8` and `NRPluginUnload @ 0x79d24`; no export returns an
address or fd. Whether this specific V4L2 implementation, rather than the libusb implementation, is
selected for the Beam Pro/One Pro combination remains **UNRESOLVED**. Even if selected, the existing
Unity ABI does not expose it.

**UNRESOLVED — dmabuf export capability.** The binary does not call `VIDIOC_EXPBUF`, and no fd is
carried through the 24-byte core image descriptor observed above. A live device could still reveal
that the kernel driver accepts `VIDIOC_EXPBUF`, but neither supplied library attempts it.

### 6. Supporting exports and adjacent frame functions

**CONFIRMED — `GetRgbCameraPluginState` is status-only.** The export at `0x4a884` calls
`NativeGlasses::GetRgbCameraPluginState @ 0x5798c`; that function invokes the glasses/API vtable
slot `+0x158` with the caller's `NRRgbCameraPluginState*` at `0x579b0..0x579c0`. It does not access
camera image storage or return a transport handle.

**CONFIRMED — validity and disposal only manage the handle map.** `IsRGBCameraDataHandleValid @
0x86cf0` searches the map; `DisposeRGBCameraDataHandle @ 0x86e08` erases its entry. They reveal no
lower-layer object.

**CONFIRMED — `CreateFrame`, `GetFrameMetaData`, and `SetFocusPlane` are unrelated compositor
exports.** Each obtains the `DisplayManager` singleton and tail-calls a `DisplayManager` method:
`GetFrameMetaData @ 0x53ba0..0x53bac`, `CreateFrame @ 0x53bd8..0x53be4`, and `SetFocusPlane @
0x53be8..0x53c3c`. Their adjacency to `StartRGBCameraDataCapture` is accidental; they are not the
RGB frame-object family.

## Recommendation

1. **Do not implement callback mode as a performance optimization.** It receives the same copied
   wrapper vector and merely moves consumption onto the SDK callback thread.
2. **Keep the direct render-thread upload as the production lower-copy path** if its render-thread
   cost is acceptable. Cache/reuse client staging, but do not attempt to EGL-import the returned CPU
   pointer; it has no native-buffer identity. Pointer-address-based cache entries may be used only as
   an opportunistic optimization with handle lifetime validation, never as permanent ring slots.
3. **Use `TryGetRGBCameraFrame` as the sole poller's new-frame gate.** Call it immediately before
   acquire, and skip acquire plus all GPU uploads when it returns false. At 30 Hz camera / 60 Hz
   render polling this should remove roughly half of duplicate poll-side work. Treat it as
   best-effort: the byte is destructive and non-atomic, so one race can skip a frame. Do not have a
   second consumer call it. Plugin state and the adjacent compositor exports still offer no image
   resource.
4. If removing the SDK-side 1.3824 MB copy is worth maintaining an RE-only path, build a device PoC
   around the private `NRGetProcAddr` family: intercept or reproduce the NRRgbCamera callback and
   call `NRRgbCameraImageGetRawData` while its image handle is alive. This would expose the core raw
   pointer before `0xa61e0..0xa62dc`. The create/session signatures and coexistence with the existing
   Unity wrapper are not yet verified, so this must not enter the normal FFI without being marked RE
   and documented in `docs/reference/reverse-engineering.md`.
5. Treat the current “2.5 ms acquire floor” as **not established**. Before engineering a private
   ABI, separately time the two C exports, the GL call, and the asynchronous wrapper callback. If the
   getters measure in microseconds as their code predicts, the practical remaining target is GL
   upload/synchronization, not acquire.

The conservative stopping point is therefore: **there is no further zero-copy win in the supported
Unity C ABI.** The only demonstrated removable SDK work is the wrapper's row-copy/flip, and reaching
before it requires unsupported private ABI or patching.

## Unresolved / how to make progress

The following device experiments would settle the remaining static-analysis gaps, in priority
order:

- **Time the boundaries independently.** Record `CLOCK_MONOTONIC_RAW` around
  `TryAcquireLatestImage`, each `TryGetRGBCameraDataPlane`, and `glTexSubImage2D`, plus a fence/wait
  where applicable. Run both when a new frame is ready and when repeatedly acquiring the same
  latest frame. This directly tests the revised cost attribution.
- **Log pointer recurrence and lifetime.** For several thousand frames, record handle, Y/U/V
  addresses, then repeat while retaining 1, 2, 4, and 8 outstanding handles. Deliberately trigger
  stop/start and any resolution transition. This determines the effective pool size and whether a
  stable CPU-slot cache is useful on this build.
- **Hook the wrapper `memcpy` calls.** A PLT hook or temporary binary instrumentation can log caller
  PCs, sizes, thread IDs, and elapsed time for `0xa61e0..0xa62dc`. This gives the exact milliseconds
  removable by a core-raw route without changing the camera consumer.
- **Probe the private core callback.** Resolve the NRRgbCamera names through `NRGetProcAddr`, capture
  the core image handle, call `NRRgbCameraImageGetRawData`, and log `/proc/self/maps` ownership and
  pointer lifetime before/after image destroy. Do this only in an isolated PoC because create and
  ownership signatures are RE/unverified.
- **Identify the active backend.** Hook `ioctl`, `mmap`, libusb submit callbacks, and thread creation,
  or use Perfetto/simpleperf stacks during capture. If V4L2 is active, duplicate the internal fd and
  try `VIDIOC_EXPBUF` for each queried buffer. Success would establish a possible dmabuf route that
  the SDK does not expose; failure would close it.
- **Identify the decoder and source format.** Hook `VIDIOC_S_FMT`/`G_FMT`,
  `AMediaCodec_createDecoderByType`, and both `AMediaCodec_configure` calls. Log the MIME,
  `AMediaFormat_toString`, caller stack, and thread. Correlate compressed callback size with the core
  raw pointer/size. This settles whether the sole core decoder is the RGB HEVC decoder and whether
  the private pointer is already I420. The static pass already establishes that `RgbTranscoder`
  handles YUY2-to-I420/RGB24, not HEVC.
- **Check metadata visibility.** Dump the first two and last two rows of the wrapper Y plane and the
  last 128 bytes of the backend capture buffer, then compare metadata magic/frame IDs. This decides
  whether metadata is removed before decode or appears in the top rows after the wrapper flip.
- **Stress the new-frame gate race.** Count backend publications, positive
  `TryGetRGBCameraFrame` results, unique timestamps acquired, and duplicate uploads at 60 Hz. Run a
  second gate consumer only in the test build to demonstrate destructive consumption. This
  quantifies any dropped notification from the plain-byte load/clear race.
- **Verify callback thread and retention.** Log Linux TID/name at the core callback, Unity callback,
  poll caller, and libusb/V4L2 completion. Poison or checksum the pointer only within controlled
  lifetimes to determine when the backend requeues and overwrites it.

Until those experiments are run, claims about the active backend, raw-pointer persistence, dmabuf
exportability, or milliseconds saved by bypassing the wrapper remain unresolved.
