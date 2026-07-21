# Startup latency: phone screen → glasses rendering

Status: **two fixes shipped (`750a5e7` + the input-source default), one lead open.** Phone-to-glasses
went from **4.42 s to ~2.9 s**; process-start-to-glasses from **5.39 s to ~3.9 s**, against the
reference Unity app's 2.76 s. Measured 2026-07-22 on an X4000 (Beam Pro)
+ XREAL One Pro, cold start via `adb shell am start`, against the reference Unity app
(`com.kadinche.layeredclient.xreal`, Unity 6000.0.77f1, XREAL SDK 3.1.0) measured the same way.

## The symptom

The glasses start rendering noticeably after the phone screen does. In the Unity app they come up
together; in ours the phone is up first and the glasses follow a few seconds later.

## Baseline

Marker for "glasses rendering starts" = `GfxThreadStart` (ours) / `[XR] [DisplayManager]
GfxThreadStart` (Unity). Marker for "phone screen" = the renderer coming up (`OpenGL API` /
Unity's engine banner).

| | Unity | ours (before) |
|---|---|---|
| process start | 0 | 0 |
| phone screen | +1.42 s | **+0.97 s** |
| NRFakeActivity displayed | +1.42 s | +2.56 s |
| **glasses rendering** | **+2.76 s** | **+5.39 s** |
| **phone → glasses** | **1.34 s** | **4.42 s** |

Note the shape of it: **our phone screen is the faster of the two.** Unity's phone content and
glasses appear together because Unity's phone side is also slow. Ours races ahead and then waits,
which is exactly what makes the wait visible.

Our 4.42 s broke down as:

| segment | time | whose |
|---|---|---|
| renderer up → first `[xreal]` log (Godot loads the main scene) | 1.45 s | Godot / ours |
| bootstrap retry backoff | 1.07 s | **ours** |
| provider registration → display start callback | 0.40 s | ours / SDK |
| SDK input start (device state 0→2→3→4) | 1.39 s | SDK |
| rig creation + `GfxThreadStart` | 0.11 s | ours |

## Fixed: the retry backoff (`750a5e7`)

`session::shared()` is called every frame; when a bootstrap attempt found the Activity unpublished
it refused to look again for 60 calls — a full second at 60 fps. Measured, the Activity appeared
**145 ms** after that first failed probe, so ~0.93 s was spent doing nothing.

Now the backoff doubles from one frame (1, 2, 4 … capped at the old 60) and resets on success.

| | before | after (3 runs) |
|---|---|---|
| gap between attempts | 1070 ms | 53 / 54 / 101 ms |
| process start → `GfxThreadStart` | 5.39 s | 4.09 / 4.05 / 4.59 s |
| phone → glasses | 4.42 s | ~3.32 s |

## Tried and rejected: publishing the Activity from an autoload

The Activity reaches the native side through `XrealBridge.register(activity)`, which the demo calls
from `demo/main.gd`'s `_ready()` — i.e. after the whole main scene is built. `XrealBridge.java`'s own
comment says to call it "once, early (from `GodotApp.onCreate`)", so moving it earlier looked like
free money, and **an autoload is the Godot project setting for "run before the main scene"**:
autoloads are instantiated and readied before the main scene is loaded.

It works — and it does not help.

| | ① backoff only | ① + autoload |
|---|---|---|
| process start → `GfxThreadStart` | 4.59 / 4.09 / 4.05 → **4.24 s** | 5.33 / 4.33 / 4.97 → **4.88 s** |
| phone → glasses | **3.32 s** | **3.68 s** |

The Activity genuinely arrives earlier — NRFakeActivity moves from +2.56 s to +1.92 s, and the log
drops from two `desc_ptr` lines to one, meaning the *first* bootstrap attempt now succeeds. But the
time of that first attempt does not move, because:

> **Autoload `_ready()` runs before the main scene loads, but autoload `_process()` does not.**
> Nothing on the GDScript side runs *during* main-scene loading, so the first `session::shared()`
> call is pinned to "main scene finished loading" no matter who publishes the Activity or when.

So the only thing the autoload buys is removing one failed attempt — which the backoff fix had
already reduced to 50–100 ms. The two overlap, and `register()`'s `runOnUiThread` work (companion
window + auto-PiP) now competes with scene loading, which is likely why it measured slightly worse.

