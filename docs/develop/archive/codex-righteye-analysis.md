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

## Follow-up RE (2026-07-13) - why our Multiview path still submits one viewport

The right-eye-black A/B result is consistent with a one-viewport `DisplayOverlay`, not with a
render-target or descriptor problem. The decisive gate is **not** `UnityXRNextFrameDesc.renderParamsCount`;
it is `OverlayBase+0x14`, initialized when `DisplayManager::CreateDisplayLayer` constructs the
display overlay.

### (a) Two-viewports vs one: exact gate

`DisplayOverlay::CreateViewport @0xa6a68` gates the viewport count on `*(overlay+0x14)`:

```text
0xa6a7c  ldr w8, [x0,#0x14]
0xa6a88  cbz w8, 0xa6af0        ; zero => one viewport for overlay+0x50 component

0xa6a8c..0xa6aec                ; non-zero path: append viewport 0, component 0
0xa6cb8..0xa6d18                ; non-zero path: append viewport 1, component 1
0xa6e10  str wzr, [viewport0,#0x64] ; multiviewLayer = 0
0xa6eb0  select viewport[1]
0xa6ebc  str w9, [viewport1,#0x64]  ; multiviewLayer = 1

0xa6af0..0xa6b50                ; zero path: append one viewport for [overlay+0x50]
0xa6ea8  mov x8, xzr
0xa6eac  mov w9, wzr
0xa6ebc  str w9, [viewport0,#0x64]  ; multiviewLayer = 0 only
```

`overlay+0x14` is written by `OverlayBase::OverlayBase(NRSize2i,uint,uint) @0xa7be8`:

```text
0xa7bfc  str w2, [x0,#0x14]
```

`DisplayOverlay::DisplayOverlay @0xa6988` forwards its third argument (`w2`) into that base
constructor, then stores only the target component at `overlay+0x50`:

```text
0xa6998  bl 0xa7be8             ; w2 -> overlay+0x14
0xa69a8  str w19, [x20,#0x50]   ; component, usually 6 for display overlay
```

For Multiview, `DisplayManager::CreateDisplayLayer @0x6dc18` computes that `w2` argument from
`DisplayManager+0x62`, **not** from the next-frame descriptor:

```text
0x6dc48  ldr w9, [settings,#0x4] ; stereo_rendering_mode
0x6dc50  cmp w9, #2
0x6dc54  b.ne 0x6dc68
0x6dc58  cmp w8, #0              ; w8 = *(DisplayManager+0x62)
0x6dc5c  cset w8, eq
0x6dc60  lsl w21, w8, #1         ; w21 = (*(DM+0x62)==0) ? 2 : 0
...
0x6dd44  mov w2, w21
0x6dd48  bl 0xa6988              ; real DisplayOverlay ctor
```

So the concrete condition is:

```text
stereo_rendering_mode == 2 AND DisplayManager+0x62 == 0
```

Only then `overlay+0x14 == 2`, and `DisplayOverlay::CreateViewport` creates two viewports. If
`DM+0x62 == 1`, the same Multiview descriptor can still have `renderParamsCount=2`, but the display
overlay is constructed with `overlay+0x14=0` and therefore creates only the single component-6 viewport.

`DM+0x62` is set earlier in `DisplayManager::GfxThreadStart @0x67d14` from
`NativeRendering::GetFrameBufferMode @0x68034`:

```text
0x67d90  bl 0x68034              ; NativeRendering::GetFrameBufferMode()
0x67d94  cmp w0, #2
0x67d98  b.eq 0x67e10
...
0x67dc4  strb w21, [x19,#0x62]  ; framebuffer mode 1 + setting byte +0xc => DM+0x62 = 1
...
0x67e20  strb wzr, [x19,#0x62]  ; framebuffer mode 2 => DM+0x62 = 0
```

This explains the observed mismatch: `PopulateNextFrameDesc` can be in the `stereo_rendering_mode==2`
branch and write `renderParamsCount=2`, while the display overlay was already constructed as a
one-viewport component-6 overlay because `GetFrameBufferMode()` did not leave `DM+0x62 == 0`.

