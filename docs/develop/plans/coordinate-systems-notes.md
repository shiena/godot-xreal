# Coordinate systems: official XREAL doc vs. this port's RE

Comparison of XREAL's official "NRSDK Coordinate Systems" doc (Unity NRSDK) against what this Godot
port reverse-engineered from the native C ABI. Written 2026-07-19.

## What the official doc says (authoritative, Unity NRSDK only)

- **All NRSDK poses/extrinsics are in Unity's coordinate system — LEFT-handed** (+X right, +Y up, +Z
  forward). `NRFrame.HeadPose` is the `NRCameraRig` device pose in Unity world.
- `GetDevicePoseFromHead(device)` returns a device's extrinsic **relative to Head**, in Unity space, for
  `RGB_CAMERA` / `LEFT_GRAYSCALE_CAMERA` / `RIGHT_GRAYSCALE_CAMERA` (grayscale = SLAM stereo pair).
- **Unity(LH) → OpenCV(RH, Y-down): negate the Y axis only** — `cv_T_unity = Scale(1,-1,1)`, then
  `cvA_T_cvB = cv_T_unity * unityA_T_unityB * cv_T_unity⁻¹`. OpenCV is right-handed (for CV devs).
- Camera model: intrinsic `K(fx, fy, cx, cy)` in pixels; distortion `(k1, k2, p1, p2, k3, k4, k5)`.
- Explicitly **Unity-NRSDK-only** ("does not apply to other NRSDKs").

## This port's RE (native C ABI, converted to Godot RH)

Godot is right-handed, Y-up, **-Z forward** (OpenGL-style) — different from BOTH Unity (LH, +Z fwd) and
OpenCV (RH, Y-down). Our conversions (from the code):

| subsystem | position | quaternion | source |
|-----------|----------|------------|--------|
| hand tracking | `(x, y, -z)` | `(-x, -y, z, w)` | `src/hand_tracking.rs` |
| depth mesh | `(x, y, z)` — raw NR is already Godot | — | `src/system.rs::mesh_block_to_dict` |
| planes / anchors / images | `(x, y, -z)` | `(-x, -y, z, w)` | `src/system.rs::unity_pose_to_transform` |
| head pose (calibration) | — | `(-x, -y, z, w)` | `src/ffi.rs::to_godot_quaternion` |
| DISP head pose | — | `(-x, -y, -z, w)` | `src/node.rs::display_rotation` |

## Comparison / takeaways

1. **Foundation confirmed.** The doc authoritatively states the native poses are **Unity left-handed** —
   exactly what every `UnityPose` struct and "Unity space" comment in this port assumed. The whole RE
   approach (treat SDK poses as Unity LH, convert to Godot RH) is validated.

2. **Our Y-flip was a render-pipeline artifact, not a coordinate fundamental — and it is gone.** The
   *canonical* Unity(LH,+Z fwd) → Godot(RH,-Z fwd) conversion is **negate Z only** (position
   `(x, y, -z)`, quaternion `(-x, -y, z, w)`). The code used to negate Y on top, attributing it to "this
   port's eye SubViewports render with an inverted Y", i.e. a Godot rendering quirk. That prediction held:
   once the eye image stopped being submitted mirrored, every consumer of the extra flip had to drop it.
   Hands and the depth mesh were the last two, both verified on an Air 2 Ultra (2026-08-14). (The doc's
   own Y-negation is a *different* thing — Unity→OpenCV, because OpenCV is Y-down — it just
   coincidentally also touches Y.)

3. **Every tracked subsystem now uses the canonical conversion.** `(x, y, -z)` and `(-x, -y, z, w)`,
   for hands, planes, anchors and image tracking alike. The last three arrived there by derivation
   rather than measurement: reflecting Y maps a quaternion `(a, b, c, w)` to `(-a, b, -c, w)`, and
   applying that to the canonical quaternion gives exactly the `(x, -y, -z, w)` they used to emit —
   so they were the canonical conversion plus the same mirror, and dropping it lands on canonical.
   Device-verified on an Air 2 Ultra (2026-08-14): plane tracking, world anchors and image tracking
   all land where they should. Their signs had been "pending on-device verification with real
   planes" ever since they were written, so this was the first check any of the three ever had.

   DISP is a separate case on purpose: `display_rotation` returns `(-x, -y, -z, w)`, which pairs with
   the vertical mirror the eye image gets on every submission path.

## Renderer coverage of the 2026-08-14 verification

