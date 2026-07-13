# Roll tracking investigation

Status: **investigation (device-blocked).** The rotation fix (commit `05dc80d`) made pitch/yaw
world-locked, but **roll is never output by the pose API we use** — physically tilting the head
leaks into pitch. This note captures why and the concrete experiments to recover roll (each needs
the glasses connected to verify).

## Finding

- We read head pose via `libXREALNativeSessionManager.so::XREALGetHeadPoseAtTime` (the compact
  7-float `NrPose`), with tracking type **MODE_6DOF (0)** (`src/session.rs`).
- Device calibration (2026-07-02, XREAL One Pro): the pose quaternion is **w-first (w, x, y, z)**;
  across pure pitch, pure yaw, and pure roll head moves the **4th float (z / roll) is always 0.000**.
  - pitch (nod) → float `x` (f1), yaw (turn) → float `y` (f2): both track correctly.
  - roll (head tilt) produces **no z**; instead it leaks into `x` (pitch) because the tracker
    attributes the tilt's gravity change to pitch.
  - ⇒ the compact NRPose from this API + MODE_6DOF is effectively **horizon-stabilized** (roll = 0).

## Symbols (`libXREALNativeSessionManager.so`)

Only `XREALGetHeadPoseAtTime` (used), `XREALGetDevicePoseFromHead`, `XREALGetImuOfflineBias`,
`XREALGetTrackingReason` relate to pose/IMU — no separate "raw orientation / roll" entry point.

## Options to recover roll (all require on-device verification)

1. **Switch to MODE_3DOF (1)** — pure IMU orientation, which typically includes roll.
   - Change `TrackingType::Mode6Dof` → `Mode3Dof` in `src/session.rs` (both the settings struct and
     `initial_tracking_type`), redeploy, tilt the head, and check the calibration log: does the 4th
     float (z) become non-zero?
   - Lowest effort (a two-line change). We only consume rotation, so losing 6DoF position is fine.
   - Risk: MODE_3DOF may still be horizon-stabilized for the display path; if z stays 0, roll is not
     recoverable this way.
2. **Alternate pose API** `libXREALXRPlugin.so::GetHeadPoseAtTime` (the Unity InputManager pose
   block — larger, different layout; see `docs/reverse-engineering.md`). May carry full orientation.
   - Needs the bigger pose struct layout reverse-engineered first. Medium effort.
3. **Raw IMU** — `XREALGetImuOfflineBias` hints at IMU access; a raw gyro/accel path could
   reconstruct roll, but that means implementing an orientation filter. High effort; last resort.

## Recommendation

Try **option 1** first — a two-line tracking-mode change + a device tilt test. If MODE_3DOF's pose
still has `z = 0`, the display path is roll-stabilized by design and roll needs option 2 or 3.

(Blocked on hardware: needs the glasses connected + a head-tilt while watching the
`[xreal] pose q(wxyz)=…` calibration log.)

## Result (2026-07-13, on device) — option 1 FAILED

Made the tracking mode selectable at startup (`debug.xreal.tracking_type`, see
`session.rs::tracking_mode()`); confirmed **MODE_3DOF (1)** active in the log
(`tracking_type = 1`, `native session created … tracking_type=Some(1)`), Multipass.

Captured a deliberate head-**roll** (ear-to-shoulder) sweep. The compact `NrPose` behaves
**identically to 6DoF**: the 4th quaternion float (**z / roll) stays exactly `0.000`** through the
whole sweep, while the roll leaks into **x (pitch)** — e.g. at the tilt extreme
`q(wxyz)=(0.634,-0.750,0.189,0.000)` → `pitch/x=71.9°`. So **MODE_3DOF is also horizon-stabilized**;
roll is not recoverable from `XREALGetHeadPoseAtTime` in either tracking mode.

⇒ Pursue **option 2**: the XR-plugin display pose we already bind as
`XrealNative::head_pose_display` (the 16-float block from `NativePerception::GetHeadPose`, the SAME
pose source the compositor reprojects the layer against — it visibly *does* carry roll on the
glasses). Drive the Godot eye cameras from that pose (full orientation incl. roll) so the app render
and the compositor reprojection share one pose → a correct peek window on all three axes. Blocker:
the 16-float layout (quaternion offset/order vs 4×4 matrix) must be pinned down first, and only ONE
head-pose pipeline may be queried per frame/thread (the `0x3f800000` GLThread rule) — so this
replaces, not augments, the `head_pose()` read in `node.rs`.
