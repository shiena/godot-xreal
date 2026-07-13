# Multiview right-eye black analysis

Date: 2026-07-13

Context: XREAL SDK 3.1.0, `jniLibs/arm64-v8a/libXREALXRPlugin.so` plus `libnr_loader.so`.
This follows the multiview registration fix and the HMD `updateType=1` head-lock fix.

## Finding

The SDK's Multiview path is **one render pass with two render params**, not two render passes.

Our current `run_frame_tick` reads eye 1 from `renderPasses[1]` at `desc+0xfc`. In Multiview,
`renderPassesCount == 1`, so `desc+0xfc` is the next render-pass slot and its `textureId` is `0`.
The real right-eye render param is inside `renderPasses[0]`, at:

```text
renderPasses[0]                 desc+0x000
renderPasses[0].textureId       desc+0x000
renderPasses[0].renderParams[0] desc+0x008  left pose block
renderPasses[0].renderParams[1] desc+0x080  right pose block
renderParams stride             0x078
renderParamsCount               desc+0x0f8  == 2 in Multiview
renderPasses[1]                 desc+0x0fc  unused in Multiview
renderPassesCount               desc+0x580  == 1 in Multiview
```

The current code's `EyeProj` reader uses a "render-pass-relative" base and then reads FOV at
`base+0x28`. With that convention, the Multiview eye bases are `0x000` and `0x078`.

So the immediate bug in our emulation is the right-eye descriptor read. `STEREO_PROJ[1]` must be read
from the second render param in pass 0, and validity should use `renderPasses[0].renderParamsCount >= 2`, not
`renderPassesCount > 1`.

## PopulateNextFrameDesc evidence

`DisplayManager::PopulateNextFrameDesc @0x68c7c` branches into the Multiview path when
`stereo_rendering_mode == 2`.

It writes one render pass:

```text
0x68e08  mov w8, #1
0x68e0c  str w8, [x19,#0x580]        ; renderPassesCount = 1
```

For the single-display-overlay path it writes one render param for component 6 and sets
`renderParamsCount = 1`, but the actual Multiview stereo branch is the later `stereo == 2` branch:

```text
0x68ef0  ldr w9, [settings,#0x4]
0x68ef8  cmp w9, #2
0x68efc  b.ne 0x69078
...
0x68f30  mov w9, #2
0x68f34  str w9, [x19,#0xf8]         ; renderPasses[0].renderParamsCount = 2
0x68f38  stp w8, wzr, [x19]          ; renderPasses[0].textureId, flags/unused
```

Left render param writes are at `desc+0x08` plus fields:

```text
0x68f78  stp s8, s9, [x19,#0x28]     ; FOV left/right
0x68f7c  stp s10,s11,[x19,#0x30]     ; FOV top/bottom
0x68f8c  stur q1, [x19,#0x14]        ; pose tail
0x68f90  stur q0, [x19,#0x08]        ; pose head
```

Right render param writes are at the second render param. Its pose block starts at `desc+0x80`, and its
FOV block starts at `desc+0xa0`:

```text
0x68fd8  stp s8, s9, [x19,#0xa0]     ; right FOV left/right
0x68fdc  stp s10,s11,[x19,#0xa8]     ; FOV top/bottom
0x68fec  stur q1, [x19,#0x8c]        ; desc+0x80+0x0c
0x68ff0  str  q0, [x19,#0x80]        ; desc+0x80
```

Using the already validated field interpretation from the left eye, the current code's
`base + field_offset` convention wants:

```text
eye 0 base = 0x000
  pose block: base+0x08..0x24 = desc+0x008..0x024
  fov:        base+0x28..0x34 = desc+0x028..0x034

eye 1 base = 0x078
  pose block: base+0x08..0x24 = desc+0x080..0x09c
  fov:        base+0x28..0x34 = desc+0x0a0..0x0ac
```

The key point is that `desc+0xfc` is not the right eye in Multiview. It is the start of
`renderPasses[1]`, which is unused because `renderPassesCount == 1`.

## Layer mapping evidence

