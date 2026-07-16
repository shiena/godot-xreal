# docs/

Documentation, grouped by genre. Archived investigation logs are kept verbatim — their
"status" lines reflect the time of writing; the current outcome is noted here.

## guides/ — current setup & build how-tos

| Doc | Summary |
|---|---|
| [android-setup.md](guides/android-setup.md) | Android/manifest scaffolding required to enter the XREAL glasses display mode (vs. gray mirror). |
| [build-and-release.md](guides/build-and-release.md) | Full command reference: cargo-ndk, Godot export, Gradle, signing, release packaging. |

## reference/ — settled reference material

| Doc | Summary |
|---|---|
| [reverse-engineering.md](reference/reverse-engineering.md) | XREAL native ABI RE notes — the source of truth for `src/ffi.rs` / `src/native.rs`. Includes the direct-NR-path probe log. |
| [native-api-reference.md](reference/native-api-reference.md) | RE'd native functions ↔ GDScript bindings table (Japanese). Which symbols are callable, which are probe-only. |

## plans/ — feature design & implementation plans

| Doc | Status | Summary |
|---|---|---|
| [port-plan.md](plans/port-plan.md) | living | Overall Unity→Godot port plan and phase tracking — all display/tracking milestones achieved (statuses updated 2026-07-16). |
| [frame-submission-plan.md](plans/frame-submission-plan.md) | implemented | The architectural verdict: speak the full `IUnityXRDisplayInterface` provider protocol; direct NR compositor calls are a dead end. Rationale for `src/unity_plugin.rs`. |
| [camera-feed-plan.md](plans/camera-feed-plan.md) | implemented 2026-07-13 | XREAL RGB camera → Godot `CameraFeed` (full colour, needs 3DoF). |
| [input-plan.md](plans/input-plan.md) | implemented (Phase B open) | One Pro input: glasses keys + wear sensor (device-verified), event callback ABI, phone-as-3D-pointer (Phase C, 2026-07-14). State get/set (Phase B) not implemented. |
| [ar-features-plan.md](plans/ar-features-plan.md) | plane + anchor implemented; image/mesh RE'd | All 4 AR features' C ABI confirmed (codex RE). **Plane detection + spatial anchors ported** (`XrealSystem.poll_planes` / `poll_anchors` etc.; `nr_spatial_anchor.aar` vendored; on-device verify pending). Also adds `is_hmd_feature_supported` / `is_camera_supported` (the SDK's device gate — Air 2 Ultra has no RGB camera). Image/mesh signatures + struct layouts documented for follow-up. |
| [render-metrics-gdscript-plan.md](plans/render-metrics-gdscript-plan.md) | implemented 2026-07-16 | Exposes the SDK's Render Metrics (present FPS / dropped / early / latency) to GDScript via the `NRMetrics*` C API (`src/metrics.rs`), queried directly instead of reviving `UpdateMetrics`. |
| [hand-tracking-plan.md](plans/hand-tracking-plan.md) | implemented 2026-07-16 (Air 2 Ultra-verified) | Hand tracking → Godot `XRHandTracker` via the SDK's exported `UpdateHandPose`/`GetHandJointsPose` wrappers (`src/hand_tracking.rs`), enabled via `NativePerception::SetHandTrackingEnabled` + `input_source=3`; `demo/hand_visualizer.gd` draws world-locked joint spheres. **Hardware-gated to the Air 2 Ultra.** |
| [hotplug-session-recovery.md](plans/hotplug-session-recovery.md) | fix not implemented | Session never recovers when the glasses are plugged in after app start. |
| [background-render-plan.md](plans/background-render-plan.md) | Milestone 0 shipped | Keep the glasses rendering while the app is backgrounded (multi-resume). **Shipped via auto-enter Picture-in-Picture** (`XrealBridge.enableAutoEnterPiP`, no engine patch; device-verified — the submit counter keeps advancing past background). Milestone 1 (SurfaceView reparent + ~15-line `godot-lib` guard) is designed but unneeded unless a true full-hide background is later wanted. |
| [android-export-plugin-migration.md](plans/android-export-plugin-migration.md) | DONE | How the addon's `EditorExportPlugin` injects manifest entries and ships the `.aar`s (current export design). |

## archive/ — resolved or shelved investigation logs (kept as-is)

| Doc | Outcome |
|---|---|
| [codex-headlock-analysis.md](archive/codex-headlock-analysis.md) | Led to the head-lock fix: `UpdateDeviceState(updateType=1)` refreshes the compositor render pose. Solved 2026-07-13. |
| [roll-tracking-investigation.md](archive/roll-tracking-investigation.md) | Roll was missing from the session-manager `NrPose`; solved by switching the eye cameras to the display pose (`GetHeadPoseAtTime` 4×4). |
| [multiview-investigation.md](archive/multiview-investigation.md) | Handoff written while head-lock was unsolved (it is now). Multiview itself was shelved. |
| [codex-multiview-analysis.md](archive/codex-multiview-analysis.md) | Multiview swapchain registration analysis. Multiview shelved — Multipass is the only stereo mode. |
| [codex-righteye-analysis.md](archive/codex-righteye-analysis.md) | Root cause of the Multiview black right eye: the NR compositor (`libnr_api`) can't import a client `GL_TEXTURE_2D_ARRAY`. Confirmed the shelving. |
| [codex-stub-callbacks-analysis.md](archive/codex-stub-callbacks-analysis.md) | 2026-07-16: ruled out the stubbed Unity callbacks (`RegisterTextureProvider` / `GetPlatformData` / `RegisterDisplayProvider` / gfx device-event) as the Multiview layer-1 blocker — the cause is purely `libnr_api`'s 2D client-texture import. |
| [codex-floatingmanager-analysis.md](archive/codex-floatingmanager-analysis.md) | 2026-07-16: refuted "floating return button not feasible", implemented + device-verified it, then **removed it in favour of Picture-in-Picture** (the button returned to the app but didn't keep the glasses rendering). RE record retained. |
| [codex-background-render-analysis.md](archive/codex-background-render-analysis.md) | 2026-07-16: **why the glasses freeze when our app is backgrounded.** Root cause: our NR submit is gated by Godot's `process()` on the display-0 window, whose SurfaceView is destroyed on `onPause` (`EGL_BAD_SURFACE`). The reference Unity app keeps rendering via **SurfaceView reparent onto the resumed glasses-display Activity** + pause-suppression (not a Service — that hypothesis is disproven). Fix = port that (Godot Android-template patch); substantial. Corrects the old "multiResume keeps rendering after Home" over-claim. |
