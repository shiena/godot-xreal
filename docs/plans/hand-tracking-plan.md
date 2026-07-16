# Hand tracking plan

Status: **IMPLEMENTED + device-verified on the Air 2 Ultra (2026-07-16).** Both hands track live (26
OpenXR joints each) through `src/hand_tracking.rs` → Godot `XRHandTracker`s; `demo/hand_visualizer.gd`
draws world-locked joint spheres. What it took to go from "code prepared" to "working":

1. **Enable path (two gates).** `UpdateHandPose` no-ops until BOTH (a) NR hand tracking is enabled and
   (b) the Hands input source is selected:
   - `src/hand_tracking.rs::ensure_enabled()` calls the internal `NativePerception::SetHandTrackingEnabled(true)`
     (`libXREALXRPlugin.so 0x97174`) via `LIB_BASE + offset`, recovering `NativePerception` from
     `*(InputManager+0x48)` (InputManager singleton at `0x47a10`) and guarding on session `+0x28` / config
     `+0x38` readiness. It does NOT poke `+0x290` (that STOP latch would force `UpdateHandPose` false).
   - `src/session.rs` sets `UserDefinedSettings.input_source = 3` (`ControllerAndHands`; `NRInputSource`
     `1=Controller / 2=Hands / 3=Both`). Bit 1 = Hands is the gate `UpdateHandPose` checks at
     `(*(InputManager+0x30))+0x18`. With `0` (none) hand tracking never runs. On the One Pro the feature
     bit is simply never satisfied (`IsHandTrackingSupported()==false`).
2. **Coordinate conversion.** `pos = (x, -y, -z)`, `quat = (x, -y, -z, w)`: Unity→Godot negates Z, and this
   port's eye cameras use a Y-inverted head pose (`display_rotation` → `(x,-y,z,w)`) so we also negate Y —
   without it the hand rendered upside-down (device-confirmed).
3. **World-locked, not head-locked.** The joint poses are in world/tracking space, so `demo/main.gd`
   parents `HandVisualizer` under **Main (a fixed node), not the head rig**. Under the rotating rig the
   head rotation cancels against the eye cameras and the hand sticks to the screen; under a fixed node the
   rotating eye cameras see the fixed hand, so it stays on the real hand when the wearer looks around.

Device-verified: both hands, all 26 joints, live, world-locked, stable. `IsHandTrackingSupported()` is
`true` on the Air 2 Ultra / `false` on the One Pro (hardware gate). The sections below are the RE that got
us here (Approach 2 = the SDK's exported wrappers; Approach 1 = the raw `NRHand*` flat API, kept for
reference; the enable-path RE).

## 実装した経路 — Approach 2（SDK の export ラッパー、session ハンドル不要）

下の「## 結論と実現性」以降は raw `NRHand*` flat API（Approach 1）の完全な ABI RE で、参照用に残す。**実際に
実装したのは Approach 2**: プラグインが export する 3 つのラッパーを直接呼ぶ。これらは SDK 内部の
`InputManager` シングルトンを使うので **NR session ハンドルが不要**（Approach 1 は session を要求するが、我々は
SDK 内部の session を保持していない）。`EnableTearedFrameCount` と同じ流儀で、Unity SDK の `XREALHandSubSystem`
が毎フレームやっていることそのもの。

- `bool IsHandTrackingSupported()`  — `libXREALXRPlugin.so 0x47c08` → `InputManager::IsHandTrackingSupported`
- `bool UpdateHandPose()`           — `0x47fe4` → `InputManager::UpdateHandPose`（両手を1回/frame 更新）
- `bool GetHandJointsPose(int32 handType, HandJointsPose* out)` — `0x47ff4` → `InputManager::GetHandJointsPose`

`HandType`: Left=0 / Right=1。`HandJointsPose` = `int32 isTracked` + `Pose[26]`（各 `Pose` = position xyz +
rotation xyzw の 7 float、C# 既定マーシャリングで確定）。pose は SDK が既に **Unity 空間**へ変換済なので、
Godot へは pos `(x, y, -z)` / quat `(-x, -y, z, w)` で変換。配列は **Unity `XRHandJointID` 順**
（`[0]=Wrist, [1]=Palm, [2..25]=fingers`）で、Godot は `PALM=0, WRIST=1`（指の順は同一）なので**先頭2つだけ swap**。

### Godot 統合（実装済）