### (b) Where the viewport vector is populated

The viewport vector is populated once during overlay initialization, not rebuilt from
`UnityXRNextFrameDesc` per frame.

`OverlayBase::Init @0xa7cc4` calls virtual methods in this order:

```text
0xa7ce0  vtable+0x18  InitSwapchain
0xa7cf0  vtable+0x30  CreateBuffer
0xa7d00  vtable+0x40  CreateViewport
```

For a real `DisplayOverlay`, `vtable+0x40 -> DisplayOverlay::CreateViewport @0xa6a68`, which appends
one or two `Viewport` records into `[overlay+0x38..0x40]`, stride `0x88`.

Per frame, `DisplayManager::SetBufferViewport @0x681f4` calls
`OverlayBase::SetBufferViewport @0xa7f74` for each display overlay in `DM+0x128`. That function first
calls virtual `UpdateViewport` and then walks the existing vector:

```text
0xa7f90  ldr x8, [vtable,#0x48]
0xa7f94  blr x8                  ; UpdateViewport()
0xa7f98  ldp x21,x22,[overlay,#0x38]
0xa7fc0  bl 0xa2770              ; NativeRendering::SetBufferViewport
0xa7fc4  add x21, x21, #0x88
```

`DisplayOverlay::UpdateViewport @0xa6f28` updates the existing records. It does not append a second
record:

```text
0xa6f3c  ldp x9,x8,[overlay,#0x38]
0xa6f40  sub x20,x8,x9
0xa6f48  cmp x20,#0x88
0xa6f4c  b.ne 0xa6fa8

; exactly one viewport: update only component [overlay+0x50]
0xa6f50  ldr w20,[overlay,#0x50]
...
0xa6f64  stp s0,s1,[viewport0,#0x3c]
0xa6f88  stur q0,[viewport0,#0x24]

; not exactly one viewport, i.e. the two-viewport Multiview vector:
0xa6fa8..0xa6fe4  update viewport0 with component 0
0xa6fec..0xa7028  update viewport1 with component 1
```

`ProjectionOverlay::UpdateViewport @0xa8c18` has the same shape for composition projection overlays:
it branches on current vector byte length `==0x88`, updates either one record or records at
`viewport0 + 0x00` and `viewport0 + 0x88`, and does not rebuild the vector from the descriptor. It is
not the display-overlay path used by the Multiview swapchain in `DM+0x128`.

Therefore a one-element `[overlay+0x38..0x40]` cannot be fixed by writing more fields into
`UnityXRNextFrameDesc` during `run_frame_tick`; the second viewport must exist before
`SetBufferViewport` runs.

### (c) Minimal lever for our emulation

Keep the existing `CreateDisplayLayer+0x80` patch:

```text
lib+0x6dc98: cbz w8, 0x6dd18 -> b 0x6dd18
encoding: 0x14000020
```

That patch is still required to force the real `DisplayOverlay` instead of `DummyDisplayOverlay`.
However, it only selects the class. It does **not** force the two-viewport constructor parameter.

Add one small Multiview-only code patch in `src/signal_guard.rs`:

```text
lib+0x6dc60: lsl w21, w8, #1 -> mov w21, #2
encoding: 0x52800055
```

Why this is the smallest/safest lever:

1. `0x6dc60` is inside the `stereo_rendering_mode == 2` block (`0x6dc50 cmp w9,#2`, `0x6dc54 b.ne`),
   so Multipass construction is not affected.
2. It preserves the real `DisplayOverlay`, real swapchain, real `CreateBuffer`, and real
   `SetSwapChainBuffers` paths already proven necessary for texture registration.
3. It makes `OverlayBase+0x14 = 2`, which is exactly the value `DisplayOverlay::CreateViewport`
   tests to take the two-viewport path.
4. It avoids direct NR API calls and avoids manufacturing a `Viewport` struct in our code.

Expected flow after both patches:

