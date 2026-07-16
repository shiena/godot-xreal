# Background rendering (multi-resume) — why the glasses freeze when our app is backgrounded

Date: 2026-07-16. Codename: codex.
Evidence base: the apktool smali decompilation of the reference XREAL Unity APK
(`tools/apk-unity/`), our render path (`src/node.rs`, `src/unity_plugin.rs`), our Android glue
(`addons/godot_xreal/android/`, `export_plugin.gd`), and on-device logcat (XREAL One Pro).

## TL;DR / Verdict

While our Godot app is backgrounded, the **glasses stop rendering** (freeze on the last frame) and
resume on return. This is a **Godot render-lifecycle + surface-lifetime** problem, not a
process-death problem — the process stays alive.

- **Root cause:** our glasses submit is driven synchronously from Godot's main loop `process()` on
  the phone's display-0 window; on HOME, `GodotFragment.onPause` fires and SurfaceFlinger destroys
  that window's `SurfaceView` → `eglSwapBuffers failed: EGL_BAD_SURFACE` → Godot's render thread and
  `process()` stop → our NR compositor submit is never called again → the compositor re-presents the
  last frame.
- **The reference Unity app avoids it not with a Service but with Activity topology + a SurfaceView
  reparent:** it keeps the Unity player unpaused in multiResume, keeps a second Activity resumed on
  the glasses display, and **reparents the Unity GL `SurfaceView` into that glasses-display Activity**
  so its EGL surface is never destroyed. The owner's "declare a Service" hypothesis is **disproven**
  — the only service in the reference is a screen-capture `MediaProjectionService`, unrelated to
  rendering.
- **Fix for us = Option B (+C):** reparent Godot's render `SurfaceView` onto the resumed
  glasses-display companion Activity + suppress Godot's renderer pause. This requires **patching the
  Godot Android template** and is substantial and somewhat fragile. Options A (foreground service),
  C-alone, and D-alone do **not** produce live background frames.
- **Memory correction:** the 2026-07-13 "multi-resume keeps rendering after Home (device-verified)"
  note is most likely an **over-claim** conflating "app/session stays alive" (native SDK/pose threads,
  which do keep running) with "glasses keep showing new frames" (the `process()`-gated compositor
  submit, which stops).

## 1. Root cause (precise)

Our submit cadence to the glasses is gated by two things that both live on display 0:

1. `XrealHeadTracker::process()` runs per frame and calls
   `RenderingServer::singleton().call_on_render_thread(run_render_thread_tick)`
   (`src/node.rs:81`, `:113-117`).
2. `run_render_thread_tick()` (`src/unity_plugin.rs:1139`) drives the NR compositor: first call runs
   `GfxThreadStart` (`CreateSwapchainEx` → allocates the eye GL textures, **requires a live EGL
   context**, `src/unity_plugin.rs:979-982`, `:1128-1139`); subsequent calls drive
   `PopulateNextFrameDesc` → `SubmitFrame`.

On HOME: `GodotFragment.onPause` → Android stops the display-0 Activity → SurfaceFlinger destroys the
window's `SurfaceView` (logcat: `dequeueBuffer: BufferQueue has no connected producer` →
`eglSwapBuffers failed: EGL_BAD_SURFACE`). With no surface, Godot's GL thread blocks, `onDrawFrame`
stops, `process()` stops being stepped, `run_render_thread_tick` is never called again. The NR
compositor keeps re-presenting the **last submitted frame** → the glasses freeze. The process stays
alive (companion Activity + native session threads keep running).

## 2. Reference Unity mechanism

### 2a. Activity / Service inventory (`tools/apk-unity/AndroidManifest.xml`)

All Activities run in **one process** (no `android:process` anywhere). Load-bearing facts: two
different `taskAffinity` values + `resizeableActivity`.

| Activity | line | launchMode | taskAffinity | notable |
|---|---|---|---|---|
| `NRXRActivity` | 48-55 | singleTask | **`xreal.unity`** | THE LAUNCHER (MAIN/LAUNCHER); thin bootstrap → `NRXRApp.init` |
| `UnityPlayerActivity` (ai.nreal) | 57-59 | singleTask | **`xreal.unity`** | hosts the Unity player on display 0; `unityplayer.UnityActivity` meta; no intent-filter |
| `NRFakeActivity` | 60 | singleTask | **default (package)** | runs on the **glasses display**; excludeFromRecents |
| `NRShadowActivity` | 56 | singleTask | default | noHistory; used by `moveToBackOnNR` |
| `com.unity3d.player.UnityPlayerActivity` | 38-43 | singleTask | default | stock, unused as launcher |