`XrealHandTracker`（`Node`）が `ready()` で `/user/hand_tracker/left` と `/user/hand_tracker/right` の
2 つの `XrHandTracker` を `XRServer` に登録し、`process()` で毎フレーム `UpdateHandPose()` →
`GetHandJointsPose(Left/Right)` → 26 関節を `set_hand_joint_transform` + `set_hand_joint_flags` で流す
（`set_has_tracking_data(false)` で非検出フレームをクリア）。GDScript 側は `XRHandModifier3D`（tracker 名一致）
でスケルトンを駆動、または `XRServer.get_tracker()` で直接読める。gdext は現状の `api-4-6` に `XrHandTracker`
フル API があり **bump 不要**。

### ⚠️ 未解決の bring-up 事項（Air 2 Ultra で最初に検証）

`UpdateHandPose` は hand tracking が**有効化されるまで no-op**（`InputManager+0x290` のフラグで guard、
`0x802dc`–`0x80308`）。SDK は perception 設定時に `NRConfigSetHandTrackingEnabled(session, config, true)`
（`0x9719c`、session/config は `NativePerception` 内部保持）で有効化するが、我々はその経路を駆動していない。
export された有効化ラッパーは**無い**（`SetDominantHand` はあるが別物）。よって Air 2 Ultra bring-up の**最初のタスク**は:
(a) Air 2 Ultra の通常 session init で既定有効かを確認（`update()` の戻り値をログ）、無効なら
(b) `NativePerception` シングルトンから session/config を回収して `NRConfigSetHandTrackingEnabled` を呼ぶ、
または `InputManager+0x290` を立てる。現コードは毎フレーム `UpdateHandPose()` を試み、`false` の間は手を報告しない
（有効化されれば自動で流れ始める防御的設計）。

### Air 2 Ultra 実機チェックリスト（実装検証）

1. `is_supported()` が Air 2 Ultra で true（One Pro で false）。
2. `update_frame()` が true を返すか（false なら上記 enable を wire）。
3. 左右で `isTracked`、Palm/Wrist/指 26 関節の transform が実手を追う（座標変換・swap の目視確認）。
4. `XRHandModifier3D` でスケルトンが動く。6DoF profile と RGB camera 同時利用、handle leak、frame cost。
5. Unity→Godot の quat 変換（`(-x,-y,z,w)`）の左右・向き妥当性を rest/pinch で確認。

---

## 結論と実現性

手追跡は `libnr_loader.so` の flat NR C API だけで移植できる。Unity の managed hand provider は薄い整形層であり、実データは `NativePerception::GetHandState` (`libXREALXRPlugin.so` `0x97330`) が同じ `NRHand*` dispatch table を呼んで取得している。

ただし機能は **HARDWARE-GATED to XREAL Air 2 Ultra — not the One Pro** とする。One Pro は必要な外向き SLAM カメラを持たず、SDK の判定も perception feature bit 4 と HMD feature 5 の両方を要求する (`InputManager::IsHandTrackingSupported`, `0x7f730`–`0x7f768`)。SDK 3.0.0 は 26 joints/OpenXR 準拠へ更新されている。One Pro + RGB accessory を対応機として扱ってはならない。

本調査は静的 RE である。ABI（レジスタ幅、引数順、出力サイズ）はプラグインの実呼び出しから確定した。一方、プラグインが解釈せず通過させる discovery/gesture の各 bit の名称はバイナリからは確定できないため、値を捏造せず `u64`/`i32` の raw value として保持し、Ultra 実機 probe を最終 gate とする。

## ABI の共通規約

```c
typedef uint64_t NRHandle;
typedef int32_t  NRResult;       // 0 = success
typedef uint8_t  NRBool;         // getter out-param; setter input is normalized to bit 0
```

loader trampoline は dispatch entry が未設定なら `w0=1` を返す。各 trampoline は引数を並べ替えず backend へ転送する。setter だけは bool を `and w2,w2,#1` で正規化する (`0x1f64c0`)。

### Config

| export (loader offset) | exact C signature | 根拠 |
|---|---|---|
| `NRConfigCreate` (`0x1f62cc`) | `NRResult (NRHandle session, NRHandle *out_config)` | config dispatch slot `+0x00`; SDK は session と config を別々に保持 |
| `NRConfigDestroy` (`0x1f6300`) | `NRResult (NRHandle config)` | slot `+0x08` |
| `NRConfigIsHandTrackingEnabled` (`0x1f6470`) | `NRResult (NRHandle session, NRHandle config, NRBool *out_enabled)` | slot `+0x40`;同系列の setter が第3引数を bool 化 |
| `NRConfigSetHandTrackingEnabled` (`0x1f64a4`) | `NRResult (NRHandle session, NRHandle config, NRBool enabled)` | `and w2,#1` (`0x1f64c0`); SDK call site `0x9719c`–`0x971b0` は `x0=session`, `x1=config`, `w2=bool` |

