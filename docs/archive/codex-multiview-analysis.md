# Multiview swapchain registration analysis

Date: 2026-07-13

Binary: `jniLibs/arm64-v8a/libXREALXRPlugin.so`, XREAL SDK 3.1.0, arm64-v8a.

## Conclusion

The Multiview texture is skipped because our existing runtime patch at
`CreateDisplayLayer+0x80` (`lib+0x6dc98`) changes:

```asm
6dc98: cbz w8, 0x6dd18
```

into `nop`.

For the Multiview path, this is the wrong direction. The branch target
`0x6dd18` is the real `DisplayOverlay` allocation path. The fallthrough path
`0x6dc9c` constructs a `DummyDisplayOverlay`.

That dummy overlay exactly matches the observed failure:

- `DummyDisplayOverlay::InitSwapchain @0x70e54` sets `overlay+0x8 = 1`.
- `OverlayBase::CreateBuffer @0xa8078` still runs afterward and, through
  `DummyDisplayOverlay::GetRecommandBufferCount @0x70e60`, creates one texture.
- `PopulateNextFrameDesc @0x68c7c` later sees `overlay+0x8 != 0` and skips
  `OverlayBase::SetSwapChainBuffers @0xa7d0c`.
- Therefore `DisplayManager::QueryTextureDesc @0x695e8` is never called and the
  NR swapchain is never given our `GL_TEXTURE_2D_ARRAY`.

Multipass works because it does not hit the patched branch at `0x6dc98`; it goes
through the later two-overlay path and branches to the real `DisplayOverlay`
paths at `0x6de74` and `0x6dfb4`.

## CreateDisplayLayer path

`DisplayManager::CreateDisplayLayer @0x6dc18` first checks the display overlay
vector:

```asm
6dc30: ldp x8, x9, [x0, #0x128]
6dc34: cmp x8, x9
6dc38: b.ne 0x6e070
```

So display overlays are stored in the vector at `DisplayManager+0x128`.

The stereo-mode split is:

```asm
6dc3c: ldr x8, [x0, #0x40]
6dc48: ldr w9, [x8, #0x4]
6dc4c: ldrb w8, [x0, #0x62]
6dc50: cmp w9, #0x2
6dc54: b.ne 0x6dc68
```

When `stereo_rendering_mode == 2`, it enters the single display-overlay path.
It computes the overlay's `w21` parameter from `DisplayManager+0x62`, then gets
component 6 resolution:

```asm
6dc58: cmp w8, #0x0
6dc5c: cset w8, eq
6dc60: lsl w21, w8, #1
6dc70: mov w1, #0x6
6dc78: bl 0x6e2fc ; GetDeviceResolution(NRComponent=6)
```

The critical gate is immediately after reading the static guard byte at
`lib+0xdb410`:

```asm
6dc90: adrp x8, 0xdb000
6dc94: ldrb w8, [x8, #0x410]
6dc98: cbz w8, 0x6dd18
```

The branch target `0x6dd18` is the real `DisplayOverlay` path:

```asm
6dd18: mov w0, #0x70
6dd1c: bl 0xd2940 ; operator new
6dd20: adrp x8, ...
6dd24: add x8, x8, #0x7d8 ; shared_ptr_emplace<DisplayOverlay> vtable
6dd38: mov w3, #0x6
6dd48: bl 0xa6988 ; DisplayOverlay::DisplayOverlay
6dd5c: str x23, [x8]      ; store shared_ptr payload into DM+0x128 vector
```

The fallthrough `0x6dc9c` path constructs a dummy:

```asm
6dc9c: mov w0, #0x70
6dca0: bl 0xd2940 ; operator new
6dca4: adrp x8, ...
6dca8: add x8, x8, #0x710 ; shared_ptr_emplace<DummyDisplayOverlay> vtable
6dcbc: mov w3, #0x6
6dccc: bl 0xa6988 ; first runs DisplayOverlay ctor
6dcd4: add x8, x8, #0x760 ; DummyDisplayOverlay object vptr
6dcdc: str x8, [x24, #0x18]
```

Because our patch converts `cbz` to `nop`, Multiview always falls through to the
dummy path when this block is reached.

## PopulateNextFrameDesc skip

`PopulateNextFrameDesc @0x68c7c` registers overlays before acquiring the frame.
It checks `DisplayManager+0x150`, then `+0x178`, then the display overlay vector
at `DisplayManager+0x128`.

The `DisplayManager+0x128` loop is:

```asm
68d6c: ldp x21, x22, [x20, #0x128]
68d70: cmp x21, x22
68d74: b.ne 0x68dac
...
68dac: ldr x0, [x21]
68db0: ldrb w8, [x0, #0x8]
68db4: cbnz w8, 0x68da0
68db8: bl 0xa7d0c ; OverlayBase::SetSwapChainBuffers
```

For the dummy Multiview overlay, `overlay+0x8` is already set before this loop:

