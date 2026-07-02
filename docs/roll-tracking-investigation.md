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