`bool` setter を Rust で直接 `bool` にせず `u8` とするのは、C ABI と loader の実命令をそのまま表すためである。

### Discovery と frame data

| export (loader offset / dispatch slot) | exact C signature |
|---|---|
| `NRHandTrackingGetAvailableHandJoint` (`0x1f6654` / `+0x00`) | `NRResult (NRHandle session, uint64_t *out_joint_mask)` |
| `NRHandTrackingGetAvailableGestureType` (`0x1f6688` / `+0x08`) | `NRResult (NRHandle session, uint64_t *out_gesture_mask)` |
| `NRHandTrackingGetSupportedFunctions` (`0x1f66bc` / `+0x10`) | `NRResult (NRHandle session, uint64_t *out_function_mask)` |
| `NRHandTrackingDataCreate` (`0x1f66f0` / `+0x18`) | `NRResult (NRHandle *out_data)` |
| `NRHandTrackingDataAcquire` (`0x1f6724` / `+0x20`) | `NRResult (NRHandle session, uint64_t hmd_time_ns, NRHandle *out_data)` |
| `NRHandTrackingDataDestroy` (`0x1f6758` / `+0x28`) | `NRResult (NRHandle data)` |
| `NRHandTrackingDataGetHMDTimeNanos` (`0x1f678c` / `+0x30`) | `NRResult (NRHandle data, uint64_t *out_time_ns)` |
| `NRHandTrackingDataGetHandsCount` (`0x1f67c0` / `+0x38`) | `NRResult (NRHandle data, int32_t *out_count)` |

`Acquire` は data handle の in-place update ではない。SDK call site `0x97364`–`0x9737c` は `x0=session`, `x1=requested HMD timestamp`, `x2=&data`; 成功後 `GetHandsCount(data,&count)` (`0x974c0`–`0x974d4`) を呼び、最後に `DataDestroy(data)` (`0x9892c`–`0x98938`) する。引数なし create の共通規約どおり `DataCreate` は out-handle のみだが、SDK の per-frame path では使用されない。したがって推奨 path は毎 frame の Acquire-owned snapshot である。

Discovery の出力は bitmask である。現 SDK wrapper はこれらを利用せず、対応可否は別経路（perception bit 4 + HMD feature 5）で判定するため、個々の function/gesture bit 名は未検証として raw mask を公開する。

### Hand state と joint

| export (loader offset / dispatch slot) | exact C signature | SDK call site |
|---|---|---|
| `NRHandStateCreate` (`0x1f67f4` / `+0x40`) | `NRResult (NRHandle data, int32_t hand_index, NRHandle *out_hand)` | `0x97698`–`0x976ac` |
| `NRHandStateDestroy` (`0x1f6828` / `+0x48`) | `NRResult (NRHandle hand)` | `0x987ac`–`0x987b8` |
| `NRHandStateGetHandType` (`0x1f685c` / `+0x50`) | `NRResult (NRHandle hand, int32_t *out_type)` | `0x977e4`–`0x977f4` |
| `NRHandStateGetImageTimestampNanos` (`0x1f6890` / `+0x58`) | `NRResult (NRHandle hand, uint64_t *out_ns)` | `0x97c38`–`0x97c48` |
| `NRHandStateGetGestureType` (`0x1f68c4` / `+0x60`) | `NRResult (NRHandle hand, int32_t *out_gesture)` | `0x97ad4`–`0x97ae4` |
| `NRHandStateIsTracked` (`0x1f68f8` / `+0x68`) | `NRResult (NRHandle hand, NRBool *out_tracked)` | `0x97954`–`0x97974`;出力先は byte field |
| `NRHandStateGetConfidence` (`0x1f692c` / `+0x70`) | `NRResult (NRHandle hand, float *out_confidence)` | `0x97d9c`–`0x97dac`;出力先は 4-byte float field |
| `NRHandStateGetHandJointCount` (`0x1f6960` / `+0x78`) | `NRResult (NRHandle hand, int32_t *out_count)` | `0x98064`–`0x98078` |
| `NRHandStateGetPinchStrength` (`0x1f6994` / `+0x80`) | `NRResult (NRHandle hand, float *out_strength)` | `0x97f00`–`0x97f10` |
| `NRHandJointCreate` (`0x1f69c8` / `+0x88`) | `NRResult (NRHandle hand, int32_t joint_index, NRHandle *out_joint)` | `0x981f8`–`0x9820c` |
| `NRHandJointDestroy` (`0x1f69fc` / `+0x90`) | `NRResult (NRHandle joint)` | `0x98630`–`0x9863c` |
| `NRHandJointGetType` (`0x1f6a30` / `+0x98`) | `NRResult (NRHandle joint, int32_t *out_type)` | `0x98338`–`0x98348` |
| `NRHandJointGetPose` (`0x1f6a64` / `+0xa0`) | `NRResult (NRHandle joint, NRMat4f *out_pose)` | `0x9849c`–`0x984ac`; 64 bytes を事前 zero-fill |

