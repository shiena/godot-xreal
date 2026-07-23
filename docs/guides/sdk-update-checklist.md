# XREAL SDK for Unity update checklist

What to re-check when bumping the vendored `com.xreal.xr` package. **Key point: this port does not
use the SDK's public C# API — it reads the native `.so` by reverse-engineered byte offsets, `dlsym`s
native exports, and consumes SDK config/binaries directly. So "the public API is unchanged" is NOT a
safety guarantee.** A recompiled `.so` with an identical API surface can still shift struct layouts
and internal-call offsets under us (`src/*.rs` carries ~190 hardcoded offsets / `dlsym`s).

Two cases:
- **A. Public API unchanged** — still do steps 1–7 below (implementation internals can move).
- **B. Public API changed** — do A, plus revisit the C ABI structs/signatures in `src/ffi.rs` /
  `src/native.rs` (see [reverse-engineering.md](../reference/reverse-engineering.md)).

## Even when the public API is unchanged

### 1. Re-vendor the binaries + config (mechanical)

Run the vendoring for the new package — pick ONE:
- `pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <pkg-root-or-com.xreal.xr.tar.gz>` (Windows)
- `./scripts/vendor_xreal_libs.sh <pkg-root-or-com.xreal.xr.tar.gz>` (mac/Linux)
- or the in-editor **XREAL vendor dock** (`addons/godot_xreal/editor/vendor_import_dock.gd`).

This stages (all git-ignored): 3 core `.so` + `libmedia_codec.so` → `jniLibs/arm64-v8a/`; 7 `.aar` +
`nr_plugins.json` → `addons/godot_xreal/android/`; `trackableImageTools` → `addons/godot_xreal/tools/`.

- The `.so` and the Rust offsets (step 2) are a **pair** — never ship a new `.so` against stale
  offsets, or vice-versa.
- `nr_plugins.json` (the NR perception manifest, incl. its opaque `"versions"` hash) is re-extracted
  here, so a hash change is picked up automatically — this is why it is vendored, not committed.

### 2. Re-verify the RE'd native offsets / struct layouts ON DEVICE (highest risk)

The **silent** failure mode: no compile error, no API diff — just crashes or wrong data (the
depth-mesh block-offset / winding bug was exactly this class). Recompilation can move:
- struct field offsets (e.g. `MeshBlockInfo` in `src/depth_mesh.rs`, pose offsets, `InputManager+0x60`)
- internal-call offsets / function-table indices (e.g. the meshing/image-tracking entry points).

Exercise each RE-dependent feature on hardware and watch for crashes and garbage:
head/eye pose, 6DoF position, input (keys/wear), hand tracking (Air 2 Ultra), planes, anchors,
image tracking, depth mesh, camera feed, metrics, FPV streaming.

### 3. Bump / confirm the version gate

`MIN_VERSION` (currently `3.1.0`) is duplicated in **three** places — keep them in sync:
`scripts/vendor_xreal_libs.ps1`, `scripts/vendor_xreal_libs.sh`, `vendor_import_dock.gd`.
It exists precisely because the offsets were RE'd against a specific version. After step 2 confirms
the new version, decide whether to raise it.

### 4. Confirm `dlsym` symbols still resolve

The port pulls specific exports by name out of `libXREALXRPlugin.so` etc. A rename/strip breaks
resolution even with an unchanged public API. Check the startup logs for failed symbol lookups
(`src/native.rs` / `src/ffi.rs`; see [native-api-reference.md](../reference/native-api-reference.md)).

### 5. Check the hardcoded file lists / paths

If the SDK reorganizes its libraries, these directories of literals need updating:
- vendored file names: `scripts/vendor_xreal_libs.*` **and** `vendor_import_dock.gd` **and** the
  required-file checks in `scripts/build.ps1` / `scripts/build.sh` (kept in sync by hand).
- `.aar` names shipped by `addons/godot_xreal/export_plugin.gd` (`_get_android_libraries`).
- the `libmedia_codec.so` source path (`Runtime/Scripts/Android/Camera Features/...`).
- `.so` names listed in `godot_xreal.gdextension` `[dependencies]`.
- the **`nractivitylife*.aar` exclusion** assumption (Unity-only launcher — must stay out).

### 6. Check manifest meta-data / internal class references

`export_plugin.gd` injects `<meta-data>` (`nreal_sdk`, `com.nreal.supportDevices`, `nr_features`,
`autoLog`) and the companion/`NRFakeActivity` declarations. Internal-class references also matter:
`Log-Control`'s `GlassesInitProvider` → `com.xreal.logcontrol.LogControl` (a rename crashes at
startup with `NoClassDefFoundError` before Godot starts — device-confirmed once already). Verify a
clean cold start.

### 7. Regenerate anything derived from the SDK

If offsets/enums that feed generated artifacts changed, refresh them (e.g. the dummy stub members /
API docs — see [api-docs-generation](../../MEMORY.md) and `scripts/gen_docs.*`). Commit generated
files separately from source changes.

## When the public API DID change

In addition to 1–7:
- Revisit the `repr(C)` structs and function signatures in `src/ffi.rs` / `src/native.rs` against the
  new headers/disassembly ([reverse-engineering.md](../reference/reverse-engineering.md)).
- Re-derive any coordinate conventions if pose/extent formats moved
  ([coordinate-systems-notes.md](../plans/coordinate-systems-notes.md)).

## Quick reference — what each artifact is and why it can drift

| Artifact | Where it lands | Drifts on SDK update because… |
|---|---|---|
| 3 core `.so` + `libmedia_codec.so` | `jniLibs/arm64-v8a/` | recompiled → offsets/symbols move (steps 2, 4) |
| 7 `.aar` | `addons/godot_xreal/android/` | internal classes / carried `.so` change (steps 5, 6) |
| `nr_plugins.json` | `addons/godot_xreal/android/` | opaque `"versions"` hash + backend path (step 1) |
| `trackableImageTools` | `addons/godot_xreal/tools/` | host tool; CLI flags could change |
| RE'd offsets | `src/*.rs` | **not vendored** — must be re-verified by hand (step 2) |
