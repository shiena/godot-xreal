# AR features (Plane / Image / Anchor / Mesh): confirmed C ABI + plan

Status: **Plane detection + Spatial Anchor IMPLEMENTED (2026-07-16, compiles clean; on-device
verification pending). All four features' C ABI is RE-confirmed** (codex, cross-checking the SDK C#
`[DllImport]` sources against `llvm-nm`/AArch64 disassembly of `libXREALXRPlugin.so`). Implementation
order: **Plane (done) → Spatial Anchor (done) → Image Tracking (blob pipeline solved — run XREAL's
`trackableImageTools` CLI; port pending) → Depth Mesh (shelve)**.

**Device capability gate:** `IsHMDFeatureSupported(XREALSupportedFeature)` (flat export, `-> bool`) is
the device-accurate feature check the SDK itself uses — e.g. the **Air 2 Ultra has no RGB camera**, so
`RGB_CAMERA(1)` returns false there (opening it anyway froze the app). Enum: `RGB_CAMERA=1`,
`WEARING_STATUS=2`, `CONTROLLER=3`, `HEAD_TRACKING_ROTATION=4`, `HEAD_TRACKING_POSITION=5` (6DoF).
Exposed as `XrealSystem.is_hmd_feature_supported(feature)` / `is_camera_supported()`.

**Plane-toggle gotcha:** `SetPlaneDetectionMode(mode) -> bool` — the return is **not** a success
signal (the SDK's own `XREALPlaneSubsystem.cs` setter discards it, and it can read false even when the
mode takes). Gate the UI on `is_plane_detection_available()` (ABI resolved) and poll for actual planes,
not on that return.

Every AR export is a flat C `T` symbol (`LibName = "XREALXRPlugin"`) — the **same dlsym pattern** as
the RGB camera / hand tracking already in `src/ffi.rs`. All 21 exports confirmed present.

**Universal thunk pattern (verified for all 21):** each flat export saves its args, calls
`TSingleton<InputManager>::GetInstance()` (`0x47a10`, `this`→x0), shifts the caller's args up one
register (x0→x1, …; x8 sret preserved), and tail-calls the `InputManager::` method. So **the flat-C
signature = the demangled internal minus the leading `this`**, with identical register usage — exactly
like `GetHandJointsPose` (`src/hand_tracking.rs`).

**No ARCore.** `package.json` depends only on `com.unity.xr.management` + `.core-utils`; the
"requires AR Foundation" is a C# `#if XR_ARFOUNDATION` frontend guard. The detection engines are
XREAL's own native code; Godot bypasses the C# layer entirely.

## Shared structs & enums (repr(C), device-confirmed offsets)

### `ARSubsystemChanges` — the changes-poll out-struct (**48 bytes**)
```rust
#[repr(C)] pub struct ArSubsystemChanges {
    added_ptr:   *mut c_void, // 0x00 -> [added_count] element structs
    added_count:   i32,       // 0x08  (pad 0x0c)
    updated_ptr: *mut c_void, // 0x10 -> [updated_count] element structs
    updated_count: i32,       // 0x18  (pad 0x1c)
    removed_ptr: *mut c_void, // 0x20 -> [removed_count] TrackableId  (16 B each, NOT the element!)
    removed_count: i32,       // 0x28
    element_size:  i32,       // 0x2c  runtime-validate == 104 / 80 / 72 (plane / image / anchor)
}
```
- **removed** is a `TrackableId[]` (16 B) — all three internals compute `removed_count = bytes >> 4`
  (÷16), *not* the element stride.
- Pointers alias InputManager's internal cached vectors → **copy out before the next `Get*Changes`**.
- `element_size` is a compile-time constant baked in the `.so`; assert it.

### `TrackableId` (**16 B**, by value in 2 GPRs), `Guid` (**16 B**, opaque blob), `UnityPose` (**28 B**)
```rust
#[repr(C)] pub struct TrackableId { sub_id_1: u64, sub_id_2: u64 }  // planes: sub_id_1=0, sub_id_2=native id
#[repr(C)] pub struct Guid { lo: u64, hi: u64 }                     // .NET Guid; persistence key
#[repr(C)] pub struct UnityPose { position: [f32;3], rotation: [f32;4] }  // pos@0, rot(x,y,z,w)@0xc
```
- **`UnityPose` (28 B, non-HFA) is passed INDIRECTLY** on AArch64 (hidden pointer). In Rust `extern
  "C"`, declare it **by value** — Rust does the indirect pass to match the thunk.