`NRHandType` は `Left=0`, `Right=1`。SDK は type 0 を left output、非 0 を right output に選ぶ (`0x97950`–`0x97970`)。gesture は `int32_t` enum だが、このプラグインは値を保存するだけで分岐しないため、列挙子の意味名は未検証である。`confidence` と `pinch_strength` は float field（HandState の `+0x18`, `+0x1c`）で、通常想定範囲は 0..1。joint count は対応機/SDK 3.x では 26 を期待するが、必ず getter の値を上限にする。

## `NRMat4f` の正確な layout

`NRHandJointGetPose` は `NrPose`/7-float ではなく、column-major 4×4 の 16 floats（64 bytes）を書く。

```rust
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NrMat4f {
    pub m: [f32; 16],
}
```

byte offsets は次の通り。

```text
rotation 3x3: +0x00,+0x04,+0x08; +0x10,+0x14,+0x18; +0x20,+0x24,+0x28
position xyz: +0x30,+0x34,+0x38
homogeneous:  +0x0c,+0x1c,+0x2c,+0x3c
```

この判定は load-bearing である。SDK は hashmap node の value 開始 `node+0x14` から行列を読み、回転に `value+0x00/04/08`, `+0x10/14/18`, `+0x20/24/28` を使用 (`0x803d8`–`0x80418`)、translation に `value+0x30/34/38` を使用 (`0x803dc`–`0x80444`) する。GetPose の直前には 64 bytes を zero-fill (`0x98490`–`0x984ac`)、直後には q-register 4本で 64 bytes を hashmap へコピーする (`0x98618`–`0x9862c`)。

SDK の Unity 座標変換は position の Z を反転し、matrix 3×3 から quaternion を作った後 quaternion X/Y を反転する (`0x803e8`–`0x80444`)。Godot でも同じ LH→RH basis change を中央化する。ただし `NrPose::to_godot_quaternion` は入力 layout が異なるので `NRMat4f` をそれへ cast してはならない。

## Joint enum と Godot mapping

NR joint type は OpenXR/Godot の `XRHandTracker::HandJoint` 値と一致する。

| NR value | Godot/OpenXR joint |
|---:|---|
| 0 | Palm |
| 1 | Wrist |
| 2–5 | Thumb Metacarpal, Proximal, Distal, Tip |
| 6–10 | Index Metacarpal, Proximal, Intermediate, Distal, Tip |
| 11–15 | Middle Metacarpal, Proximal, Intermediate, Distal, Tip |
| 16–20 | Ring Metacarpal, Proximal, Intermediate, Distal, Tip |
| 21–25 | Little Metacarpal, Proximal, Intermediate, Distal, Tip |

根拠は `InputManager::GetHandJointsPose`。Unity array は Wrist-first なので key 1 を array[0] (`0x803b0`–`0x80444`)、key 0 を array[1] (`0x80448`–`0x804b0`)、その後 key 2..25 を array[2..25] (`0x804b4`–`0x80540`) に書く。Godot/OpenXR は Palm=0, Wrist=1 のため flat API の type を直接使え、Unity 用 swap は不要である。

Godot 4.3+ の integration target は左右各 1 個の `XRHandTracker`。hand type から handedness を設定し、各 type 0..25 に `set_hand_joint_transform` と tracking flags を設定する。Palm/Wrist のどちらを tracker root として別途公開するかは Godot の tracker API に従い、Unity wrapper の「Wrist を root」という都合を引き継がない。radius は NR API に無いため未設定または保守的な既定値とする。

## Enable、session、tracking mode

1. NR runtime/session の初期化後に `NRConfigCreate(session,&config)`。
2. session start/config apply 前に `NRConfigSetHandTrackingEnabled(session,config,1)`。
3. 通常の session configuration/start を行う。
4. 対応可否は SDK と同様に hardware feature gate を優先し、加えて discovery masks を診断出力する。

