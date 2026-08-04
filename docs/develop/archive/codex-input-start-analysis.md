# XREAL XR input-start latency analysis

Date: 2026-07-22. Binaries examined: SDK 3.1.0 arm64-v8a
`libXREALXRPlugin.so`, `libnr_loader.so`, and `libnr_api.so`, using NDK 27.2
`llvm-objdump`, `llvm-nm`, `llvm-readelf`, and `llvm-strings`. The unstripped wrapper at
`jniLibs/arm64-v8a/libXREALXRPlugin.so` and the stripped wrapper extracted from the Unity APK have
the same exported-function addresses and code layout; the size/hash difference is symbol stripping.
This is static analysis correlated with the supplied device timestamps. Claims about the operation
inside a dynamically selected runtime provider are explicitly left unresolved.

## Verdict

**CONFIRMED — approximately 878 ms of the 1.39 s is recoverable for a non-hand-tracking session.**
It is not 6DoF SLAM initialization. Godot passes `input_source=3` (ControllerAndHands), while the
captured Unity reference settings say `inputSource=0` and its `InputStart` trace resumes only HMD,
perception, and controller. In `InputManager::InputStart @ 0x78794`, the only operation between the
callback reporting device 2 and the callback reporting device 3 is
`NativePerception::SetHandTrackingEnabled(true) @ 0x97174`. The measured 878 ms therefore belongs
to that synchronous config call. Passing a controller-only input source (`1` in the mapping already
used by this repository) makes the bit-1 test at `0x788d0..0x788d8` fail and skips the call and both
hand device notifications. This is the concrete first change to try.

**CONFIRMED — the preceding 394 ms is a composite interval, not one identifiable sleep.** It
contains HMD `StartOrResume`, device/category and six eye-geometry queries, perception
`StartOrResume(6DoF)`, and controller `StartOrResume`. There is no sleep, retry loop, condition wait,
or timeout in the wrapper code between device callbacks 0 and 2. The first invocation in our process
selects native `Start`/tracking-type creation paths; the Unity trace explicitly selects `Resume` and
reports 7 + 69 + 75 ms for the three subsystems. Static analysis cannot assign portions of our
394 ms to those lower calls because there is no intermediate Unity-interface callback.

**LIKELY — much of the remaining Unity/Godot difference is lifecycle placement/warm state, not a
different tracking setting.** Both captures request tracking type 0 (6DoF). Unity's measured call is
resuming already-started wrapper subsystems, whereas ours initializes the input provider and
immediately performs its first start in one synchronous sequence. There are no separate public
`NativeHMD`/`NativePerception`/`NativeController` pre-start exports for Unity to call. Unity can only
have reached the `Resume` branches by having driven the same input lifecycle start earlier (and then
paused/stopped it), or by equivalent lifecycle history outside the measured window. Moving our
provider initialize/start earlier could overlap or move cold work, but invoking a hidden subsystem
method is not recommended and is not required to remove the confirmed hand-tracking cost.

No sleep or timeout constant explains either observed gap. The hand-config operation ultimately
enters a dynamically selected `libnr_api.so` provider and performs nontrivial synchronous config
dispatch, but whether its 878 ms is IPC, service-side initialization, or a wait inside that provider
is **UNRESOLVED**. No fixed delay value can be honestly named from these binaries.

## The input-start path, traced

The relevant wrapper path is:

```text
input lifecycle start thunk                              wrapper 0x77fb4
  sets InputManager+0x18 = 1                             0x77fc0..0x77fc4
  InputManager::InputStart                               0x77fd0 -> 0x78794
    InputSubsystem_DeviceConnected(context, 0)           0x787b0..0x787c4
    NativeHMD::StartOrResume (virtual +0x40)              0x787c8..0x787d4
    GetDeviceType / GetDeviceCategory                    0x787d8..0x787f0
    UpdateEyeData (three pose + three FOV queries)        0x787f4..0x787f8
    select/adapt requested tracking type                  0x787fc..0x78858
    NativePerception::StartOrResume(type)                 0x78858..0x78864
    if input-source bit 0:
      NativeController::StartOrResume (virtual +0x40)     0x78898..0x788ac
      InputSubsystem_DeviceConnected(context, 1 or 2)     0x788b0..0x788cc
    if input-source bit 1:
      IsHandTrackingSupported                             0x78868..0x78890
      NativePerception::SetHandTrackingEnabled(true)      0x788dc..0x788e4
      InputSubsystem_DeviceConnected(context, 3)          0x788e8..0x788f8
      InputSubsystem_DeviceConnected(context, 4)          0x788fc..0x7890c
```

