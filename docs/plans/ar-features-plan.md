# AR features (Plane / Image / Anchor / Mesh): feasibility survey

Status: **survey only, nothing implemented.** Findings from reading the vendored **XREAL SDK for
Unity 3.1.0** package sources + `llvm-nm`/`strings` on the vendored `libXREALXRPlugin.so`
(2026-07-14). Recommended implementation order: **Plane Detection → Spatial Anchor →
Image Tracking → Depth Mesh (shelve)**.

## TL;DR

| Feature | Native path | Portable to Godot? | Extra runtime payload |
|---|---|---|---|
| Plane Detection | flat C exports in `libXREALXRPlugin.so` | ✅ same pattern as head tracking | none found (likely inside `nr_plugin_6dof`) |
| Spatial Anchor | flat C exports in `libXREALXRPlugin.so` | ✅ same pattern | `nr_spatial_anchor.aar` (`libnr_spatial_anchor.so`, 12.9 MB) |
| Image Tracking | flat C exports + a reference-image database | ✅ but DB format needs RE | `nr_image_tracking.aar` (`libnr_image_tracking.so`, 10.9 MB) + `nr_plugins.json` + DB |
| Depth Mesh | Unity-native `IUnityXRMeshInterface` provider | ⚠️ hard — needs Unity XR SDK emulation | `nr_meshing.aar` (`libnr_meshing.so`, 28.9 MB) |

**No ARCore anywhere.** `package.json` depends only on `com.unity.xr.management` +
`com.unity.xr.core-utils`; zero grep hits for `arcore` / `com.google.ar` in `Runtime/` and
`Editor/`. The "requires AR Foundation" in XREAL's Unity docs is a **C# frontend compile guard**:
every AR-feature C# file starts with `#if XR_ARFOUNDATION` (planes, images, anchors, mesh
extensions alike), because the providers extend AR Foundation base classes (`XRPlaneSubsystem`
etc.). The detection engines are XREAL's own native code; Godot bypasses the C# layer entirely.

## Shared plumbing: the changes struct

Planes, images and anchors all poll the same shape
(`Runtime/Scripts/Android/AR Features/XREALPlaneSubsystem.cs:86`):

```c
struct ARSubsystemChanges {
    void* addedPtr;   int addedCount;
    void* updatedPtr; int updatedCount;
    void* removedPtr; int removedCount;
    int   elementSize;   // stride — runtime validation for our repr(C) structs
};
```

The elements are AR Foundation's public blittable structs (`BoundedPlane`, `XRTrackedImage`,
`XRAnchor` — layouts in Unity's open ARSubsystems source), so the Rust `repr(C)` mirrors can be
written from public code, validated at runtime via `elementSize`. Implement the poll once, reuse
for all three.

## Plane Detection (do first)

C# provider: `XREALPlaneSubsystem.cs` — 113 lines of pure marshalling. Exports (all confirmed
present in our vendored `libXREALXRPlugin.so`):

```
GetPlaneDetectionMode() -> PlaneDetectionMode
SetPlaneDetectionMode(PlaneDetectionMode) -> bool     // horizontal / vertical / both
GetPlaneDetectionChanges(out ARSubsystemChanges)      // elements: BoundedPlane
GetPlaneBoundaryVertexCount(TrackableId) -> int
GetPlaneBoundaryVertexData(TrackableId, void* boundary)
```

Needs 6DoF. No extra `.aar` found — plane detection appears to live in the already-shipped
`nr_plugin_6dof`. Godot surface sketch: `XrealSystem.set_plane_detection_mode()` + a
`planes_changed(added, updated, removed)` signal or an `XrealPlaneTracker` node emitting
per-plane data (pose, size, boundary polygon).

## Spatial Anchor (second)

C# provider: `XREALAnchorSubsystem.cs` (+ `XREALAnchorManagerExtension.cs`). All 9 exports
confirmed in our vendored lib:

