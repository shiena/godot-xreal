# XREAL → Godot port plan

> **Status (2026-07-16): the plan's end goal is achieved and exceeded.** Stereo display
> (Multipass, head-locked via the DISP pose, 6DoF tracking), glasses input (hardware keys +
> wear sensor), RGB camera feed, phone-as-3D-pointer, hand tracking (Air 2 Ultra) and render
> metrics are all implemented and device-verified. Milestone statuses below are updated in
> place; per-feature docs live in `docs/plans/` (see `docs/README.md`). Main open item:
> hot-plug session recovery (`hotplug-session-recovery.md`).

Goal: drive XREAL glasses from Godot 4, minimum bar **3DoF (head rotation) shown on screen**.
Approach: a single C++/Rust GDExtension (this repo, godot-rust) that `dlopen`s the vendored
XREAL `.so` files and feeds Godot. We do **not** port the Unity C#; see
`docs/reference/reverse-engineering.md` for why the native libraries are the integration surface.

Target is necessarily a **Godot Android app** (the XREAL natives are Android arm64 only),
running on an XREAL-compatible host (phone / Beam) with the glasses on USB-C.

## Milestones

### Phase 0 — toolchain
- godot-rust (`godot = "0.5"`, `api-4-4`), `cargo-ndk`, Android NDK, an XREAL device.
- `cargo build` loads the extension in the desktop editor (native libs absent → node no-ops).

### Phase 1 — perception spike ✅ DONE
- `XrealHeadTracker` (`src/node.rs`) dlopens `libXREALNativeSessionManager.so`, calls
  `XREALLoadAPI()` then `XREALGetHeadPoseAtTime(time, &pose)` with `time` from
  `XREALGetHMDTimeNanos(&time)`, and applies the rotation to itself.
- **Done (round a):** the perception ABI is RE-confirmed (signatures, `GetHMDTimeNanos` out-param,
  `LoadAPI` requirement) — see `docs/reference/reverse-engineering.md`.
- **Done (round b):** session bootstrap (`src/session.rs`): `InitUserDefinedSettings` (3DoF,
  Activity from `src/jni_bridge.rs`) → `CreateSession(false)` → `XREALLoadAPI()`.
- **Done (round c, device-driven):** exposed as ONE process-global lazily-initialized session
  (`session::shared()`). The SessionManager is a process singleton, so both `XrealHeadTracker`
  and `XrealSystem` share it and it must be built before any query — querying `IsSessionStarted`
  first (the old "reuse host session" probe) SIGSEGV'd on device. There is no Unity launcher to
  reuse a session from, so that probe is gone; we always bootstrap our own. Also: project renderer
  switched to **Compatibility/GLES3** (Vulkan crashed in the Forward Mobile swapchain). See
  `docs/guides/android-setup.md`.
- **Done (round d, device-driven):** Activity wiring. Godot does NOT populate `ndk_context`
  (confirmed: `activity_ptr()` panicked → "1337" spam), so a Java bridge supplies it:
  `GodotApp.onCreate` calls `XrealBridge.register(this)` →
  `Java_com_godot_game_XrealBridge_nativeRegisterActivity` (in `src/jni_bridge.rs`, `jni` crate)
  creates a leaked global ref and calls `ndk_context::initialize_android_context(vm, activity)`.
  `session::shared()` is now retry-friendly (`Option`, not `Result`): `WaitingForActivity` returns
  `None` quietly and retries; libs-absent is terminal + logged once.
- **Done (device verify, resolved):** (1) `CreateSession` succeeds with the Activity supplied
  (`native session created`, `session_started=true` in logcat); (2) the app runs in the XREAL
  glasses display mode directly (manifest markers, `docs/guides/android-setup.md`) — no Nebula
  VirtualDisplay involved; (3) `NrPose` field order confirmed on device: rotation first,
  **w-leading** (w, x, y, z) — fixed with unit tests in `NrPose::to_godot_quaternion`
  (`src/ffi.rs`). Note the head tracker has since moved to the XR plugin's **display pose**
  (`head_pose_display`, full attitude incl. roll) — see `docs/archive/roll-tracking-investigation.md`.