```text
CreateDisplayLayer:
  settings stereo_rendering_mode == 2
  lib+0x6dc60 forced w21 = 2
  lib+0x6dc98 forced branch to real DisplayOverlay
  DisplayOverlay ctor writes overlay+0x14 = 2

OverlayBase::Init:
  DisplayOverlay::CreateViewport sees overlay+0x14 != 0
  creates viewport[0] component 0, multiviewLayer 0
  creates viewport[1] component 1, multiviewLayer 1

SubmitCurrentFrame:
  SetBufferViewport walks two 0x88 records
  NativeRendering::SetBufferViewport calls NRBufferViewportSetMultiviewLayer for viewport[1]
  NativeRendering::SubmitFrame presents both array layers
```

Verification on device:

```text
adb shell setprop debug.xreal.stereo_mode 2
```

Then relaunch and verify logcat:

```text
[xreal] patch_display_layer ... cbz->b 0x6dd18
[xreal] patch_display_layer ... lsl w21,w8,#1->mov w21,#2   ; add this log with the new patch
[xreal] CreateTexture ... arraylen=2 ...
[xreal] QueryTextureDesc ... same texture id ...
[xreal] frame_tick ... passes=1 renderParamsCount0=2 tex0=<array> tex1=0
[xreal] blit_to_layer ... layer=0 ... framebuffer-complete
[xreal] blit_to_layer ... layer=1 ... framebuffer-complete
```

For the visual A/B, keep the decisive probe: blit the LEFT source into both array layers. With the
two-viewport patch active, the right half of:

```text
adb exec-out screencap -d 4626964009369245188 -p
```

must no longer be black. If left-to-both shows in both halves, restore left->layer0 and right->layer1
and verify the right half renders the right-eye SubViewport.

## Follow-up RE (2026-07-13) - after `overlay+0x14 == 2`, right eye is fixed gray

Device result after both patches:

```text
lib+0x6dc98: cbz -> b 0x6dd18
lib+0x6dc60: lsl w21,w8,#1 -> mov w21,#2
```

The right half is no longer black; it is a fixed uniform gray (`mean == 0.2503`) and does not change
when the left SubViewport is blitted into both array layers. That changes the diagnosis: the second
viewport is now present enough to produce a right-eye image, but it is not sampling the contents we
write into the registered array texture.

### (a) Second viewport binding: same swapchain, not a second swapchain

`DisplayOverlay::CreateViewport @0xa6a68` still creates the two viewport records exactly as mapped
above. The important missing piece is what `CreateProjectionViewport` writes into each record:

```text
0xa8468  str w1, [x8]          ; Viewport+0x00 = target component
0xa8478  ldr x8, [x0,#0x18]    ; overlay+0x18 = swapchain handle
0xa8488  str x8, [x20,#0x8]    ; Viewport+0x08 = same swapchain handle
```

Both calls in `DisplayOverlay::CreateViewport` use the same `overlay+0x18`:

```text
0xa6a94  mov w1, wzr
0xa6a98  bl 0xa8468            ; viewport[0], component 0, swapchain = overlay+0x18

0xa6cbc  mov w1, #1
0xa6cc4  bl 0xa8468            ; viewport[1], component 1, swapchain = overlay+0x18
```

Layer assignment is still per viewport:

```text
0xa6e10  ldr x9, [overlay+0x38]
0xa6e18  str wzr, [viewport0,#0x64] ; layer 0
0xa6eb0  select viewport[1]
0xa6ebc  str w9, [viewport1,#0x64]  ; layer 1
```

Per-frame submission also uses the same frame and increments only the frame viewport index:

```text
OverlayBase::SetBufferViewport @0xa7f74
0xa7fa8  ldr w3, [counter]
0xa7fb8  add w8, w3, #1
0xa7fbc  str w8, [counter]
0xa7fc0  bl 0xa2770            ; NativeRendering::SetBufferViewport(viewport, frame, index)

NativeRendering::SetBufferViewport @0xa3990
0xa39a0  mov w2, w20           ; index
0xa39b0  ldr x3, [sp,#0x58]    ; native viewport handle
0xa39bc  mov w2, w20
0xa39c0  blr [wrapper+0x2b8]   ; NRFrameSetBufferViewport(rendering, frame, index, viewport)
```

