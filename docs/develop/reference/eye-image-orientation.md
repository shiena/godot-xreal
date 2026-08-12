# Eye-image orientation and the head pose are one setting, not two

**Settled on device 2026-08-12.** The Vulkan path submits a vertically mirrored eye image, and the
head pose has to be mirrored to match. Neither can be chosen on its own: the compositor reprojects
every submitted frame onto the latest head pose, so mirroring the image also mirrors the direction
that reprojection pulls.

A vertical mirror reverses exactly two axes, **pitch and roll**, and leaves **yaw** alone. That is
the whole rule.

| submission path | eye image | head pose quaternion |
|---|---|---|
| GL (Compatibility) | as rendered | `(x, -y, z, w)` |
| Vulkan bridge | mirrored vertically | `(-x, -y, -z, w)` |

`XrealHeadTracker::display_rotation` picks between them with `vk_bridge::mirrors_eye_image()`.

## Why the Vulkan path mirrors at all

Godot's Vulkan render target has its origin at the top-left. The image reaches the SDK as a GL
texture over the same allocation (OPAQUE_FD), and GL reads it bottom-left. The blit in
`vk_bridge.rs` swaps the destination's Y bounds to reconcile the two. The GL path has no such
mismatch and copies straight across.

## The failure modes, and how to tell them apart

Each symptom below points at exactly one mistake. They were all observed on device while getting
this right.

| symptom | cause |
|---|---|
| View is upside-down, head tracking otherwise correct | image mirrored, pose not (or the reverse) |
| Looking up moves the view down | pitch inverted relative to the image |
| One axis swings about **twice as far** as the head, in the wrong direction | that axis is flipped in the pose but not in the image: the app's rotation and the compositor's reprojection add instead of cancelling |
| Yaw correct, pitch and roll both wrong | the image/pose pairing is inconsistent - a vertical mirror is exactly those two axes |

The doubling is the useful signal. A wrong sign that only *looks* wrong still tracks 1:1 with the
head; a wrong sign that fights the reprojection tracks at 2:1. If an axis overshoots, the image and
the pose disagree about it.

## Screenshots cannot settle this

`adb shell screencap -p -d <glasses display>` reads the compositor's buffer, not the optics, and
comes out **vertically flipped relative to what the wearer sees**. Judging orientation from a
screenshot inverts the answer. It remains the right tool for left/right questions (it is how the
Multiview black-right-eye work was verified, side by side) and for asking whether content is being
drawn at all.

Judge orientation on device, with a wearer, on content that has an unmistakable top and bottom.

## Why demo/ could not catch any of this

`demo/ar_scene.tscn` is a ring of boxes, **all at y = 0**, in a black see-through environment. A
vertical mirror of that content is indistinguishable from the correct image, and a pitch that
tracks backwards is hard to notice with nothing above or below to move against. The port ran this
way for a long time, and the mirror was even noticed and worked around rather than fixed:
`demo/phone_pointer.gd` carried "the glasses buffer reads +Y as down, so a positive Y puts the
origin at the bottom", and `xr_input_router.gd`'s `hand_offset` was calibrated against that
mirrored view.

The first application with a horizon (a planetarium: sky above, ground below) exposed all of it.

**When touching this area, test with content that has a top and a bottom.**

## Related: two other places the same day

Both were found while chasing the above and are independent of it.

- **6DoF position had its Y negated** (`src/node.rs`). The comment called it "the same
  NRSDK-to-Godot Y-flip as the rotation", but the rotation performs no Y flip - `(x,y,z,w) ->
  (-x,-y,z,w)` mirrors Z, the left-handed-to-right-handed change. Lifting the glasses 0.78 m off the
  desk logged `pos.y = -0.776`: the head sank as it rose, so putting them on dropped the viewpoint
  under the floor.
- **`get_transform_for_view` discarded its `cam_transform`** (`src/xr_interface.rs`). Godot's
  contract is `cam_transform` composed with the tracking-space pose; dropping it pinned the
  multiview path to the tracking origin, so moving `XROrigin3D` did nothing. The multiview rig's
  camera also sits inside a SubViewport, inheriting no transform of its own, so `node.rs` now places
  it at the origin's world transform - that is what Godot reads back as `cam_transform`.