```
SetTrackableAnchorEnabled(bool)
SetAnchorMappingFileDirectory(string path)            // persistence dir (must be writable)
AcquireNewTrackableAnchor(Pose, out XRAnchor) -> bool // create at pose
GetTrackableAnchorChanges(out ARSubsystemChanges)     // elements: XRAnchor
SaveTrackableAnchor(TrackableId, ref Guid) -> bool    // persist -> guid
LoadTrackableAnchor(Guid, out XRAnchor) -> bool
RemoveTrackableAnchor(TrackableId) -> bool
RemapTrackableAnchor(TrackableId) -> bool
EstimateTrackableAnchorQuality(TrackableId, Pose, ref XREALAnchorEstimateQuality) -> bool
```

Marshalling known: `Pose` = Vector3 + Quaternion (28 B), `Guid` = 16 B by value,
`TrackableId` = 128-bit. The saved-anchor-id enumeration in C# just lists files in the mapping
dir (no native call). Needs 6DoF (anchors bind to the SLAM map — check
`EstimateTrackableAnchorQuality` before saving). Ship `nr_spatial_anchor.aar` like the other 5
(add to `export_plugin.gd` + `vendor_xreal_libs.ps1`/`.sh`).

## Image Tracking (third — one extra RE chunk)

C# provider: `XREALImageTrackingSubsystem.cs` + `XREALImageDatabase.cs`. Exports confirmed:

```
InitImageTrackingDatabase / SetImageTrackingDatabase(IntPtr) / ReleaseImageTrackingDatabase
GetImageTrackingChanges(out ARSubsystemChanges)       // elements: XRTrackedImage
GetReferenceImage / GetReferenceImageCount
```

Two extra requirements beyond the shared plumbing:

1. **Reference-image database format** — whatever `SetImageTrackingDatabase(IntPtr)` consumes.
   `XREALImageDatabase.cs` + `Marker~/InterMarker.bin` are the leads; Unity bakes it at edit
   time, so Godot needs either a converter or reuse of XREAL's baked file.
2. **`nr_plugins.json`** — the Editor `MarkerTrackingTool.cs` copies
   `{"perception":{"versions":"…","plugins_64":[{"id":"nr_image_tracking_id","path":"libnr_image_tracking.so"}]}}`
   into `StreamingAssets` (= APK `assets/`). That is how the perception pipeline is told to load
   the feature plugin. In Godot: pack the same file into APK assets (an `.aar` can carry an
   `assets/` dir, or extend the export plugin).

## Depth Mesh (shelve)

The odd one out. Only supplementary flat export:
`GetMeshLabels(TrackableId, out label*, out count)` (per-vertex semantic labels,
`XREALMeshSubsystemExtensions.cs`). The mesh data itself does **not** go through flat C exports:

- Unity uses the **engine-built-in `XRMeshSubsystem`** (`XREALXRLoader.cs:192`,
  `CreateSubsystem<XRMeshSubsystemDescriptor>("XREAL Meshing")`).
- `libXREALXRPlugin.so` contains `UnityXRMeshDataAllocator` / `UnityXRMeshInfoAllocator` /
  `UnityXRMeshDescriptor` symbols → the plugin registers a native mesh provider through Unity's
  XR SDK plugin interface (`IUnityXRMeshInterface`), and the **Unity engine** pulls meshes via
  `GetMeshInfos` / `AcquireMesh` callbacks with engine-supplied allocators.

Using it from Godot means emulating that interface: pass a fake `IUnityInterfaces` to
`UnityPluginLoad`, hand out a fake `IUnityXRMeshInterface`, capture the registered provider
callback table, then drive `GetMeshInfos`/`AcquireMesh` ourselves with our own allocators. The
headers (`IUnityXRMeshing.h`) are public, so it is feasible, but it is a different class of work
from the flat-ABI ports and `UnityPluginLoad` may drag in side effects. The alternative route
(obfuscated NR proc table via `NRGetProcAddr`) is against this project's approach. → shelve until
Plane/Anchor/Image are done.

## Open question (all features)

How the feature `.so` get loaded/registered: `nr_plugins.json` covers image tracking, but nothing
equivalent was found for meshing/anchor in the SDK sources, and `libnr_loader.so` / `libnr_api.so`
carry no plaintext hints (lower layer is obfuscated). To be RE'd at implementation time — worst
case `System.loadLibrary` them ourselves and mirror a working Unity APK's layout.