- `NativeView { data: *mut c_void, count: i32 }` (16 B, 2 GPRs) — image DB init.

### Coordinate systems (all features)
Returned poses are already **Unity space** (the SDK bakes the NR→Unity flip). Convert to this port's
display space with the **same conversion the hand tracker uses** (`src/hand_tracking.rs:221`):
**position `(x, -y, -z)`, quaternion `(x, -y, -z, w)`** (the extra Y-negation compensates this port's
inverted-Y eye cameras). Boundary vertices already have **Y negated by the SDK**. Axis signs pending
on-device verification (as head/hand poses were).

## 1. Plane Detection — IMPLEMENTED

Native path is in the already-shipped 6DoF core (`nr_plugin_6dof`) — **no extra `.aar`**. Internally
gated on `IsFeatureSupported(1)`; no-op until 6DoF perception is up.

| Flat C signature | C# DllImport (`XREALPlaneSubsystem.cs`) | internal symbol |
|---|---|---|
| `GetPlaneDetectionMode() -> i32` | `:97` | `0x806c0` |
| `SetPlaneDetectionMode(i32) -> bool` (masks `& 0x3`) | `:100` | `0x806fc` |
| `GetPlaneDetectionChanges(*mut ArSubsystemChanges)` | `:103` | `0x80748` |
| `GetPlaneBoundaryVertexCount(TrackableId) -> i32` | `:106` | `0x80ac8` |
| `GetPlaneBoundaryVertexData(TrackableId, *mut Vector2)` | `:109` | `0x80b20` |

`PlaneDetectionMode` flags: `1` horizontal, `2` vertical, `3` both (pass 3 to enable). Boundary data
is `count` × `Vector2(x, -y)` (Y already negated), plane-local.

**`BoundedPlane` element — 104 bytes (`element_size = 0x68`), SDK write offsets (note `center`
precedes `pose` here, unlike stock AR Foundation):**
```
trackable_id @0x00 (16)  | subsumed_by @0x10 (16, ={u64::MAX,u64::MAX})
center       @0x20 (8)   | pose        @0x28 (28: pos@0x28, rot@0x34, Unity space)
size         @0x44 (8)   | alignment   @0x4c (4: 100=horizontal / 200=vertical)
tracking_state @0x50 (4: 2=Tracking) | native_ptr @0x58 (8) | classification @0x60 (4)
```

**Shipped implementation:** `src/ffi.rs` (`TrackableId`, `UnityPose`, `ArSubsystemChanges`,
`bounded_plane` offsets, `plane_detection_mode`, 5 fn-pointer types) → `src/native.rs`
(`plane_detection_mode` / `set_plane_detection_mode` / `poll_plane_changes` — reads the element array
by `element_size` stride at the offsets above and copies out immediately; `plane_boundary`) →
`src/session.rs` wrappers → `src/system.rs` `XrealSystem`: `is_plane_detection_available()`,
`set_plane_detection_mode(mode)`, `get_plane_detection_mode()`, `poll_planes() -> Dictionary`
(`{added, updated, removed}`; each plane `{id, transform, center, size, alignment}`),
`get_plane_boundary(id) -> PackedVector2Array`, `PLANE_NONE/HORIZONTAL/VERTICAL/BOTH` constants.

**On-device verification TODO:** 6DoF session (not 3DoF — conflicts with the RGB camera); call
`set_plane_detection_mode(PLANE_BOTH)`; look at a floor/wall; confirm `poll_planes().added` populates,
`element_size == 104`, plane transforms sit on the real surface (adjust the coordinate flip if not),
and `alignment` reads 100/200.

## 2. Spatial Anchor — IMPLEMENTED

Ships in **`nr_spatial_anchor.aar`** (`libnr_spatial_anchor.so`) — **now vendored** (`export_plugin.gd`
`_get_android_libraries` + `vendor_xreal_libs.{sh,ps1}` + `build.{sh,ps1}` checks). No
`nr_plugins.json` entry (loaded directly once the `.so` is present). Needs 6DoF. Exports
(`XREALAnchorSubsystem.cs`, thunks `0x481c4`–`0x4833c`):

