# XREAL native ABI — reverse-engineering notes

Source of truth for the FFI in `src/ffi.rs` / `src/native.rs`. Everything here was
recovered from the Unity package `com.xreal.xr` v3.1.0 (`Runtime/Plugins/Android/arm64-v8a`
+ `Runtime/Scripts/*.cs`). No official C headers exist, so signatures are recovered from
the C# `[DllImport]` declarations and binary symbol tables.

> For a task-oriented view — which RE'd functions are callable, their confirmed signatures,
> and how each surfaces in GDScript (or why it stays internal) — see
> [`native-api-reference.md`](native-api-reference.md). This file is the low-level derivation.

## Library layering

| Library | Role | Use from Godot |
|---|---|---|
| `libXREALNativeSessionManager.so` | Clean `XREAL*` perception C API (pose, device info, IMU, camera) | **Primary** — 6DoF head pose |
| `libXREALXRPlugin.so` | Unity XR provider **+** flat C compositor/session API (274 exports) | Session, recenter, display layers |
| `libVulkanSupport.so` | Vulkan helper | Linked transitively by the plugin |
| `libnr_api.so` (+ `libnr_plugin_6dof.so`) | Lower NRSDK; only `NRAPICreate`/`NRGetProcAddr` public (obfuscated proc table) | Avoid — superseded by the two above |

`libXREALXRPlugin.so` NEEDED list is system-only (`libandroid/libc/libdl/liblog/libm`); it
resolves the NRSDK at runtime. So we never have to touch the obfuscated `NRGetProcAddr` table.

