# Glasses render position / head-lock investigation

> **Current handoff (2026-07-13): see [`multiview-investigation.md`](multiview-investigation.md).** Head-lock
> is still unsolved; the reachable levers are exhausted (device-tested) and the last grounded path is
> Multiview, blocked on the NR-compositor (libnr_loader) single-buffer swapchain registration. This file is
> the earlier head-pose-pipeline writeup; the Multiview doc supersedes it for next steps.


Status: **fix implemented; pending a wearer to confirm the visual result.** Root cause identified by RE
(we drove the Godot cameras from the session-manager head-pose pipeline while the compositor reprojects
with the *display* `InputManager` pose — two different tracking sources) and the eye cameras are now driven
from the display pose. It builds, installs, and **runs stably on device** (4/4 launches, no crash); the
display pose is read correctly (see "Implemented" below). What still needs the glasses **worn**: confirming
the render is now a head-locked peek window, and calibrating the rotation axis signs against head motion.

The goal (user's model): the glasses display should be **head-locked** (the rendered rectangle
always fills the FOV, in front of the eyes) and the **Godot camera acts as a peek window** into
world-locked 3D. Two symptoms were reported: (1) render appeared higher than centre, (2) content
did not track head movement the way a peek window should.

## What was wrong, and what fixed it

### Symptom 1 — "render higher than centre" → FIXED (app-side recenter)

Root cause: **the SDK's `RecenterGlasses` (libXREALXRPlugin.so, display subsystem) does NOT reset
the pose we read via `XREALGetHeadPoseAtTime` (libXREALNativeSessionManager.so, session-manager
subsystem).** Device-confirmed: the pose quaternion is unchanged before/after calling it. So the
tracking origin stayed frozen at "session-start attitude"; if the app started with the glasses on a
desk (tilted), the view was off-centre and no `recenter()` fixed it (only a full restart did).

Fix (`node.rs`): app-side recenter. `recenter()` captures the current raw pose rotation as
`recenter_reference`; `process()` applies `recenter_reference.inverse() * raw` before
`set_quaternion`. This makes "wherever you look at recenter" the forward direction and cancels the
desk-tilt offset. Verified on device: `[xreal] recenter: reference euler=(…)` logs, and the Godot
camera visibly recenters. Also wired to the wear sensor: `_on_wearing_changed(true)` recenters the
instant the glasses are put on (see `demo/main.gd` + the "put on the glasses" prompt).

### Symptom 2 — peek-window head-lock → STILL OPEN

The glasses **display layer is world-anchored to the session-start direction** (turn your head and
the rendered rectangle stays put), while the correct behaviour is head-locked. Established by a
sequence of on-device experiments:

- **Godot camera DOES get the head rotation.** Diagnostic log of the eye camera's rendered
  transform showed `cam0 euler == head euler` every frame — Godot-side tracking is correct.
- **Removing the head rotation from the Godot eye cameras changed nothing** — the content still
  responded to head movement and the rectangle stayed anchored. ⇒ the head tracking / anchoring is
  done by the **XREAL compositor**, not our Godot cameras. Our camera rotation was *redundant* and,
  combined with the compositor, produced the "overshoot" (≈2× motion) the user first saw.
- **Reference-app comparison (decisive).** The user's real Unity build `com.kadinche.layeredclient.xreal`
  ("LayeredClient (XREAL)") is correctly head-locked. Its NRSDK display init logged **byte-identical**
  to ours: `frame buffer mode:1 / frame submit mode:1 / single_buffer:1 / Time warp is off /
  predict_ms:20`, and it too logs `Faield to get display roi` (so that error is normal, not our bug).
  We drive libXREALXRPlugin.so through our **emulated** `IUnityXRDisplay` in `src/unity_plugin.rs`,
  whereas LayeredClient uses the real Unity XR display provider inside `libil2cpp.so`.

