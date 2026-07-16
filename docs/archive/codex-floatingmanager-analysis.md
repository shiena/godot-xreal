# FloatingManager (multi-resume 復帰ボタン) feasibility analysis

Date: 2026-07-16. Codename: codex.
Evidence base: the **apktool smali decompilation of the reference XREAL Unity APK** vendored at
`tools/apk-unity/smali/ai/nreal/activitylife/`, cross-checked against the SDK package
(`com.xreal.xr/package/Runtime/Plugins/Android/nractivitylife*-release.aar` and the C# glue in
`Runtime/Scripts/…`), plus our own `addons/godot_xreal/android/` sources and `export_plugin.gd`.

## Verdict: FEASIBLE — the recorded "not feasible" conclusion is REFUTED on both grounds

The floating on-phone "return" button (tap to bring the glasses app back to the foreground while
multi-resume keeps it running) **is feasible from our non-Unity Godot app**. The entire
floating-button subsystem in the reference APK is **pure Android/Java with zero Unity references**,
and it lives in a **separate `TYPE_APPLICATION_OVERLAY` top-level window** that never touches the
app's GL `SurfaceView`. Both previously-recorded blockers are wrong as stated:

- **Blocker #1 — "a self-overlay disturbs Godot's GL surface" → refuted.** The reference floating
  view is a 200×200 `TYPE_APPLICATION_OVERLAY` (type `2038`) window added via
  `WindowManager.addView(view, params)` — its **own Surface**, composited independently by
  SurfaceFlinger. It is never inserted into the Activity content view and never touches the GL
  SurfaceView. Our earlier freeze/`GLThread SIGSEGV` came from *how our overlay was built* (into the
  content view / on the GL thread / focusable full-screen), not from overlays being fundamentally
  incompatible.
- **Blocker #2 — "NR `FloatingManager` throws `ClassNotFoundException`" → refuted as a packaging
  problem, not an intrinsic one.** The exception occurs only because we exclude the whole
  `nractivitylife` AAR, so `ai.nreal.activitylife.FloatingManager` is simply absent from our APK.
  `FloatingManager` itself has **0** Unity references and loads nothing reflectively; the Unity
  coupling lives entirely in `NRXRApp`/`NRXRActivity`/`UnityPlayerActivity`, which the floating
  classes do not require.

**Recommended path: reimplement from scratch** (~120 lines of standard Android Java in our existing
`addons/godot_xreal/android/src/` source set), using the reference smali as the literal blueprint.
We do **not** need to ship the AAR and do **not** need `FloatingManager`/`NRXRApp`. The two
permissions this needs (`SYSTEM_ALERT_WINDOW`, `REORDER_TASKS`) are **already injected** by
`export_plugin.gd`.

> ⚠️ **Superseded 2026-07-16: the floating button was implemented, device-verified, then REMOVED in
> favour of auto-enter Picture-in-Picture.** The button only solved *returning* to the app; it did
> **not** keep the glasses rendering while backgrounded (that turned out to be a Godot render-lifecycle
> issue — `docs/archive/codex-background-render-analysis.md`). PiP solves both (keeps rendering + its
> tile taps back), so `XrealFloatingReturnButton` was deleted and PiP shipped instead
> (`docs/plans/background-render-plan.md`). The analysis below stands as the RE record; the "did not
> materialise" crash and the `TYPE_APPLICATION_OVERLAY` feasibility were all confirmed on device.
>
> ✅ **Device-verified 2026-07-16** (XREAL One Pro), while it existed. Implemented as
> `XrealFloatingReturnButton` (path (b)/(c) below). Confirmed on hardware: the button shows on the phone as a
> `TYPE_APPLICATION_OVERLAY` window (`dumpsys` `mAttrs={(x,y)(200x200) ty=APPLICATION_OVERLAY}`),
> **tap returns the app** (`getLaunchIntentForPackage` + `setLaunchDisplayId(0)`), **drag-to-move +
> edge-snap works**, and it is **gated on `nr_features=multiResume`**. No crash / no `SIGSEGV` — the
> "self-overlay → GLThread SIGSEGV" fear did not materialise. Two on-device notes:
> - **Hidden over Settings / permission screens** (`mForceHideNonSystemOverlayWindow=true`) — Android's
>   anti-tapjacking policy for all non-system overlays; unavoidable, the reference app has it too. The
>   window reappears over the launcher and normal apps.
> - **Open issue (separate):** the glasses stop rendering while the app is backgrounded and resume on
>   return. logcat shows this is Godot pausing its render loop on the main Activity's `onPause`
>   (`Godot: OnPause` → main SurfaceView `BufferQueue ... no connected producer` → `eglSwapBuffers
>   failed: EGL_BAD_SURFACE`), which precedes and is independent of the overlay (the overlay is added
>   ~0.5 s *after*). So it is NOT caused by the button; it questions whether multi-resume truly keeps
>   rendering in the background at all. Being investigated separately (how the reference declares its
>   Activities/Services to keep the render alive).
>
> Original RE-based verdict (below) is retained; it proved correct on device.

