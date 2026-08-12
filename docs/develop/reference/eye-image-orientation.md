# Eye-image orientation: the vertical mirror, and why demo/ could not show it

**Fixed 2026-08-12.** Every eye image handed to the XREAL compositor was mirrored vertically, on
all four paths (GL Multipass, GL fbo-0, Vulkan bridge at native scale, Vulkan bridge scaled). The
scene rendered upside-down on the glasses. Head tracking was correct throughout: look left and the
view went left, so only the vertical axis was wrong.

## Why it survived this long

`demo/ar_scene.tscn` is a ring of boxes, **all at y = 0**, inside a black see-through environment.
A vertical mirror of that content is indistinguishable from the correct image: the same boxes sit
at the same screen positions. Nothing in the demo has a top or a bottom.

The mirror was noticed, understood as a property of the buffer, and worked around instead of
fixed. `demo/phone_pointer.gd` carried this comment:

> On the glasses buffer +Y reads as down, so a positive Y puts the origin at the bottom.

and `addons/godot_xreal/xr_input_router.gd` set `hand_offset = Vector3(0.28, 0.32, -0.3)`, calling
the sign "device-verified". It was verified — against a mirrored image. The pointer beam did come
out at hand height, because the mirror flipped the offset along with everything else.

`src/gl.rs` recorded the opposite conclusion in a code comment:

> Straight copy (no Y-flip): fbo 0 and the eye texture share GL bottom-left origin, so flipping
> made it upside-down on the glasses.

Flipping *did* look wrong at the time, because the reference for "right" was a scene with no
vertical structure. The first app with a horizon (a planetarium: sky above, ground below) settled
it in one screenshot.

## The mechanism

Godot renders with its origin at the top-left. The XREAL compositor reads the eye texture with the
opposite vertical origin. Handing the image across unchanged mirrors it.

## The fix, path by path

| Path | Was | Now |
|---|---|---|
| GL Multipass (`gl.rs::blit_texture`) | `glCopyImageSubData` when formats and sizes matched, `glBlitFramebuffer` otherwise | always `glBlitFramebuffer` with the destination's Y bounds swapped |
| GL fbo 0 (`gl.rs::blit_default_framebuffer`) | `glBlitFramebuffer`, no flip | destination Y bounds swapped |
| Vulkan, equal size (`vk_bridge.rs`) | `vkCmdCopyImage` | `vkCmdBlitImage`, `VK_FILTER_NEAREST`, `dst_offsets` Y swapped |
| Vulkan, scaled (`vk_bridge.rs`) | `vkCmdBlitImage`, `VK_FILTER_LINEAR`, no flip | same, `dst_offsets` Y swapped |

**Texel-for-texel copies had to go.** `glCopyImageSubData` and `vkCmdCopyImage` move bytes and
cannot transform coordinates, so neither can express a mirror. Both were the fast path for the
equal-size case; both are now blits. At equal size the filter is NEAREST, so the pixels are
identical to what the copy delivered — only their rows are reversed.

One case still cannot flip: an sRGB-typed Vulkan source, where blitting into the UNORM bridge
image would decode the color values. That path keeps the raw copy and renders upside-down; it
warns once (`COPY_FALLBACK_LOGGED`). The node normally detects such a source and rebuilds a
full-size viewport, so it should not be reachable in practice.

`hand_offset` in `xr_input_router.gd` and `demo/phone_pointer.gd` went from `+0.32` to `-0.32`,
since Y now means what it says.

## Cost (Beam Pro, Adreno 710, 1574x907 -> 1968x1134 per eye)

| Renderer | Before | After |
|---|---|---|
| Vulkan + XR multiview | 57 fps | **57 fps** |
| Vulkan + Multipass | 51 fps | 41 fps |

The flip is free under multiview, which blits once for both views, and costs about 2.4 ms per eye
under Multipass. Adreno's blit slows down when the destination rows are written in reverse order,
which is the tile-locality penalty. If Multipass ever needs that back, the escalation is a
fullscreen sampling pass that flips in the shader (the `fill v2` option from
[`vulkan-path-plan.md`](../plans/vulkan-path-plan.md)) rather than a return to the raw copy.

## What to check when this area changes

Render something with an unambiguous top and bottom — a horizon, a floor, text — and photograph or
screenshot the glasses display:

```powershell
adb shell dumpsys SurfaceFlinger --display-id
adb shell screencap -p -d <glasses id> /sdcard/g.png
adb pull /sdcard/g.png
```

The screenshot is the compositor's own buffer, so it shows exactly what the wearer sees. A ring of
boxes at one height proves nothing.
