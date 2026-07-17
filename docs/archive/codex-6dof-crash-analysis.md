# Dynamic HMD update crash and 6DoF side-effect analysis

Date: 2026-07-18

Context: XREAL SDK 3.1.0, arm64 `libXREALXRPlugin.so`, driven by the fake Unity XR input host in
`src/unity_plugin.rs`. This follows `docs/archive/codex-headlock-analysis.md`.

## Findings

The tombstone is consistent with an undersized `IUnityXRInputInterface`, with one address correction:
`0x7ad6c` is not the indirect call. It is the final argument move. The call is at `0x7ad70`, and the
saved return address is consequently `0x7ad74` in an architectural unwind. The reported `0x7ad70` frame
can still denote the call site depending on the tombstone's LR presentation.

The target is unambiguously input-interface slot `+0xb8`,
`IUnityXRInputInterface::DeviceState_SetRotationValue`:

```text
0x7ad50  ldr x8, [x20,#0x20]    ; x8 = InputManager::m_UnityXRInput
0x7ad54  ldr w1, [x20,#0xd4]    ; feature index
0x7ad58  mov x0, x19             ; UnityXRInputDeviceState *
0x7ad5c  mov v0.16b, v12.16b     ; quaternion x
0x7ad60  ldr x8, [x8,#0xb8]      ; slot 23
0x7ad64  mov v1.16b, v11.16b     ; quaternion y
0x7ad68  mov v2.16b, v10.16b     ; quaternion z
0x7ad6c  mov v3.16b, v9.16b      ; quaternion w
0x7ad70  blr x8
```

Its ABI is:

```c
UnitySubsystemErrorCode DeviceState_SetRotationValue(
    UnityXRInputDeviceState *state,
    UnityXRInputFeatureIndex featureIndex,
    UnityXRVector4 featureValue);
```

On AArch64, the four-float aggregate is passed in `s0` through `s3`, exactly matching the disassembly.
The current Rust table ends at `+0x10`, so the `ldr [table,#0xb8]` is an out-of-bounds read from unrelated
static storage. **This establishes the crash mechanism.**

The crash is not guarded by a rare tracking event. The four initial HMD state-helper calls, including
the call at `0x7ad70`, are unconditional for both update types. The intermittent delay instead comes
from repeatedly entering unrelated Rust code with an invalid ABI and invalid inherited register state.
The frequency scaling is consistent with cumulative undefined behaviour, but it is not evidence of a
conditional XREAL tracking-state path.

The 6DoF effect is separate from those invalid calls. Dynamic update (`updateType == 0`) stores the live
head pose into `InputManager+0x60..+0x78`; before-render update (`updateType == 1`) deliberately skips that
store. This is the only type-0-specific pose-state mutation before the Unity helper calls and is the best
static explanation for the observed position world-lock.

## A. Interface provenance and the failing call

### How `InputManager+0x20` is populated

`InputManager::LoadInput @0x77b74` obtains and caches Unity interfaces. The relevant pointer is already
present by `InputInitialize`; the latter reads it consistently from `InputManager+0x20`.

`InputInitialize @0x78258` registers the provider through slot `+0x08`:

```text
0x782b0  ldr x8, [x19,#0x20]
0x782b4  mov x1, sp              ; UnityXRInputProvider const *
0x782b8  mov x0, x20             ; subsystem handle
0x782bc  ldr x8, [x8,#0x08]      ; RegisterInputProvider
0x782c0  blr x8
```

The provider table placed on the stack contains the visible lambdas `$_8..$_16`; `$_9 @0x785e8` is the
`UpdateDeviceState` callback that eventually reaches `UpdateHMDState`.

`InputStart @0x78794` later calls slot `+0x10` (`InputSubsystem_DeviceConnected`) at `0x787c0..0x787c4`.
Thus the first three Rust entries happen to cover startup, but not definition filling or state filling.

### Update-type control flow

The entry split is:

```text
0x7aa68  cmp w1,#1
0x7aa70  b.ne 0x7aa88
0x7aa74  bl  DisplayManager::GetInstance
0x7aa78  bl  DisplayManager::OnBeforeRender
0x7aa7c  bl  DisplayManager::GetInstance
0x7aa80  ldr x1,[x0,#0x100]
0x7aa84  cbnz x1,0x7aa94
0x7aa88  ldr x0,[x20,#0x48]
0x7aa8c  bl  NativePerception::GetHMDTimeNanos
0x7aa94  ...
0x7aab4  bl  NativePerception::GetHeadPose
```

PROVEN:

- Type 1 first executes `DisplayManager::OnBeforeRender` and normally uses `DM+0x100` as the pose-time
  argument to `GetHeadPose`.
- Type 0 skips `OnBeforeRender` and obtains the HMD time from `NativePerception`.
- Both paths call `NativePerception::GetHeadPose @0x96578` and set `DM+0x108` from its returned timestamp.
- Both paths then reach all state-helper calls. There is no tracking-state guard around `0x7ad70`.

### Every interface slot used by `UpdateHMDState`

The function uses only four distinct helper slots, repeatedly:

| Offset | Header member | Calls in `UpdateHMDState` | ABI |
|---:|---|---:|---|
| `+0x90` | `DeviceState_SetBinaryValue` | 1 | `(state*, uint32_t index, bool value) -> int32_t` |
| `+0x98` | `DeviceState_SetDiscreteStateValue` | 1 | `(state*, uint32_t index, uint32_t value) -> int32_t` |
| `+0xb0` | `DeviceState_SetAxis3DValue` | 4 | `(state*, uint32_t index, UnityXRVector3 value) -> int32_t` |
| `+0xb8` | `DeviceState_SetRotationValue` | 4 | `(state*, uint32_t index, UnityXRVector4 value) -> int32_t` |

The first pair is:

```text
0x7acfc..0x7ad14  table+0x90, state, IM+0xc8, true
0x7ad18..0x7ad2c  table+0x98, state, IM+0xcc, 3
```

The four vector/quaternion pairs use feature-index fields `IM+0xd0/+0xd4`, `+0xd8/+0xdc`,
`+0xe0/+0xe4`, and `+0xe8/+0xec`. Their call sites are `0x7ad30..0x7ad70`,
`0x7ae28..0x7ae78`, `0x7af30..0x7af80`, and `0x7b038..0x7b088`.

### Slots reached during initialization and definition filling

`InputInitialize` itself touches only `+0x08`. The callbacks it registers cause the following additional
slots to be used during normal initialization:

- `InputStart`: `+0x10`, `InputSubsystem_DeviceConnected`.
- `FillDeviceDefinition -> FillHMDDefinition`: `+0x40`, `+0x48`, `+0x50`, and `+0x78`.
- Controller/hand definition paths use `+0x40`, `+0x48`, and `+0x78` (plus the same feature builder
  repeatedly).

The names are respectively `DeviceDefinition_SetName`,
`DeviceDefinition_SetCharacteristics`, `DeviceDefinition_SetManufacturer`, and
`DeviceDefinition_AddFeatureWithUsage`. For example, `FillHMDDefinition` loads these at
`0x7a238`, `0x7a254`, `0x7a26c`, and repeatedly at `0x7a284` onward.

The complete published layout, needed to prevent future out-of-bounds calls, is:

| Offset | Member |
|---:|---|
| `0x00` | `RegisterLifecycleProvider` |
| `0x08` | `RegisterInputProvider` |
| `0x10` | `InputSubsystem_DeviceConnected` |
| `0x18` | `InputSubsystem_DeviceDisconnected` |
| `0x20` | `InputSubsystem_DeviceConfigChanged` |
| `0x28` | `InputSubsystem_TrackingOriginUpdated` |
| `0x30` | `InputSubsystem_SetTrackingBoundary` |
| `0x38` | `InputSubsystem_GetPlatformData` |
| `0x40` | `DeviceDefinition_SetName` |
| `0x48` | `DeviceDefinition_SetCharacteristics` |
| `0x50` | `DeviceDefinition_SetManufacturer` |
| `0x58` | `DeviceDefinition_SetSerialNumber` |
| `0x60` | `DeviceDefinition_SetCanQueryForDeviceStateAtTime` |
| `0x68` | `DeviceDefinition_AddFeature` |
| `0x70` | `DeviceDefinition_AddCustomFeature` |
| `0x78` | `DeviceDefinition_AddFeatureWithUsage` |
| `0x80` | `DeviceDefinition_AddUsageAtIndex` |
| `0x88` | `DeviceState_SetCustomValue` |
| `0x90` | `DeviceState_SetBinaryValue` |
| `0x98` | `DeviceState_SetDiscreteStateValue` |
| `0xa0` | `DeviceState_SetAxis1DValue` |
| `0xa8` | `DeviceState_SetAxis2DValue` |
| `0xb0` | `DeviceState_SetAxis3DValue` |
| `0xb8` | `DeviceState_SetRotationValue` |
| `0xc0` | `DeviceState_SetBoneValue` |
| `0xc8` | `DeviceState_SetHandValue` |
| `0xd0` | `DeviceState_SetEyesValue` |
| `0xd8` | `DeviceState_SetDeviceTime` |

