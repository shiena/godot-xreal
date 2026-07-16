# Background rendering (multi-resume) — Option B implementation plan

> **Shipped: Milestone 0 (Picture-in-Picture), device-verified 2026-07-16.** The glasses now keep
> rendering while the app is backgrounded, via auto-enter PiP — **zero engine patch**. Implemented as
> `XrealBridge.enableAutoEnterPiP(activity)` (a direct `Activity.setPictureInPictureParams` with
> `setAutoEnterEnabled(true)` + a 16:9 aspect ratio, gated to display 0), driven from
> `demo/main.gd`. This bypasses Godot's own `isPiPEnabled()` gate, so **no `GodotApp.java` /
> `godot-lib` edit is needed** (robust across template reinstalls). The manifest already carries
> `android:supportsPictureInPicture="true"` on the launcher with PiP-adequate `configChanges`.
> Device-verified: the `frame_tick` submit counter, which **froze** on background before PiP, keeps
> advancing (`#1500→#2100`) in PiP; `mIsInPictureInPictureMode=true`; return exits PiP and resumes
> fullscreen. UX trade-off (accepted): a small tile remains on the phone (not a full HOME hide). The
> floating return button was removed in favour of the PiP tile.
>
> **Milestone 1 (SurfaceView reparent, below) is NOT implemented** and is only needed if a *true*
> full-hide background (no tile) is later required; it needs the ~15-line `godot-lib` guard.

Status: **Milestone 0 shipped; Milestone 1 designed, not implemented.** Follows
`docs/archive/codex-background-render-analysis.md` (root cause + why the reference Unity app keeps
rendering). Evidence: decompiled `godot-lib.template_debug.aar` (`4.7.1.stable`, jadx + `javap` on
`classes.jar`), the reference APK smali, and our sources. "Proven" = from bytecode/smali; "inferred"
= AOSP `GLSurfaceView` semantics.

## Lead finding: there is no app-level reparent "trick" — Unity just has a detach-tolerant render thread

We hypothesised that `NRXRApp.bindFloatingView` uses an engine-agnostic app-level trick we could port.
**The smali refutes this.** `bindFloatingView` (`NRXRApp.smali:360-618`) is a **bare
`removeView` + `addView` of the same View instance** (`android.R.id.content`'s subtree,
`:414-422`, `:567-595`), with:
- **no `SurfaceHolder.Callback` interception, no `setZOrderOnTop/MediaOverlay`, no `Presentation`, no
  `WindowManager.addView`** (whole-class grep finds only `ViewGroup.removeView/addView`), and
- **zero `UnityPlayer` method calls** (`grep 'Lcom/unity3d/player/UnityPlayer;->'` → empty) — the
  `getSurfaceView()`/`mGlView` reflection (`:448-530`) only **captures references** for
  `unbindFloatingView` (`:1760-1861`) to restore the tree; it never swaps or re-associates a surface.

So what makes the reference work is a **property of the Unity engine**: its native gfx/render thread
is **decoupled from the Android `View` lifecycle** (a `View` detach does not kill it; it only
recreates the EGL *window surface* on `surfaceDestroyed`→`surfaceCreated`, keeping its context). A
straight port of the same `removeView`/`addView` to Godot does **not** freeze — it **destroys the
engine**, because Godot ties its render-thread + engine lifetime to `onDetachedFromWindow`:

- `GLSurfaceView.onDetachedFromWindow()` → `mGLThread.requestExitAndWait()` (`GLSurfaceView.java:254-261`).
- thread exit → `stopEglSurfaceLocked()` + `stopEglContextLocked()` **unconditionally** (the
  `mPreserveEGLContextOnPause` guard applies only to the *pause* path, not the *exit* path) → EGL
  context destroyed.
- the exiting thread also calls `Renderer.onRenderThreadExiting()` → `GodotLib.ondestroy()`
  (`GodotRenderer.java:29-33`) → **the whole Godot engine is torn down.**

There is no app-side interception point: `onDetachedFromWindow` is dispatched by the framework on the
package-private `GodotGLRenderView`, and `requestExitAndWait` is `protected final`. So closing this
one gap — *make Godot's render thread survive a View detach, like Unity's* — is the entire task, and
it lives inside `godot-lib`.

## Feasibility verdict

- **The surface-reparent cannot be done purely app-side; a small `godot-lib` change is unavoidable.**
  Everything else (the reparent move, pause suppression, the companion host) is app-side.
- **The change is minimal (~15 lines) and lives in plain-Java classes** (`gl/GLSurfaceView.java` +
  `GodotGLRenderView.java`), **not** the Kotlin core. It can be applied by a proper Godot 4.7.1 lib
  rebuild **or**, for a fast prototype, by recompiling just those two classes against the AAR's
  `classes.jar` + `android-35/android.jar` and swapping the `.class` entries into `classes.jar`. **The
  engine `.so` is untouched.**