The callback argument is a Unity XR input device ID/type, despite our current log text calling it a
"device state." The sequence `0, 2, 3, 4` proves that the controller branch and both hand branches
ran. Devices 3 and 4 are emitted back-to-back after the hand config call, which explains why their
timestamps are identical.

### Device 0 to device 2: 394 ms

**CONFIRMED — all HMD, perception, eye-geometry, and controller work is inside this interval.** The
first callback is issued before any native subsystem call:

```text
0x787b0  ldr x8, [x20,#0x20]       ; IUnityXRInputInterface
0x787b8  mov x0, x19               ; Unity subsystem context
0x787bc  mov w1, wzr               ; device 0
0x787c0  ldr x8, [x8,#0x10]        ; SetDeviceConnected slot
0x787c4  blr x8
```

The next notification does not occur until after HMD start, the geometry calls, perception start,
and controller start:

```text
0x787c8..0x787d4  NativeHMD virtual +0x40
0x787d8..0x787f8  GetDeviceType, GetDeviceCategory, UpdateEyeData
0x78858..0x78864  NativePerception::StartOrResume(requested_type)
0x788a0..0x788ac  NativeController virtual +0x40
0x788b0..0x788cc  IUnityXRInputInterface+0x10(context, 1-or-2)
```

Each integrated subsystem has a started byte at object offset `+0x18`. Its generic dispatcher
selects `Start` when zero and `Resume` when nonzero; for example perception does so at
`IntegratedSubsystem<NRPerceptionWrapper>::StartOrResume @ 0x9f2a4..0x9f2bc`, and the HMD and
controller equivalents are at `0x91850` and `0x8cd20`. The native methods then make one lower
wrapper-table call: HMD `Start @ 0x90fcc` calls slot `+0x08` at `0x90ff0..0x90ffc`, HMD
`Resume @ 0x91340` calls slot `+0x28` at `0x91360..0x9136c`, controller `Start @ 0x8a2d4` calls
slot `+0x50` at `0x8a2f8..0x8a304`, and controller `Resume @ 0x8cb1c` calls slot `+0x70` at
`0x8cb3c..0x8cb48`.

Perception has an additional first-use path. `NativePerception::StartOrResume @ 0x9558c` branches
to virtual `Resume` only when its own `+0x18` is set; otherwise it enters
`NativePerception::SwitchTrackingType @ 0x955a4`. That routine locks the type-switch mutex
(`0x955d0..0x955dc`), enumerates/checks perception groups and selects the requested type
(`0x955e0..0x95794`), then creates/configures or switches the selected perception. Thus 6DoF cold
bring-up is genuine work in our 394 ms interval. It is not, however, the 878 ms interval.

**UNRESOLVED — the 394 ms cannot be split further from the supplied timestamps.** Adding timestamps
around the wrapper's existing `NativeHMD`, `NativePerception`, and `NativeController` log lines on a
device would settle the split. The disassembly contains no wrapper-side polling loop or fixed delay.

### Device 2 to devices 3/4: 878 ms

**CONFIRMED — this interval is exactly hand-tracking config enable.** After the device-2 callback
returns, the wrapper rechecks input-source bit 1 and makes one call before notifying device 3:

```text
0x788d0  ldr  x8, [x20,#0x30]
0x788d4  ldrb w8, [x8,#0x18]
0x788d8  tbz  w8,#1,0x78910
0x788dc  ldr  x0, [x20,#0x48]      ; NativePerception*
0x788e0  mov  w1,#1
0x788e4  bl   0x97174              ; SetHandTrackingEnabled(true)
0x788e8..0x788f8                   ; notify device 3
0x788fc..0x7890c                   ; notify device 4
```