This ordering and the signatures above are corroborated by the public Unity XR SDK header. The package
tree supplied for this analysis did not contain `IUnityXRInput.h`.

## B. Why the failure is delayed

There is no branch between the common path and the first four helper calls after the update-type-dependent
cache store:

```text
0x7ace8  cbnz w21,0x7acfc
0x7acec  stp s13,s8,[x20,#0x60]
0x7acf0  stp s14,s12,[x20,#0x68]
0x7acf4  stp s11,s10,[x20,#0x70]
0x7acf8  str s9,[x20,#0x78]
0x7acfc  ldr x8,[x20,#0x20]      ; common helper sequence begins
```

Therefore a zeroed state buffer cannot suppress `0x7ad70`, and stale bytes in that buffer are not its
dominant guard condition. The helper treats the buffer as an opaque Unity-owned object; zeroing 1024 bytes
does not make it a valid Unity state, but a correct fake helper may safely ignore it.

The bogus `+0xb8` entry resolves into `libgodot_xreal.so` near `0xf1980`. That address is not a function
entry in the available stripped release image. It is in the middle of a large JNI/Rust-generated routine:

```text
0xf197c  ldr x8,[x23,#0xa38]
0xf1980  sub x0,x29,#0x28
0xf1984  blr x8
0xf1988  bl  0x157dd0
```

Entering at `0xf1980` assumes valid `x29`, `x23`, `x8`, stack layout, and unwind state from the real
function prologue. None is supplied by `DeviceState_SetRotationValue`. Whether it faults immediately,
returns, corrupts state, or faults in a later probe depends on volatile register values and the arbitrary
neighbouring static layout. The `x1=0`, `x2=3`, and garbage `x3` in the tombstone are consequently not a
valid rotation-helper argument snapshot; they are state after execution has escaped into unrelated code.

INFERRED: the approximately linear probe-count relationship is caused by repeated undefined calls and/or
cumulative corruption. Static analysis cannot identify a deterministic 60-call counter in the XREAL path,
and none dominates the call site. Type 1 also executes these helpers, so the fact that the observed crash
rate tracks type-0 probes likely reflects different volatile-register/cache state or cumulative corruption,
not a type-0-only call to slot `+0xb8`.

## C. The 6DoF side effect

After `GetHeadPose`, the function converts the returned matrix into Unity position and quaternion values.
The decisive update-type split is at `0x7ace8`:

```text
0x7acd8..0x7ace4  converted quaternion components
0x7ace8  cbnz w21,0x7acfc
0x7acec  stp s13,s8,[x20,#0x60]  ; position x,y
0x7acf0  stp s14,s12,[x20,#0x68] ; position z, rotation x
0x7acf4  stp s11,s10,[x20,#0x70] ; rotation y,z
0x7acf8  str s9,[x20,#0x78]      ; rotation w
```

PROVEN: exactly when `updateType == 0`, `InputManager+0x60` receives a contiguous seven-float head pose.
Type 1 skips it. Conversely, type 1 updates the three component/eye pose caches at `IM+0x344`, `+0x3cc`,
and `+0x454` after each matrix multiply; type 0 skips those writes at `0x7ad84`, `0x7ae8c`, and `0x7af94`.

PROVEN: neither behaviour depends on the return value from a Unity state helper. All helper return values
are discarded. The type-0 cache write occurs before the first helper call. This explains how type 0 can
produce a useful native side effect even though the fake Unity state implementation is invalid.

INFERRED: `IM+0x60` is the provider's dynamic HMD pose cache used by later display/camera logic, and keeping
its translation current stops the compositor/host combination from cancelling eye-camera translation.
This is a stronger explanation than a runtime “switch to 6DoF” setter: no NativePerception tracking-mode
setter occurs in `UpdateHMDState`, and both update types call the same `NativePerception::GetHeadPose`.
The disassembly proves the unique cache write but does not, by itself, prove the downstream visual consumer.

The minimal native operation is therefore conceptually:

```text
time = NativePerception::GetHMDTimeNanos()
NativePerception::GetHeadPose(time, ..., &matrix, ..., &timestamp)
convert matrix exactly as UpdateHMDState does
store position.xyz + rotation.xyzw at InputManager+0x60
```

Directly reproducing that sequence is not recommended yet. `InputManager` and `NativePerception` methods
are C++ internal symbols, the matrix conversion is substantial, and direct singleton-field writes are
version-fragile. There is no proven exported one-call setter for this cache.