Everything above was checked on both builds: GL Compatibility + multipass, and Vulkan Mobile. Hand
tracking, plane detection, image tracking, world anchors and the depth mesh all read correctly on
each.

Only the *appearance* checks could have differed. The pose conversions and the depth mesh's
geometry are renderer-independent data, and the front-face convention is not: Godot reports
clockwise as front on `gl_compatibility`, `mobile` and `forward_plus` alike (measured with a single
triangle). What the renderer does change is the eye-image submission path - GL flips in
`gl.rs::blit_texture` / `blit_default_framebuffer`, Vulkan in the bridge's blit - and a path that
failed to mirror would make every one of these read inverted. Both mirror.

Multiview needs no separate pass: it changes how Godot *draws* the two eyes, not how the image is
handed over, and the mirror rides on the handover.

Note for anyone repeating this: the Vulkan build draws nothing on the glasses until
`debug.xreal.vulkan_glasses` is set to 1 (`vk_bridge.rs::glasses_enabled`, default off while that
path is in bring-up). It is a `setprop`, so it does not survive a reboot.

## Unused native APIs the doc revealed (all C exports in `libXREALXRPlugin.so`, dlsym-able)

Confirmed the exact export symbols (no prefix) with `llvm-objdump -T`; component ids
(`XREALComponent`): `DISPLAY_LEFT=0, DISPLAY_RIGHT=1, RGB_CAMERA=2, GRAYSCALE_CAMERA_LEFT=3,
GRAYSCALE_CAMERA_RIGHT=4, MAGNETIC=5`.

| symbol | signature (C#) | use |
|--------|----------------|-----|
| `GetDevicePoseFromHead` | `(XREALComponent, ref Pose) → bool` | device extrinsic rel. to head (Unity Pose = pos[3]+quat[4]) |
| `GetDeviceResolution` | `(XREALComponent, ref Vector2Int) → bool` | pixel resolution |
| `GetCameraIntrinsic` | `(XREALComponent, ref Vector2 focal, ref Vector2 principal) → bool` | fx,fy / cx,cy |
| `GetCameraProjectionMatrix` | `(XREALComponent, float near, float far, ref Matrix4x4) → bool` | projection matrix |

These are geometry we never used: the RGB camera's offset from the head, its FOV/intrinsics, and its
projection. The **blend** currently overlays the head-POV AR straight onto the camera image (ignoring the
camera's offset + FOV), so the AR is only approximately aligned. Using the RGB-camera pose + projection
(or intrinsics) would let the blend match the camera view geometrically.

**Wired up (`src/ffi.rs` types, `src/native.rs` dlsym + methods, `src/system.rs` `#[func]`s) and
device-verified 2026-07-19** on the One Pro RGB camera — the values are self-consistent, which proves the
FFI is correct:

- `get_device_resolution(RGB_CAMERA)` → `(1280, 720)`.
- `get_camera_intrinsics(RGB_CAMERA)` → `fx=608.4, fy=608.3, cx=642.9, cy=361.0` (px). Principal point is
  near the image centre (640, 360); horizontal FOV ≈ 2·atan(640/608) ≈ 93°.
- `get_device_pose_from_head(RGB_CAMERA)` → position `(0.0012, 0.0062, 0.0276) m` (≈ 2.8 cm in front of
  the head, slightly up/right, Unity +Z fwd), rotation ≈ identity (w = 0.99996). Plausible camera mount.
- `get_camera_projection_matrix(RGB_CAMERA, 0.1, 100)` → a standard OpenGL projection whose `m00 = 0.951 =
  2·fx/w`, `m11 = 1.690 = 2·fy/h`, and whose depth terms match the passed near/far — i.e. it agrees with
  the intrinsics exactly.

GDScript: `XrealSystem.COMPONENT_RGB_CAMERA` (= 2) etc.; getters return `Vector2i` / `PackedFloat32Array`
(empty on failure). **Now applied to the blend (`f46e2b9`):** the camera+AR blend drives its AR camera
from the RGB geometry — vertical FOV from `fy` (default 75° → the camera's ~61°) plus the
`GetDevicePoseFromHead` forward offset (~2.8 cm) for parallax — so the holograms match the camera image
instead of the previous naive full-frame overlay. In `demo/stream_manager.gd` (live blend) and
`demo/blend_manager.gd` (Blend Photo). Possible refinement: the exact off-centre projection (`cx/cy` is
only ~0.5% off-centre here, so a symmetric FOV is already a close match) and on-device tuning of the
offset sign.
