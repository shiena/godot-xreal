# XREAL RGB camera → Godot `CameraFeed` plan

Status: **IMPLEMENTED 2026-07-13 (device-verified, full colour)** — `src/camera_feed.rs`
(`XrealCameraFeed`). Deltas from the plan below:

- The `CameraFeed` subclass + `set_ycbcr_images` shipped as designed, **but sampling via
  `CameraTexture` shows only a placeholder for script-fed feeds** — so the feed also keeps two
  plain `ImageTexture`s (`get_y_texture()` / `get_cbcr_texture()`) that the 3D panel shader
  samples directly.
- Since that made the `set_ycbcr_images` route consumer-less (every addon consumer — panel, blend,
  stream, photo — samples the plain textures), the per-frame `set_ycbcr_images` push is **gated
  behind `feed_camera_server` (default OFF)** as of 2026-07-21: it cost a duplicate GPU upload per
  frame for a route nothing displays. Set it `true` only for external code that consumes
  `CameraServer` feeds through the standard API.
- `set_ycbcr_images` only exists from godot-rust **`api-4-6`** (crate feature bumped).
- Colour conversion is a port of the SDK's `YUVTransRGB` (full-range BT.601) in the panel shader.
- **The RGB camera does _not_ conflict with 6DoF SLAM** (disproved on-device 2026-07-18, commit
  `f56dd3a`): SLAM runs on the Leopard grayscale cameras and the RGB camera is a separate device —
  One Pro logcat shows 6DoF + RGB coexisting with zero `GetPoseWithStates` failures. The demo
  originally forced 3DoF with the feed on (`set_tracking_type(1)`, on the shared-camera theory);
  that force is dropped, so the configured tracking mode (6DoF by default) stays live with the
  camera on.
- Known failure mode: a pink/magenta panel = wedged camera service — a crashed client kept the
  camera held (`Recv Frame -99` in logcat, `StartRGBCameraDataCapture` returns the `u64::MAX`
  error sentinel). Recovery requires **replugging the glasses' USB AND then fully restarting the
  app** (2026-07-21: a replug alone is not enough — a running process's native session is bound to
  the old connection, so its retries keep failing; and an app restart alone is not enough either).
  Handled in commit `1461f7b`: `src/native.rs` surfaces the sentinel as a failed start,
  `demo/main.gd` latches `_cam_failed` instead of hammering the dead service, and the capture is
  released on graceful exit.
- Second wedge signature (2026-07-21): after an app kill mid-capture, `Start…` can *succeed*
  (handle=0) with **zero frames ever arriving** — and the stuck pipeline destabilised SLAM into a
  position runaway (~0.5 m/s drift, no `GetPoseWithStates` errors). `xreal_camera.gd` catches this
  with a 5 s first-frame watchdog (fails as "wedged", same recovery). The demo pops a modal dialog
  on either signature (`demo/main.gd` `_show_error_dialog`).

## Per-frame cost: 208 ms/s → 15.7 ms/s (2026-07-21, device-verified)

`poll_frame` cost **3,466 µs per grab, 60 times a second — 208 ms of CPU per second, 20.8 % of a
core**. It is now **~525–700 µs at 29.4 grabs/s, i.e. 15–21 ms/s (1.5–2 %)**. Four changes, each
measured on the X4000 + One Pro before being kept.

**Read the absolute numbers with their caveat.** They track the SoC clock, which moves a lot during
a session: the same build measured 489–793 µs per grab across runs, ~525 µs early on and ~690 µs
later with `cpu7` at 1.34 of its 2.21 GHz max while streaming. Each comparison below is like-for-like
(before/after within minutes of each other) so the *deltas* are sound, but do not treat any single
absolute figure as the number.

| # | change | per grab | commit |
|---|---|---|---|
| 0 | baseline | 3,466 µs × 60/s | |
| 1 | skip the grab when the frame timestamp is unchanged | ×29.4/s instead of ×60/s | `ae1d305` |
| 2 | vectorise the CbCr interleave (903 → 167 µs) | 3,148 µs | `497117e` |
| 3 | upload through a PBO, drop `PackedByteArray`/`Image` | 1,130 µs | `fcd7a79` |
| 4 | upload straight from the SDK's plane pointers | **525 µs** (same run) | `e7bb8f7` |

Four findings worth keeping:

- **The SDK calls are free.** `TryAcquireLatestImage` = 4 µs, `DisposeRGBCameraDataHandle` = 5 µs,
  and the three `TryGetRGBCameraDataPlane` calls together = **0 µs**. This refutes the old
  "the floor is the SDK acquire" conclusion in `docs/archive/camera-zero-copy-investigation.md`;
  every microsecond was on our side. Disassembly: `docs/archive/codex-camera-acquire-analysis.md`.