これは `UserDefinedSettings` の field ではない。`NativePerception` は `session` (`this+0x28`) と `config` (`this+0x38`) を別 handle として保持し、`SetHandTrackingEnabled` が両方を渡す (`0x9719c`–`0x971b0`)。従って `InitUserDefinedSettings` に field を足してはならない。

config setter は session configuration 用で、SDK の input-source 切替から呼ばれ得るが、安全な初期実装は session 開始前のみとする。runtime toggle は「同じ config が live session に再適用される」ことを Ultra 実機で確認するまで未検証。停止中に config を作り直す path を fallback とする。

手追跡はカメラ perception feature であり、SDK gate は perception supported feature bit 4 (`0x7f748`–`0x7f74c`) と HMD supported feature enum value 5 (`0x7f760`–`0x7f768`) を要求する。外向き SLAM カメラと perception pipeline が必要なので、3DoF 強制のまま動くと仮定しない。Ultra では session を 6DoF で起動する hand-tracking profile を別に設ける。RGB camera access と同時利用可能性も device test 項目とする。

## Thread、lifecycle、cost

Unity provider の `TryUpdateHands` は通常の subsystem update から `UpdateHandPose` を呼び (`XREALHandSubSystem.cs:106`–`136`)、native は HMD time を取得して直ちに `GetHandState` を実行する (`0x802d4`–`0x80344`)。GL command、texture、EGL context への参照は無い。したがって controller probe 同様に Godot main thread で poll し、render thread へ移さない。

frame ownership は厳密に次の通り。

```text
per frame: Acquire -> GetHandsCount
  per hand: HandStateCreate -> getters -> GetHandJointCount
    per joint: HandJointCreate -> GetType -> GetPose -> HandJointDestroy
  HandStateDestroy
DataDestroy
```

snapshot/hand/joint handle はすべてその frame 内で destroy する。SDK 自身も最大 2 hands × 26 joints について毎 frame create/get/destroy しており、Acquire は同期的 CPU/perception query と見なす。main thread の frame time を計測し、必要なら tracker 更新頻度を落とすが、同じ handles を frame 間で cache しない。

## Ready-to-paste Rust ABI

```rust
use libloading::Library;

pub type NrHandle = u64;
pub type NrResult = i32;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct NrMat4f {
    pub m: [f32; 16],
}

type FnSessionOutHandle = unsafe extern "C" fn(NrHandle, *mut NrHandle) -> NrResult;
type FnOutHandle = unsafe extern "C" fn(*mut NrHandle) -> NrResult;
type FnHandle = unsafe extern "C" fn(NrHandle) -> NrResult;
type FnHandleOutI32 = unsafe extern "C" fn(NrHandle, *mut i32) -> NrResult;
type FnHandleOutU64 = unsafe extern "C" fn(NrHandle, *mut u64) -> NrResult;
type FnHandleOutU8 = unsafe extern "C" fn(NrHandle, *mut u8) -> NrResult;
type FnHandleOutF32 = unsafe extern "C" fn(NrHandle, *mut f32) -> NrResult;
type FnParentIndexOutHandle =
    unsafe extern "C" fn(NrHandle, i32, *mut NrHandle) -> NrResult;
type FnAcquire = unsafe extern "C" fn(NrHandle, u64, *mut NrHandle) -> NrResult;
type FnJointPose = unsafe extern "C" fn(NrHandle, *mut NrMat4f) -> NrResult;
type FnConfigGetBool =
    unsafe extern "C" fn(NrHandle, NrHandle, *mut u8) -> NrResult;
type FnConfigSetBool = unsafe extern "C" fn(NrHandle, NrHandle, u8) -> NrResult;

pub struct NrHandApi {
    pub lib: Library,
    pub config_create: FnSessionOutHandle,
    pub config_destroy: FnHandle,
    pub config_is_enabled: FnConfigGetBool,
    pub config_set_enabled: FnConfigSetBool,
    pub available_joints: FnHandleOutU64,
    pub available_gestures: FnHandleOutU64,
    pub supported_functions: FnHandleOutU64,
    pub data_create: FnOutHandle,
    pub data_acquire: FnAcquire,
    pub data_destroy: FnHandle,
    pub data_hmd_time: FnHandleOutU64,
    pub data_hands_count: FnHandleOutI32,
    pub hand_create: FnParentIndexOutHandle,
    pub hand_destroy: FnHandle,
    pub hand_type: FnHandleOutI32,
    pub hand_image_time: FnHandleOutU64,
    pub hand_gesture: FnHandleOutI32,
    pub hand_is_tracked: FnHandleOutU8,
    pub hand_confidence: FnHandleOutF32,
    pub hand_joint_count: FnHandleOutI32,
    pub hand_pinch_strength: FnHandleOutF32,
    pub joint_create: FnParentIndexOutHandle,
    pub joint_destroy: FnHandle,
    pub joint_type: FnHandleOutI32,
    pub joint_pose: FnJointPose,
}

// Load each field with lib.get::<Type>(b"NR...\0"), exactly as controller_probe.rs.
// Keep `lib` in the struct so every function pointer remains valid.
```

