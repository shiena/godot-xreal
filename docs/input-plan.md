# One Pro input plan — expose exactly what the hardware supports

Goal: make the input set the **XREAL One Pro** actually supports usable from Godot — and
nothing more. The One Pro is a 3DoF, camera-less device (the RGB camera is the detachable
"XREAL Eye" accessory), so most of the Unity SDK's input surface (hand tracking, 6DoF
position, plane/image tracking, anchors) is out of scope by hardware.

Evidence base: XREAL XR Plugin v3.1.0 C# (`XREALCallbackHandler.cs`, `XREALPlugin.cs`,
`DeviceLayouts.cs`, `XREALVirtualController.cs` in the `com.xreal.xr` package) + dynamic
export tables of our vendored `.so`s (`llvm-nm -D --defined-only`, 2026-07-03).

## One Pro input inventory

| # | Input | One Pro | godot-xreal status |
|---|---|---|---|
| 1 | Head rotation (3DoF) | ✅ | **Done** (`XrealHeadTracker`); roll gap tracked in `roll-tracking-investigation.md` |
| 2 | Glasses connect / disconnect | ✅ | **Done** (`glasses_connected/disconnected`, commit `365eee8`); session recovery gap in `hotplug-session-recovery.md` |
| 3 | Glasses hardware keys (MENU / MULTI / brightness±, CLICK / DOUBLE_CLICK / LONG_PRESS) | ✅ | **Missing** — Phase A below |
| 4 | Wearing state (P-sensor PUT_ON / TAKE_OFF) | ✅ | **Missing** — Phase A |
| 5 | Glasses state events + get/set (brightness, volume, electrochromic level, temperature, audio algorithm) | ✅ | **Missing** — Phase B |
| 6 | Phone as 3DoF controller (pose + touch + buttons + haptics) | ✅ (host phone) | **Missing, optional** — Phase C |
| 7 | Hand tracking | ❌ (no camera) | out of scope — runtime-gate via `IsHandTrackingSupported` |
| 8 | 6DoF position / plane / image / anchor | ❌ | out of scope |

Head 6DoF note: we currently init with `tracking_type = Mode6Dof` but only rotation is real
on One Pro; position stays ~0 and roll is suppressed. The `SwitchTrackingType`(→3DoF)
experiment from the roll investigation belongs to this plan's Phase A verification run.

## Native surface (confirmed exports in our vendored libs)

### `libXREALXRPlugin.so` — glasses events, buttons, state (already dlopen'd by `session.rs`)

```
SetGlassesHardwareEventCallback        <- single public callback registration
StartGlassesEventsReport / StopGlassesEventsReport   (Start takes an int, per
                                        _ZN13NativeGlasses24StartGlassesEventsReportEi)
ControlKeyEventGetType   (u64 event, int*)   \
ControlKeyEventGetFunction (u64 event, int*)  | parse a packed u64 key event
ControlKeyEventGetParam  (u64 event, int*)   /
ControlGetPsensorIsWearing (int*)  / GetProximityWearingState
ControlGetBrightness / ControlSetBrightness / ControlGetBrightnessLevelNumber
GetDisplayBrightnessLevel / GetDisplayBrightnessLevelCount / SetDisplayBrightnessLevel
ControlGetOLEDBrightness / ControlSetOLEDBrightness
ControlGetElectrochromicLevel / ControlSetElectrochromicLevel (One Pro EC dimming)
GetAudioAlgorithm / SetAudioAlgorithm
GetInputSource / SetInputSource        (None/Controller/Hands/ControllerAndHands)
IsHandTrackingSupported                (capability gate; expect false on One Pro)
RecenterController / SetControllerOffset / SetControllerType
```

**RESOLVED (2026-07-03): the event ABI needs no binary RE.** The C# side registers via a
plain `[DllImport]` in `XREALCallbackHandler.cs`:

```c
struct GlassesEventData { int32_t actionType; uint32_t para, para2; float para3; }; // 16B
void SetGlassesEventCallback(void (*cb)(GlassesEventData));   // struct BY VALUE (x0/x1)
```

confirmed as a dynamic export of our vendored `libXREALXRPlugin.so` (0x54d7c). Unity
registers it at `RuntimeInitializeOnLoad` and never calls `Start/StopGlassesEventsReport`
— registration alone is the reference behavior. The callback fires on an SDK thread; the
C# handler queues to the main thread (we mirror this with `src/glasses_events.rs`).
The internal `NativeGlasses` per-topic callbacks (`ControlSetKeyEventCallback`, …) are
not dynamic exports and are superseded by this funnel; the separate
`SetGlassesHardwareEventCallback` export remains unused/un-RE'd. Event-type constants
(`XREALActionType`) come from `XREALCallbackHandler.cs`: key click / key state /
brightness / volume / EC level / wearing / temperature / screen on-off / power-saving /
disconnect, plus `ACTION_TYPE_AUDIO_ALGORITHM_CHANGE = 2020`.

### `libnr_loader.so` — public NRSDK controller C API (36 `NRController*` exports)