**Services:** exactly one — `ai.nreal.sdk.MediaProjectionService` (line 63),
`foregroundServiceType="mediaProjection"`. It is **screen-capture only** (`startForeground` reached
only from `onBind`, `MediaProjectionService.smali:142`, `:152-171`); not started at boot, does
nothing for rendering. **→ The "keep-alive Service" hypothesis is disproven as the render mechanism.**

App meta `nr_features=multiResume` (line 61). Permissions of note: `SYSTEM_ALERT_WINDOW`,
`REORDER_TASKS`, `ACTIVITY_EMBEDDING`, `FOREGROUND_SERVICE`.

### 2b. The mechanism (smali-traced): two resumed Activities on two displays, one shared player

1. **Bootstrap:** `NRXRActivity.onCreate` → `NRXRApp.init(this)` (`NRXRActivity.smali:59-63`);
   `init` decides `mMultiResumeMode` (`NRXRApp.smali:2432-2606`).
2. **Unity on the phone:** on `onActivityResumed(NRXRActivity)`, `NRXRApp` launches
   `UnityPlayerActivity` with `setLaunchDisplayId(0)` (`startUnityActivityInternal`,
   `NRXRApp.smali:1517`, `:1575-1587`).
3. **Fake activity on the glasses:** when the Unity activity resumes and
   `mMultiResumeMode && !mIsFakeActivityResumed`, `tryStartFakeActivity()`
   (`NRXRApp.smali:3340-3353`) → `startActivityOnDisplay(nrealDisplay, NRFakeActivity)` with
   `setLaunchDisplayId(glassesId)` + `FLAG_ACTIVITY_NEW_TASK` (`:1703-1758`, `:1409-1514`,
   `setLaunchDisplayId` at `:1500`). Its **different taskAffinity** puts it in its own task; Android
   allows one resumed activity **per display**, so both stay RESUMED.

**Why HOME doesn't stop rendering — three gates:**

- **(i) The phone Activity does not pause the player in multiResume.** `UnityPlayerActivity.onPause`
  (`:308-373`): if `mMultiResumeMode`, it **skips `mUnityPlayer.onPause()`** and only sends
  `UnitySendMessage("NRNativeMediator","SetMultiResumeBackground","true")` (`:355-362`).
  `onWindowFocusChanged` also does not forward focus loss to the player (`:639-676`). The Unity
  render thread is never told to pause.
- **(ii) The glasses Activity stays resumed.** `NRFakeActivity.onPause/onStop` pause Unity only when
  `PauseByFakeActivity()` is true **and** it is on the XREAL display (`NRFakeActivity.smali:148-279`,
  `:483-542`). HOME on display 0 does not fire the display-24 activity's `onPause`, so this path is
  never taken. The resumed display-24 activity also keeps the process at foreground importance.
- **(iii) The render surface is reparented to a live window.** On `onActivityStopped(UnityActivity)`
  → `refreshControlView()` (`NRXRApp.smali:3733-3735`); with
  `mIsFakeActivityResumed && !mIsUnityActivityResumed`, → `showControlView()` → **`bindFloatingView()`**
  (`:1185-1228`, `:1341-1357`). `bindFloatingView` pulls the Unity GL `SurfaceView` out of the phone
  activity (reflection `getSurfaceView()`/`mGlView`, `:442-530`) and `addView`s it into
  `mFakeActivity`'s `android.R.id.content` on the glasses display (`:567-595`). `unbindFloatingView`
  reverses it on return (`:1760-1861`). The `SurfaceView` now lives in a **resumed** window, so its
  EGL window-surface is never destroyed → Unity's `eglSwapBuffers` never hits `EGL_BAD_SURFACE`.

Net: the player is never paused (i, ii) and always has a valid surface (iii), so Unity's **own
free-running render thread** keeps producing and submitting eye frames while the phone is on HOME.
(Proven from manifest attrs + the cited smali; the "reparent keeps the EGL surface valid" step is a
strong inference from Android SurfaceView semantics.)

## 3. Why Godot stops but Unity doesn't

