# Glasses render position / head-lock investigation

Status: **partially solved.** Recenter now works (app-side); the "peek-window" head-lock of the
glasses layer is **still open** — narrowed to the XR frame-submission emulation, not a mode call.

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

⇒ **Prime suspect (in our code): the all-zero `hints` buffer.** `run_frame_tick` calls
`PopulateNextFrameDesc(context, user_data, hints=&[0u8; 0x80], desc)` with the **hints buffer zero-filled
every frame**. In the real Unity path that input struct (the per-frame "render params": current tracking
pose / reference-space transform / viewport scale / focus plane) carries the **current head pose** the frame
should be anchored to. Feeding zeros plausibly makes the SDK anchor every frame at a *fixed* pose →
world-anchored layer at session-start attitude, which is exactly the observed symptom. The blocker is the
**hints/params struct layout is unknown** (it's the *input* to PopulateNextFrameDesc; our RE mapped the
*output* `desc` only). Next: recover that layout (RE of how PopulateNextFrameDesc reads `hints`, or Frida-dump
what Unity passes in LayeredClient), then feed the live head pose in. Secondary angle: render each eye from
the pose the SDK writes into `desc` so the compositor reprojection delta is ~0, instead of from our
independent `head_pose()` read.

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