So the two-viewport path does **not** expect a second `CreateTexture` result or a second swapchain.
The second viewport is supposed to reference the same multiview swapchain and select layer 1.

### (b) Buffer allocation: one 2-layer swapchain, not two buffers

`DisplayOverlay::CreateBufferSpec @0xa6a48` copies `overlay+0x14` into `BufferSpec+0x18`:

```text
0xa6a48  ldur x9, [overlay,#0xc] ; width/height
0xa6a50  str w10, [spec,#0x8]    ; texture format/type field = 2
0xa6a58  str x9, [spec]          ; size
0xa6a5c  ldr w9, [overlay,#0x14]
0xa6a60  str w9, [spec,#0x18]    ; multiview layer count
```

`NativeRendering::CreateBufferSpec @0xa0878` then calls the loader's
`NRBufferSpecSetMultiviewLayers` only when that field is `2`:

```text
0xa0e70  ldr w8, [spec,#0x18]
0xa0e74  cmp w8, #2
0xa0e78  b.ne 0xa0ff4
0xa0e88  mov w2, #2
0xa0e8c  ldr x8, [wrapper,#0x128]
0xa0e90  blr x8                 ; NRBufferSpecSetMultiviewLayers(rendering, spec, 2)
```

`OverlayBase::CreateBuffer @0xa8078` then allocates/registers one vector of swapchain buffers. In the
normal GLES path it loops the recommended swapchain buffer count and passes `overlay+0x14` as
`textureArrayLength` to `DisplayManager::CreateTexture`:

```text
0xa8204  cbz w20, 0xa831c       ; w20 = recommended buffer count
0xa823c  ldp w1,w2,[overlay,#0xc] ; width/height
0xa8240  ldr w3,[overlay,#0x14] ; textureArrayLength = 2 after the patch
0xa8244  mov x4,xzr             ; color = NULL, engine allocates
0xa8248  bl 0x69530             ; DisplayManager::CreateTexture(width,height,arraylen,NULL)
```

`OverlayBase::SetSwapChainBuffers @0xa7d0c` walks that same `overlay+0x20` buffer-id vector and
collects one GL handle per swapchain buffer:

```text
0xa7d64  ldp x23,x24,[overlay,#0x20]
0xa7d94  ldr w1,[x23,#0x8]      ; UnityXRRenderTextureId
0xa7d9c  bl 0x695e8             ; DisplayManager::QueryTextureDesc(id)
0xa7da0  ldr x8,[desc,#0x08]    ; returned GL name
0xa7e70  bl 0xa1938             ; NativeRendering::SetSwapChainBuffers(swapchain, vector<GL names>)
```

Therefore, with `overlay+0x14 == 2`, the SDK's intended allocation is one swapchain containing GL
texture-array buffers. It does not allocate a second right-eye swapchain, and our `CreateTexture` is
not supposed to return an extra texture id for viewport 1.

### (c) Why fixed gray is plausible

The fixed gray means the second NR viewport is likely accepted and submitted, but its selected
multiview layer is not backed by the texture data path we think it is. It is not evidence for a
missing second `CreateTexture`, because the disassembly shows both viewports point at the same
`Viewport+0x08 = overlay+0x18` swapchain.

The most suspicious emulator-side inconsistency is now our `QueryTextureDesc` lying about the texture
shape. `CreateTexture` receives `textureArrayLength == 2` and allocates/stores a `GL_TEXTURE_2D_ARRAY`,
but `QueryTextureDesc` returns `texture_array_length: 1` and `flags: 0`:

```rust
// current
texture_array_length: 1,
flags: 0,
```

`DisplayManager::QueryTextureDesc @0x695e8` only uses `color`, `width`, `height`, and `flags` for its
own log:

```text
0x6960c  ldr x3, [desc,#0x08]   ; color / GL texture name
0x69610  ldr w4, [desc,#0x2c]   ; flags
0x69614  ldp w5,w6,[desc,#0x20] ; width/height
```