## D. Recommended fix

Implement the complete interface through `+0xd8`, then call type 0 safely. This is more robust than writing
XREAL private object fields.

The state setters can return `kUnitySubsystemErrorCodeSuccess` and otherwise be no-ops for the current
Godot host. They have no out-parameters, and XREAL discards their return values. Their exact C signatures are:

```c
int32_t SetCustom(UnityXRInputDeviceState*, uint32_t, const void*, uint32_t);
int32_t SetBinary(UnityXRInputDeviceState*, uint32_t, bool);
int32_t SetDiscrete(UnityXRInputDeviceState*, uint32_t, uint32_t);
int32_t SetAxis1D(UnityXRInputDeviceState*, uint32_t, float);
int32_t SetAxis2D(UnityXRInputDeviceState*, uint32_t, UnityXRVector2);
int32_t SetAxis3D(UnityXRInputDeviceState*, uint32_t, UnityXRVector3);
int32_t SetRotation(UnityXRInputDeviceState*, uint32_t, UnityXRVector4);
int32_t SetBone(UnityXRInputDeviceState*, uint32_t, UnityXRBone);
int32_t SetHand(UnityXRInputDeviceState*, uint32_t, UnityXRHand);
int32_t SetEyes(UnityXRInputDeviceState*, uint32_t, UnityXREyes);
int32_t SetDeviceTime(UnityXRInputDeviceState*, UnityXRTimeStamp);
```

Important AArch64 detail: vector, bone, hand, and eyes parameters are aggregates passed according to the
platform ABI. Each Rust declaration must model the real by-value struct, not replace it with `*const void`.
For immediate HMD safety, `UnityXRVector2/3/4` plus the four used state helpers are mandatory; completing
the table avoids the same class of fault when controllers or hands become active.

Definition stubs must not all return zero. `DeviceDefinition_AddFeature` and
`DeviceDefinition_AddFeatureWithUsage` return a `UnityXRInputFeatureIndex`; assign monotonically increasing
indices per definition. XREAL saves those returned indices in `IM+0xc8..+0xec` and passes them back to the
state setters. `AddCustomFeature` needs the same policy. Metadata setters and `AddUsageAtIndex` may return
success. `InputSubsystem_GetPlatformData` must set `*platformData = NULL` before returning failure or
success; never leave an out-pointer uninitialized. Boundary/config notification stubs have no out values.

Recommended call order on the render thread remains:

```text
UpdateDeviceState(HMD, updateType=0, valid opaque-or-ignored state)  ; refresh IM+0x60 6DoF cache
UpdateDeviceState(HMD, updateType=1, valid opaque-or-ignored state)  ; OnBeforeRender / DM+0x100
PopulateNextFrameDesc
SubmitCurrentFrame
```

After installing the real helper table, the 1024-byte zero buffer may remain only because the fake state
helpers intentionally do not dereference it. It should be documented as an opaque sentinel, not as a
reconstructed `UnityXRInputDeviceState`.

## Confidence summary

PROVEN from the SDK disassembly:

- The failing indirect call is `blr x8 @0x7ad70`, loaded from input-interface slot `+0xb8`.
- `+0xb8` is called with the ABI of `DeviceState_SetRotationValue`.
- The call is unconditional for update types 0 and 1.
- Type 0 uniquely stores a seven-float head pose at `InputManager+0x60..+0x78`.
- Type 1 uniquely runs `OnBeforeRender` and refreshes later eye/component caches.
- Unity helper return values do not control the native pose-cache side effects.

INFERRED, requiring device validation:

- Repeated entry into the interior Rust address explains the delayed/cumulative failure rather than an
  XREAL tracking-event guard.
- `InputManager+0x60` is the specific downstream value responsible for the visual 6DoF world-lock.
- A complete no-op state-helper table plus unique definition indices is sufficient for all future device
  paths; controller/hand paths should be exercised separately.

## Device validation

1. Add the full table and log each helper's slot, index, and call count without dereferencing `state`.
2. Confirm type 1 calls `+0x90/+0x98/+0xb0/+0xb8` every frame; this directly tests the unconditional result.
3. Run type 0 every frame. The prior frequency-dependent crash should disappear.
4. Temporarily omit only the `IM+0x60` effect by testing type 1 alone; position world-lock should disappear
   while rotation head-lock remains.
5. Test type 0 plus type 1 and confirm both translation world-lock and stable frame submission for at least
   ten times the previous failure interval.
6. Later enable controllers/hands to validate the by-value `Bone`, `Hand`, and `Eyes` ABI before those paths
   are considered supported.