## Core symbols (head pose + session)

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
SetGlassesEventCallback                                  [used — glasses keys/wear/brightness
                                                          events; C# ABI, see docs/plans/input-plan.md]
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

`SetInitialTrackingTypeConfig` is exported, but it is **not** a simple
`void(int trackingType)` helper. Disassembly shows the C wrapper forwards four arguments to
`InputManager::SetInitialTrackingTypeConfig(int*, int*, int*, int)`. Do not bind or call it
until the three pointer parameters are identified from the Unity C# side or a call-site.

Unity One/One Pro reference logs initialize `trackingType=0` and later call
`NativePerception SwitchTrackingType: 0`, which maps to `TrackingType.MODE_6DOF`. This is the
path that creates the native 6DoF head tracker on the tested device (Godot consumes both its
rotation and position).

### ⚠️ `libXREALXRPlugin.so` is a Unity native plugin (device-confirmed)

`InitUserDefinedSettings` → `SessionManager::InitUserDefinedSettings` → `DisplayManager::LoadDisplay(IUnityInterfaces*, shared_ptr<UserDefinedSettings>)`. The `IUnityInterfaces*` comes from a global the Unity engine sets via `UnityPluginLoad(IUnityInterfaces*)` at startup. Godot never calls it, so the pointer is null and `LoadDisplay+44` (`ldr x8,[x20]`) segfaults.

`LoadDisplay` disasm shows it only needs:
- `IUnityInterfaces->GetInterface(IUnityGraphics_GUID)` then `IUnityGraphics->GetRenderer()`.
- If `GetRenderer()` returns `kUnityGfxRendererOpenGLES30 (11)`, the `renderer==Vulkan(0x15)` branch (which needs `IUnityGraphicsVulkan` etc.) is skipped (`cmp w8,#0x15; b.ne`).
- The first requested interface (GUID `0x940E64D2E52243EC,0xA348F3026B1B1193`, an XR iface) is used only behind `cbz` — returning NULL skips it.

So a minimal fake `IUnityInterfaces` (GetInterface → fake `IUnityGraphics{GetRenderer→11}` for the IUnityGraphics GUID, NULL otherwise) called via `UnityPluginLoad` BEFORE `InitUserDefinedSettings` gets past the first crash. See `src/unity_plugin.rs`. Struct layouts/GUIDs from Unity's public PluginAPI headers (`<UnityEditor>/Editor/Data/PluginAPI/IUnityInterface.h`, `IUnityGraphics.h`). `UnityInterfaceGUID` is a non-trivial C++ type → passed by hidden pointer (so `GetInterface(const UnityInterfaceGUID*)`).

Later device logs showed that the graphics-only fake is not enough: Unity reference logs run
`DisplayManager LoadDisplay`, `InputManager LoadInput`, `InputInitialize`,
`NativeHMD Create`, `NativePerception Create`, `InputStart`, and
`NativePerception Start`. Godot skipped the provider lifecycle because the Unity XR interfaces
returned NULL, so `NativeHMD`/`NativePerception` stayed absent.

Current RE / unverified Unity XR provider stub:

- `DisplayManager::LoadDisplay` requests GUID
  `0x940E64D2E52243EC,0xA348F3026B1B1193`, then calls vtable slot `+0x00` as
  `RegisterLifecycleProvider(id, name, provider)`.
- The same display interface uses slot `+0x08` for the display-state provider and slot `+0x10`
  for the graphics-thread provider. An earlier Godot stub had those two reversed; the tell was
  receiving a callback table with `start=false`/`stop=false` for the supposed graphics provider.
- `InputManager::LoadInput` requests GUID
  `0x2B53FA871CDA6802,0x942BCA0C8EF13193`, then calls vtable slot `+0x00` as
  `RegisterLifecycleProvider(id, name, provider)`.
- `InputManager::LoadInput` requests GUID
  `0x3007FD5885A346EF,0x9EEB2C84AA0A9DD9`, then calls vtable slot `+0x28` as the meshing
  lifecycle registration.
- `DisplayManager::LoadDisplay` also requests GUID
  `0xAB695A1C94114266,0x0BDB5A1B3F7A54B8`; `DisplayManager::Initialize` uses slot `+0x00`
  and `+0x08`, so the fake provides no-op texture registration and stable property-id hashing.
- Lifecycle provider layout is inferred from relocation entries:
  - input provider copied from `0xd4c28`: offset `+0x08` -> `InputInitialize`, `+0x10` ->
    `InputStart`, `+0x18` -> `InputStop`, `+0x20` -> shutdown.
  - display provider copied from `0xd4690`: offset `+0x08` -> `DisplayInitialize`, `+0x10` ->
    display start, `+0x18` -> display stop, `+0x20` -> shutdown.
- Godot now stores those providers in `src/unity_plugin.rs` and invokes initialize/start after
  `InitUserDefinedSettings` and `CreateSession`. This is intentionally narrow: stop/shutdown and
  full Unity XR frame submission are not implemented yet.

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

Return convention update (device-confirmed after Unity XR provider lifecycle was stubbed):
`libXREALXRPlugin.so` exports `GetHMDTimeNanos` and `GetHeadPoseAtTime` through
`InputManager`; those wrappers return bool-style `1` on success. The lower SessionManager wrappers
were originally treated as NRResult-style `0` success. Rust now accepts both `0` and `1` while also
requiring non-zero HMD time for clock queries.

Do **not** pass `NrPose` to `libXREALXRPlugin.so`'s `GetHeadPoseAtTime`: the InputManager wrapper
stores four 16-byte vector chunks (`stp q*, q*`) into the output pointer, i.e. a 64-byte
Unity-facing pose block. The compact 7-float `NrPose` path is kept on
`libXREALNativeSessionManager.so::XREALGetHeadPoseAtTime`.

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

## Direct `libnr_loader.so` rendering path (RE / unverified)

`libXREALXRPlugin.so` does not link NR rendering symbols through ELF `NEEDED`. It loads them
with `dlopen`/`dlsym` and stores them in `NRRenderingWrapper::InitWrapper(void*)`. That means
we can bypass the Unity native-plugin stub for compositor work by `dlopen("libnr_loader.so")`
ourselves and resolving the same flat C symbols.

The rendering wrapper table starts at the wrapper object's `+0x18`. The `NativeRendering`
methods dispatch through `this+0x8` to these offsets:

| Offset | Symbol |
|---:|---|
| `0x000` | `NRRenderingCreate` |
| `0x008` | `NRRenderingStart` |
| `0x010` | `NRRenderingStop` |
| `0x018` | `NRRenderingDestroy` |
| `0x020` | `NRRenderingPause` |
| `0x028` | `NRRenderingResume` |
| `0x030` | `NRRenderingGetFormatsCount` |
| `0x038` | `NRRenderingGetFormats` |
| `0x040` | `NRRenderingGetApiRequirements` |
| `0x048` | `NRRenderingGetMaxLayerCount` |
| `0x050` | `NRRenderingGetFrameBufferMode` |
| `0x058` | `NRRenderingDoRenderEx` |
| `0x060` | `NRRenderingGetFramePresentTime` |
| `0x068` | `NRRenderingInitSetFlags` |
| `0x070` | `NRRenderingInitSetFrameBufferMode` |
| `0x078` | `NRRenderingInitSetFrameSubmitMode` |
| `0x080` | `NRRenderingInitSetScreenBufferMode` |
| `0x088` | `NRRenderingWaitFrame` |
| `0x090` | `NRRenderingAcquireFrame` |
| `0x098` | `NRRenderingSetRefreshScreen` |
| `0x0a0` | `NRRenderingInitSetGraphicContext` |
| `0x0a8` | `NRRenderingSetPersistentProtect` |
| `0x0b0` | `NRRenderingGetVulkanInstanceExtensionCount` |
| `0x0b8` | `NRRenderingGetVulkanInstanceExtensions` |
| `0x0c0` | `NRRenderingGetVulkanDeviceExtensionCount` |
| `0x0c8` | `NRRenderingGetVulkanDeviceExtensions` |
| `0x0d0` | `NRRenderingGetVulkanGraphicsDevice` |
| `0x0d8` | `NRRenderingCreateVulkanInstance` |
| `0x0e0` | `NRRenderingCreateVulkanDevice` |
| `0x0e8` | `NRRenderingSetEmbeddedDataMode` |
| `0x0f0` | `NRBufferSpecCreate` |
| `0x0f8` | `NRBufferSpecDestroy` |
| `0x100` | `NRBufferSpecGetSize` |
| `0x108` | `NRBufferSpecSetSize` |
| `0x110` | `NRBufferSpecGetSamples` |
| `0x118` | `NRBufferSpecSetSamples` |
| `0x120` | `NRBufferSpecSetTextureFormat` |
| `0x128` | `NRBufferSpecSetMultiviewLayers` |
| `0x130` | `NRBufferSpecSetCreateFlags` |
| `0x138` | `NRBufferSpecSetExternalSurfaceFlag` |
| `0x140` | `NRBufferSpecSetExternalSurfaceHolder` |
| `0x148` | `NRSwapchainCreate` |
| `0x150` | `NRSwapchainCreateEx` |
| `0x158` | `NRSwapchainCreateAndroidSurface` |
| `0x160` | `NRSwapchainDestroy` |
| `0x168` | `NRSwapchainSetBuffers` |
| `0x170` | `NRSwapchainGetBufferCount` |
| `0x178` | `NRSwapchainAcquireBuffer` |
| `0x180` | `NRSwapchainGetRecommendBufferCount` |
| `0x188` | `NRSwapchainGetBuffers` |
| `0x190` | `NRSwapchainUpdateExternalSurface` |
| `0x198` | `NRBufferViewportCreate` |
| `0x1a0` | `NRBufferViewportDestroy` |
| `0x1a8` | `NRBufferViewportGetSourceUV` |
| `0x1b0` | `NRBufferViewportSetSourceUV` |
| `0x1b8` | `NRBufferViewportGetTransform` |
| `0x1c0` | `NRBufferViewportSetTransform` |
| `0x1c8` | `NRBufferViewportGetSourceFov` |
| `0x1d0` | `NRBufferViewportSetSourceFov` |
| `0x1d8` | `NRBufferViewportSetSceneNearFar` |
| `0x1e0` | `NRBufferViewportSetDepthData` |
| `0x1e8` | `NRBufferViewportGetTargetComponent` |
| `0x1f0` | `NRBufferViewportSetTargetComponent` |
| `0x1f8` | `NRBufferViewportGetSwapchain` |
| `0x200` | `NRBufferViewportAddSwapchain` |
| `0x208` | `NRBufferViewportSetSwapchain` |
| `0x210` | `NRBufferViewportGetMultiviewLayer` |
| `0x218` | `NRBufferViewportSetMultiviewLayer` |
| `0x220` | `NRBufferViewportSetFlags` |
| `0x228` | `NRBufferViewportSetType` |
| `0x230` | `NRBufferViewportSetQuadSize` |
| `0x238` | `NRBufferViewportSetPerceptionId` |
| `0x240` | `NRBufferViewportSetFocusPlane` |
| `0x248` | `NRFrameCreate` |
| `0x250` | `NRFrameDestroy` |
| `0x258` | `NRFrameSetColorTextures` |
| `0x260` | `NRFrameSetColorTextureType` |
| `0x268` | `NRFrameSetRenderingPose` |
| `0x270` | `NRFrameSetPresentTime` |
| `0x278` | `NRFrameSetDevicePresentTime` |
| `0x280` | `NRFrameSetFlag` |
| `0x288` | `NRFrameSetFocusPlanePoint` |
| `0x290` | `NRFrameSetFocusPlaneNormal` |
| `0x298` | `NRFrameAcquireBuffers` |
| `0x2a0` | `NRFrameSubmit` |
| `0x2a8` | `NRFrameGetViewportCount` |
| `0x2b0` | `NRFrameGetBufferViewport` |
| `0x2b8` | `NRFrameSetBufferViewport` |
| `0x2c0` | `NRFrameCompose` |
| `0x2c8` | `NRFrameSendMetaData` |
| `0x2d0` | `NRFrameSetStartTime` |

Observed call signatures from `NativeRendering` wrappers:

```c
// RE / unverified. All handles are opaque uint64_t-sized values in the Unity plugin.
int NRRenderingCreate(uint64_t* out_rendering);
int NRRenderingStart(uint64_t rendering);
int NRRenderingStop(uint64_t rendering);
int NRRenderingDestroy(uint64_t rendering);
int NRRenderingPause(uint64_t rendering);
int NRRenderingResume(uint64_t rendering);
int NRRenderingInitSetGraphicContext(uint64_t rendering, const NRGraphicContext* context);
int NRRenderingInitSetScreenBufferMode(uint64_t rendering, int mode);
int NRRenderingSetEmbeddedDataMode(uint64_t rendering, int mode);
int NRRenderingGetFrameBufferMode(uint64_t rendering, int* out_mode);

int NRBufferSpecCreate(uint64_t rendering, uint64_t* out_spec);
int NRBufferSpecDestroy(uint64_t rendering, uint64_t spec);
int NRBufferSpecSetSize(uint64_t rendering, uint64_t spec, uint32_t width, uint32_t height);
int NRBufferSpecSetTextureFormat(uint64_t rendering, uint64_t spec, int format);
int NRBufferSpecSetSamples(uint64_t rendering, uint64_t spec, uint32_t samples);
int NRBufferSpecSetCreateFlags(uint64_t rendering, uint64_t spec, uint64_t flags);

int NRSwapchainCreate(uint64_t rendering, uint64_t buffer_spec, uint64_t* out_swapchain);
int NRSwapchainCreateEx(uint64_t rendering, uint64_t buffer_spec, uint64_t* out_swapchain);
int NRSwapchainCreateAndroidSurface(
    uint64_t rendering,
    uint64_t swapchain,
    void** out_surface,
    void** out_native_window_or_holder);
int NRSwapchainDestroy(uint64_t rendering, uint64_t swapchain);
int NRSwapchainSetBuffers(uint64_t rendering, uint64_t swapchain, uint32_t count, void** buffers);
int NRSwapchainGetRecommendBufferCount(uint64_t rendering, uint64_t swapchain, uint32_t* out_count);

int NRBufferViewportCreate(uint64_t rendering, uint64_t* out_viewport);
int NRBufferViewportDestroy(uint64_t rendering, uint64_t viewport);
int NRBufferViewportSetType(uint64_t rendering, uint64_t viewport, int type);
int NRBufferViewportSetTargetComponent(uint64_t rendering, uint64_t viewport, int component);
int NRBufferViewportSetTransform(uint64_t rendering, uint64_t viewport, const NRTransform* transform);
int NRBufferViewportSetSourceUV(uint64_t rendering, uint64_t viewport, const NRRectf* uv);
int NRBufferViewportSetSourceFov(uint64_t rendering, uint64_t viewport, const NRFov4f* fov);
int NRBufferViewportSetSceneNearFar(uint64_t rendering, uint64_t viewport, float near_z, float far_z);
int NRBufferViewportSetSwapchain(uint64_t rendering, uint64_t viewport, uint64_t swapchain);
int NRBufferViewportAddSwapchain(uint64_t rendering, uint64_t viewport, uint64_t swapchain);
int NRBufferViewportSetQuadSize(uint64_t rendering, uint64_t viewport, float width, float height);
int NRBufferViewportSetMultiviewLayer(uint64_t rendering, uint64_t viewport, uint32_t layer);
int NRBufferViewportSetFlags(uint64_t rendering, uint64_t viewport, uint64_t flags);

int NRFrameCreate(uint64_t rendering, uint64_t* out_frame);
int NRFrameDestroy(uint64_t rendering, uint64_t frame);
int NRFrameAcquireBuffers(uint64_t rendering, uint64_t frame, uint64_t* out_buffers, uint32_t* out_count);
int NRFrameSetBufferViewport(uint64_t rendering, uint64_t frame, uint32_t index, uint64_t viewport);
int NRFrameCompose(uint64_t rendering, uint64_t frame);
int NRFrameSubmit(uint64_t rendering, uint64_t frame);
int NRFrameSendMetaData(uint64_t rendering, uint64_t frame, uint64_t a, uint64_t b, const void** data, uint32_t* sizes);
```

Likely minimal compositor sequence, mirroring `DisplayManager`:

1. `dlopen("libnr_loader.so", RTLD_NOW | RTLD_LOCAL)` and resolve the symbols above.
2. Initialize the XREAL/NR session first (`InitUserDefinedSettings`/`CreateSession` or the lower
   `NRAPICreate` path still needs separate RE).
3. Set rendering init parameters before `NRRenderingCreate` where needed:
   `NRRenderingInitSetGraphicContext`, `NRRenderingInitSetScreenBufferMode`,
   `NRRenderingSetEmbeddedDataMode`.
4. `NRRenderingCreate` then `NRRenderingStart`.
5. Create `NRBufferSpec` -> `NRSwapchain` -> `NRBufferViewport`.
6. Per frame: `NRFrameCreate`, fill viewports with `NRFrameSetBufferViewport`,
   `NRFrameCompose`, `NRFrameSubmit`, then destroy/reuse the frame according to device behavior.

Open questions:

- Whether `NRRenderingCreate` can succeed without the higher-level XREAL session bootstrap.
- Exact `NRGraphicContext`, `NRTransform`, `NRRectf`, `NRFov4f`, and enum layouts.
- Whether Godot GLES textures can be passed through `NRSwapchainSetBuffers` directly or need
  `ANativeWindow`/external surface mode.
- Whether quad/compositor layers need `NRFrameSendMetaData` or only buffer viewports.

2026-06-27 device probe:

- Holding the `NRRendering` handle open reaches `NRRendering RUN`, `Start create surface`,
  `OnDisplaySurfaceCreate`, and `DisplayOnShow`.
- Unity differs immediately after `NRRendering RUN`: it logs `SetEmbeddedDataMode`,
  `Get screen buffer mode:1`, `CreateBufferSpec: 1968 1134`, `CreateSwapchainEx`,
  recommended buffer count `7`, and then creates seven `1968x1134` textures.
- Godot previously logged `Get screen buffer mode:0` and did not create a buffer spec,
  swapchain, or textures. As a narrow RE probe, the Rust path now calls
  `NRRenderingInitSetScreenBufferMode(rendering, 1)` after `NRRenderingCreate` and
  `NRRenderingSetEmbeddedDataMode(rendering, 2)` after `NRRenderingStart`. Both calls are
  non-fatal until the enum values and exact timing are verified.
- If the glasses still remain blank/phone-only after this probe, the next missing piece is
  actual frame submission: `NRBufferSpec` -> `NRSwapchainCreateEx` ->
  `NRSwapchainSetBuffers`/external surface -> `NRBufferViewport` -> `NRFrameSubmit`.
- Follow-up probe: after persistent `NRRenderingStart`, Rust now creates an `NRBufferSpec` at
  Unity-observed size `1968x1134`, sets samples to `1`, calls `NRSwapchainCreateEx`, reads
  `NRSwapchainGetRecommendBufferCount`, then retains the swapchain/spec until shutdown.
  Expected Unity-observed count on XREAL One Pro is `7`. This is still only a capability probe;
  it does not submit Godot frames.
- `NRSwapchainCreateAndroidSurface` resolves, but an automatic call against the retained
  swapchain caused SIGABRT on the Godot GL thread before returning. Keep the binding dormant
  until the required call timing, thread, ownership, and expected output object types are
  understood. Nebula/Unity later logs `Get android surface!` from its own path, so this is likely
  not a simple arbitrary-time probe.
- `NRBufferViewportCreate` + `NRBufferViewportSetSwapchain` is now probed after swapchain
  creation. This intentionally avoids viewport type/FOV/transform/frame submit until the enum and
  struct layouts are confirmed.
- The viewport probe now also sets low-risk defaults: type `0`, target component `0`,
  full-source UV `{0,0,1,1}`, near/far `0.1/1000.0`, and flags `0`. FOV/transform remain
  unconfirmed and are not set yet.
- The viewport probe now also tries an approximate one-eye `NRFov4f` derived from One Pro
  calibration logs (`1920x1080`, `fx/fy ~= 2190/2215`): left/right about `+-0.414 rad`,
  up/down about `+-0.239 rad`. The struct order and units are still RE / unverified.
- The viewport probe now also sets an identity `NRTransform` using the same rotation-first,
  position-second 7-float layout assumed for `NRPose`.
- The viewport probe now creates two viewports for stereo and attaches them to frame indices
  `0` and `1`, using target component candidates `1` and `2`.
- The frame probe now calls `NRFrameAcquireBuffers` immediately after `NRFrameCreate`, before
  assigning viewports, to test whether compose requires the acquire step.
- `NRFrameCreate` + `NRFrameSetBufferViewport` + `NRFrameCompose` is probed without
  `NRFrameSubmit`. This checks the frame object path while avoiding compositor submission until
  real color buffers are wired.
- GLES texture probe: Rust now `dlopen`s `libGLESv3.so`, creates the Unity-observed recommended
  number of empty `1968x1134` RGBA8 `GL_TEXTURE_2D` textures, and passes their texture IDs to
  `NRSwapchainSetBuffers`. This is not Godot frame capture yet; it only checks whether XREAL
  accepts GLES texture IDs as swapchain buffers.
- Latest frame result: `NRFrameCreate` returns status `1` before any viewport assignment. The
  `libnr_loader.so` export is only a trampoline: it loads a separate frame-wrapper global and
  returns `1` when the table or slot is null. In the same process the rendering-wrapper table is
  initialized (`NRRendering*`, swapchain, viewport calls work), but the direct frame-wrapper table
  is not. Treat direct `NRFrame*` exports as blocked until the missing initializer is found.
- Unity's public `libXREALXRPlugin.so` exports provide an alternate path:
  `InitializeRendering(void)` calls `DisplayManager::InitializeRendering()`, and
  `CreateFrame(void)->bool` calls `DisplayManager::CreateFrame()`. Internally this constructs
  `NativeRendering` and calls the rendering wrapper table at offset `0x248` for frame creation,
  rather than the public `libnr_loader.so` frame-wrapper global. This is a candidate path, but not
  Godot texture submission by itself.
- `GetFrameMetaData(void)` is the paired DisplayManager export used by Unity's display path.
  Disassembly shows it calls `DisplayManager::SetBufferViewport()` and then the NativeRendering
  metadata sender, returning a pointer plus byte count in registers. The Rust binding models this
  as a 16-byte `{ptr, size}` return struct; this is RE / unverified and should be checked on
  device by the startup `unity_display_frame_probe` log.
- Unity XR display provider callback table from relocations at `0xd46b8`:
  offset `+0x00` is provider `user_data`, then:
  - `0x67688`: graphics-thread start -> `DisplayManager::GfxThreadStart(UnityXRRenderingCapabilities*)`
  - `0x676a0`: submit current frame -> `DisplayManager::SubmitCurrentFrame()`
  - `0x676b0`: populate next frame desc ->
    `DisplayManager::PopulateNextFrameDesc(void*, UnityXRFrameSetupHints const*, UnityXRNextFrameDesc*)`
  - `0x676e0`: graphics-thread stop -> `DisplayManager::GfxThreadStop()`
- `PopulateNextFrameDesc` wrapper ABI is now clearer from disassembly. The wrapper receives
  `(context, user_data, hints*, desc*)`, preserves `x2` as `hints` and `x3` as `desc`, then calls
  the internal `DisplayManager` method. The internal method reads from `hints` at least through
  `+0x50`, and writes `desc` through at least `+0x584`; a small dummy struct is therefore unsafe.
  Observed accesses include:
  - `hints`: bytes/floats/words at `+0x00`, `+0x04`, `+0x0c`, `+0x1c`, `+0x20`, `+0x28`,
    `+0x2c`, `+0x34`, `+0x3c`, pointer at `+0x50`.
  - `desc`: reads at `+0x584`, writes at `+0x00`, `+0x04`, `+0x08`, `+0x14`, `+0x24`,
    `+0x28`, `+0x30`, `+0x38`, `+0x3f0`, `+0x410`, `+0x420`, `+0x450`, `+0x580`.
- Godot now stores the graphics-thread callback table and invokes only `GfxThreadStart` after the
  input provider `start` callback. The capabilities struct is modeled as a zeroed 64-byte buffer;
  disassembly currently shows XREAL writes only offsets `0` and `2`, but the real Unity layout is
  still RE / unverified. `PopulateNextFrameDesc` and `SubmitCurrentFrame` are not called yet
  because their Unity structs are larger and must be mapped before use.
- Updated device result for the public `InitializeRendering()` probe: after the fake Unity XR
  lifecycle callbacks create/start `NativeHMD` and `NativePerception`, a one-shot
  `InitializeRendering()` call no longer crashes. It still returns `CreateFrame=false`, with
  `GetFrameMetaData()` returning `{ptr=null, size=0}`. So the earlier null-HMD crash is fixed, but
  this path still lacks the Unity frame state required for real presentation.
- Reference Unity project (local, not in this repo): the reference app uses the XREAL Unity package through
  `XREALXRLoader`, which creates and starts Unity's `XRDisplaySubsystem` named `XREAL Display`.
  Application C# reads render information through Unity APIs such as `XRDisplaySubsystem`
  `GetRenderPass()` / `GetRenderParameter()`; it does not call `CreateFrame` or
  `GetFrameMetaData` directly. Therefore a Godot port cannot be completed by copying app-side
  C# calls. We need either a fuller fake Unity XR display provider callback surface or a direct
  `NRRendering*` texture submission path.

2026-06-27 Godot 4.7 Java bridge result:

- Godot 4.7's `JavaClassWrapper` and `AndroidRuntime` are available in the exported Android app.
  Calling `JavaClassWrapper.wrap("com.godot.game.XrealBridge")` from `demo/main.gd`, then
  dispatching `XrealBridge.register(activity)` and
  `XrealBridge.startCompanionOnXrealDisplayIfNeeded(activity)` through
  `activity.runOnUiThread(android_runtime.createRunnableFromGodotCallable(...))` works.
- Device log confirms the fallback path:
  `[demo] XrealBridge registered via JavaClassWrapper`.
- In that run Android still exposed only the internal display:
  SurfaceFlinger display `4630946175150030210`, `dumpsys display` logical display count `1`.
  The bridge therefore logged `display-routing-v3: no XREAL display available for companion
  Activity`, and `CreateSession(false)` returned false.
- Interpretation: the JavaClassWrapper fallback fixes/validates the Godot-to-Java bridge path,
  but it does not replace the need for Android to detect the XREAL glasses as a secondary display,
  and it does not solve native frame submission.

## Unity APK display-routing reference

Reference APK: the reference app's build (local, not in this repo).
Decoded with `apktool` for local inspection (the decoded tree is local-only, not in this repo).

Manifest differences that matter:

- Unity declares `ai.nreal.activitylife.NRXRActivity`, `NRShadowActivity`,
  `ai.nreal.activitylife.UnityPlayerActivity`, and `ai.nreal.activitylife.NRFakeActivity`.
- Unity sets `nr_features=multiResume`.
- Unity declares `ai.nreal.sdk.MediaProjectionService`.
- Unity declares `uses-native-library android:name="libnr_api.xreal.so" required="false"`.

Godot status:

- `uses-native-library libnr_api.xreal.so`, `nreal_sdk=true`, `supportDevices`,
  `GlassesInitProvider`, and the XREAL `.so` files are present after manifest/AAR merge.
- `nr_features=multiResume` is safe and now mirrored.
- `NRXRActivity`/`NRFakeActivity`/`NRXRApp` are intentionally not included because the
  available `nractivitylife` implementation directly references `com.unity3d.player.UnityPlayer`.
  The current Godot AAR set does not contain these classes.
- `MediaProjectionService` is also not in the current Godot AAR set, so declaring it would create
  an invalid manifest entry unless the owning AAR is added.
- `com.xreal.sdk.display.GlassesDisplay` / `nrdisplay.jar` was not found in the inspected Unity
  APK or the local package copy. The visible display path is instead hidden inside
  `libXREALXRPlugin.so` (`DisplayManager`, `NativeDisplay`, `NativeRendering`) plus the public
  `NRRendering*` exports in `libnr_loader.so`.

Relevant Unity log sequence:

1. `NRXRApp tryStartFakeActivity`.
2. `NRXRApp startActivity on display: Display id 15 ... XREAL One Pro`.
3. `NRFakeActivity onResume: isXRDisplay=true`.
4. Later, native display startup:
   `DisplayManager DisplayStart`, `NativeDisplay Start`, `NRDisplay START!`,
   `start GlassesDisplay in second screen`, `NRDisplay RUN!`,
   `NativeRendering Start`, swapchain creation, `OnDisplaySurfaceCreate`, `DisplayOnShow`.

Interpretation:

- Launching an Android activity or presentation on display 15 is only the Java-side display
  bootstrap. Unity still relies on its XREAL native display/rendering pipeline to reach
  `NRDisplay START/RUN`.
- The Godot log currently reaches glasses detection and `NRDisplaySubmitState_SetCallback`, but
  not `NRFakeActivity` or `NRDisplay START/RUN`, so the missing part is the native display
  pipeline, not only display discovery.
- The current Godot Android experiment logs `display-routing-v3` and starts
  `XrealCompanionActivity` on the XREAL display. This mirrors only the `NRFakeActivity`
  display-selection shape and deliberately avoids Unity's `UnityPlayer` references.
- The earlier Godot-side whole-activity relaunch experiment is not Unity-like and was removed
  because it causes Godot startup to abort before `_start_success` is reached.
- If `XrealCompanionActivity` appears in logcat but `DisplayManager DisplayStart` /
  `NRDisplay START/RUN` still does not, the next blocker is native compositor startup rather than
  Android display routing.
- If the app starts before Android exposes the XREAL display, `XrealBridge` logs
  `no XREAL display available` and the session remains retryable. Once `dumpsys display` shows the
  external XREAL display, force-stopping and restarting the Godot app launches
  `XrealCompanionActivity` on that display and lets `CreateSession(false)` succeed.

## Godot compositor probe status

Device result on XREAL One Pro:

- `adb shell screencap` can capture the glasses framebuffer when the physical display is present.
  Use the physical SurfaceFlinger id, not the logical Android display id:

  ```powershell
  adb shell dumpsys SurfaceFlinger --display-id
  adb shell screencap -p -d 4626964009369245188 /sdcard/godot_xreal_external.png
  adb pull /sdcard/godot_xreal_external.png .\godot_xreal_external.png
  ```

- Godot now reaches `NRDisplay START!`, `NRDisplay RUN!`, `OnDisplaySurfaceCreate`, and
  `DisplayOnShow==`. The external framebuffer capture is valid 3840x1080 but black, so the
  compositor/display path is alive but no Godot texture is being submitted to it yet.
- Current confirmed route with the display already present:
  `XrealBridge` starts `XrealCompanionActivity` on logical display `27`
  (`HDMI画面`, physical SurfaceFlinger display `4626964009369245188`), the companion registers
  an Activity on display `27`, and the native session logs `session_started=true`.
- SurfaceFlinger for the external physical display shows the XREAL/Godot SurfaceView stack, but
  the composed output layer is still `Background for SurfaceView[]`. Capturing
  `adb shell screencap -p -d 4626964009369245188` yields a black 3840x1080 frame.
- Starting the main `GodotAppLauncher` directly on display `27` via `adb shell am start --display
  27 ...` also captures black. Android standard external-display rendering is therefore covered by
  the XREAL SDK SurfaceView; the remaining route is native compositor texture/frame submission.
- Calling the registered Unity XR display `SubmitCurrentFrame` callback repeatedly from Godot's
  GL thread is unsafe without the missing Unity `PopulateNextFrameDesc`/metrics state. It crashed
  in `DisplayManager::SubmitCurrentFrame -> DisplayManager::UpdateMetrics` with a null/invalid
  callback address (`SIGBUS BUS_ADRALN`, fault address `0x13`). Keep that path dormant until the
  frame descriptor ABI and required callbacks are implemented.
- The direct `NRRendering*` probe can create/start rendering, create a swapchain, create GL
  textures, and attach those textures to the swapchain, but current probes still fail before
  presentation:
  - `NRSwapchainGetRecommendBufferCount` returns `7`.
  - `glCreateTextures` generates `7` texture IDs; `NRSwapchainSetBuffers` now returns `Ok(7)`.
  - `NRFrameCreate` returns status `1`, reported by Rust as `Err(-4001)`, when called from the
    current startup timing.
- A one-shot registered Unity XR `PopulateNextFrameDesc` diagnostic now returns status `0` without
  crashing when backed by oversized zeroed buffers (`hints=0x80`, `desc=0x600`). The descriptor
  changes 180 bytes, with observed values:
  - `u32[0x580]=2`, `u32[0x584]=0`.
  - `u32[0x00]=0`, `u32[0x04]=0`.
  - Representative non-zero float-ish fields appear at `0x08`, `0x14`, `0x24`, `0x28`, `0x30`,
    `0x3f0`, `0x410`, and `0x450`.
  - `CreateFrame()` still returns `false` before and after one-shot `SubmitCurrentFrame()`, after
    one-shot public `InitializeRendering()`, and the external SurfaceFlinger capture remains black.

Current interpretation: Android display routing and XREAL display startup are no longer the main
blockers. The remaining blocker is a real frame submission path: either implement enough of
Unity's XR display frame callback surface (`PopulateNextFrameDesc` and metrics), or identify the
correct `NRRendering*` texture/swapchain setup that accepts a Godot GL texture.

## DisplayManager CreateFrame gate — disassembly round b (2026-06-27)

Derived from `llvm-objdump -d libXREALXRPlugin.so` and `--relocs` analysis.

### `DisplayManager::CreateFrame()` (0x6ed90) gate checks

```
ldr x8, [x0, #0x58]       // x8 = DisplayManager->native_rendering (NativeRendering*)
cbz x8, return_false       // if null → return false
ldrb w8, [x8, #0x18]      // NativeRendering+0x18 = started flag (set by NativeRendering::Start())
cbz w8, return_false       // if 0 → return false
adrp x8, 0xdb000
add x8, x8, #0x610        // guard variable for function-local static at 0xdb400
ldarb w8, [x8]
tbz w8, #0, lazy_init     // if not yet initialized → lazy init
ldrb w8, [x8, #0x410]     // 0xdb410 = byte at static+0x10 (gate byte)
cbz w8, return_false       // if 0 → return false
// → proceed to NativeRendering::CreateFrame()
```

So `CreateFrame()` has two main gates:
1. `NativeRendering+0x18 != 0` — set by `NativeRendering::Start()` (called via `GfxThreadStart` →
   `StartOrResume`). Confirmed: the XREAL SDK's own `NRDisplay START/RUN` lifecycle already sets
   this when the XrealCompanionActivity lands on the XREAL display.
2. `libXREALXRPlugin.so + 0xdb410 != 0` — the byte at offset `+0x10` of a function-local static
   (a C++ object at `lib_base + 0xdb400`, initialised once by the lazy-init in `CreateFrame` /
   `InitializeRendering`). This byte starts as **0** and only becomes non-zero when
   `PopulateNextFrameDesc` is called with `desc = lib_base + 0xdb400`.

### NativeRendering vtable (vptr stored in the object)

The "vtable for NativeRendering" symbol at `0xd55f8` holds the ABI header (offset-to-top at
`0xd55e8`, typeinfo ptr at `0xd55f0 → 0x9F2A4`). The actual function pointers start at
`0xd5608` — this is what the object's `vptr` field stores at runtime.

| vptr offset | Address | Function |
|---|---|---|
| `+0x00` | 0xd5608 → 0xa03e4 | `NativeRendering::~NativeRendering()` (in-place) |
| `+0x08` | 0xd5610 → 0xa0468 | `NativeRendering::~NativeRendering()` (deleting) |
| `+0x10` | 0xd5618 → 0xa04f8 | `NativeRendering::Create()` |
| `+0x18` | 0xd5620 → 0xa06ac | `NativeRendering::Start()` |
| `+0x20` | 0xd5628 → 0xa4f60 | `NativeRendering::Pause()` |
| `+0x28` | 0xd5630 → 0xa5114 | `NativeRendering::Resume()` |
| `+0x30` | 0xd5638 → 0xa52c8 | `NativeRendering::Stop()` |
| `+0x38` | 0xd5640 → 0xa547c | *(unresolved)* |
| `+0x40` | 0xd5648 → 0xa5630 | `IntegratedSubsystem<NRRenderingWrapper>::StartOrResume()` |

`GfxThreadStart` (called by `start_registered_providers`) calls `vptr[+0x40]` =
`IntegratedSubsystem::StartOrResume()`. `StartOrResume` reads `this+0x18` (the started flag):
- if `0` → calls `vptr[+0x18]` = `NativeRendering::Start()` → calls `NRRenderingStart(handle)`
  → on success sets `NativeRendering+0x18 = 1`.
- if `1` → calls `vptr[+0x28]` = `NativeRendering::Resume()`.

`NativeRendering::DestroyFrame` (at 0xa2224) calls `wrapper[+0x250]` = `NRFrameDestroy`.
`NativeRendering::CreateFrame` (at 0xa2060) calls `wrapper[+0x248]` = `NRFrameCreate`.

### `lib_base + 0xdb400` — the function-local static

This is a C++ object (vtable at `0xd4610`) shared by `CreateFrame()`, `InitializeRendering()`,
and `SubmitCurrentFrame()`. It is initialised lazily (guarded by byte at `lib_base + 0xdb610`).
Layout (confirmed by `stp`/`strh`/`str` init sequences):

```
+0x00: vtable ptr (0xd4610)
+0x08: 0
+0x10: 0 (strh wzr — the gate byte read by CreateFrame / SubmitCurrentFrame)
...large zero/1.0f initialisation block...
```

`PopulateNextFrameDesc(ctx, user_data, hints, desc)` called with `desc = lib_base + 0xdb400`
writes `0xa6` to `desc+0x10` on XREAL One Pro. After this call `CreateFrame()` passes the second
gate. Device-confirmed: `gate_byte_before=0x00, gate_after=0xa6`.

### `DisplayManager+0x120` — SDK-managed frame handle

`CreateFrame()` reads `DisplayManager+0x120` to destroy the previous frame before creating a
new one. The value is set by a previous `NativeRendering::CreateFrame()` call and cleared by
`SubmitCurrentFrame()`. **Critical:** the XREAL SDK has its own rendering thread (GLThread, tid
distinct from the Godot thread) that calls `DisplayManager::CreateFrame()` and manages this field.
Calling `CreateFrame()` or `SubmitCurrentFrame()` from Godot's thread races with the SDK's
rendering thread; `NativeRendering::DestroyFrame` returns an error (frame in use) and
`LogHelper::Error` crashes at `SIGSEGV fault addr 0xb9a40998bac55c8a` (MTE-tagged frame handle
interpreted as a string pointer). Do NOT call `CreateFrame` or `SubmitCurrentFrame` directly.

### `SubmitCurrentFrame()` gate branches (0x685fc)

```c
// first check:
if (0xdb410 == 0):
    SetBufferViewport();                         // set up overlay viewports
    NativeRendering::SubmitFrame(rendering, DisplayManager+0x120, ...);  // SUBMIT path
else:  // 0xdb410 != 0 (after PopulateNextFrameDesc writes to 0xdb400)
    NativeRendering::DestroyFrame(rendering, DisplayManager+0x120);  // CLEANUP path
// after both paths:
if (0xdb410 == 0):
    UpdateMetrics();  // CRASHED in previous probe (null callback)
    NativeRendering::FrameWait();
else:
    // skip UpdateMetrics/FrameWait
WaitForTargetFrameRate();
return 0;
```

So `0xdb410 != 0` (after populate with global desc) → `SubmitCurrentFrame` DESTROYS the current
frame instead of submitting it. The actual rendering path goes through `0xdb410 == 0` →
`SetBufferViewport` → `NativeRendering::SubmitFrame`. Godot must not pre-populate 0xdb400 before
the SDK's rendering thread calls `SubmitCurrentFrame`.

### `PopulateNextFrameDesc()` desc output fields (device probe, XREAL One Pro)

Called as `PopulateNextFrameDesc(ctx, user_data, zero_hints, desc=lib_base+0xdb400)`:
```
status = 0
desc+0x10 = 0xa6 (gate byte, non-zero → render passes present)
desc+0x18 = 0xb9a40998bac55c8a  // MTE-tagged NativeRendering frame handle
                                   // = same value as DisplayManager+0x120
desc+0x580 = 0x0000000000000002  // u32[0x580]=2 → confirmed render pass count = 2 (stereo)
desc+0x08, +0x3f0  = floats (likely projection matrix / IPD data)
desc+0x24, +0x28, +0x30 = floats (likely per-eye parameters)
desc+0x410, +0x450 = floats / swapchain references (unverified)
```

### Library base address derivation

`lib_base = runtime_addr(CreateFrame) - 0x53bd8`
→ `desc_ptr = lib_base + 0xdb400`
→ confirmed on device: `desc_ptr = 0x7a9fd68400`, `lib_base = 0x7a9f224628`

Computed in `XrealNative::load()` via `transmute::<FnCreateFrame, usize>(create_frame_fn) - 0x53bd8`.

### Display state after PopulateNextFrameDesc probe (2026-06-27)

- `PopulateNextFrameDesc(desc=lib_base+0xdb400)`: status=0, gate_byte=0xa6. ✓
- `session_started=true`, `tracking_type=6DoF`, `tracking_state=2`. ✓
- SurfaceFlinger capture of XREAL One Pro (physical id `4626964009369245188`):
  **3840x1080 stereo, shows Android home launcher** (previously black).
  Progress: XREAL compositor is active and compositing Android background.
  Godot scene content is not yet submitted to the compositor.

### NativeGlasses null crash — root cause and fix (2026-06-27)

`SessionManager::HandleActionCallback` (0x849a8) reads `SessionManager+0x60`
(`NativeGlasses*`) without a null check:
```
ldr x0, [x0, #0x60]   // NativeGlasses* (may be null)
bl NativeGlasses::GetActionData(action_id)
```
When null, `GetActionData+28` (or +44 as seen by libsigchain) crashes with
`SIGSEGV fault addr 0x8`.

Known writers to `SessionManager+0x60`:
| Address | Function | Action |
|---|---|---|
| `0x8454c` | `SessionManager::CreateSession` | stores new `NativeGlasses*` |
| `0x7987c` | `InputManager`-related | stores (non-null) |
| `0x667c8` | `SessionManager::~SessionManager` (destructor) | clears to null |
| `0x848bc` | `SessionManager::DestroySession` | clears to null |
| `0xaa9f0` | `recursive_timed_mutex::unlock` (mutex state field, NOT SessionManager) | stores null |

The crash is triggered ~6 seconds after session creation when the Nebula service
sends a periodic action callback (action_id=2026) on its own IPC thread. At that
point `SessionManager+0x60` may be null because:
- The `TSingleton<SessionManager>` on the Nebula callback thread returns a different
  (uninitialised) instance than the one we created via `CreateSession`, OR
- `SessionManager::~SessionManager` was called internally.

**SIGSEGV handler approach failed**: Android ART installs `libsigchain` which
intercepts SIGSEGV signals BEFORE calling user `sigaction` handlers. The process
is terminated by libsigchain/ART before our handler runs. Confirmed: no output
to `/data/local/tmp/xreal_guard.txt` file written from signal handler.

**Code-patch fix (device-confirmed working)**:
Replace `bl NativeGlasses::GetActionData` at `lib_base + 0x849c4` with a BL to
`null_safe_handle_action` trampoline in `godot_xreal.so`:
```asm
null_safe_handle_action:
    ldr x0, [x0, #0x60]    // replicate original ldr
    cbz x0, 1f              // if null → skip
    ret                     // non-null: return to 0x849c0 → proceed normally
1:
    add x30, x30, #12       // skip: mov x19,x1 + bl GetActionData + mov x21,x0
    mov x21, xzr            // x21 = 0 (null action data)
    ret                     // return to 0x849cc (LogHelper::Info is safe)
```
`mprotect(RWX) → write BL → mprotect(RX) → DC CVAU + IC IVAU + ISB` (cache flush).
Applied once per process via `OnceLock` in `XrealNative::load()`.
Device-confirmed: app survives 25 s+ past the 6-second crash window with the patch.

---

## Display routing — getting Godot content on the XREAL glasses (2026-06-28)

### Root cause: CreateDisplayLayer always created DummyDisplayOverlay

`CreateDisplayLayer` at `lib_base + 0x6dc98` contains:
```asm
cbz w8, 0x6dd18   // if 0xdb410 == 0 → DummyDisplayOverlay (no textures, no swapchain)
```
`0xdb410` is only set non-zero by `PopulateNextFrameDesc(lib_base+0xdb400)`. Since we never
call that (it would crash `SubmitCurrentFrame` later), this branch always took the dummy path.

### Fix: `cbz → nop` patch at `lib_base + 0x6dc98`

Patched in `src/signal_guard.rs::patch_create_display_layer()`, called once from
`XrealNative::load()` via `PATCHED.get_or_init`. Forces the real `DisplayOverlay` branch
unconditionally, so `GfxThreadStart → CreateDisplayLayer → OverlayBase::CreateBuffer →
NativeRendering::CreateSwapchainEx` runs and creates 7 GL textures registered with the
XREAL compositor via `SetSwapChainBuffers`.

First `PopulateNextFrameDesc` call after `GfxThreadStart` returned `(status=0, nonzero=124)`
confirming the swapchain was initialized.

### Android display routing — Godot directly on the glasses

XREAL One Pro appears as Android display 38 (name `HDMI画面`, 3840×1080).

Initial attempt: `XrealCompanionActivity` (simple `TextView`) launched on display 38.
Confirmed working via `dumpsys window displays`: `ActivityRecord{…XrealCompanionActivity}` on
`mDisplayId=38`.

Final solution (`GodotApp.java::tryRedirectToXrealDisplay()`):
- If `GodotApp.onCreate()` is on display 0 AND an XREAL display is available →
  launch a new `GodotApp` instance on the glasses display via
  `ActivityOptions.setLaunchDisplayId()` → `finish()` the display-0 trampoline.
- The display-38 `GodotApp` detects it is not on display 0, skips redirect, runs normally.

Result (device-confirmed via `dumpsys SurfaceFlinger`):
```
SurfaceView[com.example.godotxreal/com.godot.game.GodotApp](BLAST) w/h:3840×1080
layerFilter={layerStack=38 toInternalDisplay=false}
```
Godot's scene renders at 3840×1080 on display 38 (XREAL glasses). The phone screen (display 0)
shows no Godot UI — only one engine instance runs, on the glasses.

### XREAL SDK session on display 38

`XrealBridge.register()` is called from the display-38 `GodotApp.onCreate()` (after
`super.onCreate()`), handing the glasses Activity to the native XREAL bootstrap. The XREAL
SDK session initialises normally with `session_started=true`, `tracking_state=2`.

`run_frame_tick()` (called every frame from `node.rs::process()`) calls
`PopulateNextFrameDesc` with a temp buffer each frame so the SDK's GLThread can call
`SubmitCurrentFrame` with the acquired frame handle.

### Display ID note

`dumpsys SurfaceFlinger --display-id` reports display ID `4626964009369245188` (HWC display 1)
for XREAL One Pro, but Android's display handle integer ID seen by apps is `38`. Use `38` for
`setLaunchDisplayId()` and `screencap -d`.

`screencap -d 38` returns 0 bytes — external physical displays (HDMI) are not capturable via
screencap. Use `dumpsys SurfaceFlinger` to verify layer presence instead.

### Per-frame rendering architecture (2026-06-27 analysis)

The XREAL SDK starts a **rendering loop thread** (GLThread, inside the Godot process) when
`NativeRendering::Start()` runs via `GfxThreadStart`. This thread calls:
- `SubmitCurrentFrame()` at 60 fps (SDL-style, not driven by our code)
- `DisplayManager+0x120` = current frame handle (managed by the SDK thread)

Godot's rendering runs on Godot's own GL thread. There are TWO concurrent GL threads.

To get Godot content on the XREAL display, either:
1. **Take over frame submission** (stop the SDK thread, drive frames from Godot's GL thread):
   - `populate_next_frame_desc(ctx, user_data, hints, desc)` → XREAL fills desc with swapchain textures
   - Read texture IDs from desc (e.g. at desc+0x3f0 / desc+0x410 / desc+0x450)
   - Render Godot scene to those textures
   - `submit_current_frame(ctx, user_data)` → submit to XREAL compositor
2. **Direct NRRendering path** (still blocked: `NRFrameCreate` through libnr_loader.so direct export fails; `NativeRendering::CreateFrame()` through the rendering wrapper at offset 0x248 might work)

Known patches needed for per-frame rendering:
- `HandleActionCallback+20` (`ldr x0,[x0,#0x60]`): replace with `bl null_safe_handle_action`.
  Prevents null NativeGlasses crash when XREAL Nebula sends action callbacks.
  CONFIRMED WORKING: app survives 35+ seconds with this patch.
- `CreateFrame()+56` (`cbz x1, skip_destroy` at lib+0x6edc8): replace with `b skip_destroy`
  (`0x14000003`). This skips `NativeRendering::DestroyFrame` when `DisplayManager+0x120` has
  a live SDK-managed frame handle (0xb9a40998bac55c8a). Without this patch, calling
  `CreateFrame()` crashes when an existing frame is in flight.

### Unity reference log analysis (2026-06-27, log-unity)

Full sequence from `log-unity` (Unity 6000.0 / XREAL SDK 3.1.0 / XREAL One Pro):

**Key parameters (from `InitUserDefinedSettings`):**
- `colorSpace=1, stereoRendering=2, trackingType=0, supportMonoMode=0, inputSource=0`
- **`stereoRendering=2`** = Single Pass Instanced — NOT 0 (which is what Godot was using).

**GfxThreadStart creates the swapchain (on Unity GL thread 5714):**
```
[XR] [NativeDisplay] Start
NRDisplay START! → NRDisplay RUN!
[XR] [NativeRendering] Start → NRRendering RUN!
Get screen buffer mode:1  frame_buffer_mode:1  frame_submit_mode:1
[XR] [NativeRendering] SetEmbeddedDataMode
[XR] [NativeMetrics] Start / Start Finish
[XR] [NativeRendering] GetFrameBufferMode → stereo mode
[XR] [NativeHMD] GetComponentResolution
[XR] [OverlayBase] CreateBuffer Start
  [XR] [NativeRendering] CreateBufferSpec: 1968 1134
  [XR] [NativeRendering] CreateSwapchainEx
  [XR] [NativeRendering] GetRecommandBufferCount → 7
  [XR] [DisplayManager] CreateTexture: 1968 1134 2 0x0  (×7)
  [XR] [DisplayManager] Init centerDisplay
[XR] [DisplayManager] GfxThreadStart End
```

Note: `GfxThreadStart` is called on **Unity's GL thread**, not the main thread.

**After display surface creation (`OnDisplaySurfaceCreate`):**
```
[XR] [NativeRendering] SetSwapChainBuffers   ← GL thread 5714
NRRendering "Start rendering" / "Start rendering success"
[XR] [NRMetrics] FramePresent: 0 …
[XR] [NRMetrics] FramePresent: 1, DroppedFrame: 57, DisplayRefreshRate: 56
```

**`SetSwapChainBuffers`** (called on GL thread after surface creation) registers 7 GL textures
(1968×1134, format `2` = likely `GL_SRGB8_ALPHA8`) with the XREAL swapchain.  Without this call,
the XREAL compositor has no textures to display.

**Per-frame rendering path (Unity GL thread):**
1. Unity `XRDisplaySubsystem.Update()` calls `GFX_THREAD_PROVIDER.populate_next_frame_desc` →
   XREAL fills `desc` with which swapchain buffer slot to render to this frame.
2. Unity renders the stereo scene to that GL texture (1968×1134, single-pass instanced).
3. Unity calls `GFX_THREAD_PROVIDER.submit_current_frame` →
   `DisplayManager::SubmitCurrentFrame()` → internal frame submission.

**Swapchain buffer mode (`0xdb410` analysis):**
- `0xdb410 == 0` (screen buffer): `SubmitCurrentFrame` takes `SetBufferViewport + SubmitFrame` path
  (actual composite submission). Requires non-null `DisplayManager+0x120`.
- `0xdb410 != 0` (embedded data): `SubmitCurrentFrame` takes `DestroyFrame` path (disposal).
  `CreateFrame()` works in this mode (check 2 passes), but `SubmitCurrentFrame` disposes rather
  than composites.
- Unity keeps `0xdb410 = 0` (screen buffer mode) throughout — `CreateFrame()` is NOT called via
  the public C export by Unity. The internal `NativeRendering::CreateFrame()` path (via rendering
  wrapper at offset 0x248) is used instead, driven by `populate_next_frame_desc` / `SubmitFrame`.

**Critical missing step in Godot (confirmed 2026-06-27):**
`SetSwapChainBuffers` is never called, so the swapchain has no GL textures.
Fix: after `GfxThreadStart`/`OnDisplaySurfaceCreate`, call `NativeRendering::SetSwapChainBuffers`
with 7 RGBA textures at 1968×1134 on Godot's GL thread.  Then render Godot scene to those
textures each frame and call `submit_current_frame`.

The `NativeRendering::SetSwapChainBuffers` is accessible via the internal `DisplayManager` overlay
list (`DisplayManager+0x150/+0x178`), or by keeping a reference to the overlay created in
`GfxThreadStart → CreateDisplayLayer → OverlayBase::CreateBuffer`.

### Next steps (frame submission path)

The XREAL SDK's rendering thread (GLThread) already manages `DisplayManager+0x120` and calls
`SubmitCurrentFrame` internally. The remaining work:
1. Understand `UnityXRNextFrameDesc` layout (which offset holds the per-eye GL texture IDs
   that Godot should render to — likely near `+0x3f0`/`+0x410` based on desc output).
2. In Godot's `_process()` (per-frame), call `PopulateNextFrameDesc` with a Godot-side buffer
   to get the texture IDs, render the Godot scene to those textures, then let the SDK's
   rendering thread call `SubmitCurrentFrame` on its own (no Rust involvement needed).
3. Alternatively, use `DisplayManager::SetBufferViewport()` directly to register Godot's
   swapchain textures as the source for the compositor layers.

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