| Flat C signature | internal |
|---|---|
| `SetAnchorMappingFileDirectory(*const c_char)` | `0x81678` |
| `SetTrackableAnchorEnabled(bool)` | `0x81680` |
| `AcquireNewTrackableAnchor(UnityPose, *mut XRAnchor) -> bool` | `0x816b4` |
| `GetTrackableAnchorChanges(*mut ArSubsystemChanges)` | `0x81fe0` |
| `SaveTrackableAnchor(TrackableId, *mut Guid) -> bool` | `0x819f8` |
| `LoadTrackableAnchor(Guid, *mut XRAnchor) -> bool` | `0x81c4c` |
| `RemoveTrackableAnchor(TrackableId) -> bool` | `0x81fac` |
| `RemapTrackableAnchor(TrackableId) -> bool` | `0x819c8` |
| `EstimateTrackableAnchorQuality(TrackableId, UnityPose, *mut i32) -> bool` | `0x818a4` |

**`XRTrackedAnchor` element — 72 bytes (`element_size = 0x48`), layout DEVICE-CONFIRMED by
disassembling both `AcquireNewTrackableAnchor` (out ptr → x2→x19) and `LoadTrackableAnchor` (out ptr
→ x3→x20), which write identical fields:** `trackable_id@0x00 (16)`, `pose@0x10 (28, Unity space)`,
`tracking_state@0x2c (i32; 2=Tracking/0=None)`, `native_ptr@0x30 (8, zeroed on output)`,
`session_id(Guid)@0x38 (16)`. `element_size=72` is baked as an immediate (`mov w10,#72`) and the
change-array counts divide by a 72-byte stride. Quality enum: `INSUFFICIENT=0 / SUFFICIENT=1 /
GOOD=2`. Init: `SetTrackableAnchorEnabled(true)` → `SetAnchorMappingFileDirectory(writable_dir)`;
`EstimateTrackableAnchorQuality` ≥ SUFFICIENT before `SaveTrackableAnchor`. Saved-anchor enumeration is
pure C# file-listing (no native call).