- **A zero-patch partial path exists (Milestone 0): Picture-in-Picture.** Godot already ships PiP and
  `GodotApp.isPiPEnabled()` returns `true` (`GodotApp.java:101-104`). In PiP the activity is
  *paused but visible* → `onStop` is **not** called → `pauseGLThread()` not called → the Surface
  stays alive → the GL thread keeps `onDrawFrame`→`GodotLib.step()`→our `process()`→submit. Zero code
  change beyond enabling auto-enter. Limitation vs. the reference: a small PiP tile stays visible on
  the phone; it is not a true HOME-to-launcher background.

Recommendation: **PiP first** (validation + possibly acceptable UX), then the minimal `GLSurfaceView`
detach-decouple patch for true reference-parity background rendering.

## Godot 4.7 GL render-view + lifecycle map (proven from the AAR)

View tree (`Godot.onInitRenderView`, `Godot.java:858-1096`):
```
GodotApp (extends GodotActivity)
 └─ R.id.godot_fragment_container
     └─ GodotFragment (onCreateView → containerLayout)
         └─ containerLayout : FrameLayout            (public getContainerLayout$lib_templateDebug, Godot.java:525)
             ├─ GodotEditText
             ├─ GodotGLRenderView (extends GLSurfaceView extends SurfaceView)   ← THE GL SurfaceView
             └─ plugin views
```
- GL SurfaceView class: **`org.godotengine.godot.GodotGLRenderView`** (package-private). Reachable
  app-side only as `GLSurfaceView`/`SurfaceView` via `getGodot().getRenderView().getView()`
  (`getRenderView()` public `Godot.java:534`; `getView()` returns `this`, `GodotGLRenderView.java:44`).
  Project is `gl_compatibility` (`project.godot:32-33`) → GL path, not Vulkan.
- Single render/main thread: `GodotRenderer.onDrawFrame()` → `GodotLib.step()` (`GodotRenderer.java:16-27`).
  So `XrealHeadTracker::process()` runs on the GL thread, and `call_on_render_thread(run_render_thread_tick)`
  (`src/node.rs:113-117`) runs in that step. If `onDrawFrame` stops, our submit stops.

Lifecycle → renderer (proven):

| Fragment cb | Godot host | RenderView effect |
|---|---|---|
| `onStart` | `onActivityStarted` | `resumeGLThread()` |
| `onResume` | `onActivityResumed` | `focusin` (thread not touched) |
| `onPause` | `onActivityPaused` | `focusout` + `onRendererPaused()`; **thread NOT paused** |
| `onStop` | `onActivityStopped` | **`pauseGLThread()`** ← hard stop |
| `onDestroy` | `blockingExitRenderer` | `requestExitAndWait` → `GodotLib.ondestroy()` |

`setPreserveEGLContextOnPause(true)` is set (`GodotGLRenderView.java:178`) — so the *pause* path keeps
the context. The gap is **thread lifetime on detach**, not context preservation.

**Root cause (matches the device-confirmed analysis doc):** HOME → `onStop`→`pauseGLThread()` **and**
SurfaceFlinger destroys the Surface (`mHasSurface=false`) → `readyToDraw()` false → `onDrawFrame`
stops → `GodotLib.step()`/`process()` stop → submit stops. Context + thread survive (no detach) →
recovers on return.

## The implementation

### App-side (new code in `GodotApp` + `XrealBridge`; plain casts, no reflection)

1. On background-with-live-glasses: `view = getGodot().getContainerLayout$lib_templateDebug()` (public)
   — or mirror the reference and move `findViewById(android.R.id.content)`'s child.
2. `((GLSurfaceView) getGodot().getRenderView().getView()).setKeepRenderingWhenDetached(true)` (new
   setter, see patch) **before** the move.
3. `oldParent.removeView(view)`; `companionContentView.addView(view)` where
   `companionContentView = companionActivity.findViewById(android.R.id.content)`. Same instance.
4. On foreground: reverse (restore to the phone parent), then `setKeepRenderingWhenDetached(false)` —
   exactly like `unbindFloatingView`.

Companion host: reuse the already-running glasses-display activity. Note
`XrealBridge.startCompanionOnXrealDisplayIfNeeded` currently launches **`NRFakeActivity`**
(`XrealBridge.java:202-222`), not `XrealCompanionActivity`; either can host — it needs a reachable
`android.R.id.content` and must stay **RESUMED** (Android allows one resumed activity per display).
Do **NOT** host on a `TYPE_APPLICATION_OVERLAY` (that is what crashed before — the same
detach→exit→`ondestroy` path).

### Engine-side (the unavoidable minimum — plain-Java `godot-lib`)