## Class graph / API surface

Evidence: `FloatingManager.smali` (+`$1`), `IFloatingViewProxy.smali`, `IFloatingManagerCallback.smali`,
`FloatingRootView.smali` (+`$TouchHandler`), `NRDefaultFloatingViewProxy.smali` (+`$CustomTouchHandler`).

**`FloatingManager`** — thin lifecycle coordinator (singleton):
- `getInstance()` (@91) lazily constructs; private `<init>` (@41).
- `init(Context)` (@167) stores context and grabs `WindowManager` — but **never uses it itself**; the
  proxy does all window work.
- `setFloatingViewProxy(IFloatingViewProxy)` (@216): on the UI thread (`FloatingManager$1.run()`)
  destroys any existing proxy view, swaps in the new proxy, re-`show()`s if it was showing.
- `show()` (@244): if a proxy is set and no view exists yet → `proxy.CreateFloatingView()`; then
  `proxy.Show()`; fires `mCallback.onFloatingViewShown()`.
- `dismiss()` (@128): `proxy.Hide()`; fires `onFloatingViewDismissed()`.
- `onClickFloatingView()` (@194): fires `mCallback.onFloatingViewClicked()`.
- `setNRXRAppCallback(IFloatingManagerCallback)` (@113, static): sets `mCallback`.

**`IFloatingViewProxy`** — the seam: `CreateFloatingView() : View`, `Show()`, `Hide()`,
`DestroyFloatingView()`. `FloatingManager` owns *lifecycle*; the proxy owns *the actual window*.

