# Stubbed Unity callbacks and the Multiview right eye

Date: 2026-07-16.  Binary examined: the vendored arm64-v8a
`libXREALXRPlugin.so` (SDK 3.1.0), with NDK 27.2 `llvm-objdump`/`llvm-nm`.

## Verdict

**None of the four stubs is the cause of the black/gray right eye.**  The SDK does not use
`RegisterTextureProvider`'s argument as a callback object, does not call `GetPlatformData` in this
binary, and does not use the ordinary display-provider callbacks to allocate or import the
swapchain.  Multiview layer count and per-viewport layer selection are constructed inside
`libXREALXRPlugin.so` and sent directly to NR.  The remaining failure is the previously identified
`libnr_api.so` client-GL-name path, which binds the submitted name as `GL_TEXTURE_2D`, not
`GL_TEXTURE_2D_ARRAY`.

Implementing these stubs cannot make NR import layer 1.  Multipass remains the fix available from
this port; true Multiview would require changing/patching the NR backend's client texture import.

## 1. `RegisterTextureProvider` does not pass a provider object to Unity

The current Rust comment calls the argument an "internal texture-provider object", but the call
site proves that interpretation is wrong.  In `DisplayManager::Initialize @ 0x670d8`, the lifecycle
callback's first argument is saved as `x20` and at `0x67110` is also saved to
`DisplayManager+0x38`.  That value is the Unity display provider handle/context.

The texture-helper registration is:

```text
0x6739c  ldr x8, [x19,#0x68]   ; IUnityXRDisplayHelper*
0x673a0  mov x0, x20           ; the same Unity provider context
0x673a4  ldr x8, [x8]          ; helper slot +0: RegisterTextureProvider
0x673a8  blr x8
```

No SDK callback table or vtable address is placed in `x0`; it is exactly the opaque handle that
Unity supplied to the SDK's Initialize callback.  The following calls demonstrate the direction of
the interface.  The SDK continues to pass that same handle to helper slot `+0x08`
(`PropertyToID`) and stores only the integer property IDs:

```text
0x673ac  ldr x8, [x19,#0x68]
0x673b8  mov x0, x20
0x673bc  ldr x8, [x8,#0x08]
0x673c4  blr x8
0x673cc  str w0, [x19,#0xe4]
; repeated through 0x67488 for other property names/IDs
```

Thus there is no pointer for the SDK to store and later call back through.  Registration tells the
Unity helper which display-provider context owns subsequent texture properties.  Ignoring it in the
emulator loses Unity-side bookkeeping that Godot does not use, not an SDK texture creation hook.

The actual swapchain texture handoff bypasses this helper.  `DisplayManager::CreateTexture
@ 0x69530` builds the 0x30-byte `UnityXRRenderTextureDesc`; `w20` (the requested array length) is
written to `desc+0x28` at `0x69580`, then it calls `IUnityXRDisplay+0x18` at
`0x695a8..0x695b0`.  `DisplayManager::QueryTextureDesc @ 0x695e8` calls
`IUnityXRDisplay+0x20` at `0x695f0..0x69608`, and the returned GL name is read from `desc+0x08` at
`0x6960c`.  These are the emulator functions that already allocate/query the two-layer array.

## 2. `GetPlatformData` is not on this binary's rendering path

The display interface is stored at `DisplayManager+0x08`.  Exhaustive disassembly inspection of its
indirect calls finds uses of slots `+0x00` (lifecycle registration), `+0x08` (display-provider
registration), `+0x10` (graphics-thread registration), `+0x18` (CreateTexture), `+0x20`
(QueryTextureDesc), and `+0x28` (DestroyTexture).  There is no call through that interface's
`+0x30` slot (`GetPlatformData`) anywhere in `libXREALXRPlugin.so`.

Consequently the Rust function's `0` return is never observed by this SDK build, and no null/zero
platform-data value participates in choosing a GL target, `numViews`, or layer count.  (Other
unrelated C++ vtables do have `+0x30` calls; those are not loads from `DisplayManager+0x08`.)

The layer count is decided independently:

```text
DisplayOverlay::CreateBufferSpec @ 0xa6a48
0xa6a5c  ldr w9, [overlay,#0x14]
0xa6a60  str w9, [spec,#0x18]
```