One flag, reused for detach-survival and pause-suppression:
- `gl/GLSurfaceView.java`: add `public void setKeepRenderingWhenDetached(boolean)`; guard
  `onDetachedFromWindow()`: `if (mGLThread != null && !mKeepRendering) mGLThread.requestExitAndWait();`
  and keep `mDetached=false` when `mKeepRendering` (so `onAttachedToWindow` doesn't spawn a **second**
  GLThread — two threads on one context = SIGSEGV).
- `GodotGLRenderView.java`: `onActivityStopped()` → `if (!mKeepRendering) pauseGLThread();`; optionally
  guard the `GodotLib.focusout()` in `onActivityPaused()`.
- Apply via (a) a Godot 4.7.1-stable lib rebuild, or (b) recompile just those two `.java` against the
  AAR `classes.jar` + `android-35/android.jar` and replace the `.class` entries. Native `.so` untouched.

`GodotLib.onRendererPaused()` is native; whether it throttles `step()` is **unverified** — watch it in
Milestone 1 and guard `renderer.onActivityPaused()` too if needed.

## EGL / swapchain preservation (the crux)

Our NR swapchain eye textures are allocated by `GfxThreadStart`→`CreateSwapchainEx` in Godot's EGL
context on Godot's render thread (`src/unity_plugin.rs:979-982`, `:1128-1139`), gated once by
`GFX_THREAD_STARTED` (`:352`, `:1140`). They survive **iff Godot's EGL context survives.**
- With the detach-guard, a cross-window reparent = `surfaceDestroyed`→`surfaceCreated` with the
  **thread alive + `mPreserveEGLContextOnPause=true`** ⇒ context preserved, only the EGL window
  surface recreated on the new holder (`EglHelper.createSurface`, `GLSurfaceView.java:448-478`). **Our
  swapchain textures are NOT lost; no `GfxThreadStart` re-init.** (Proven: preserve flag + guardedRun
  preserve branch; inferred: create/makeCurrent on the new holder.)
- Eye render is size-independent of the on-screen surface: SubViewports are fixed `1968×1134` FBOs
  (`src/node.rs:16-17`); the on-screen SurfaceView only needs to keep `readyToDraw()` true.
- **Contingency if context is ever lost:** reset `GFX_THREAD_STARTED=false` + null the gfx provider so
  the next tick re-runs `GfxThreadStart`; Godot's own `onSurfaceCreated`→`GodotLib.newcontext()`
  rebuilds engine GL state (sequence Godot's `newcontext` before our re-init). Heavier/racier —
  fallback only.

## Display / input notes

- The companion window's size drives `surfaceChanged`→`GodotLib.resize()` on Godot's *main* viewport —
  harmless to the glasses output (eye FBOs fixed; the glasses image comes from `SubmitFrame`, not this
  window). No `DisplayServer` assert expected (normal holder destroy/recreate, not a window swap).
- Input: backgrounded SurfaceView is on the glasses display (no touch); glasses key/wear events flow
  through the native callback path (`src/node.rs` `poll_hardware_events`), independent of focus. Keep
  `XrealFloatingReturnButton` as the phone-side refocus affordance.

## Phased plan + risks

- **Milestone 0 (zero patch — proves the thesis):** enable PiP auto-enter (`updatePiPParams(true,…)`
  public, `GodotActivity.java:505`; `onUserLeaveHint`→`enterPiPMode` already wired). Background →
  confirm `[xreal] frame_tick #N … submit=` (`src/unity_plugin.rs:1353`) keeps advancing + `DISP euler`
  keeps updating past background. Validates "GL-thread-alive + surface-alive ⇒ live glasses".
- **Milestone 1 (the patch + reparent):** land the guard; on background set the flag +
  `removeView`(container)→`addView` into the resumed companion. Success = `frame_tick` advances with
  the phone on the launcher, swapchain **not** re-inited (no 2nd `GfxThreadStart`), no `EGL_BAD_SURFACE`;
  on return, restore view + input.
- **Milestone 2 (hardening):** order vs. the floating button, glasses hot-unplug mid-background, pose
  continuity, confirm `onRendererPaused` doesn't throttle.

Failure modes: engine teardown if the flag isn't set before `removeView`; a **second GLThread** if
`onAttachedToWindow` sees `mDetached=true` (→ SIGSEGV); context loss on cross-display reparent (→ reset
+ re-init, or prefer a phone-side stay-resumed host); `EGL_BAD_SURFACE` persisting if the companion
isn't actually RESUMED at the move; the prior overlay-host GLThread SIGSEGV (guard removes the cause —
keep the companion, not an overlay, as host); PiP not entered on some launchers (Milestone 0 only, no
regression).

**Bottom line:** the reparent is app-trivial; the only thing Godot lacks is Unity's detach-tolerant
render thread. A ~15-line plain-Java `godot-lib` guard supplies it (no full engine build). With it, an
app-side `removeView`/`addView` onto the resumed companion gives reference-parity live background
rendering with the swapchain preserved. PiP is the zero-patch validation/fallback available today.