`OverlayBase::SetSwapChainBuffers` only consumes `desc+0x08`. However, returning a 1-layer descriptor
for a 2-layer object is still wrong for a Unity display-provider emulation, and it is the only concrete
bad metadata in our path after the two patches. If any loader/runtime side path validates or caches the
Unity texture descriptor outside the visible wrapper code, it would explain layer 0 behaving like a
normal 2D texture while layer 1 resolves to the compositor's cleared gray.

### Concrete minimal fix in our emulation

Do **not** allocate/register a second swapchain for viewport 1. The SDK's two-viewport path wants one
multiview swapchain.

Fix the texture descriptor bookkeeping in `src/unity_plugin.rs`:

1. Extend `XrTexture` to store the original `color_format`, `flags`, and `texture_array_length`
   received by `xr_create_texture`.
2. In `xr_query_texture_desc`, return `texture_array_length: entry.layers as u32` for array textures
   instead of hard-coded `1`.
3. Return the original `flags` from `CreateTexture` instead of `0` (`CreateTexture @0x69584` passes
   `flags = 2` or `0x12` depending on provider state).
4. Keep `color = entry.gl_id`, `width`, and `height` unchanged.

Minimal shape:

```rust
struct XrTexture {
    id: u32,
    gl_id: u32,
    width: i32,
    height: i32,
    layers: i32,
    color_format: u32,
    flags: u32,
}

// in xr_create_texture
textures.push(XrTexture {
    id,
    gl_id,
    width,
    height,
    layers,
    color_format: desc.color_format,
    flags: desc.flags,
});

// in xr_query_texture_desc
*out = UnityXrRenderTextureDesc {
    color_format: entry.color_format,
    _pad0: 0,
    color: entry.gl_id as u64,
    depth_format: 0,
    _pad1: 0,
    depth: 0,
    width: entry.width as u32,
    height: entry.height as u32,
    texture_array_length: entry.layers as u32,
    flags: entry.flags,
};
```

Also add one diagnostic log before changing behavior permanently:

```text
[xreal] QueryTextureDesc id=<id> -> gl_tex=<n> <w>x<h> layers=<entry.layers> flags=<entry.flags>
```

Verification:

```text
adb shell setprop debug.xreal.stereo_mode 2
```

Launch and check:

```text
CreateTexture ... arraylen=2 -> id=<id> gl_tex=<array>
QueryTextureDesc ... layers=2 flags=2|18
frame_tick ... passes=1 renderParamsCount0=2 tex0=<id> tex1=0
blit_to_layer ... layer=0 ... framebuffer-complete
blit_to_layer ... layer=1 ... framebuffer-complete
```

Then run the same visual A/B:

```text
adb exec-out screencap -d 4626964009369245188 -p
```

Expected result for the A/B build that copies left into both layers: the right half should no longer be
the fixed `0.2503` gray. If it is still gray after `QueryTextureDesc` reports `layers=2`, the
remaining likely failure is inside NR's GLES array-texture import/sampling path rather than in the SDK
overlay wiring. At that point the practical fallback is Multipass (two 2D swapchain buffers), because
the disassembly does not expose a second-swapchain multiview path to populate from our side.

## Device result (2026-07-13) — QueryTextureDesc fix landed; right eye STILL gray → NR blocker

Applied all three SDK-side fixes and device-confirmed each:
- descriptor read: `STEREO_PROJ[1]` = real right-eye projection.
- `signal_guard` two patches: `cbz->b 0x6dd18` + `lsl->mov w21,#2` (`overlay+0x14=2`).
- QueryTextureDesc now echoes the array shape: logcat shows `layers=2 flags=18 color_format=0` for the
  registered swapchain textures (was `layers=1 flags=0`).

**Right eye is STILL the fixed uniform gray (right-half mean == 0.2503, unchanged).** Per the caveat
above, this places the remaining failure **inside libnr_loader's GLES multiview array-texture
import/sampling path** — not in the libXREALXRPlugin overlay/viewport/descriptor wiring, which is now
correct on our side. That NR internal path is not controllable from our emulation without RE'ing
libnr_loader's swapchain/GL import (a large, uncertain effort). **Practical outcome: Multipass is the
working stereo path (both eyes + camera + tracking); Multiview is left-eye-only and blocked in the NR
compositor.** The SDK-side fixes here are kept because they are individually correct and are
prerequisites if the NR array path is ever solved.