```asm
70e54: mov w8, #0x1
70e58: strb w8, [x0, #0x8]
70e5c: ret
```

So the exact skip condition is the `cbnz w8, 0x68da0` at `0x68db4`, with
`w8 = *(overlay+0x8) = 1` set by `DummyDisplayOverlay::InitSwapchain`.

For a real `DisplayOverlay`, `OverlayBase::InitSwapchain @0xa7fe0` creates the
NR swapchain and stores it at `overlay+0x18`; it does not set `overlay+0x8`.
The subsequent `PopulateNextFrameDesc` loop therefore calls
`SetSwapChainBuffers`.

## Buffer vector state

The Multiview dummy overlay's buffer vector is not empty. The reason
`QueryTextureDesc` is absent is not an empty vector; it is the early skip flag.

`OverlayBase::Init @0xa7cc4` calls:

```asm
a7ce0: ldr x8, [x8, #0x18] ; InitSwapchain
a7ce4: blr x8
a7cf0: ldr x8, [x8, #0x30] ; CreateBuffer
a7cf4: blr x8
a7d00: ldr x1, [x8, #0x40] ; CreateViewport
a7d08: br x1
```

For `DummyDisplayOverlay`, vtable relocations show:

```text
vptr = 0xd4760
vptr+0x18 -> 0x70e54 DummyDisplayOverlay::InitSwapchain
vptr+0x30 -> 0xa8078 OverlayBase::CreateBuffer
vptr+0x38 -> 0x70e60 DummyDisplayOverlay::GetRecommandBufferCount
vptr+0x40 -> 0xa6a68 DisplayOverlay::CreateViewport
```

The dummy buffer count method returns one:

```asm
70e60: mov w0, #0x1
70e64: ret
```

Then the GLES path in `OverlayBase::CreateBuffer` calls `CreateTexture` once and
stores the returned Unity texture id into the vector at `overlay+0x20..0x28`:

```asm
a81e0: ldr x8, [x19]
a81e8: ldr x8, [x8, #0x38]
a81ec: blr x8              ; recommended buffer count = 1 for dummy
...
a823c: ldp w1, w2, [x19, #0xc]
a8240: ldr w3, [x19, #0x14]
a8244: mov x4, xzr
a8248: bl 0x69530          ; DisplayManager::CreateTexture
...
a82c8: cmp x26, #0x1
a82cc: str xzr, [x29]      ; native color pointer = null
a82d0: str w25, [x29, #0x8]; Unity texture id
a82f0: stp x27, x9, [x19, #0x20]
```

`SetSwapChainBuffers @0xa7d0c` would have registered that id if called:

```asm
a7d64: ldp x23, x24, [x19, #0x20]
a7d68: cmp x23, x24
a7d6c: b.eq 0xa7e58
a7d94: ldr w1, [x23, #0x8]
a7d9c: bl 0x695e8          ; DisplayManager::QueryTextureDesc
...
a7e68: ldr x1, [x19, #0x18]
a7e70: bl 0xa1938          ; NativeRendering::SetSwapChainBuffers
```

But the dummy `InitSwapchain` also leaves `overlay+0x18 == 0`, so even forcing
`SetSwapChainBuffers` on the dummy object would be insufficient. The right fix
is to create the real `DisplayOverlay`.

## Recommended fix

Use option (2): change the existing `patch_create_display_layer` code patch.

For `lib+0x6dc98`, do not patch `cbz` to `nop`. Either leave the original
instruction in place, or more robustly patch it to an unconditional branch to
the real `DisplayOverlay` path:

```asm
6dc98: b 0x6dd18
```

The AArch64 encoding is:

```text
delta = 0x6dd18 - 0x6dc98 = 0x80
imm26 = 0x80 >> 2 = 0x20
encoding = 0x14000020
```

This is the smallest and least risky fix because it only corrects the existing
patch's branch direction. It does not require changing `CreateTexture` or
`QueryTextureDesc`, and it avoids directly calling NR swapchain APIs from our
emulation.

Expected behavior after this fix in Multiview:

1. `CreateDisplayLayer @0x6dc18` stores a real `DisplayOverlay` shared_ptr in
   `DisplayManager+0x128`.
2. `OverlayBase::InitSwapchain @0xa7fe0` creates the swapchain and writes
   `overlay+0x18`.
3. `OverlayBase::CreateBuffer @0xa8078` creates one array texture and stores it
   in `overlay+0x20`.
4. `PopulateNextFrameDesc @0x68dac..0x68db8` sees `overlay+0x8 == 0` and calls
   `OverlayBase::SetSwapChainBuffers`.
5. `SetSwapChainBuffers @0xa7d90..0xa7e70` calls `QueryTextureDesc @0x695e8`
   for the array texture and then `NativeRendering::SetSwapChainBuffers
   @0xa1938`.

Runtime verification should be simple: Multiview should produce one
`CreateTexture` log and one `QueryTextureDesc` log for the same texture id.