- **The camera publishes at 30 fps** (confirmed: 29.4 grabs/s against 59.7 polls/s), so a 60 Hz poll
  did every frame's work twice. Gate on the timestamp `TryAcquireLatestImage` already returns —
  *not* on the SDK's `TryGetRGBCameraFrame` flag, which is a destructive unlocked read-and-clear.
- **Adreno's `glTexSubImage2D` from client memory costs ~1.78 ns per _texel_, not per byte** (1651 µs
  for 921,600 R8 texels; 409 µs for 230,400 RG8 texels — equal per texel, 2× apart per byte). That is
  the driver tiling every texel on the CPU. Sourcing from a pixel-unpack buffer moves that pass to
  the GPU and the cost becomes per-byte (2.3 GB/s), which is where change 3's 2,060 → 597 µs came from.
- **Godot's Android main loop is the GL thread** — `eglGetCurrentContext()` from `_process` returns
  non-null — so `poll_frame` can issue GL inline. `crate::gl::has_current_context()` checks that at
  runtime rather than assuming it; where it is false the feed falls back to the `Image` path.

Not adopted, both measured and rejected:

- **Splitting CbCr into two R8 textures** to drop the interleave: it removed the expected 100 µs but
  added ~60 µs of chroma upload and ~65 µs to the *luma* upload (three PBO orphans per frame instead
  of two), for a net 525 → 543 µs. No gain, and it would have changed the public
  `get_cbcr_texture()` API across five GDScript consumers and three shaders. Reverted.
- **Zero-copy via AHardwareBuffer/EGLImage**: impossible through the public C ABI — see the archive
  memo. Commit `c5b9a67` should not be revived.

Per-stage timing is still in the code, behind
`adb shell setprop debug.xreal.camera_timing 1` (then toggle the camera off/on):

```
[xreal] camera timing/grab (us, n=120): acquire=4 planes=0 interleave=99 dispose=5 \
        packed=0 image=0 feed=0 upload_y=270 upload_cbcr=138 | total=525
```

### Getting pixels on the CPU (OpenCV etc.)

The GPU path deliberately keeps no CPU copy, so retaining one is opt-in:

```gdscript
feed.cpu_luma_step = 1                      # 0 = off (default), 1 = 1280x720, 2 = 640x360, 4 = 320x180
feed.frame_changed.connect(_on_frame)       # CameraFeed's own signal

func _on_frame() -> void:
    var data := feed.get_y_data()           # dense 8-bit greyscale -> CV_8UC1, no conversion
    var size := feed.get_y_data_size()
```

Read it from **`frame_changed`**, not by polling the getters: the signal fires only on a grab that
actually happened, so the "no new frame this poll" state — the source of a real `(0, 0)` bug during
development — cannot be observed at all. Device-verified 2026-07-21:

- **The engine emits `frame_changed` from `set_ycbcr_images` itself.** With `feed_camera_server` on,
  the signal rate doubled (56.7/s against 29.4 grabs/s) and matched the grab rate exactly with it
  off. So the feed emits its own only when that flag is off, and `set_ycbcr_images` is deliberately
  the *last* call on that path so the engine's emission lands after everything is updated.
- **Calling back into the feed from a handler is safe.** The handler runs inside `poll_frame`'s
  `&mut self`; a re-entrant `get_y_data()` reborrows rather than panicking.
- Note `set_*_image*` are plain class methods, not `ICameraFeed` virtuals, so they cannot be
  overridden — ordering, not overriding, is what makes this correct.

Cost, measured (`debug.xreal.camera_timing`):

| `cpu_luma_step` | output | `cpu_luma` |
|---|---|---|
| 1 | 1280x720, 921,600 B | **~296 us** |
| 2 | 640x360, 230,400 B | **~278 us** |

**Decimating does not make the extraction cheaper** — 6 % between step 1 and 2, because a strided
read still touches every cache line of the source (3.3 GB/s on the read side either way). Its value
is entirely downstream: 4x fewer pixels for OpenCV to chew on. Do not pick a step expecting to save
copy time.

Only luma is retained. Colour would need the other two planes plus a `cvtColor` — and note the
plane order is **YV12, not I420** (`cv::COLOR_YUV2BGR_YV12`); the three planes are contiguous in one
buffer, so one memcpy would do, but that contiguity is an implementation detail of
`TryGetRGBCameraDataPlane` and would need a runtime assert. Left unimplemented until something needs
it: the ~1-3 ms `cvtColor` would dominate the ~340 us copy anyway.

If the CV code is native (your own Rust/C++ GDExtension), skip all of this:
`XrealNative::rgb_camera_with_frame()` lends `&[u8]` straight into the SDK's buffer, so work done
inside that closure costs **nothing extra**. The pointers die when the closure returns, so anything
threaded still needs a copy.