Config enable:

```rust
let mut config = 0;
nr_ok((api.config_create)(session, &mut config))?;
nr_ok((api.config_set_enabled)(session, config, 1))?;
// Apply/start the session while config is alive; destroy it at session teardown.
```

Per-frame read skeleton (error paths must use the same nested destroy guards in production):

```rust
pub struct RawJoint { pub ty: i32, pub pose: NrMat4f }
pub struct RawHand {
    pub ty: i32,
    pub tracked: bool,
    pub gesture: i32,
    pub confidence: f32,
    pub pinch: f32,
    pub joints: Vec<RawJoint>,
}

unsafe fn poll(api: &NrHandApi, session: NrHandle, hmd_ns: u64) -> Vec<RawHand> {
    let mut data = 0;
    if (api.data_acquire)(session, hmd_ns, &mut data) != 0 || data == 0 { return vec![]; }
    let mut result = Vec::new();
    let mut hand_count = 0;
    if (api.data_hands_count)(data, &mut hand_count) == 0 {
        for hi in 0..hand_count.clamp(0, 2) {
            let mut hand = 0;
            if (api.hand_create)(data, hi, &mut hand) != 0 || hand == 0 { continue; }
            let mut raw = RawHand {
                ty: -1, tracked: false, gesture: -1,
                confidence: 0.0, pinch: 0.0, joints: Vec::new(),
            };
            let mut tracked = 0u8;
            let mut joint_count = 0;
            (api.hand_type)(hand, &mut raw.ty);
            (api.hand_is_tracked)(hand, &mut tracked);
            (api.hand_gesture)(hand, &mut raw.gesture);
            (api.hand_confidence)(hand, &mut raw.confidence);
            (api.hand_pinch_strength)(hand, &mut raw.pinch);
            (api.hand_joint_count)(hand, &mut joint_count);
            raw.tracked = tracked != 0;
            for ji in 0..joint_count.clamp(0, 26) {
                let mut joint = 0;
                if (api.joint_create)(hand, ji, &mut joint) != 0 || joint == 0 { continue; }
                let mut item = RawJoint { ty: -1, pose: NrMat4f::default() };
                if (api.joint_type)(joint, &mut item.ty) == 0
                    && (0..26).contains(&item.ty)
                    && (api.joint_pose)(joint, &mut item.pose) == 0
                { raw.joints.push(item); }
                (api.joint_destroy)(joint);
            }
            (api.hand_destroy)(hand);
            result.push(raw);
        }
    }
    (api.data_destroy)(data);
    result
}
```

`NRMat4f` から Godot `Transform3D` を作る際は上記 column-major offsets を読み、`B = diag(1,1,-1)` として basis/position を `B M B` 相当に変換する。joint type は Godot enum へ直接変換する。tracked=false または getter failure の frame では該当 tracker の tracking flags を clear する。

## Ultra 実機 acceptance checklist

1. capability masks と SDK-style feature gate が有効、One Pro では無効になる。
2. `Acquire(session,hmd_ns,&data)` が成功し、hands 0..2、joints 26 を返す。
3. identity/rest pose で matrix の homogeneous 要素と translation offsets を raw log で再確認する。
4. left/right、Palm=0、Wrist=1、finger 2..25 を目視確認する。
5. tracked byte、confidence/pinch float の範囲、gesture raw value を採取して enum 名を確定する。
6. 6DoF profile と RGB camera 同時利用、pause/resume、session teardown、1000-frame handle leak を検証する。
7. main-thread Acquire の p50/p95 cost を測定する。

## Evidence index

