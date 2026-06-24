# XREAL native ABI — reverse-engineering notes

Source of truth for the FFI in `src/ffi.rs` / `src/native.rs`. Everything here was
recovered from the Unity package `com.xreal.xr` v3.1.0 (`Runtime/Plugins/Android/arm64-v8a`
+ `Runtime/Scripts/*.cs`). No official C headers exist, so signatures are recovered from
the C# `[DllImport]` declarations and binary symbol tables.

## Library layering

| Library | Role | Use from Godot |
|---|---|---|
| `libXREALNativeSessionManager.so` | Clean `XREAL*` perception C API (pose, device info, IMU, camera) | **Primary** — head pose for 3DoF |
| `libXREALXRPlugin.so` | Unity XR provider **+** flat C compositor/session API (274 exports) | Session, recenter, display layers |
| `libVulkanSupport.so` | Vulkan helper | Linked transitively by the plugin |
| `libnr_api.so` (+ `libnr_plugin_6dof.so`) | Lower NRSDK; only `NRAPICreate`/`NRGetProcAddr` public (obfuscated proc table) | Avoid — superseded by the two above |

`libXREALXRPlugin.so` NEEDED list is system-only (`libandroid/libc/libdl/liblog/libm`); it
resolves the NRSDK at runtime. So we never have to touch the obfuscated `NRGetProcAddr` table.

## Symbols used today (3DoF MVP)

From `libXREALNativeSessionManager.so` (`llvm-nm -D --defined-only`):

```
XREALLoadAPI
XREALIsSessionStarted
XREALGetHMDTimeNanos        -> uint64_t (ns)            [used]
XREALGetHeadPoseAtTime      -> head pose                [used, RE signature]
XREALGetPresentTime
XREALGetDevicePoseFromHead
XREALGetDeviceResolution / XREALGetDeviceFov / XREALGetDeviceIntrinsic
XREALGetTrackingReason / XREALGetDeviceType / XREALGetDeviceCategory
XREALProjectPoint / XREALUnProjectPoint / XREALUndistortImage
XREALStartImuDataCapture / XREALStart{RGB,Gray}CameraDataCapture …
```

From `libXREALXRPlugin.so` (recenter + future display):

```
RecenterGlasses                                          [used]
CreateSession / IsSessionStarted / ResumeSession / PauseSession / DestroySession
InitUserDefinedSettings / GetPluginVersion / JNI_OnLoad
GetHeadPoseAtTime / GetTrackingState / GetTrackingReason / GetHMDTimeNanos
InitializeRendering / DeinitializeRendering
CreateDisplayLayer / CreateProjectionRigLayer / CreateProjectionSurfaceLayer
CreateQuadCompositionLayer / CreateFrame / GetFrameMetaData
GetDeviceResolution / GetCameraProjectionMatrix / UpdateIPD / SetPredictTime
```

## Known signatures (from C# DllImport — `Runtime/Scripts/XREALPlugin.cs`)

```c
// UnityEngine.Pose = Vector3 position + Quaternion rotation, 7x f32
struct Pose { float px, py, pz, qx, qy, qz, qw; };

struct UserDefinedSettings {                  // InitUserDefinedSettings(arg)
    int   colorSpace, stereoRenderingMode, trackingType;
    bool  supportMonoMode;
    void* unityActivity;                       // <- Android Activity ptr (JNI) required
    int   inputSource;
};
enum TrackingType { MODE_6DOF=0, MODE_3DOF=1, MODE_0DOF=2, MODE_0DOF_STAB=3 };
enum XREALComponent { DISPLAY_LEFT=0, DISPLAY_RIGHT=1, ..., HEAD=6, IMU=7, NUM=8 };

bool GetDevicePoseFromHead(XREALComponent, Pose* /*ref*/);
bool GetDeviceResolution(XREALComponent, Vector2Int* /*ref*/);
bool GetCameraProjectionMatrix(XREALComponent, float znear, float zfar, Matrix4x4* /*ref*/);
void UnityPluginLoad(IUnityInterfaces*);   // Unity native-plugin entry — see below
void InitUserDefinedSettings(UserDefinedSettings);
bool CreateSession(bool directPresent);
const char* GetPluginVersion(void);
```

### ⚠️ `libXREALXRPlugin.so` is a Unity native plugin (device-confirmed)

`InitUserDefinedSettings` → `SessionManager::InitUserDefinedSettings` → `DisplayManager::LoadDisplay(IUnityInterfaces*, shared_ptr<UserDefinedSettings>)`. The `IUnityInterfaces*` comes from a global the Unity engine sets via `UnityPluginLoad(IUnityInterfaces*)` at startup. Godot never calls it, so the pointer is null and `LoadDisplay+44` (`ldr x8,[x20]`) segfaults.