- **Static teardown of LayeredClient's APK (jadx + aapt2, decisive on where the logic ISN'T).**
  Pulled `base.apk` and decompiled it. Findings:
  - Launcher is a **plain `com.unity3d.player.UnityPlayerActivity`** — *not* `ai.nreal.activitylife.NRXRActivity`.
    (This **disproves** the earlier "NRXRActivity sets up head-lock" hypothesis; `NRXRActivity` is only an
    overlay-permission wrapper that calls `NRXRApp.init`, and it isn't even in the launch path.)
  - The whole `ai.nreal.*` Java layer has **zero** head-lock / pose / compositor logic (activitylife is
    lifecycle plumbing; `IXRDisplayListener` is just display add/remove).
  - Config is **standard**: `UnitySubsystemsManifest.json` = "XREAL XR Plugin" 3.1.0 with only display/
    input/meshing ids; `boot.config` has `xrsdk-pre-init-library=XREALXRPlugin`, `gfx-threading-mode=4`,
    nothing head-lock-related.
  - It ships the **same `libXREALXRPlugin.so` + `libnr_loader.so`** we do; the app-specific rendering
    is all in `libil2cpp.so` (the XREAL SDK C# compiled to native).
  ⇒ Head-lock is **entirely** in the native per-frame pose/viewport submission (libil2cpp → libXREALXRPlugin
  → libnr_loader). No manifest, config file, or Java class controls it. Static Java analysis is exhausted.

> Note: an earlier draft guessed the **all-zero `hints` buffer** was the culprit (that the per-frame
> `UnityXRFrameSetupHints` carries the head pose). **RE disproved this** — `frame-submission-plan.md` and the
> disassembly show `UnityXRFrameSetupHints` holds only viewport/zNear/zFar/scale, and `PopulateNextFrameDesc`
> sources the pose from `InputManager`, not from `hints`. A zero-filled hints buffer is correct.

⇒ **Root cause (RE-confirmed mechanism; pending on-device verification): we render from a different
head-pose pipeline than the compositor reprojects with.** Disassembly of `DisplayManager::PopulateNextFrameDesc`
@0x68c7c shows it fills each `renderParams` purely from `InputManager::GetEyeFov` (per-eye FOV tangents) and
`InputManager::GetEyeXRPosFromHead` (eye-from-head offset) — and writes **no head world-orientation** into the
descriptor. So the head orientation that composites/reprojects the glasses layer comes from
**libXREALXRPlugin.so's own `InputManager` HMD pose**. Meanwhile our Godot eye cameras are driven by
`XREALGetHeadPoseAtTime` from **libXREALNativeSessionManager.so** — a *different* library / tracking pipeline
(the very split that made `RecenterGlasses` a no-op on our pose). The compositor reprojects the submitted
buffer by *its* (InputManager) pose while our content was baked with the *session-manager* pose, so the two
never cancel → the layer reads as world-anchored at the session-start attitude instead of a head-locked peek
window. This also explains the earlier on-device experiments precisely: with the Godot camera at **identity**
the content is head-locked (baked rotation 0, compositor still reprojects); with the **session-manager**
rotation it is wrong (baked ≠ compositor); the predicted-correct case (baked == compositor) was never tried
because we were reading the wrong library.

**The fix (scoped):** `libXREALXRPlugin.so` exports `GetHeadPoseAtTime` @0x48cc8, a thin wrapper that
tail-calls `InputManager::GetHeadPoseAtTime` @0x7f4a0 — i.e. **the exact pose the compositor uses** — and it
is a dlsym-able exported `T` symbol in the **already-loaded** `plugin_lib`. Read the head pose from that
export and drive the eye cameras from it, replacing the session-manager read. Caveat on its output ABI: it
writes a **64-byte / 16-float block** (copied straight from `NativePerception::GetHeadPose`'s struct return),
**not** the compact 7-float `NrPose`. The quaternion's offset within those 16 floats needs a one-shot on-device
log to pin down (find the 4 floats that form a unit quaternion, exactly as `NrPose`'s w-first order was
originally confirmed), then convert like `NrPose::to_godot_quaternion`. Bonus: `InputManager::RecenterHmd()`
@0x7bbb0 (and the exported `RecenterGlasses`) act on **this** InputManager pose, so once the camera reads it,
hardware recenter should finally take effect — superseding the app-side recenter workaround in `node.rs`.

### Concrete implementation sketch

1. `native.rs::load` — after `plugin_lib` is opened, `dlsym` `GetHeadPoseAtTime` into a new
   `xp_get_head_pose_at_time: Option<fn(u64, *mut [f32; 16]) -> i32>` (own type; do **not** reuse
   `FnGetHeadPoseAtTime`, which points at the 7-float `NrPose`). Keep the session-manager read as a fallback.
2. Add `XrealNative::head_pose_display(&self, time_ns) -> Option<[f32; 16]>` and, for the first N frames,
   `godot_print!` all 16 floats so we can identify the unit-quaternion quad + position on device.
3. `session.rs` — add `head_pose_display()` that calls it at `hmd_time_nanos()` (the display clock), mirroring
   the existing `head_pose()`.
4. `node.rs::process` — switch the eye-camera source to `head_pose_display()`; once the layout is confirmed,
   convert the quaternion (reuse/clone `to_godot_quaternion`'s handedness flip) and **drop the app-side
   recenter** (call the SDK recenter instead). Verify on device: turning the head should now pan the
   world-locked scene inside a face-locked window (peek window), and `RecenterGlasses`/MENU-long-press should
   re-forward it.

> ⚠️ Regression watch: the removed `get_head_rotation` #[func] SIGSEGV was traced to pulling `head_pose` into
> an `XrealSystem` #[func] thunk (see `input-feature-glthread-crash`). This change touches the head-tracker
> node's own `process()` path (not an `XrealSystem` #[func]), so it should be clear of that trap, but keep the
> new dlsym/read out of any `XrealSystem` #[func] body.

### Implemented — device findings (verified as far as an app-launch allows)

Wired exactly as the sketch (ffi `FnGetHeadPoseDisplay`; `native.rs` dlsym `GetHeadPoseAtTime` from
`plugin_lib` + `head_pose_display`; `session.rs` `head_pose_display`; `node.rs` drives the eye cameras from it,
with the session-manager path kept as a fallback). Two things were pinned down on device:

- **The 16-float block is a 4×4 row-major transform, NOT a quaternion** (device raw log): the upper-left 3×3 is
  the head rotation (each row a unit vector, so `norms(0,4,8)=1.000`), the last **row** (floats 12,13,14) is the
  small position (~2 cm), and the homogeneous column (floats 3,7,11)=0 with float 15 = 1. So `display_rotation`
  validates that structure and extracts the quaternion from the 3×3 (Shepperd), then applies the same NRSDK→Godot
  handedness flip as `NrPose::to_godot_quaternion`. Reads unit-norm and stable on device (e.g. desk-tilt euler
  ≈ pitch −16°, roll −17°, matching the raw matrix's `m01/m10` and `m12/m21` terms). The exact axis **signs**
  still need a wearer moving their head to confirm (nod=pitch/X, turn=yaw/Y, tilt=roll/Z) — adjust the flip if
  one axis reads inverted.

- **New crash rule (device-confirmed): never query BOTH `head_pose()` (session-manager) and
  `head_pose_display()` (XR-plugin) in the same frame.** Doing so deterministically re-triggers the
  `SIGSEGV 0x3f800000` GLThread crash (a build that added a per-frame session-manager read *for comparison*
  crashed 3/3 launches; removing it → 4/4 clean). Same fault addr / render-thread signature as the
  `get_head_rotation` case, so it belongs to the same fragile head-pose codegen/runtime interaction. The two
  reads are now **strictly mutually exclusive** in `process()`: the session-manager fallback runs only when the
  display export is entirely absent (`head_pose_display() == None`), never merely when its matrix is unusable
  for a frame. On this device the export is always present, so only the display pose is ever read at runtime.

Still **app-side recenter** is left on the (unused-on-device) session-manager fallback path; on the display path
we drive the raw compositor pose and delegate recenter to the SDK — to be validated/finished with the glasses on.

## What a graphics profiler / debug tool can (and cannot) reveal here

The user asked whether an on-device GPU profiler could analyse the running LayeredClient to find the head-lock
setup. Conclusion after the teardown above:
- **Standard GPU capture tools do not help.** RenderDoc / Android GPU Inspector / Snapdragon Profiler capture
  GL/Vulkan draw calls + GPU timing. But (a) the glasses layer is **`FLAG_SECURE`** so the composited output
  can't be captured, and (b) the head-lock difference is **not a draw-call difference** — both apps draw the
  same eye textures — it's in the **pose/viewport arguments** handed to the compositor each frame. A frame
  capture shows the textures, not the reprojection pose.
- **The decisive tool is Frida** (dynamic instrumentation): hook `PopulateNextFrameDesc` / `SubmitCurrentFrame`
  in `libXREALXRPlugin.so` and the `NRSwapchain` / `SetBufferViewport` calls in `libnr_loader.so`, dump the
  per-frame pose/viewport arguments in **LayeredClient**, then run the identical hooks in our app and **diff**.
  That reads out the exact `hints`/pose values LayeredClient submits — the one unknown. Cost: Frida needs root
  **or** a re-signed debuggable repackage (frida-gadget) since the XREAL device is unrooted and the app is a
  release build. This is the recommended next investment if the static/RE angle stalls.

## Dead ends (ruled out on device — do not retry)

- **`SetGlassesSpaceMode(0..3)`** — newly RE'd + wired (native→session→`XrealSystem.set_glasses_space_mode`).
  All four values return `ret=0` (accepted) but produce **no visual change**. Does not control the
  layer anchoring for our XR path.
- **XREAL follow-mode / lock-mode toggle** (firmware `ACTION_TYPE_TRIGGER_SWITCH_SPACE_MODE`, MULTI
  click) — user tried both; neither head-locks our render.
- **A missing NRSDK mode/config call** — ruled out: the display init params are identical to the
  reference app's (see above).
- **Display ROI failure** (`Faield to get display roi`) — a red herring; the reference app logs it too.
- **Godot eye-camera rotation** — confirmed correct and *not* the cause (removing it changed nothing).

## Candidate SDK controls not yet probed (if resuming the mode angle)

`ControlSet2D3DMode`, `SetGlassesSceneMode`, `SetDpWorkingMode`, `ControlSetDisplayDefaultStartMode`
(all `libXREALXRPlugin.so` exports). Lower priority than the frame-submission angle, since the
reference-app config already matches ours.

## Tooling notes for on-device iteration

- Build/export with **Godot 4.7** only (template match). `cargo ndk -t arm64-v8a -o ./jniLibs build
  --release`; `ANDROID_NDK_HOME=…\ndk\27.2.12479018`.
- Use **scrcpy's adb (v37)** — `…\scoop\apps\scrcpy\4.0\adb.exe` — not platform-tools v35; mixing
  versions kills the adb server and drops the Wi-Fi connection mid-iteration.
- The Godot export **hangs after writing the APK**. Detect completion by the output APK's size being
  stable AND a valid ZIP EOCD (`50 4B 05 06`) before installing — killing Godot mid-write corrupts
  the APK (`INSTALL_PARSE_FAILED_NOT_APK`).
- The glasses display is `FLAG_SECURE`, so `screencap` of it returns nothing; diagnose via logcat
  (`[xreal] …`) and the reference app, not screenshots.