- loader trampolines: config `0x1f62cc`–`0x1f64d8`; hand table `0x1f6654`–`0x1f6a98`。
- SDK exported wrappers: `IsHandTrackingSupported 0x47c08`, `UpdateHandPose 0x47fe4`, `GetHandJointsPose 0x47ff4`。
- SDK implementation: capability `0x7f730`, update `0x802d4`, joint mapping/conversion `0x80350`, config enable `0x97174`, acquisition/ownership `0x97330`。
- managed call order and 26-joint consumer: `Runtime/Scripts/Android/XREALHandSubSystem.cs:58`–`165`; public support P/Invoke: `XREALPlugin.cs:118`–`149`。

## Enable path RE 2026-07-16 (device: UpdateHandPose=false → enable)

### 結論: 最小 enable は `NativePerception::SetHandTrackingEnabled(true)` 1回

このビルドで SDK が使う正規の START 経路は `InputManager::InputStart(void*)` (`LIB_BASE+0x78794`) である。
手入力を含む `InputSource`（byte の bit 1）が選ばれていれば、次の順序になる。

1. `InputManager+0x48` から `NativePerception*` を読み、選択済み tracking type で
   `NativePerception::StartOrResume` を呼ぶ (`0x78858`–`0x78864`)。これが session/perception/config の
   bring-up を先に完了させる。
2. `InputManager+0x30` の `InputSource` byte (`+0x18`) の bit 1 を検査する
   (`0x78868`–`0x78870`)。要求されているのに `IsHandTrackingSupported()` (`0x7f730`) が false なら、
   `InputSource` を `1` に置換して hand bit を落とす (`0x78874`–`0x78890`)。
3. 残った bit 1 を再検査し (`0x78894`–`0x788d8`)、`x0=[InputManager+0x48]`, `w1=1` で
   `NativePerception::SetHandTrackingEnabled(bool)` (`LIB_BASE+0x97174`) を呼ぶ
   (`0x788dc`–`0x788e4`)。続いて provider callback の input type `3`, `4` を開始する
   (`0x788e8`–`0x7890c`)。

`AdaptInputSource(InputSource&)` (`0x790a8`) 自体は enable 関数ではない。bit 1 が要求された場合に
capability を確認し、未対応なら byte を `1` に置換するだけである (`0x790ac`–`0x790d0`)。
また、START 側は `+0x204`, `+0x24c`, `+0x290` のいずれも書かない。したがって、既に通常 session が
立ち上がっている本 emulation から `InputStart` 全体を再入してはならない。必要な正規操作だけを表す最上位の
単一内部関数は `NativePerception::SetHandTrackingEnabled(true)` である。

対照的に STOP 経路 `InputManager::InputStop(void*)` (`LIB_BASE+0x790dc`) は、hand bit が立っているとき、
次の順に `+0x204=0`, `+0x24c=0`, `+0x290=1` を byte store し (`0x79160`, `0x79164`,
`0x79168`)、その後 `x0=[InputManager+0x48]`, `w1=0` で同じ setter を呼ぶ (`0x79154`–`0x7916c`)。
`+0x204` と `+0x24c` は左右 `HandState`（base `+0x200` と `+0x248`）の tracked byte `+4` であり、
更新時にその2構造体が `GetHandState` の出力先になることも `0x80338`–`0x80340` で確認できる。

### instance と初期化済み handle

- `TSingleton<InputManager>::GetInstance()` は `LIB_BASE+0x47a10`。返値が `InputManager*` で、その
  `+0x48` が `NativePerception*` であることは、START の `StartOrResume` (`0x7885c`–`0x78864`)、
  enable setter (`0x788dc`–`0x788e4`)、更新の `GetHMDTimeNanos` / `GetHandState`
  (`0x80328`–`0x80340`)、STOP の setter (`0x79154`–`0x7916c`) という複数の型付き call site で一致する。
  shutdown はこの pointer を `0` にしてから destructor を呼ぶ (`0x791f8`–`0x7920c`) ため、常に null guard が必要。
- `NativePerception` constructor は active bank `+0x20..+0x40` と staging bank `+0x70..+0x88` を
  zero 初期化する (`0x945dc`–`0x945ec`)。tracking-type switch は wrapper slot `+0x38` を呼び、
  staging session を `NativePerception+0x70` に作る (`0x95c18`–`0x95c2c`)。成功後に virtual slot
  `+0x18` の `SwapHandle` を呼ぶ (`0x95d94`–`0x95da8`)。swap は staging `+0x70/+0x80/+0x88` と
  active `+0x28/+0x38/+0x40` を交換する (`0x95e44`–`0x95e70`)。