## Static RE follow-up (2026-07-13) — libnr_loader dispatches; GL import/compose lives in libnr_api

Scope: `jniLibs/arm64-v8a/libnr_loader.so` and `libnr_api.so`, with NDK 27.2 LLVM tools.

### 1. libnr_loader is not the GL compositor

`libnr_loader.so` exports the public NR entry points, but its NR symbols are dispatch trampolines.
There are no GL/EGL relocations in `libnr_loader.so` (`llvm-readelf -r` only shows libc/runtime
entries matching `gl` as part of `GLOB_DAT`, not GLES/EGL symbols). Relevant trampoline offsets:

- `NRBufferSpecSetMultiviewLayers @ libnr_loader.so+0x1f8680`: loads `*(0x490490)+0x38`, then `blr`.
- `NRBufferViewportSetMultiviewLayer @ +0x1f8b9c`: loads `*(0x490598)+0x80`, then `blr`.
- `NRFrameSetBufferViewport @ +0x1f93dc`: loads `*(0x490580)+0x70`, then `blr`.
- `NRFrameCompose @ +0x1f9410`: loads `*(0x490580)+0x78`, then `blr`.
- `NRSwapchainSetBuffers @ +0x1f9fa8`: loads `*(0x490508)+0x20`, then `blr`.

So the import/sample question is in the backend resolved by the loader, not in the loader itself.
The backend in this SDK is `libnr_api.so`.

### 2. NRGetProcAddr mappings in libnr_api

`libnr_api.so` is stripped, but `NRGetProcAddr` string dispatch maps public NR names to wrapper
functions:

- `NRSwapchainSetBuffers` string at `.rodata+0x636dc2` is compared at `libnr_api.so+0xc1acbc`;
  the returned wrapper is `+0xc1b2dc`. That wrapper calls `+0xd1aad4`, which resolves the active
  backend object and dispatches through `+0xc7cb10`. `+0xc7cb10` then branches through vtable slot
  `backend+0x38`.
- `NRFrameSetBufferViewport` string at `.rodata+0x3bc2d0` is compared at `+0xc18680`; the returned
  wrapper is `+0xc199ac`. It calls `+0xd1d768`, then `+0xc7d868`, which dispatches through vtable
  slot `backend+0x40`.
- `NRBufferSpecSetMultiviewLayers` string at `.rodata+0x664771` is compared at `+0xc14eb0`; the
  returned wrapper is `+0xc15898`. It calls `+0xd17b8c`, then `+0xc7bde0`, which dispatches through
  vtable slot `backend+0x08`.
- `NRBufferViewportSetMultiviewLayer` string at `.rodata+0x6a943e` is compared at `+0xc16124`; the
  returned wrapper is `+0xc17868`. It calls `+0xd20bdc`, then `+0xc7d148`, which dispatches through
  vtable slot `backend+0x60`.

This confirms both multiview setters exist and are callable, but their public wrappers only forward
state into the backend object. The actual GL behavior is in the backend methods and helper routines.

### 3. GL/EGL helpers found in libnr_api

`libnr_api.so` imports the GLES/EGL functions needed for the compositor path:
`glBindTexture`, `glTexImage2D`, `glTexImage3D`, `glFramebufferTextureLayer`,
`glGetUniformLocation`, `glUniform1i`, `eglCreateImageKHR`, `eglGetNativeClientBufferANDROID`, and
`glEGLImageTargetTexture2DOES`.

Important helper offsets:

- `+0xebea88` validates a caller-provided GL texture name as a **2D texture**. If its texture id
  argument is non-zero and an object flag at `object+0x30` is set, it does:
  - `+0xebeb74`: `w0 = 0x0de1` (`GL_TEXTURE_2D`)
  - `+0xebeb78`: `w1 = submitted_gl_name`
  - `+0xebeb84`: `glBindTexture(GL_TEXTURE_2D, submitted_gl_name)`
  - `+0xebeb88..+0xebebac`: `glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, WIDTH/HEIGHT, ...)`
  - `+0xebebb0..+0xebebb8`: `glBindTexture(GL_TEXTURE_2D, 0)`

  There is no `GL_TEXTURE_2D_ARRAY` bind/query in this caller-provided texture validation block.