**Answer to "can a Godot project setting fix this?": no.** The autoload setting addresses the wrong
half — it moves the *input* to the bootstrap, not the bootstrap's start.

## Open lead: the bootstrap start is pinned to main-scene load (~1.45 s)

With the two fixes below in, this is the largest single item left.

Unity starts XR during engine initialisation; we start it from the first `_process` of a node in the
scene tree. Closing this needs the bootstrap to run without a scene, which means one of:

- **Java side**: call `XrealBridge.register` from `GodotApp.onCreate` (what the Java doc suggests)
  *and* drive the first `try_start()` from JNI, not from `_process`. Touches the Android build
  template, which is `skip-worktree`'d — see `docs/plans/android-export-plugin-migration.md`.
- **Off-thread bootstrap**: run `try_start()` on a worker thread from the GDExtension's
  `InitLevel::Scene` hook, which fires before the main scene loads. Needs the SDK's threading
  assumptions checked first — the provider registration and input start are not documented as
  thread-safe, and this codebase has history with SDK threading (see `src/signal_guard.rs`).
- **Smaller main scene**: whatever the demo can shed shortens this directly. Not a library fix.

## Fixed: the Hands input bit was costing 878 ms (`xreal/input_source`)

Between our `display start callback` and `native session created` the SDK spent ~1.39 s, reported to
us as device-state callbacks `0 → 2 → 3 → 4`. Unity's equivalent took 0.20 s. RE'd in
`docs/archive/codex-input-start-analysis.md`; the answer is not 6DoF SLAM and not our hand-built
`IUnityXRInput` table:

**We passed `input_source = 3` (ControllerAndHands) to `InitUserDefinedSettings`, and the only thing
`InputManager::InputStart @ 0x78794` does between reporting device 2 and device 3 is call
`NativePerception::SetHandTrackingEnabled(true) @ 0x97174`, synchronously.** The reference Unity app
ships `inputSource=0`. Hand tracking is Air 2 Ultra only — the code comment next to our hardcoded `3`
even said so — so on every other headset that call bought nothing.

The default is now **Controller only**, with `xreal/input_source` to opt back in (Unity exposes the
same choice in its Project Settings). Measured with the setting left unset, hands feature still
present in the demo scene:

| | before | after |
|---|---|---|
| device-state sequence | 0 → 2 → 3 → 4 | **0 → 2** (hand notifications gone) |
| device 0 → 2 | 394 ms | ~325 ms |
| device 2 → `GfxThreadStart` | 878 ms + | **~88 ms** |
| input start total | ~1.39 s | **~0.41 s** |
| process start → `GfxThreadStart` | 4.24 s | 3.87 / 3.90 s |

`xreal_hands.tscn` cannot turn the bit on for you — `input_source` is read once at bootstrap, before
any feature node could react — so it emits a `push_warning` naming the setting instead of failing
silently.

**This repo's own `project.godot` sets `input_source=3`**, because the demo exists to show every
feature and hand tracking is one of them. So the demo keeps paying the 878 ms; the numbers above are
what an app that does not ask for hands gets. If you are timing the demo's startup, that is why.

The remaining 394 → ~325 ms is a composite (HMD `StartOrResume`, device/category and six
eye-geometry queries, perception `StartOrResume(6DoF)`, controller `StartOrResume`) with no sleep,
retry or timeout in the wrapper. Unity reaches the same subsystems' `Resume` branches instead, at
7 + 69 + 75 ms, which **LIKELY** reflects lifecycle placement rather than a setting we can change.
Not pursued further.

## Measurement notes

- The "before" column is a single cold start; the after-fix columns are three each. Absolute numbers
  move with SoC clock (see `docs/plans/camera-feed-plan.md` for how much), so compare within a set.
- `am start -W` reports only up to first-frame-displayed, which is the *phone* side; the glasses
  marker has to come from logcat.
- Killing the capture with `Stop-Process` on "recently started adb" also kills the adb server and
  drops a WiFi-attached device. Capture the `Start-Process -PassThru` id and stop that.