- したがって正常な `StartOrResume` 成功後は、setter が読む session `[NativePerception+0x28]` と config
  `[NativePerception+0x38]` は SDK 自身が生成・格納済みである。setter は wrapper `[this+0x8]` の slot
  `+0x190` を `x0=session`, `x1=config`, `w2=(enabled&1)` で呼ぶ (`0x97184`–`0x971b0`)。
  外部から `NRConfigCreate` を追加で呼ぶ必要はなく、SDK 所有 config と別の config を作ってもこの live path には
  適用されない。安全条件は perception pointer、session、config がすべて non-null であること、および
  `NativePerception+0x18 != 0`（正常 start の結果を格納する箇所は `0x94a80`–`0x94a98`）である。

### `InputManager+0x290` の lifecycle

`+0x290` は persistent enabled flag でも毎 frame request でもなく、STOP 後の最初の hand update を1回だけ
捨てる one-shot latch である。constructor は `0` に初期化する (`0x47b04`)。STOP は tracked bytes を消した後に
`1` を書く (`0x79160`–`0x79168`)。`UpdateHandPose` は、perception pointer / started byte / input-source hand bit
の各 gate (`0x802d8`–`0x802f4`) を通った後、`+0x290` が `1` なら false を返す直前に `0` へ戻す
(`0x80304`–`0x80318`)。`0` なら通常取得を行って true を返す (`0x8031c`–`0x8034c`)。

従って steady state は **`+0x290=0` のまま**であり、enable 時に書いてはいけない。毎 frame 書くと毎 frame
false になる。実機で観測した `UpdateHandPose=false` はこの latch が一度残った場合にも起きるが、その場合は
1回の call で消費され、次 frame は進む。継続的に false なら `NativePerception+0x18` または
`InputManager+0x30` の `InputSource+0x18` bit 1 の gate も診断対象であり、config setter だけでは後者を変更しない。

### emulation からの exact minimal Rust call

以下はシンボルで裏付けられた内部 C++ ABI を、この特定 `.so` の compile-time offset で呼ぶ RE / unverified
経路である。`InputManager` の通常 bring-up が完了した後、Godot main thread から1回だけ呼ぶ。

```rust
type GetInputManager = unsafe extern "C" fn() -> *mut u8;
type SetHandTrackingEnabled = unsafe extern "C" fn(*mut u8, bool);

unsafe fn enable_hand_tracking(lib_base: usize) -> Result<(), &'static str> {
    if lib_base == 0 {
        return Err("XREAL plugin base is not published");
    }

    let get_input_manager: GetInputManager =
        core::mem::transmute(lib_base + 0x47a10);
    let input_manager = get_input_manager();
    if input_manager.is_null() {
        return Err("InputManager is null");
    }

    let input_source = (input_manager.add(0x30) as *const *const u8).read();
    if input_source.is_null() || (input_source.add(0x18).read() & 0x02) == 0 {
        return Err("InputSource does not include hand tracking");
    }

    let perception = (input_manager.add(0x48) as *const *mut u8).read();
    if perception.is_null() {
        return Err("NativePerception is null");
    }

    let started = perception.add(0x18).read();
    let session = (perception.add(0x28) as *const u64).read();
    let config = (perception.add(0x38) as *const u64).read();
    if started == 0 || session == 0 || config == 0 {
        return Err("NativePerception session/config is not ready");
    }

    let set_enabled: SetHandTrackingEnabled =
        core::mem::transmute(lib_base + 0x97174);
    set_enabled(perception, true);
    Ok(())
}
```

呼び順は **singleton取得 → `[InputManager+0x30]+0x18` の hand bit guard → `+0x48` pointer guard →
`+0x18/+0x28/+0x38` readiness guards → `LIB_BASE+0x97174(perception, true)`**。
`+0x290`、`+0x204`、`+0x24c` は一切書かない。hand bit が無い場合、その場で byte を raw poke しても
START が行う provider callback `3/4` を再現しないため、この one-shot 関数では失敗させる。hand を含む
`InputSource` を通常 bring-up 前に選ぶのが正規経路である。
setter は内部 wrapper pointer `[perception+0x8]` を即時 dereference する (`0x9719c`) ため、session bring-up 前、
shutdown と競合する時、異なる `.so` build の offset では crash し得る。main-thread lifecycle と直列化し、現在の
binary identity を確認した場合だけ実行すること。setter の C++ 戻り値は `void` で、内部 NR error はログされる。
呼出し後は次の通常 frame から export `UpdateHandPose()` を毎 frame 1回呼ぶ。最初の1回だけ false で次が true なら
STOP latch の正常消費であり、`+0x290` を再設定してはならない。