The setter itself performs no local wait or retry. It forwards `(session, config, true)` through
the perception wrapper table slot `+0x190`:

```text
0x9719c  ldr x8, [x19,#0x8]
0x971a0  ldr x0, [x19,#0x28]       ; session handle
0x971a4  ldr x1, [x19,#0x38]       ; config handle
0x971a8  mov w2, w20               ; enabled
0x971ac  ldr x8, [x8,#0x190]
0x971b0  blr x8
```

At the loader boundary, `NRConfigSetHandTrackingEnabled @ libnr_loader.so:0x1f64a4` loads the config
dispatch table at `0x490550`, selects slot `+0x48`, normalizes the bool at `0x1f64c0`, and invokes it
at `0x1f64c4`. It contains no other operation.

The runtime target can be followed one level farther in `libnr_api.so`. Its `NRGetProcAddr` name
dispatch compares `"NRConfigSetHandTrackingEnabled"` at `0xc0c784..0xc0c794` and returns
`0xc0d34c` at `0xc0c834..0xc0c83c`. That function normalizes the bool and calls the shared config
implementation at `0xcecf24`:

```text
0xc0d380  adrp x0, 0x244d000
0xc0d384  and  w3, w20,#1
0xc0d388  add  x0, x0,#0xe30
0xc0d38c  mov  x1, x21             ; session
0xc0d390  mov  x2, x19             ; config
0xc0d394  mov  w4,#1               ; config field selector
0xc0d398  bl   0xcecf24
```

`0xcecf24` is a substantial generic config/provider dispatcher: it resolves the session/config
objects, selects a provider/handler, applies the boolean field, and performs synchronous internal
dispatch before returning. It has no direct `nanosleep`, `usleep`, timed condition wait, or literal
394/878/1000-ms loop. Several deeper calls are indirect or stripped, so the static evidence does not
distinguish an IPC round trip from service-side initialization or an internal wait. Classification of
the lower 878 ms as one of those mechanisms remains **UNRESOLVED**; only its ownership by the hand
config setter is confirmed.

## Per-question findings

### 1. Where the 394 ms and 878 ms go

**CONFIRMED:** 394 ms covers HMD start/resume, HMD metadata/eye geometry, perception
start/switch-to-6DoF, and controller start/resume, in that order. It is not one visible retry loop.

**CONFIRMED:** 878 ms is the synchronous `SetHandTrackingEnabled(true)` call and nothing else at the
wrapper level. Devices 3 and 4 are notified immediately after it.

**UNRESOLVED:** the hand setter's lower provider mechanism. No sleep/timeout constant was found, so
there is no defensible constant/value to name. The roughly 0.88 s duration alone is insufficient to
call it a one-second RPC timeout.

### 2. What is different about Unity's path

**CONFIRMED:** Unity's logged operations are the same wrapper methods reached from `InputStart`, not
separate public pre-start entry points. The wrapper exposes no C exports for HMD/perception/controller
start or resume. Its lifecycle start callback is the thunk at `0x77fb4`, and that thunk directly
calls `InputManager::InputStart`.

**CONFIRMED:** Unity is on warm `Resume` branches. Our first lifecycle invocation is on cold `Start`
or first tracking-type creation branches. `InputInitialize @ 0x78258` constructs `NativeHMD`
(`0x78328..0x78338`), `NativePerception` (`0x783ac..0x783bc`), and `NativeController`
(`0x78430..0x78440`); our `start_registered_providers()` invokes initialize and start back-to-back.
Unity owns this lifecycle and may initialize/start it during XR loader/activity lifecycle before the
shown app-level interval.

**LIKELY:** driving provider initialization earlier is the supported way to prewarm this path. There
is no evidence that calling another SDK export before input start would do the same job. Calling the
private native methods by address would bypass lifecycle state and is not recommended.

**UNRESOLVED:** exactly when Unity performed the earlier start/pause that made this call a resume.
The supplied Unity excerpt begins at the measured `InputStart` and cannot establish that earlier
timestamp.

### 3. Whether a setting selects the slow path

