# XREAL → Godot port plan

Goal: drive XREAL glasses from Godot 4, minimum bar **3DoF (head rotation) shown on screen**.
Approach: a single C++/Rust GDExtension (this repo, godot-rust) that `dlopen`s the vendored
XREAL `.so` files and feeds Godot. We do **not** port the Unity C#; see
`docs/reverse-engineering.md` for why the native libraries are the integration surface.

Target is necessarily a **Godot Android app** (the XREAL natives are Android arm64 only),
running on an XREAL-compatible host (phone / Beam) with the glasses on USB-C.

## Milestones

### Phase 0 — toolchain
- godot-rust (`godot = "0.5"`, `api-4-4`), `cargo-ndk`, Android NDK, an XREAL device.
- `cargo build` loads the extension in the desktop editor (native libs absent → node no-ops).

### Phase 1 — perception spike  ← **current**
- `XrealHeadTracker` (`src/node.rs`) dlopens `libXREALNativeSessionManager.so`, calls
  `XREALLoadAPI()` then `XREALGetHeadPoseAtTime(time, &pose)` with `time` from
  `XREALGetHMDTimeNanos(&time)`, and applies the rotation to itself.
- **Done (round a):** the perception ABI is RE-confirmed (signatures, `GetHMDTimeNanos` out-param,
  `LoadAPI` requirement) — see `docs/reverse-engineering.md`.
- **Done (round b):** session bootstrap (`src/session.rs`): `InitUserDefinedSettings` (3DoF,
  Activity from `src/jni_bridge.rs`) → `CreateSession(false)` → `XREALLoadAPI()`.
- **Done (round c, device-driven):** exposed as ONE process-global lazily-initialized session
  (`session::shared()`). The SessionManager is a process singleton, so both `XrealHeadTracker`
  and `XrealSystem` share it and it must be built before any query — querying `IsSessionStarted`
  first (the old "reuse host session" probe) SIGSEGV'd on device. There is no Unity launcher to
  reuse a session from, so that probe is gone; we always bootstrap our own. Also: project renderer
  switched to **Compatibility/GLES3** (Vulkan crashed in the Forward Mobile swapchain). See
  `docs/android-setup.md`.
- **Done (round d, device-driven):** Activity wiring. Godot does NOT populate `ndk_context`
  (confirmed: `activity_ptr()` panicked → "1337" spam), so a Java bridge supplies it:
  `GodotApp.onCreate` calls `XrealBridge.register(this)` →
  `Java_com_godot_game_XrealBridge_nativeRegisterActivity` (in `src/jni_bridge.rs`, `jni` crate)
  creates a leaked global ref and calls `ndk_context::initialize_android_context(vm, activity)`.
  `session::shared()` is now retry-friendly (`Option`, not `Result`): `WaitingForActivity` returns
  `None` quietly and retries; libs-absent is terminal + logged once.
- **Open (device verify):** (1) does `CreateSession` succeed now that the Activity is supplied?
  confirm `xreal: Activity registered...` (Java) + `[xreal] native session created (3DoF)` (Rust)
  in logcat — or a fresh crash backtrace inside CreateSession; (2) does a native 3DoF session even
  start while the app runs inside Nebula's VirtualDisplay (launched via the Nebula home), or does
  that mode preclude it; (3) `NrPose` field order + quaternion sign in `NrPose::to_godot_quaternion`
  (log the 7 floats once tracking runs).

### Phase 2 — display ✅ ACHIEVED 2026-07-02
> **Engine-owned GL textures reach the glasses on device.** A test pattern (per-eye animated colour)
> displays on the XREAL One Pro. Winning recipe (see `docs/frame-submission-plan.md` + the project
> memory `2026-07-02 SOLVED`):
> 1. Full fake `IUnityXRDisplay` (`CreateTexture`/`QueryTextureDesc`/`DestroyTexture` at
>    +0x18/+0x20/+0x28) allocating GL textures the engine owns.
> 2. **`stereo_rendering_mode = 0` (Multi-pass)**, NOT Multiview(2): Multiview's single-buffer mode
>    never calls `QueryTextureDesc`; Multi-pass makes a normal multi-buffer swapchain where
>    `SetSwapChainBuffers → QueryTextureDesc` fires and registers our textures.
> 3. Per frame: `PopulateNextFrameDesc` → fill `renderPasses[k].textureId` → `SubmitCurrentFrame`.
> 4. `patch_update_metrics` (lib+0x68974 → `ret`) so `SubmitCurrentFrame`'s `UpdateMetrics` doesn't
>    SIGBUS on the null metrics callback; presentation happens earlier via `SubmitFrame`.
> Remaining for Phase 2/3: replace the test-pattern fill with real Godot content (blit the main
> viewport into the eye textures), then per-eye stereo cameras. The RE below is the historical trail.

- Start the Godot-side `XrealCompanionActivity` on the XREAL secondary display, then drive the
  compositor through either the hidden `libXREALXRPlugin.so` display path or the public
  `libnr_loader.so` `NRRendering*` API.
- **RE item:** signatures/structs for `CreateFrame` / `GetFrameMetaData` / `CreateProjectionRigLayer`.
- **RE item:** confirm whether the companion Activity is enough to trigger `NRDisplay START/RUN`;
  if not, wire direct `NRRenderingCreate` → `NRRenderingStart` → swapchain/frame submission.
- **Minimal alternative:** check whether any device/mode exposes the glasses as a plain external
  DisplayPort monitor for self-rendered side-by-side — would bypass the compositor RE entirely.

### Phase 3 — 3DoF integration
- Camera follows head; `RecenterGlasses` for forward reset; `SetPredictTime` for latency.
- "3DoF on screen" MVP complete.

### Phase 4 (post-MVP) — stereo / distortion / IPD
- `view_count = 2`, per-eye projection, `UpdateIPD`, reprojection via `CreateProjectionRigLayer`.
- At this point migrate `XrealHeadTracker` into a full `XRInterfaceExtension` so Godot's XR
  pipeline (per-eye render targets, `_get_projection_for_view`) drives rendering.

## Risks
- **Display path is the single make-or-break risk** — validate Phase 2 early.
- Reverse-engineering/redistributing the binaries is subject to XREAL's EULA — clear before any
  public distribution.
- dlsym'd symbols are internal and may change across SDK versions — pin the SDK version.
- Hardware-in-the-loop required; no emulator path.