- `+0xec1500` creates NR-owned GL textures. The array branch at `+0xec1608..+0xec16a0` binds
  `GL_TEXTURE_2D_ARRAY` (`0x8c1a`) and allocates it with `glTexImage3D` at `+0xec1694`.
  The 2D branch at `+0xec16f4..+0xec17d4` binds `GL_TEXTURE_2D` and allocates with
  `glTexStorage2D` or `glTexImage2D`.

- `+0xec7d7c..+0xec7e18` imports an Android native buffer through EGLImage:
  `eglGetNativeClientBufferANDROID`, then `eglCreateImageKHR(..., target=0x3140
  EGL_NATIVE_BUFFER_ANDROID, ...)`, then `glEGLImageTargetTexture2DOES(target, image)`.
  This path is for AHardwareBuffer/native-buffer import. It is not an `EGL_GL_TEXTURE_2D_ARRAY`
  or per-layer `EGL_GL_TEXTURE_*` import of a client GL texture name.

- `+0xec4d2c` binds a texture according to an internal texture kind:
  - kind `0`: `glBindTexture(GL_TEXTURE_2D, tex)` at `+0xec4d8c..+0xec4d9c`
  - kind `1`: `glBindTexture(GL_TEXTURE_2D_ARRAY, tex)` at `+0xec4d80..+0xec4d9c`
  - kind `2`: caller `+0xec858c..+0xec85a8` first runs `+0xec4d2c`, then additionally binds
    `GL_TEXTURE_EXTERNAL_OES` (`0x8d65`).

### 4. Array sampling shader exists, but it is not proof that client GL arrays are imported

`libnr_api.so` contains a real array sampling shader:

- `.rodata+0x6395df`: `precision lowp sampler2DArray;`
- `.rodata+0x6395ff`: `uniform sampler2DArray srcTex;`
- `.rodata+0x63961f`: `uniform int tex_layer_index;`
- `.rodata+0x639676`: `texture(srcTex, vec3(vTexcoord0, tex_layer_index))`

Program setup around `+0xec3f34..+0xec4158` calls `glGetUniformLocation` (`+0xec404c`) and then
`glUseProgram` + `glUniform1i` in a loop at `+0xec411c..+0xec4154`. Draw state setup at
`+0xec1f98..+0xec1fd8` binds both a `GL_TEXTURE_2D` slot (`object+0x24`) and a
`GL_TEXTURE_2D_ARRAY` slot (`object+0x28`).

Therefore NR can sample array layers in at least one internal path. The failing case is narrower:
Unity-provider/client-provided swapchain buffers that are just GL names are validated/imported as
2D textures, while the array-sampling path expects an internal texture object whose array slot is
populated.

### 5. Why the observed right eye becomes fixed gray

The device symptom is decisive: blitting the left SubViewport into both layers still leaves the right
half at the fixed mean `0.2503`. That excludes our layer-1 contents and the libXREALXRPlugin viewport
setup. The static evidence above explains the remaining failure:

1. `libXREALXRPlugin.so` passes one client GL name for a `GL_TEXTURE_2D_ARRAY` swapchain buffer.
2. The public NR calls do record multiview state (`NRBufferSpecSetMultiviewLayers`,
   `NRBufferViewportSetMultiviewLayer`), but the backend wrapper forwards it through vtable calls; no
   public-side code changes the GL import target.
3. The client-GL-name validation/helper path in `libnr_api.so` binds submitted names as
   `GL_TEXTURE_2D` at `+0xebeb84` and queries only 2D width/height. No client GL-name import path
   found here binds `GL_TEXTURE_2D_ARRAY`, imports an array texture through EGL, or creates per-layer
   texture views.
