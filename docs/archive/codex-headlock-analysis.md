# Head-lock compositor pose analysis

Date: 2026-07-13

Context: XREAL SDK 3.1.0, `jniLibs/arm64-v8a/libXREALXRPlugin.so` plus `libnr_loader.so`.
This follows the multiview swapchain-registration fix in `docs/archive/codex-multiview-analysis.md`.

## Finding

The submitted layer is placed by the NR compositor from the frame's **rendering pose** value, not from the
Unity XR frame descriptor's `deviceAnchorToEyePose` orientation and not from the buffer viewport transform.

`DisplayManager::SubmitCurrentFrame @0x685fc` passes three `DisplayManager` fields into
`NativeRendering::SubmitFrame @0xa3ef0`:

```text
0x68648  ldr x0, [x19,#0x58]     ; NativeRendering*
0x6864c  ldr x1, [x19,#0x120]    ; NRFrame handle
0x68650  ldp x2, x3, [x19,#0x100]
0x68654  ldr x4, [x19,#0x110]
0x68658  bl  0xa3ef0             ; NativeRendering::SubmitFrame
```

Inside `NativeRendering::SubmitFrame @0xa3ef0`, these arguments are dispatched through the NR loader:

```text
0xa3f2c..0xa3f40  x2 = DM+0x100 -> wrapper slot 0x270
0xa4088..0xa409c  x2 = DM+0x108 -> wrapper slot 0x278
0xa4200..0xa4214  x2 = DM+0x110 -> wrapper slot 0x2d0
0xa4378..0xa4388  frame only     -> wrapper slot 0x2a0
```

`NRRenderingWrapper::InitWrapper @0x76ec8` resolves those slots with `dlsym`:

```text
slot 0x270 = NRFrameSetRenderingPose
slot 0x278 = NRFrameSetPresentTime
slot 0x2d0 = NRFrameSetStartTime
slot 0x2a0 = NRFrameSubmit
```

`libnr_loader.so` confirms these are thin trampolines into the NR API table:

```text
NRFrameSetRenderingPose     @0x1f91d4 -> frame API slot +0x20
NRFrameSetPresentTime       @0x1f9208 -> frame API slot +0x28
NRFrameSetDevicePresentTime @0x1f923c -> frame API slot +0x30
NRFrameSubmit               @0x1f9340 -> frame API slot +0x58
NRFrameSetStartTime         @0x1f947c -> frame API slot +0x88
```

Therefore the critical value for compositor reprojection is `DM+0x100`, submitted as:

```text
NRFrameSetRenderingPose(frame, *(DM+0x100))
```

If `DM+0x100` remains at the session-start value, the compositor resolves the submitted frame against the
session-start head pose. That exactly matches the observed world-anchored/head-start-anchored layer.

## Where `DM+0x100` is refreshed

`DisplayManager::OnBeforeRender @0x66fa8` is the function that updates the frame pose/timing fields:

```text
0x66fb0..0x66fb8  InputManager::GetHMDTimeNanos(&DM+0x110)
```

Then it chooses one of two paths:

```text
if *(lib+0xdb410) == 0:
    DM+0x100 = NativeRendering::GetFramePresentTime(...)
else:
    DM+0x100 = DM+0x110 + DM+0x118 * 1_000_000
```

The current safe path is the first one. In that path, `OnBeforeRender` asks NR for the acquired frame's
current rendering/present-time value and stores it into `DM+0x100`.

`PopulateNextFrameDesc @0x68c7c` acquires/creates the frame and stores the handle in `DM+0x120`, but it does
not refresh `DM+0x100`.

## Why the current emulation misses it

`InputManager::UpdateDeviceState @0x7a968` dispatches HMD updates to `InputManager::UpdateHMDState @0x7aa3c`.
The update type is passed through in `w1`.

`UpdateHMDState @0x7aa3c` only calls `DisplayManager::OnBeforeRender` for update type `1`:

```text
0x7aa68  cmp w1, #0x1
0x7aa6c  b.ne 0x7aa88
0x7aa70  TSingleton<DisplayManager>::GetInstance
0x7aa78  DisplayManager::OnBeforeRender
```

For update type `0`, this `OnBeforeRender` call is skipped. The function still calls
`NativePerception::GetHeadPose` and later writes:

```text
0x7aabc..0x7aac0  DM+0x108 = [sp+0x110]
```

but it does not update `DM+0x100`.

Our current `src/unity_plugin.rs::call_input_update_hmd` passes:

```rust
// deviceId 0 = HMD; updateType 0 = Dynamic.
update(ptr::null_mut(), ptr::null_mut(), 0, 0, buf.as_mut_ptr() as *mut c_void)
```