**CONFIRMED:** `tracking_type=0` does select the cold 6DoF perception creation/switch path inside the
394 ms interval. But the reference Unity app also logs `trackingType=0`; it is not a configuration
difference and cannot explain the 878 ms interval. Unity's 69-ms perception `Resume` is not a cold
6DoF initialization measurement.

**CONFIRMED:** `input_source=3` is the major slow-path selector. It sets both controller bit 0 and
hands bit 1. The hands bit gates `SetHandTrackingEnabled(true)` at `0x788d0..0x788e4`. The repository
currently hard-codes 3 at `src/session.rs:313`, while the captured Unity settings record
`inputSource=0` and its trace has no hand-enable/device-3/device-4 stage.

**CONFIRMED:** stereo rendering mode does not branch the input-start function. Unity's captured
stereo mode 2 versus our mode 0 affects display/render setup, not this callback sequence.

### 4. Whether our hand-built `IUnityXRInput` table causes retries

**CONFIRMED:** no missing/wrong input-interface slot drives either delay. During `InputStart`, the
only Unity input-interface method called is slot `+0x10`, `SetDeviceConnected`, at
`0x787c0`, `0x788c0`, `0x788f4`, and `0x78908`. No definition/state helper slot participates in the
start path.

**CONFIRMED:** every return value from our `xr_set_device_connected` is discarded. After each `blr`,
control proceeds directly to native work or the next callback; there is no `cbz`, comparison, retry,
or fallback based on `w0`. Returning 0 therefore cannot trigger a slow path.

The input provider registration callback is used during `InputInitialize`, and the definition/state
helpers are used later for device definition and per-frame state updates, but neither can account for
the two measured gaps. The full table remains important for correctness, just not for this latency.

## Recommendation

1. Change the startup setting from `input_source=3` to controller-only `input_source=1` for the
   normal One Pro profile. Keep `3` only for an explicitly selected hand-tracking profile on hardware
   where hands are required. This should remove approximately 878 ms and the device-3/device-4
   callbacks. This is the highest-confidence, lowest-risk experiment.
2. Instrument the existing wrapper log stream (no binary patch is necessary if XREAL debug logs are
   enabled) to timestamp `NativeHMD Start/Finish`, `NativePerception SwitchTrackingType/Finish`, and
   `NativeController Start/Finish`. That will split the remaining 394 ms.
3. If the remaining cold interval matters, invoke the registered provider `initialize` earlier than
   scene startup and keep the normal lifecycle `start` callback as the only start entry point. Do not
   call `NativeHMD`/`NativePerception` private addresses. Whether an earlier initialize alone warms the
   lower service enough is a device experiment.

The concrete source change is intentionally not made here, per the task constraint.

## Unresolved / what a device experiment would settle

- Run the same cold-start build with only `input_source` changed from 3 to 1. Expected decisive
  signature: callbacks become `0,2`, the `2→3` 878-ms interval disappears, and total input start
  falls to roughly the former 0→2 interval plus small overhead. If 3/4 still appear, the assumed
  settings propagation is wrong.
- The repository's prior One Pro notes expect `IsHandTrackingSupported()==false`, but this captured
  `0,2,3,4` sequence proves that both feature checks at `0x7f748..0x7f768` passed at this startup.
  Log `GetSupportedFeatures`, HMD feature 5, and the resolved device/category on the X4000 + One Pro
  combination to explain that capability discrepancy.
- Capture all SDK debug lines between device 0 and device 2. This will attribute the 394 ms among HMD,
  first 6DoF perception switch/start, eye queries, and controller.
- Timestamp provider `initialize` and `start` separately, then add a controlled delay between them.
  If the later cold start shrinks, lower creation has asynchronous warm-up; if not, the cost is paid
  synchronously by start.
- Capture Unity from process launch through the shown `InputStart`, including input lifecycle
  initialize/start/stop and Android pause/resume lines. This will prove which earlier lifecycle event
  set the subsystem `+0x18` bytes and made the measured call select `Resume`.
- Trace or hook `libnr_api.so:0xcecf24` and its internal provider calls on device. Duration at the
  entry/exit of its deeper handler would distinguish local initialization from IPC/service waiting.
  Static code provides no reliable timeout value.