4. The array sampling shader can draw from NR-owned array textures (`object+0x28`), but for this
   client swapchain that array slot is not populated with our submitted GL texture array. Viewport 0
   still resolves through the existing 2D/left path; viewport 1 samples a fallback/cleared compositor
   resource, which matches the stable gray buffer.

So the right-eye gray is not a bad `multiviewLayer` field and not stale layer-1 pixels. It is the NR
backend not importing a caller-provided `GL_TEXTURE_2D_ARRAY` name as an array texture for the
right-eye viewport.

### 6. Concrete lever / verdict

No missing call in our current Unity-provider emulation is visible from this analysis:

- `NRBufferViewportSetMultiviewLayer` exists and is already reached through the SDK viewport setup.
- `NRBufferSpecSetMultiviewLayers(2)` exists and is reached through the SDK buffer-spec setup.
- `NRSwapchainSetBuffers` accepts a vector of submitted buffer handles, but the visible GL helper for
  client GL names treats them as `GL_TEXTURE_2D`, not `GL_TEXTURE_2D_ARRAY`.

Minimal practical fix from our side is therefore **not** another descriptor flag or another
multiview setter. The viable options are:

1. **Use Multipass**: two separate `GL_TEXTURE_2D` swapchain buffers/textures, one per eye. This
   matches the client GL-name import path (`GL_TEXTURE_2D`) and is the only path proven compatible
   with this SDK from our side.
2. **Experimental, if revisiting Multiview**: expose two 2D views of the array layers with
   `glTextureView` if GLES extension support exists, and make the SDK/NR see two 2D client texture
   names instead of one array name. This no longer uses the single client `GL_TEXTURE_2D_ARRAY`
   import path, and may require forcing the overlay back toward two swapchains/viewports rather than
   true SPI. This is unverified.
3. **Patch libnr_api backend**: the patch would need to redirect the client GL-name import/validation
   path from `GL_TEXTURE_2D` (`+0xebeb84`) to an array-aware path and ensure the compositor object
   stores the submitted name in its array slot (`object+0x28`) with `tex_layer_index` set from
   `BufferViewport.multiviewLayer`. This is higher risk than the existing `libXREALXRPlugin.so`
   branch patches because it crosses backend object layout/vtable code.

Verdict for this SDK version: **client-provided `GL_TEXTURE_2D_ARRAY` layer 1 is not achievable from
our current Unity-provider emulation without patching `libnr_api.so` internals. Multipass remains the
minimal working stereo path.**

Device note: `adb devices` sees `192.168.0.4:5555`, but no new screencap verification was run for
this section because the task was constrained to analysis only and no source behavior was changed.

## Final conclusion (2026-07-13) — Multiview shelved; Multipass is the answer

Two independent reasons close this out:

1. **NR backend blocker (codex, above).** `libnr_api.so` imports a client-submitted swapchain GL name
   only as `GL_TEXTURE_2D` (`+0xebeb84`); it has no path to import a client `GL_TEXTURE_2D_ARRAY` as an
   array (no array bind, no per-layer EGLImage). So the right-eye (layer-1) viewport samples a cleared
   compositor resource → fixed gray. Not fixable from our side without patching libnr_api internals
   (high risk, backend vtable/object layout).

2. **No benefit for our architecture.** Our stereo rig renders TWO Godot SubViewports (left/right eye
   cameras) every frame in BOTH modes, then blits them into either two 2D textures (Multipass) or the
   two layers of one array texture (Multiview). The single-pass-instanced win only exists if the ENGINE
   draws both eyes in one pass via view instancing — Godot's SubViewport rig does not. So Multiview
   would give us **zero** GPU/CPU saving even if the NR array path worked; it is purely a different
   texture hand-off to the compositor.

⇒ **Multipass is the working AND correct stereo path for this port** (both eyes + camera + tracking,
same cost as Multiview here). The SDK-side Multiview fixes (descriptor read, two-viewport patch,
QueryTextureDesc array metadata) are kept as correct emulation, but Multiview stays left-eye-only and
is not recommended. Revisiting it would require either patching libnr_api, or switching the Godot rig
to true engine multiview rendering AND still solving the NR client-array import — not worthwhile for a
mode with no benefit here.