That means the emulation updates dynamic HMD state, but it does not drive the provider's before-render path
that refreshes the `NRFrameSetRenderingPose` argument. This qualifies the earlier device finding: the tested
`UpdateDeviceState(deviceId=0)` path was not sufficient because the update type matters. Type `0` can update
pose/timestamp side data such as `DM+0x108`, while leaving `DM+0x100` stale.

## Viewport and descriptor are not the missing head pose

`NativeRendering::SetBufferViewport @0xa2770` configures the NR buffer viewport: target component,
swapchain/source buffer, multiview layer, source FOV, and related viewport fields. The per-frame call path
does not feed a live head orientation through this structure.

This matches `ProjectionOverlay::UpdateViewport @0xa8c18`: it uses `InputManager::GetEyePos` /
eye-from-head offsets, not head world orientation.

`PopulateNextFrameDesc @0x68c7c` also writes only per-eye FOV and eye-from-head position into the Unity XR
render params. For multiview it writes one render pass, with `renderParams[0]` at `desc+0x08`; for multipass
it writes two passes. The written pose is `deviceAnchorToEyePose`, not `worldFromHead`:

```text
desc+0x08..0x24   eye pose orientation/position for renderParams[0]
desc+0x28..0x34   FOV
desc+0x580        renderPassesCount
renderPass stride 0xfc
```

No code in `PopulateNextFrameDesc` writes the live head world orientation into those descriptor poses.
Therefore writing the live head orientation into `deviceAnchorToEyePose` is not the Unity provider behavior
and is not the minimal fix. The app should render each eye from its own camera/head state; the compositor's
late pose comes from the frame rendering pose submitted through NR.

## `libnr_loader.so` side

The loader exports both the frame pose/timing functions and head-tracking functions:

```text
NRFrameSetRenderingPose @0x1f91d4
NRFrameSetPresentTime   @0x1f9208
NRFrameSetStartTime     @0x1f947c
NRFrameSubmit           @0x1f9340
NRHeadTrackingAcquireHeadPose @0x1f6cc0
NRHeadPoseGetPose             @0x1f6d90
```

In the XREAL display submission path examined here, the provider does not call the head-tracking exports
directly at submit time. It submits a rendering-pose value to the frame API:

```text
NRFrameSetRenderingPose(frame, renderingPose)
NRFrameSetPresentTime(frame, presentTime)
NRFrameSetStartTime(frame, startTime)
NRFrameSubmit(frame)
```

The NR runtime/compositor then resolves that rendering-pose value internally. Supplying a stale
`renderingPose` produces a stable but wrong reference: the layer is anchored to the head pose associated
with that stale value.

## Minimal recommended fix

Do not patch code bytes and do not write head orientation into the frame descriptor first.

The smallest fix is to drive the real provider's before-render HMD update each frame:

```rust
// deviceId 0 = HMD; updateType 1 = BeforeRender.
update(ptr::null_mut(), ptr::null_mut(), 0, 1, buf.as_mut_ptr() as *mut c_void)
```

Either replace the current render-tick HMD update type with `1`, or call a second HMD update with type `1`
immediately before `PopulateNextFrameDesc` / `SubmitCurrentFrame`. The second form preserves any future need
for Unity's dynamic update type:

```text
UpdateDeviceState(deviceId=0, updateType=0)   ; optional dynamic input
UpdateDeviceState(deviceId=0, updateType=1)   ; required before render
PopulateNextFrameDesc
SubmitCurrentFrame
```

Expected effect:

```text
UpdateHMDState(type=1)
  -> DisplayManager::OnBeforeRender
     -> DM+0x100 = live NR frame rendering pose / predicted pose value
SubmitCurrentFrame
  -> NRFrameSetRenderingPose(frame, DM+0x100)
  -> NRFrameSubmit(frame)
```

This should make the compositor reproject/place the submitted layer from the current predicted head pose
instead of the session-start pose, giving the same head-locked peek-window behavior as the reference Unity app.

## Device verification

1. Add temporary logging around the HMD update path or a signal-guard probe for `DM+0x100`, `DM+0x108`,
   and `DM+0x110`.
2. Confirm with update type `0` that `DM+0x100` does not refresh reliably before submit.
3. Confirm with update type `1` that `DM+0x100` changes per frame before `SubmitCurrentFrame`.
4. Run multiview:

```text
adb shell setprop debug.xreal.stereo_mode 2
```

Relaunch and confirm existing healthy logs still appear, especially `QueryTextureDesc` and frame submit
success. Visual acceptance: with the app eye cameras tracking the HMD 1x, the image should stay filling the
glasses FOV as a head-locked peek window, with no drift out of view and no approximate 2x overshoot.

5. A/B the old behavior by reverting only the update type to `0`; the world-locked drift should return.
6. Optionally repeat in multipass:

```text
adb shell setprop debug.xreal.stereo_mode 0
```

The primary acceptance target is multiview because that is now known to register the swapchains correctly.