### Phase 2 — display ✅ ACHIEVED 2026-07-02
> **Engine-owned GL textures reach the glasses on device.** A test pattern (per-eye animated colour)
> displays on the XREAL One Pro. Winning recipe (see `docs/plans/frame-submission-plan.md` + the project
> memory `2026-07-02 SOLVED`):
> 1. Full fake `IUnityXRDisplay` (`CreateTexture`/`QueryTextureDesc`/`DestroyTexture` at
>    +0x18/+0x20/+0x28) allocating GL textures the engine owns.
> 2. **`stereo_rendering_mode = 0` (Multi-pass)**, NOT Multiview(2): Multiview's single-buffer mode
>    never calls `QueryTextureDesc`; Multi-pass makes a normal multi-buffer swapchain where
>    `SetSwapChainBuffers → QueryTextureDesc` fires and registers our textures.
> 3. Per frame: `PopulateNextFrameDesc` → fill `renderPasses[k].textureId` → `SubmitCurrentFrame`.
> 4. `patch_update_metrics` (lib+0x68974 → `ret`) so `SubmitCurrentFrame`'s `UpdateMetrics` doesn't
>    SIGBUS on the null metrics callback; presentation happens earlier via `SubmitFrame`.
> The "remaining" items (real Godot content in the eye textures, per-eye stereo cameras) have
> since shipped: two `SubViewport`s with per-eye cameras render into the acquired textures every
> frame. The RE below is the historical trail.

- Start the Godot-side `XrealCompanionActivity` on the XREAL secondary display, then drive the
  compositor through either the hidden `libXREALXRPlugin.so` display path or the public
  `libnr_loader.so` `NRRendering*` API.
- **RE item:** signatures/structs for `CreateFrame` / `GetFrameMetaData` / `CreateProjectionRigLayer`.
- **RE item:** confirm whether the companion Activity is enough to trigger `NRDisplay START/RUN`;
  if not, wire direct `NRRenderingCreate` → `NRRenderingStart` → swapchain/frame submission.
- **Minimal alternative:** check whether any device/mode exposes the glasses as a plain external
  DisplayPort monitor for self-rendered side-by-side — would bypass the compositor RE entirely.

### Phase 3 — 3DoF integration ✅ DONE
- Camera follows head (`XrealHeadTracker` driven by the DISP pose, full attitude incl. roll);
  `recenter()` for forward reset. Head-lock fully solved 2026-07-13: per-frame HMD update with
  `updateType=1` keeps the compositor reprojection base live
  (`docs/archive/codex-headlock-analysis.md`). Recommended config: **6DoF + Multipass + DISP pose**.

### Phase 4 (post-MVP) — stereo / distortion / IPD — stereo ✅, rest shelved
- Stereo ships as **two `SubViewport`s + Multipass** (per-eye cameras). Multiview is shelved:
  the NR compositor can only import client `GL_TEXTURE_2D`, so the right eye stays black with a
  `GL_TEXTURE_2D_ARRAY` (`docs/archive/codex-righteye-analysis.md`); `stereo_rendering_mode()`
  is pinned to 0 (Multi-pass).
- The `XRInterfaceExtension` migration was not needed for stereo and is not planned; hand
  tracking does integrate with Godot XR via `XRHandTracker`/`XRServer`
  (`docs/plans/hand-tracking-plan.md`).

## Risks
- **Display path is the single make-or-break risk** — validate Phase 2 early.
- Reverse-engineering/redistributing the binaries is subject to XREAL's EULA — clear before any
  public distribution.
- dlsym'd symbols are internal and may change across SDK versions — pin the SDK version.
- Hardware-in-the-loop required; no emulator path.