**`IFloatingManagerCallback`** — `onFloatingViewClicked/Shown/Dismissed()`. A pure notification hook
(Unity uses it for C# callbacks); not needed for function.

**`NRDefaultFloatingViewProxy`** — the concrete window implementation and real blueprint:
- ctor `(Context)` (@52): stores context, gets `WindowManager`.
- `CreateFloatingView()` (@279): reads screen size (`getDefaultDisplay().getMetrics`); inflates
  `R.layout.view_control` → a `FloatingRootView`; sets `iv_main` to the app icon
  (`generateIconImage` @139 via `PackageManager.getApplicationIcon`); installs the touch handler;
  builds `WindowManager.LayoutParams(200, 200, type=2038 TYPE_APPLICATION_OVERLAY,
  flags=0x20008, format=-3 TRANSLUCENT)` (@371), gravity `TOP|START`, `x = screenWidth-200, y = 0`;
  `mWindowManager.addView(floatingView, params)` (@415); starts hidden (`setVisibility(GONE)`).
- `Show()` (@485) / `Hide()` (@453): toggle `View.VISIBLE`/`GONE`.
- `DestroyFloatingView()` (@431): `mWindowManager.removeView(...)`.

**`FloatingRootView`** — trivial `FrameLayout` that routes `dispatchTouchEvent` to a `TouchHandler`.

**`NRDefaultFloatingViewProxy$CustomTouchHandler`** — interaction logic:
- `handleTouch()` (@106): ACTION_DOWN records pos/time; ACTION_MOVE drags past `touchSlop` and
  edge-snaps left/right via `WindowManager.updateViewLayout`; ACTION_UP with `!isMoving` and elapsed
  `< 300 ms` (`0x12c`) → `fullscreen()` (a genuine tap).
- `fullscreen()` (@71): `NRXRApp.getInstance().startUnityActivity(new Intent())` **then**
  `FloatingManager.getInstance().onClickFloatingView()`.

**Show/dismiss triggers** (`NRXRApp.smali`): `refreshControlView()` (@1185) shows the button when
`mMultiResumeMode && nrealDisplay != null && mIsFakeActivityResumed && !mIsUnityActivityResumed`
→ `showControlView()` (@1341) → `FloatingManager.show()`; the inverse → `dismiss()`. These are
ordinary Activity-lifecycle booleans we already have equivalents for.

**How clicking refocuses the app** (`NRXRApp.smali`) — no Unity/il2cpp API is involved:
- `startUnityActivity(Intent)` (@4148): if multiResume and `!Settings.canDrawOverlays` → request the
  overlay permission (`NRXRActivity.requestDrawOverlay` @68, `ACTION_MANAGE_OVERLAY_PERMISSION`) and
  stash the pending intent; else → `startUnityActivityInternal`.
- `startUnityActivityInternal(Intent)` (@1517): `setComponent(pkg, mMainActivityName)`,
  `addFlags(FLAG_ACTIVITY_NEW_TASK=0x10000000)`, `putExtra("multiResumeMode", true)`,
  `ActivityOptions.makeBasic().setLaunchDisplayId(0)` (0 = phone), then a plain
  `Activity.startActivity(intent, options)` (@1587).
- Parallel path (@3882): `ActivityManager.moveTaskToFront(taskId, MOVE_TASK_NO_USER_ACTION=2)` — an
  equally valid refocus primitive.

## Unity-coupling analysis

Grep for `unity3d|UnityPlayer|il2cpp` over each class's smali:

| Class | Unity refs |
|---|---|
| `FloatingManager`, `FloatingManager$1` | **0** |
| `IFloatingViewProxy`, `IFloatingManagerCallback` | **0** |
| `FloatingRootView`, `FloatingRootView$TouchHandler` | **0** |
| `NRDefaultFloatingViewProxy`, `…$CustomTouchHandler` | **0** |
| `NRXRApp` | 13 |

`FloatingManager` and the entire floating view/proxy set are **Unity-free** (proven, not inferred):
they touch only `Context`, `Activity`, `WindowManager`, `View`, `WeakReference`, `Log`.

The **only** Unity coupling in the whole floating flow is the single call inside
`CustomTouchHandler.fullscreen()` → `NRXRApp.getInstance().startUnityActivity(...)`
(`…$CustomTouchHandler.smali:84-92`). `NRXRApp` is the Unity-bound class (e.g. `bindFloatingView()`
reflectively calls `UnityPlayer.getSurfaceView()` at `NRXRApp.smali:442-465`). So the **stock
`NRDefaultFloatingViewProxy` is unusable off-the-shelf** for us (its tap handler routes into an
uninitialized, Unity-bound `NRXRApp`) — but that coupling is one line, trivially replaced by our own
`startActivity`.

How the reference actually wires it (C# side, confirming nothing exotic happens in Java):
`XREALMultiResumeMediator.cs:63-66` makes three JNI calls at load —
`FloatingManager.setNRXRAppCallback(listener)`, then
`XREALFloatingViewProvider.RegisterFloatViewProxy(new XREALDefaultFloatingViewProxy())` →
`FloatingManager.getInstance().setFloatingViewProxy(proxy)`. `XREALDefaultFloatingViewProxy`
(`XREALFloatingViewProvider.cs:28-33`) is just
`new AndroidJavaObject("ai.nreal.activitylife.NRDefaultFloatingViewProxy", UnityPlayer.currentActivity)`
— its **sole** Unity dependency is obtaining the current `Activity`, which Godot already has
(`XrealBridge.register(Activity)`). Note: **nothing in the smali calls `setFloatingViewProxy` or
constructs `NRDefaultFloatingViewProxy`** (grep confirms) — that wiring is done from C#. So we replace
~10 lines of C# glue with equivalent Java/Rust and the Unity dependency evaporates.

## Overlay-window mechanism — does it touch the GL surface? (No.)

The crux against blocker #1; the smali is unambiguous:

- `NRDefaultFloatingViewProxy.CreateFloatingView()` builds `LayoutParams` with **type `0x7f6` = 2038
  = `TYPE_APPLICATION_OVERLAY`** (@371-385) and attaches via **`WindowManager.addView(...)`** (@415).
  `TYPE_APPLICATION_OVERLAY` is a *separate top-level system window with its own Surface*, composited
  by SurfaceFlinger independently of the app's SurfaceView — it cannot corrupt the Godot GL surface,
  by construction.
- Flags `0x20008` = `FLAG_NOT_FOCUSABLE | FLAG_ALT_FOCUSABLE_IM` → never takes window focus (can't
  steal input from the GL window), yet omits `FLAG_NOT_TOUCHABLE` so the 200×200 icon still receives
  taps. Format `-3` (`TRANSLUCENT`). It's a tiny corner window (`x = screenWidth-200, y = 0`), not a
  full-screen cover.
- The hierarchy is a bare `FloatingRootView` (`FrameLayout`) inflated standalone
  (`LayoutInflater.inflate(view_control, null)`); it is **never** added to the Activity content view.
  `WindowManager.removeView` tears it down cleanly (`DestroyFloatingView` @442).

Contrast: the Unity-GL-surface manipulation in the reference lives in a **different** method —
`NRXRApp.bindFloatingView()` (`NRXRApp.smali:360`) reparents the Unity `SurfaceView` and the
`android.R.id.content` moving-view when toggling phone↔glasses. That is multi-resume view-reparenting,
**not** the overlay button, and it is Unity-specific. Our earlier freeze most likely reproduced *that*
class of mistake (touching the content view / GL surface), or built the overlay as a
focusable/full-screen window on the wrong thread — none of which the reference's floating button does.

## Which AAR / ClassNotFound root cause

- The floating classes ship in **`nractivitylife-release.aar`** (and a Unity-6 variant
  `nractivitylife_6-release.aar`) under `com.xreal.xr/package/Runtime/Plugins/Android/`. Each
  `classes.jar` contains the whole `ai.nreal.activitylife.*` set — `FloatingManager*`,
  `IFloatingViewProxy`, `IFloatingManagerCallback`, `FloatingRootView*`, `NRDefaultFloatingViewProxy*`
  — **and** `NRXRApp*`, `NRXRActivity`, (`UnityPlayerActivity` in the non-`_6` one).
- **The AAR is not splittable.** The floating classes and the Unity-bound
  `NRXRApp`/`NRXRActivity`/`UnityPlayerActivity` are in one `classes.jar`, and the AAR's own manifest
  declares `NRXRActivity` as a `MAIN`/`LAUNCHER` activity plus `UnityPlayerActivity` with
  `unityplayer.UnityActivity` meta-data. Merging it would inject an unwanted second launcher and
  Unity activities. There is no lighter AAR carrying only the floating classes.
- **Root cause of the `ClassNotFoundException`:** not FloatingManager loading something reflectively
  (it loads nothing) and not an intrinsic Unity dependency (0 refs) — simply that we **don't ship
  `nractivitylife`**, so `ai.nreal.activitylife.FloatingManager` is absent and any
  `Class.forName`/JNI `FindClass` fails. It's a consequence of the (correct) decision to exclude the
  AAR because of `NRXRActivity`/`UnityPlayerActivity`/`UnityPlayer`. The fix is **not** to ship the
  AAR but to provide the tiny, Unity-free classes ourselves.

## Recommended implementation path

Ratings:
- **(a) Ship the AAR + supply our own `IFloatingViewProxy`** — *feasible but not recommended.*
  `FloatingManager` would work, but the AAR drags in `NRXRApp`/`NRXRActivity`/`UnityPlayerActivity`
  and a LAUNCHER activity via manifest merge; you'd need `tools:node="remove"` surgery and would still
  have to write your own proxy (the stock one calls `NRXRApp`). More risk than writing ~120 lines.
- **(b) Reimplement from scratch as a `TYPE_APPLICATION_OVERLAY`** — **recommended, fully feasible.**
  No AAR, no Unity, no manifest-merge conflicts. Blueprint = the smali above.
- **(c) Lighter still** — you don't even need a `FloatingManager` indirection or the
  `IFloatingViewProxy` interface: one small Java class owning the overlay + touch handler + refocus is
  enough. This is what to build.

Concrete steps for (b)/(c):

1. **New Java class** `com.godot.game.XrealFloatingReturnButton` in
   `addons/godot_xreal/android/src/com/godot/game/` (same source set as `XrealBridge.java`, already
   shipped by `export_plugin.gd`). Port from smali:
   - Constructor takes the **phone-side `Activity`** (the `GodotApp`/main activity registered via
     `XrealBridge.register` — **not** `XrealCompanionActivity`, which lives on the glasses display).
   - `show()`: if not added, build the view (an `ImageView` set to
     `PackageManager.getApplicationIcon(getPackageName())`, mirroring `generateIconImage`, or any
     drawable), then `WindowManager.addView(view, params)` with params copied verbatim from
     `NRDefaultFloatingViewProxy.smali:371-415`:
     `LayoutParams(200, 200, TYPE_APPLICATION_OVERLAY, FLAG_NOT_FOCUSABLE|FLAG_ALT_FOCUSABLE_IM,
     PixelFormat.TRANSLUCENT)`, gravity `TOP|START`, `x=screenWidth-200, y=0`. Guard with
     `Settings.canDrawOverlays(activity)`.
   - `hide()`/`destroy()`: `setVisibility` / `removeView`.
   - Touch handler: port `CustomTouchHandler.handleTouch` (drag via `updateViewLayout`, edge-snap,
     tap = ACTION_UP with `!isMoving && elapsed < 300 ms`). On tap → step 2 (not `NRXRApp`).
2. **Refocus action** (replaces `NRXRApp.startUnityActivity`): plain
   ```java
   Intent i = new Intent();
   i.setComponent(new ComponentName(pkg, "com.godot.game.GodotApp")); // our launcher
   i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
   ActivityOptions o = ActivityOptions.makeBasic();
   o.setLaunchDisplayId(Display.DEFAULT_DISPLAY); // 0 = phone
   activity.startActivity(i, o.toBundle());
   ```
   or equivalently `ActivityManager.moveTaskToFront(taskId, MOVE_TASK_NO_USER_ACTION)`. Prefer
   `startActivity` to the launcher component — it matches `startUnityActivityInternal` exactly and
   needs no cached task id.
3. **Permission gate.** `SYSTEM_ALERT_WINDOW` (already injected, `export_plugin.gd:38`) +
   `REORDER_TASKS` (line 39, covers `moveTaskToFront`). Add a first-run request mirroring
   `NRXRActivity.requestDrawOverlay()` (`Intent(ACTION_MANAGE_OVERLAY_PERMISSION, Uri "package:"+pkg)`
   + `startActivityForResult`), gated on `Settings.canDrawOverlays`.
4. **Wire triggers into our lifecycle.** We already detect glasses connect/disconnect and run a
   companion Activity on the glasses display (`XrealBridge`). Add: when our main (phone) Activity goes
   to background while the glasses session is live (`onStop`/`onPause` with a live glasses display),
   `show()`; when it returns (`onResume`), `hide()`. This reproduces the reference's
   `mIsFakeActivityResumed && !mIsUnityActivityResumed` condition (`NRXRApp.smali:1210-1219`) with our
   own booleans. `XrealBridge.register(Activity)` already holds the Activity, so it's the natural home
   (expose two `#[func]`s or static calls Godot can drive).
5. **No manifest activity additions** beyond what exists; no AAR; no `FloatingManager`/
   `IFloatingViewProxy` classes required. (If you want SDK-name parity you can also port the tiny,
   Unity-free `FloatingManager` + `IFloatingViewProxy`, but it adds indirection with no benefit.)

## Residual on-device risks (verify — not blockers)

- Whether an OEM restricts `TYPE_APPLICATION_OVERLAY` while another app is foreground. The reference
  relies on exactly this working on the same Beam/phone hardware, so it should; `Settings.canDrawOverlays`
  is the correct gate (the reference checks it too).
- Confirm the overlay lands on **display 0** (use the phone-side Activity's `WindowManager`, step 1),
  so the button shows on the phone, not the glasses.
- The 300 ms tap window and `touchSlop` are copied from the reference and are fine as-is.

## Bottom line

The reference implements the return button as a standard, Unity-free `TYPE_APPLICATION_OVERLAY` window
plus a plain `startActivity` refocus. Every Unity touch-point (getting the Activity, the
`NRXRApp.startUnityActivity` call, the C# proxy wrapper) has a one-to-one non-Unity equivalent we
already possess. The prior "not feasible" verdict rested on two implementation-specific failures the
reference smali shows are avoidable. Feasible via path (b)/(c); no AAR, no new manifest activities.