As established in the prior analysis, `NativeRendering::CreateBufferSpec @ 0xa0e70` compares this
field with 2 and calls `NRBufferSpecSetMultiviewLayers(..., 2)` at `0xa0e8c..0xa0e90`.  No platform
data is read on that route.

## 3. Display-provider callbacks do not set up layers or textures

`DisplayManager::Initialize` registers two distinct SDK-owned callback records:

```text
0x67120..0x6712c  IUnityXRDisplay+0x10(context=x20, callbacks=sp+0x20)
                   ; graphics-thread provider (start/submit/populate/stop)
0x67160..0x67168  IUnityXRDisplay+0x08(context=x20, callbacks=sp)
                   ; ordinary display provider
```

The ordinary record is only 0x18 bytes (context plus two callbacks).  Its SDK functions are the
display-state callback (`DisplayManager::Initialize::$_9 @ 0x676f8`) and mirror-view callback
(`$_10 @ 0x67748`).  `$_9` writes display state/culling information; `$_10` delegates to
`QueryMirrorViewBlitDesc`.  Neither creates a texture, calls NR swapchain APIs, chooses a texture
target, or writes `overlay+0x14`.  Ignoring this record omits state/mirror services, but cannot alter
NR's layer-1 import.

The callbacks which do drive frames are instead the separately registered graphics-thread record,
which the emulator already stores and invokes.  The Multiview viewport setup itself remains wholly
inside the SDK: `DisplayOverlay::CreateViewport @ 0xa6a68` creates component 0 at `0xa6a94..0xa6a98`
and component 1 at `0xa6cbc..0xa6cc4`; it writes viewport layer 0 at `0xa6e18` and layer 1 at
`0xa6ebc`.  Neither operation references the ordinary display-provider callback record.

## 4. Graphics device-event callback

`SessionManager::UnityPluginLoad @ 0x8404c` requests only the trace interface and stores the
`IUnityInterfaces*`; it does not register a graphics-device event callback.  Later
`DisplayManager::LoadDisplay @ 0x66d14..0x66d30` obtains `IUnityGraphics` and calls only vtable slot
`+0x00` (`GetRenderer`).  There is no call through `IUnityGraphics+0x08` in the SDK setup path.
Therefore the empty `gfx_register_device_event_callback` has no effect on this run, much less on
the swapchain target.

## 5. Independent NR import evidence

The Unity-side metadata is already correct: one array swapchain, two viewport records, layer values
0 and 1, and `NRBufferSpecSetMultiviewLayers(2)`.  The point at which that correct model is lost is
inside `libnr_api.so`:

```text
libnr_api.so +0xebeb74  w0 = 0x0de1       ; GL_TEXTURE_2D
libnr_api.so +0xebeb78  w1 = client GL name
libnr_api.so +0xebeb84  glBindTexture(GL_TEXTURE_2D, client_name)
libnr_api.so +0xebeb88..+0xebebac
                         glGetTexLevelParameteriv(GL_TEXTURE_2D, ...)
```

No ignored Unity callback feeds this helper a target enum.  Although NR has an internal array
allocation/sampling path (`GL_TEXTURE_2D_ARRAY` allocation near `+0xec1608` and a
`sampler2DArray` shader), its caller-provided GL-name validation/import path fixes the target to 2D.
That independently explains why viewport/layer 0 works and viewport/layer 1 samples a cleared
resource despite successful writes to both client array layers.

## Practical conclusion

There is no exact callback/value to implement for this symptom:

- `RegisterTextureProvider`: retain/logging is optional; its argument is an opaque Unity context,
  not a callable SDK object.
- `GetPlatformData`: not called by this SDK binary.
- `RegisterDisplayProvider`: state and mirror callbacks only; unrelated to swapchain import.
- graphics device-event registration: not requested by this SDK path.

Accordingly, the right eye is **not fixable through these Unity-interface stubs**.  It is the
`libnr_api` client-array-import limitation.  Use the working two-`GL_TEXTURE_2D` Multipass path, or
patch/reverse-engineer the NR backend if Multiview is ever required.