| | Reference Unity | Our Godot |
|---|---|---|
| Render driver | Unity's own native render thread (`UnityMain`), free-running, decoupled from the Java Activity | Godot main loop `process()` → `call_on_render_thread`; submit only while `process()` steps |
| What pauses rendering | Only an explicit `mUnityPlayer.onPause()`, which multiResume **suppresses** | Godot `onPause`/`onStop` stop stepping the loop; nothing suppresses it |
| Surface on background | SurfaceView **reparented** to the resumed glasses-display window → EGL stays valid | display-0 SurfaceView **destroyed** → `EGL_BAD_SURFACE` |
| Glasses-display Activity | `NRFakeActivity` **hosts the moved Unity SurfaceView** | `XrealCompanionActivity` shows a static `TextView`; does **not** host Godot's GL surface (`XrealCompanionActivity.java:26-33`) |

We replicate the *shape* — `nr_features=multiResume` (`export_plugin.gd:50`), a companion Activity on
the glasses display (`XrealBridge.java:186-223`), the floating return button — but only gate (ii)
partially, and gate (iii) **not at all**: our companion window does not host Godot's render surface,
and nothing suppresses Godot's main-loop pause.

## 4. Gap analysis — options ranked

Requirement for **live** background frames: Godot must keep rendering new eye textures = (a) the main
loop keeps stepping **and** (b) a valid EGL surface. Providing only one gives at best a frozen image.

- **Option B (+C) — reparent Godot's render `SurfaceView` onto the glasses-display companion Activity
  + suppress Godot's pause. RECOMMENDED; highest fidelity; hardest.** A direct port of
  `bindFloatingView`: on phone-Activity `onStop`, move Godot's `GodotRenderView` SurfaceView into the
  companion Activity's content view on display 24 (which stays resumed), move back on return; plus a
  patch so Godot does not tear down / pause its renderer on that transition.
  *Cost/risk:* requires patching the **Godot Android template** (`GodotRenderView`/`GodotFragment`/
  `GodotHost`); Godot assumes a single window owned by one Activity and does not support reparenting
  its surface across Activities/displays. Watch: `DisplayServer`/window bookkeeping + resolution (the
  glasses window metrics differ), input routing, and that the **EGL context survives** the move (our
  NR swapchain textures live in Godot's context — a reparent preserves it; a destroy/recreate would
  invalidate the swapchain and force a `GfxThreadStart` re-init).
- **Option C alone — patch Godot to not pause its render thread on `onPause`. INSUFFICIENT.** Easy,
  but the OS still removes the display-0 window from the compositor, so the SurfaceView is destroyed
  and you still get `EGL_BAD_SURFACE`. Only useful **with B** (which supplies an alternate surface).
- **Option D — drive submit from an independent Java/native thread. PARTIAL.** You can keep calling
  `SubmitFrame` off the `process()` loop, but (1) it needs an EGL context sharing Godot's GL objects,
  and (2) Godot stops rendering *new* eye images when its loop/surface stop — so you submit the same
  stale texture forever = a frozen image. Only worthwhile *after* B keeps Godot rendering.
- **Option A — foreground Service. LOWEST VALUE.** The process is already alive (the resumed
  companion keeps it foreground). A Service provides neither a render thread nor an EGL surface. The
  reference uses no such service. Skip.

**Alternative surface host (and its known failure):** hosting Godot's GL surface in a persistent
`TYPE_APPLICATION_OVERLAY` window (which survives HOME) was tried before and **crashed** (Godot GL
surface destruction / GLThread SIGSEGV — project memory). So the glasses-display Activity reparent
(Option B) is the safer surface host.

## 5. Verdict + how to decide the memory claim

**Achievable?** Yes in principle, but only via **Option B (+C)** — a Godot Android-template patch to
reparent the render SurfaceView onto the resumed glasses-display Activity and suppress the renderer
pause, mirroring `NRXRApp.bindFloatingView`. No service-based shortcut exists. It is substantial and
somewhat fragile.

**Reconciling the memory:** the "multi-resume keeps rendering after Home" note likely conflated
"app/session stays alive" with "glasses keep showing new frames." The cited signals (camera frame /
DISP euler updates) come from native SDK/session threads and pose polling that keep running because
the process stays alive; the **compositor submit** (which *is* `process()`-gated) would still have
stopped. **What decides it definitively:** while backgrounded, watch the NR compositor counters
(`src/metrics.rs` — `PresentFps` / dropped / submit count): still advancing → frames presenting;
flatline at `EGL_BAD_SURFACE` → rendering stopped. Or a visual check with an animating scene (does
the glasses image keep moving on HOME, or freeze on the last frame?).