```
NRControllerCreate/Destroy/Start/Pause/Resume/Recenter
NRControllerGetPose(u64 time?) / GetConnectedType / GetHandheldType / GetVersion
NRControllerGroup* (count / id / description / supported features)
NRControllerStateUpdate/Destroy + StateGet{ButtonState,ButtonDown,ButtonUp,
  TouchState,TouchPose,TouchDown,TouchUp,Gyroscope,Accelerometer,Magnetometer,
  BatteryLevel,Charging,ConnectionState}
NRControllerHapticVibrate(i64, f32, f32)
```

This is the **documented legacy NRSDK C API** — signatures are recoverable from the old
NRSDK Unity package's `NativeController.cs` (public), not from binary RE. Much lower risk
than the `SetGlassesHardwareEventCallback` ABI.

## Godot-side API design

Keep the established pattern: **native callbacks land on SDK threads → store into
lock-protected queue / atomics → `XrealHeadTracker::process()` drains on the main thread
and emits signals** (same shape as the hot-plug counters in `jni_bridge.rs` +
`poll_glasses_events`, `node.rs:147`).

New signals on `XrealHeadTracker` (stays consistent with `display_started` /
`glasses_connected`):

```gdscript
signal key_event(key: int, action: int)      # KEY_MENU/KEY_MULTI/KEY_INCREASE/KEY_DECREASE
                                             # × ACTION_CLICK/ACTION_DOUBLE_CLICK/ACTION_LONG_PRESS
signal wearing_changed(wearing: bool)        # P-sensor PUT_ON / TAKE_OFF
signal brightness_changed(level: int)
signal volume_changed(level: int)
signal ec_level_changed(level: int)
```

Constants exported as class constants (`#[constant]`) mirroring the SDK C# enums.
Games that want Godot actions can `Input.parse_input_event()` themselves from `key_event`;
we don't bake an action map into the extension.

Methods (thin, all no-op safely when libs are absent — desktop editor rule):

```
get_brightness() / set_brightness(v)         # + brightness_level_count
get_ec_level() / set_ec_level(v)             # One Pro electrochromic dimming
is_wearing() -> bool                         # ControlGetPsensorIsWearing poll
is_hand_tracking_supported() -> bool         # expected false on One Pro; scope assertion
```

Phase C adds a separate `XrealController` (Node3D): pose from `NRControllerGetPose`
(drive node transform like the head tracker), signals for touch/buttons, and
`vibrate(duration_ms, frequency, amplitude)`. Note the "phone as controller" model
conflicts with the phone screen showing anything else — on our Beam Pro setup the Godot
app owns the phone display, so Phase C likely renders its own touch UI in Godot and may
not need the NR virtual-controller pose at all (Godot `Input.get_gyroscope()` on the host
is an alternative). Decide after A/B ship.

## Phasing

- **Phase A — glasses keys + wearing — IMPLEMENTED 2026-07-03 (device verification pending):**
  1. ~~RE the event callback ABI~~ → resolved from C# (`SetGlassesEventCallback`, above).
  2. `native.rs` dlsyms `SetGlassesEventCallback` (Option, absent-symbol tolerant);
     `session.rs` registers `glasses_events::on_glasses_event` right after `CreateSession`.
  3. `src/glasses_events.rs`: bounded `Mutex<VecDeque>` (cap 256, drop-oldest), unit-tested.
     `XrealHeadTracker::poll_hardware_events()` drains on the main thread and emits
     `key_event(key, action)` / `key_state_changed` / `wearing_changed(bool)` /
     `brightness_changed` / `volume_changed` / `ec_level_changed` + catch-all
     `glasses_event(type, para, para2, para3)`; `KEY_*`/`ACTION_*` exported as constants.
     Demo: MENU long-press → `recenter()` (`demo/main.gd`).
  4. **Open (device verify):** logcat `[xreal] glasses event: type=…` for every physical
     key, wear on/off, brightness rocker; confirm MENU long-press recenter. Piggyback the
     `SwitchTrackingType(3DoF)` roll check on the same run.
- **Phase B — state get/set:** brightness / EC level / audio algorithm getters+setters
  (plain int in/out, low RE risk since mangled `NativeGlasses` twins show `(Pi)`/`(i)`
  signatures); volume likely arrives only as an event (OS media volume owns the value).
- **Phase C (optional) — phone controller** via `NRControllerCreate` path, signatures from
  legacy `NativeController.cs`. Only if a decoupled pointing ray is actually wanted.

## Risks / notes

- Every surface now has a known signature source: a C# `[DllImport]` twin (new SDK), a
  mangled-name signature, or legacy NRSDK docs. (`SetGlassesHardwareEventCallback` stays
  unused — `SetGlassesEventCallback` is the supported funnel.)
- Callbacks may fire before/without a session, and across glasses replug — route through
  the same guards as hot-plug (counters survive bursts; re-register callback on reconnect,
  which ties into `hotplug-session-recovery.md`).
- Volume/brightness physical keys are *also* handled by glasses firmware (they change the
  hardware state regardless); our events are notifications, not interception.
- EULA / symbol-stability caveats identical to `port-plan.md`.