The compositor does not infer the right eye from `renderPasses[1]`. It uses NR buffer viewports.
For Multiview, the real `DisplayOverlay` creates two viewports for one swapchain, and the second viewport
is explicitly assigned layer 1.

`DisplayOverlay::CreateViewport @0xa6a68` uses a viewport stride of `0x88`. In the two-viewport path it
creates the first viewport, then creates the second viewport:

```text
0xa6a8c..0xa6aec  CreateProjectionViewport(component 0) and append viewport 0
0xa6cb8..0xa6d18  CreateProjectionViewport(component 1) and append viewport 1
```

Then it writes the multiview layer fields:

```text
0xa6e10  ldr x9, [x20]               ; viewport[0]
0xa6e18  str wzr, [x9,#0x64]         ; viewport[0].multiviewLayer = 0
0xa6eb0  madd x8, x8, #0x88, x10     ; select viewport[1]
0xa6ebc  str w9, [x8,#0x64]          ; viewport[1].multiviewLayer = 1
```

`OverlayBase::SetBufferViewport @0xa7f74` walks the overlay viewport vector `[overlay+0x38..0x40]`
with stride `0x88` and calls `NativeRendering::SetBufferViewport` once per viewport:

```text
0xa7f98  ldp x21, x22, [overlay,#0x38]
0xa7fc0  bl 0xa2770                  ; NativeRendering::SetBufferViewport
0xa7fc4  add x21, x21, #0x88
```

`NativeRendering::SetBufferViewport @0xa2770` then calls `NRBufferViewportSetMultiviewLayer` when
`Viewport+0x64` is non-zero:

```text
0xa3558  ldr w2, [x22,#0x64]
0xa355c  cbz w2, 0xa36d4
0xa356c  ldr x8, [wrapper,#0x218]
0xa3570  blr x8                      ; NRBufferViewportSetMultiviewLayer(rendering, viewport, layer)
```

`NRRenderingWrapper::InitWrapper @0x76ec8` resolves wrapper slot `+0x218` from the loader symbol
`NRBufferViewportSetMultiviewLayer`:

```text
0x77554  add x1, ..., #0x4b2         ; "NRBufferViewportSetMultiviewLayer"
0x77560  bl dlsym
0x77570  str x0, [wrapper,#0x218]
```

`libnr_loader.so` exports the matching thin trampoline:

```text
NRBufferViewportSetMultiviewLayer @0x1f8b9c -> viewport API slot +0x80
```

Therefore the compositor is expected to sample array layer 0 for the left viewport and array layer 1 for
the right viewport. The fix is not to duplicate layer 0 as the canonical rendering path. Blitting left into
both layers is still a useful A/B probe, but only to prove sampling of layer 1.

## What this rules out

This rules out the "compositor never reads layer 1" theory. The SDK does build a layer-1 right viewport.

It also rules out `renderPasses[1].textureId` as a Multiview signal. In Multiview it is expected to be
zero because there is only one pass and one array texture.

The array layer attachment path is likely sound: our logs already show both `glFramebufferTextureLayer`
attachments are framebuffer-complete. If layer 1 remains black after the descriptor fix, the next suspect
is the right Godot SubViewport content or a GL texture ownership/synchronization issue, not the SDK's
eye-to-layer mapping.

## Minimal code fix

Change only the Multiview descriptor read in `src/unity_plugin.rs::run_frame_tick`.

Current logic:

```rust
let pass_count = read_u32(0x580);
for (k, base) in [0usize, 0xfc].into_iter().enumerate() {
    proj[k] = EyeProj {
        valid: pass_count as usize > k,
        px: read_f32(base + 0x08),
        py: read_f32(base + 0x0c),
        pz: read_f32(base + 0x10),
        l: read_f32(base + 0x28),
        r: read_f32(base + 0x2c),
        t: read_f32(base + 0x30),
        b: read_f32(base + 0x34),
    };
}
```

Recommended minimal replacement:

```rust
let pass_count = read_u32(0x580);
let rp0_count = read_u32(0x0f8);
let multiview = pass_count == 1 && rp0_count >= 2 && tex_ids[0] != 0 && tex_ids[1] == 0;
let bases = if multiview {
    [0x00usize, 0x78usize]
} else {
    [0x00usize, 0xfcusize]
};

for (k, base) in bases.into_iter().enumerate() {
    let valid = if multiview {
        rp0_count as usize > k
    } else {
        pass_count as usize > k
    };
    proj[k] = EyeProj {
        valid,
        px: read_f32(base + 0x08),
        py: read_f32(base + 0x0c),
        pz: read_f32(base + 0x10),
        l: read_f32(base + 0x28),
        r: read_f32(base + 0x2c),
        t: read_f32(base + 0x30),
        b: read_f32(base + 0x34),
    };
}
```

If keeping the existing "base is render pass start" style, use:

```text
Multiview eye 0 base = 0x000
Multiview eye 1 base = 0x078
Multipass  eye 0 base = 0x000
Multipass  eye 1 base = 0x0fc
```

Also update the nearby comments: in Multiview, `renderPasses[1]` is not the right eye.

No change is needed in `src/gl.rs` for the normal path: continuing to blit left SubViewport to layer 0 and
right SubViewport to layer 1 matches the SDK viewport mapping. `src/node.rs::update_stereo` should then
receive `STEREO_PROJ[1].valid == true` in Multiview and apply the right-eye offset/frustum from the SDK.

## Device verification

Log the values needed to prove the descriptor path:

```text
[xreal] frame_tick ... passes=1 tex0=<array texture id> tex1=0
[xreal] renderParamsCount0=2
[xreal] eye proj ... R: ... pos=(nonzero right-eye offset ...)
[xreal] blit_to_layer dst=<array gl tex> layer=0 src=<left> read_ok=true draw_ok=true
[xreal] blit_to_layer dst=<array gl tex> layer=1 src=<right> read_ok=true draw_ok=true
```

A/B probes:

1. Blit the left source into both array layers. If the right eye shows the left image, the compositor is
   definitely sampling layer 1 and the problem is upstream of layer sampling.
2. After the descriptor fix, restore left->0 and right->1. The right eye should show the right SubViewport
   with the SDK's right-eye projection.
3. If right remains black even when left is copied into both layers, investigate GL array layer ownership or
   synchronization. That result would contradict the SDK viewport-layer evidence above and would mean the
   submitted layer-1 viewport is not sampling the texture contents we filled.

## Device result (2026-07-13) — descriptor fix landed, but right eye still black

The descriptor fix was applied (`unity_plugin.rs::run_frame_tick` now reads eye 1 from base `0x78`
= `renderPasses[0].renderParams[1]` @desc+0x80 in Multiview). Device-confirmed: `STEREO_PROJ[1]` is
now a real right-eye projection (`R: l=-0.458 r=0.441 t=0.256 b=-0.255 pos=(+0.0344,0,0)`), vs the
zeros it read before. So the per-eye frustum is correct now.

**But the right eye is still black** — and A/B **probe 1 failed**: blitting the LEFT source into BOTH
array layers still shows the right eye black (only layer 0 / left eye presents). Per the decision tree
above, that means the compositor is **NOT sampling / presenting array layer 1** — the right-eye
**layer-1 buffer viewport is not being created** in our path. It is NOT the descriptor read, NOT the
right SubViewport content, and NOT the `glFramebufferTextureLayer` blit (all fine).

⇒ Next: the two-viewport setup. `DisplayOverlay::CreateViewport @0xa6a68` builds two viewports
(viewport[0].multiviewLayer=0, viewport[1].multiviewLayer=1) for one array swapchain, and
`OverlayBase::SetBufferViewport @0xa7f74` submits each. Our path apparently creates only ONE viewport.
Determine what gates 2 vs 1 viewport (overlay type / a count field / a Multiview flag), why ours is 1,
and how to force the layer-1 viewport (an extra SDK call, a `signal_guard` code patch, or a descriptor
field). Until then, **Multipass is the working stereo path** (both eyes) and Multiview is left-eye-only.
