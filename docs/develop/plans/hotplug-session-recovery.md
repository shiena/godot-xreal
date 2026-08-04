# Hot-plug session recovery (finding + fix plan)

Status: **bug confirmed on device; fix not yet implemented (needs on-device verification).**

## Finding (device, 2026-07-02/03)

The `glasses_connected` / `glasses_disconnected` signals (commit `365eee8`) work. But watching a
full plug/unplug/relaunch session revealed a gap in the *session* (rendering) lifecycle:

- An app instance that **launched while the glasses were connected** creates the session fine:
  `native session created (session_started=true, tracking_state=2)` → `display_started -> recenter`
  → renders. (Observed on relaunched pids 12089 / 13368 / 13546.)
- An app instance that **launched WITHOUT the glasses** (pid 2264) **never created a session even
  after the glasses were plugged in** — `[demo] glasses connected` fired, but no
  `native session created` appeared; it stayed in the `desc_ptr=…` bootstrap-retry loop. Only an
  app **relaunch** brought the session up.

⇒ For the "start without glasses, connect later" case, the **notification arrives but rendering
does NOT auto-resume** — the app must be relaunched.

## Root-cause hypothesis (unverified)

`session::try_start()` (`src/session.rs`) re-runs `init_user_defined_settings()` + `create_session()`
on every retry. Likely the SDK's `InitUserDefinedSettings` initialises its DisplayManager **once**,
against whatever display state existed on the first call. When that first call happened with **no
XREAL display present**, the display manager latches a no-display state, so later `create_session()`
calls keep returning false even after the glasses appear. A fresh process re-inits with the display
present, which is why relaunch works.

(This is a hypothesis — the real cause could be that the SDK needs a `DestroySession` before
re-`CreateSession`, or a display-layer re-registration. Confirm on device before committing a fix.)

## Proposed fix (test on device before shipping)

Gate the first `InitUserDefinedSettings` until an XREAL display is actually present, using the
hot-plug counters already added in `src/jni_bridge.rs`:

- In `try_start()`, before `init_user_defined_settings`, return `WaitingForRuntime` while
  `glasses_connect_count() <= glasses_disconnect_count()` (i.e. no XREAL display present).
- Glasses-present-at-launch path is unchanged: `XrealBridge.register` already bumps the connect
  counter for an already-present display, so the gate passes on the first `process()`.

Risks / caveats to verify on device:
1. **Don't regress the working path.** If `isXrealDisplay` (name contains xreal/nreal OR 3840×1080)
   ever fails to flag a present display, the gate would block init forever. Confirm the counter is
   ≥1 by the first `native session created` on a normal glasses-at-launch launch.
2. Timing: `register()` (counter bump) must run before the first gated `try_start()`. It runs from
   `GodotApp.onCreate` + `demo/main.gd` `_ready`, both before `process()` — but verify.
3. If the hypothesis is wrong (init isn't the sticky part), this won't fix it — then try a
   `DestroySession` + full re-init on the `glasses_connected` transition instead.

## Test plan

1. Implement the gate. Launch **without** glasses → confirm it waits (no InitUserDefinedSettings /
   CreateSession spam). Plug in glasses → expect `native session created` → `display_started` →
   content renders, **with no relaunch**.
2. Regression: launch **with** glasses already connected → confirm session still comes up promptly
   (no added delay).