`LoadDisplay` disasm shows it only needs:
- `IUnityInterfaces->GetInterface(IUnityGraphics_GUID)` then `IUnityGraphics->GetRenderer()`.
- If `GetRenderer()` returns `kUnityGfxRendererOpenGLES30 (11)`, the `renderer==Vulkan(0x15)` branch (which needs `IUnityGraphicsVulkan` etc.) is skipped (`cmp w8,#0x15; b.ne`).
- The first requested interface (GUID `0x940E64D2E52243EC,0xA348F3026B1B1193`, an XR iface) is used only behind `cbz` — returning NULL skips it.

So a minimal fake `IUnityInterfaces` (GetInterface → fake `IUnityGraphics{GetRenderer→11}` for the IUnityGraphics GUID, NULL otherwise) called via `UnityPluginLoad` BEFORE `InitUserDefinedSettings` gets past the crash. See `src/unity_plugin.rs`. Struct layouts/GUIDs from Unity's public PluginAPI headers (`<UnityEditor>/Editor/Data/PluginAPI/IUnityInterface.h`, `IUnityGraphics.h`). `UnityInterfaceGUID` is a non-trivial C++ type → passed by hidden pointer (so `GetInterface(const UnityInterfaceGUID*)`).

## Confirmed by disassembly (round a)

Method signatures recovered from the **C++ mangled names** in
`libXREALNativeSessionManager.so` (`llvm-cxxfilt`) plus AArch64 disassembly of the C wrappers
(`llvm-objdump -d`). The C wrappers prepend the singleton `this` and **tail-call** the method,
so each C export's return value == the method's return (NRSDK uniformly returns `NRResult`/i32,
`0` = success).

```c
// XREALNativeSessionManager::GetHeadPoseAtTime(unsigned long, float*)
int  XREALGetHeadPoseAtTime(uint64_t time_ns, float* out);   // out = NRPose (7 floats)

// XREALNativeSessionManager::GetHMDTimeNanos(unsigned long*)   <- OUT-PARAM, not a return!
int  XREALGetHMDTimeNanos(uint64_t* out_time_ns);            // (the earlier `()->u64` guess was wrong)

// XREALNativeSessionManager::IsSessionStarted()
// ⚠️ DEVICE-CONFIRMED: the SessionManager is a process-global singleton. Calling this
// (or any GetHeadPose*/GetHMDTime* method) BEFORE the bootstrap below has constructed it
// dereferences a null `this` and SIGSEGVs (null+8, at IsSessionStarted+4). Only query
// after InitUserDefinedSettings → CreateSession → XREALLoadAPI.
bool XREALIsSessionStarted(void);

// XREALNativeSessionManager::LoadAPI()  — sets up the perception delegate (this+0x18 fn table
// at offset 0xb0/0xb8 that GetHeadPoseAtTime/GetHMDTimeNanos dispatch through). Call before
// any pose query.
void XREALLoadAPI(void);

// XREALNativeSessionManager::GetDevicePoseFromHead(NRComponent, NRMat4f*)  — matrix, not Pose
int  XREALGetDevicePoseFromHead(NRComponent, NRMat4f* /*4x4*/);
```

`out` for `GetHeadPoseAtTime` is a flat `float*`; mapped to NRSDK `NRPose` =
`NRRotation{x,y,z,w}` then `NRPosition{x,y,z}` (**rotation-first**, opposite of Unity's `Pose`).
See `src/ffi.rs::NrPose`.

## Still to RE (priority order)

1. **On device:** confirm the `NrPose` field order (rotation-first assumed) and the Unity→Godot
   quaternion sign convention in `NrPose::to_godot_quaternion` (log the 7 floats; check which 4
   are unit-norm; check which axis inverts when you turn your head).
2. `UserDefinedSettings` bool width / struct size — assumed C# default `bool`→4-byte BOOL
   (`src/ffi.rs` uses `i32`). Verify if `CreateSession` misbehaves.
3. Display: `CreateFrame` / `GetFrameMetaData` / `CreateProjectionRigLayer` — texture handoff.
4. `GlassesDisplay.nativeRunOnGlassesDisplay(long, View)` — the `long` is the native session handle.
5. Who publishes `ndk_context` (JavaVM/Activity) under Godot's Android runtime (see `src/jni_bridge.rs`).

## Reproduce the symbol dump

```bash
llvm-nm -D --defined-only libXREALNativeSessionManager.so | grep ' T ' | sed 's/.* T //' | sort
llvm-nm -D --defined-only libXREALXRPlugin.so            | grep ' T ' | sed 's/.* T //' | sort
```

Disassemble a specific wrapper to recover an ABI:

```bash
llvm-nm -D libXREALNativeSessionManager.so | grep XREALGetHeadPoseAtTime   # get the address
llvm-objdump -d --start-address=0x1bb0c --stop-address=0x1bba0 libXREALNativeSessionManager.so
echo _ZN25XREALNativeSessionManager17GetHeadPoseAtTimeEmPf | llvm-cxxfilt  # demangle
```

Coordinate conversion (Unity left-handed Y-up → Godot right-handed Y-up) lives in
`NrPose::to_godot_quaternion`; the sign convention is a placeholder until verified on hardware.