**Shipped implementation:** `src/ffi.rs` (`Guid`, `xr_anchor` offsets, `anchor_quality`, 9 fn-pointer
types) → `src/native.rs` (dlsym all 9 + `read_anchor_at`/`read_anchors`, mirroring the plane readers;
`AnchorSample`/`AnchorChanges`) → `src/session.rs` wrappers → `src/system.rs` `XrealSystem`:
`is_anchor_available()`, `set_anchor_enabled(bool)`, `set_anchor_mapping_dir(String)`,
`acquire_anchor(Transform3D) -> Dictionary`, `poll_anchors() -> {added, updated, removed}`,
`save_anchor(id) -> Guid str`, `load_anchor(Guid str) -> Dictionary`, `remove_anchor(id)`,
`remap_anchor(id)`, `estimate_anchor_quality(id, Transform3D) -> int`, `ANCHOR_QUALITY_*` constants.
**Demo (`demo/anchor_manager.gd`):** the phone-menu "アンカー" toggle + "配置" button / a pinch
gesture place an anchor at the hand index fingertip; each tracked anchor gets a world-locked marker
(child of Main). **Device-verified on the Air 2 Ultra:** placement at the fingertip works,
`acquire_anchor` returns valid anchors, markers stay world-locked, and OFF→ON restores them (OFF keeps
the subsystem enabled and just hides the markers, so anchors aren't lost). `save_anchor` is retried
each ~0.5 s and only succeeds once the SLAM map quality reaches SUFFICIENT — in a small/blank space it
stays INSUFFICIENT (quality 0), so save→restart→reload persistence needs a feature-rich space to
verify. **Crash-hardening:** `poll_*_changes` clamps the SDK's change counts to `MAX_TRACKABLES`
(the change pointers alias internal vectors; a stale/garbage count would drive an OOB read → SIGSEGV).
Remaining: confirm save/restart/reload once quality reaches SUFFICIENT.

## 3. Image Tracking — BINDING IMPLEMENTED (blob pipeline solved; demo/on-device pending)

**Rust binding shipped** (`src/ffi.rs`: `NativeView`, `ManagedReferenceImage` (56 B), `xr_tracked_image`
offsets (device-confirmed: element_size=80, id@0x00/source_image_id@0x10/pose@0x20/size@0x3c/state@0x44),
5 fn-pointer types → `native.rs` dlsym + `ImageSample`/`ImageChanges` + `read_image_at` → `session.rs`
wrappers → `system.rs` `XrealSystem`: `is_image_tracking_available()`,
`init_image_database(blob, image_guids, image_sizes) -> handle`, `set_image_database(handle)`,
`poll_images() -> {added, updated, removed}`, `image_reference_count(handle)`,
`release_image_database(handle)`). `nr_image_tracking.aar` + `trackableImageTools` are vendored. Still
TODO: pack `assets/nr_plugins.json`, generate + ship a DB blob, and a demo visualizer.


Ships in **`nr_image_tracking.aar`** (`libnr_image_tracking.so`; matcher
`libnr_aiimgtrack_algo.so`). Beyond the shared plumbing:
1. **`assets/nr_plugins.json`** must be packed into the APK (StreamingAssets):
   `{"perception":{"versions":"diE1v4iRCoXc8G5g","plugins_64":[{"id":"nr_image_tracking_id","path":"libnr_image_tracking.so"}]}}`
2. **Reference-image DB blob** (first `NativeView` to `InitImageTrackingDatabase`) — **NOT a format to
   RE. It is the output of XREAL's own CLI feature-extractor `trackableImageTools`**, shipped in the
   package at `Tools~/Windows/trackableImageTools.exe` (+ `Tools~/MacOS/…`). Confirmed by reading the
   editor build processor `Editor/Android/XREALImageLibraryBuildProcessor.cs` — `BuildDB()`:
   - writes an **image-list config**, one line per image: `<guid:N>|<image.png>|<width_metres>`
     (pipe-separated; images copied/re-encoded to jpg/png first);
   - runs `trackableImageTools.exe --images_config_file <list.txt> --save_path <db.bin>` (extra tuning
     flags exist: `-featureDensity`, `-initialization_extraction_level 0..3`, `-tracking_extraction_level
     0..5`, thresholds; stdout prints `Detection/Tracking/Total Score`, warns if det<20/track<20/total<50);
   - reads back `db.bin` — that IS the blob. (When "MarkerTracking" is installed it instead ships the
     pre-baked `Marker~/InterMarker.bin`, 2.8 MB — the fixed AR-marker set, not custom images.)
   → **So our build step = run the same `.exe` on the reference PNGs, ship the resulting `.bin`.** The
   `guid:N` baked into the blob is the image identity returned later as `XRTrackedImage.source_image_id`.
3. **`managedReferenceImages`** (second `NativeView` to `InitImageTrackingDatabase`) = an array of
   **`ManagedReferenceImage` (56 B, `StructLayout.Sequential`):** `guid(Guid)@0x00`,
   `textureGuid(Guid)@0x10`, `size(Vector2 w,h)@0x20`, `name(ptr)@0x28`, `texture(ptr)@0x30`. Maps each
   baked `guid` back to its metadata; `name`/`texture` are Unity GCHandles → pass null from Godot. The
   blob's guids must match these guids so the SDK correlates a detected image with its size/name.

Exports (`XREALImageTrackingSubsystem.cs`): `SetImageTrackingDatabase(u64)` (`0x80c6c`; `0` disables),
`GetImageTrackingChanges(*mut ArSubsystemChanges)` (`0x80d84`),
`InitImageTrackingDatabase(NativeView, NativeView) -> u64` (`0x811f4`),
`GetReferenceImage(u64, i32) -> ManagedReferenceImage` (`0x81294`, **56 B by value → x8 sret**),
`GetReferenceImageCount(u64) -> i32` (`0x81408`), `ReleaseImageTrackingDatabase(u64)` (`0x81524`).
**`XRTrackedImage` element — 80 bytes (`0x50`):** `trackable_id@0x00`, `source_image_id(Guid)@0x10`,
`pose@0x20`, `size@0x3c`, `tracking_state@0x44`, `native_ptr@0x48`.

## 4. Depth Mesh — IMPLEMENTED (Path B; on-device verification pending)

**Shipped** via `src/depth_mesh.rs` (internal `libXREALXRPlugin.so` calls by `LIB_BASE + offset`, like
`src/hand_tracking.rs`): `meshing_supported()` (`GetSupportedFeatures() & (1<<3)`), `set_meshing_enabled(bool)`
(`0x9a4a8`), `poll_mesh_blocks()` (`GetMeshBlockInfo` `0x9a664` → walk the `vector<MeshBlockInfo>`, copy
verts/normals/indices, free the C++ storage with libc++ `operator delete`). Exposed on `XrealSystem`
(`is_meshing_supported` / `set_meshing_enabled` / `poll_mesh_blocks() -> Array` of
`{id, state, vertices, normals, indices}`); the `(x,-y,-z)` flip is applied Godot-side. Demo
`demo/mesh_manager.gd` (phone-menu "メッシュ" toggle, Air2U tab) builds a translucent `ArrayMesh` per block.
**On-device TODO:** confirm the session-init meshing gate turns blocks on, the coordinate flip, and the
C++ vector free doesn't corrupt (behind the toggle so any crash is contained).

**Codex RE (2026-07-17) refuted the "shelve — needs Unity-interface emulation" verdict.** Under Unity
the geometry *does* flow through the engine `XRMeshSubsystem` (`XREALXRLoader.cs:192` registers "XREAL
Meshing"; the sample reads verts/normals/tris off the engine-built `Mesh`), and `GetMeshLabels` *is* the
only flat dynsym export. **But** the raw geometry lives in plain C++ `std::vector`s inside `MeshBlockInfo`,
produced by `NativePerception::GetMeshBlockInfo()` **before any Unity allocator is involved** — reachable
the same way hand tracking calls the internal `SetHandTrackingEnabled` (resolve `libXREALXRPlugin.so`
base + offset). Ships in `nr_meshing.aar` (backend `libnr_meshing.so` exports only `NRPluginCreate/Destroy`
— an opaque fn-ptr table at `NativePerception+8`, no flat NRMeshing C API).

**Recommended path (B — bypass the engine allocators):**
1. `InputManager* im = TSingleton<InputManager>::GetInstance()` (exported thunk `0x47a10`);
   `NativePerception* np = *(im + 72)` (same field `GetPlaneDetectionMode`/`GetMeshInfos` use).
2. `NativePerception::SetMeshingEnabled(np, true)` — internal `0x9a4a8` (backend fn table+416).
3. Per frame: `NativePerception::GetMeshBlockInfo(np, &out)` — internal `0x9a664` (x8 sret →
   `std::vector<MeshBlockInfo>`, backend fn table+152). Walk it (128-byte stride) and free the vectors.
4. Build a Godot `ArrayMesh` per block.

**`MeshBlockInfo` (128 B, from `AcquireMesh` disasm `0x79a28`+):** `id(=subId2)@0x00`,
`NRMeshingBlockState@0x08` (`==2` ⇒ removed), `vector<Vector3> vertices@0x38` (12 B ea, SDK writes
`{x,y,-z}`), `vector<Vector3> normals@0x50` (same Z-flip, count==verts), `vector<u32> indices@0x68`,
source labels@0x80 → dest labels@0x98 (what `GetMeshLabels` returns).

**Gating (the on-device unknown, same class as hand tracking):** `GetMeshInfos` bails unless
`NativePerception::GetSupportedFeatures() & (1<<3)` (bit 3 = meshing) and a ready flag at `NativePerception+24`
are set → meshing must likely be requested at session init via a perception-feature / `input_source` bit
(mirror the hand-tracking two-gate: internal `SetHandTrackingEnabled` + `input_source` Hands bit).
**Air 2 Ultra-only** per the compat table.

**`GetMeshLabels(TrackableId, u8** out, i32* count) -> bool`** (flat `0x4801c` → internal `0x80564`):
uses only `subId2`; returns a **borrowed** pointer into the SDK's label vector, count 0 until `AcquireMesh`
(or Path A equivalent) has populated that block — so **labels alone are a dead end** (no geometry to map
them onto). `NRMeshingVertexSemanticLabel` (`u8`): Background=0, Wall=1, Building=2, Floor=4, Ceiling=5,
Highway=6, Sidewalk=7, Grass=8, Door=10, Table=11.

**Effort MEDIUM** (vs plane/anchor/image LOW): calls non-exported symbols by base+offset, RE'd struct
layout, and C++ `std::vector` lifetime management (Path B) — or emulate a 3-fn-ptr allocator (Path A:
fake `IUnityXRMeshInterface` at `InputManager+40`, let the SDK own lifetimes, read labels via the flat
export). Not yet implemented.

## Gotchas (all features)

1. **By-value struct ABI (AAPCS64):** `TrackableId`/`Guid`/`NativeView` (16 B) → 2 GPRs; declare
   `#[repr(C)]` by value. `UnityPose` (28 B, non-HFA) → **indirect** (declare by value; Rust passes a
   pointer). `GetReferenceImage` returns 56 B → **x8 sret** (declare `-> ManagedReferenceImage`).
2. **Validate `element_size`** == 104 / 80 / 72 before trusting `added/updated`.
3. **`removed` is `TrackableId[]`**, not the element struct (`removed_count = bytes >> 4`).
4. **Changes pointers alias internal buffers** — copy out before the next poll.
5. **Coordinate flip:** poses are Unity-space; apply `(x,-y,-z)` / quat `(x,-y,-z,w)` (hand-tracker
   convention). Verify axis signs on device.
