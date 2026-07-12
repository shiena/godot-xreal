# XREAL ネイティブ関数リファレンス — RE 済み関数と GDScript バインディング

リバースエンジニアリングで **ABI が確定し、実機で呼び出しを確認済み**のネイティブ関数と、
それぞれが Godot（GDScript）にどう公開されているかの対応表。ABI 導出の詳細（逆アセンブル・
オフセット・クラッシュ回避パッチ）は [`reverse-engineering.md`](reverse-engineering.md) を、
入力系の設計背景は [`input-plan.md`](input-plan.md) を参照。

- **ABI の出所**は 3 系統: ① Unity SDK（`com.xreal.xr` v3.1.0）の C# `[DllImport]` 宣言、
  ② C++ マングル名 + AArch64 逆アセンブル、③ レガシー NRSDK の公開 C# ソース。
- Rust 側の型定義は [`src/ffi.rs`](../src/ffi.rs)、dlopen/dlsym ラッパーは
  [`src/native.rs`](../src/native.rs)。
- GDScript から見えるクラスは `XrealHeadTracker`（`Node3D`）と `XrealSystem`（`RefCounted`）の 2 つ。

対応表の「公開先」凡例:
- **`XrealSystem.foo()`** / **`XrealHeadTracker.foo()`** — GDScript から直接呼べる `#[func]`。
- **signal** — シグナルとして emit（`connect` して受ける）。
- **内部** — セッション起動などで拡張内部が使うのみ。順序・スレッド制約があり GDScript には出さない。
- **プローブ** — RE 検証用の診断関数。実運用ではなく調査用。

---

## `libXREALNativeSessionManager.so` — 知覚 API（ヘッドポーズ / クロック）

| ネイティブ関数（確定シグネチャ） | 意味 | 公開先 |
|---|---|---|
| `int XREALGetHeadPoseAtTime(uint64_t time_ns, float* out)` | `out` に 7 float のポーズ（回転 4 + 位置 3、**回転が先頭**）。3DoF は回転のみ使用 | `XrealHeadTracker` の毎フレーム回転、`XrealSystem.get_head_rotation() -> Quaternion` |
| `int XREALGetHMDTimeNanos(uint64_t* out_time_ns)` | HMD クロック（ns）を**アウトパラメータ**で返す（戻り値は成否） | `XrealSystem.get_hmd_time_nanos() -> int` |
| `void XREALLoadAPI(void)` | 知覚デリゲートを配線。ポーズ照会の前に必須 | 内部（`session::try_start`） |
| `bool XREALIsSessionStarted(void)` | セッション稼働中か | 内部（`is_session_started` のフォールバック） |

**ポーズのフィールド順（実機確定）:** 回転 4 float は **w 先頭**（w, x, y, z）。静止時に第 1 要素 ≈ 1.0。
Unity/NRSDK 左手系 → Godot 右手系の変換は `(x, y, z, w) → (-x, -y, z, w)`。
`NrPose::to_godot_quaternion`（`src/ffi.rs`）にユニットテスト付きで固定済み。
**roll(z) は SDK が出力しない**（水平安定化）ため、頭を傾けても第 4 要素は常に 0。
詳細は [`roll-tracking-investigation.md`](roll-tracking-investigation.md)。

---

## `libXREALXRPlugin.so` — セッション / トラッキング / デバイス情報 / 入力

### GDScript に公開している関数

| ネイティブ関数（確定シグネチャ） | 意味 | 公開先 |
|---|---|---|
| `void RecenterGlasses(void)` | 3DoF の正面方向をリセット | `XrealHeadTracker.recenter()`、`display_started` で自動 recenter |
| `int GetTrackingState(void)` | トラッキング状態 enum | `XrealSystem.get_tracking_state() -> int`（不能時 -1） |
| `int GetTrackingReason(void)` | トラッキング未確立の理由 enum | `XrealSystem.get_tracking_reason() -> int` |
| `int GetTrackingType(void)` | 現在の `TrackingType` enum | `XrealSystem.get_tracking_type() -> int` |
| `bool SwitchTrackingType(int type)` | トラッキングモード切替（6DoF/3DoF/0DoF/0DoF-stab） | `XrealSystem.switch_tracking_type(type) -> bool`（`TRACKING_*` 定数） |
| `char* GetPluginVersion(void)` | プラグインのバージョン文字列 | `XrealSystem.get_plugin_version() -> String` |
| `int GetDeviceType(void)` | `XREALDeviceType` enum | `XrealSystem.get_device_type() -> int` |
| `int ControlSetDisplayBypassPsensorFlag(int flag)` | 装着センサーによる自動消灯を抑止（flag=1 で常時点灯） | `XrealSystem.set_display_bypass_psensor(bypass) -> int` |
| `void SetGlassesEventCallback(cb)` | 本体ハードイベントのコールバック登録（後述） | シグナル群（`key_event` ほか） |

`XrealSystem` の `TrackingType` 定数（`switch_tracking_type` / `get_tracking_type` 用）:

| 定数 | 値 |
|---|---|
| `TRACKING_6DOF` | 0 |
| `TRACKING_3DOF` | 1 |
| `TRACKING_0DOF` | 2 |
| `TRACKING_0DOF_STAB` | 3 |

> `set_display_bypass_psensor` と `switch_tracking_type` は、内部 `NativeGlasses` が準備できるまで
> SDK 側で no-op になる。`is_session_started()` が true になってから呼ぶ（数フレーム後）。

### 起動シーケンスでのみ使う（GDScript 非公開）

| ネイティブ関数 | 非公開の理由 |
|---|---|
| `void UnityPluginLoad(IUnityInterfaces*)` | 偽 `IUnityInterfaces` を正しい順で渡す必要がある。誤ると `DisplayManager::LoadDisplay` が null 参照で SIGSEGV |
| `void InitUserDefinedSettings(UserDefinedSettings)` | Activity ポインタ・色空間などを内部が構築。`UnityPluginLoad` の後でなければ落ちる |
| `bool CreateSession(bool directPresent)` | セッション生成。順序（`InitUserDefinedSettings` → これ → `LoadAPI` → `ResumeSession`）が固定 |
| `void ResumeSession(void)` | 生成直後のセッションは pause 状態。これを呼ぶまでポーズが流れない |

これらは `session::try_start()` が唯一正しい順序で 1 回だけ呼ぶ。GDScript から任意タイミングで
叩くと確実にクラッシュするため公開しない。

### レンダリング系（プローブ止まり、非公開）

`InitializeRendering` / `CreateFrame` / `GetFrameMetaData` / `DeinitializeRendering` は RE 済みだが、
SDK 独自のレンダリングスレッド（GLThread）が管理する `DisplayManager+0x120` と競合し、直接呼ぶと
SIGSEGV/SIGABRT する（`reverse-engineering.md` の "CreateFrame gate" 節）。Godot シーンの表示は
偽 `IUnityXRDisplay` プロバイダ経由で実現済み（Phase 2）なので、これらは公開しない。

---

## 本体ハードイベント — `SetGlassesEventCallback`

C# `XREALCallbackHandler.cs` の `[DllImport]` から ABI 確定（バイナリ RE 不要）:

```c
struct GlassesEventData { int32_t actionType; uint32_t para, para2; float para3; }; // 16 バイト
void SetGlassesEventCallback(void (*cb)(GlassesEventData));   // 構造体を値渡し（AArch64: x0/x1）
```

SDK 所有スレッドでコールバックが発火 → 有界キュー（`src/glasses_events.rs`, cap 256, drop-oldest）→
`XrealHeadTracker::process()` がメインスレッドで drain → シグナル emit、という流れ
（hot-plug カウンタと同じパターン）。Unity は `RuntimeInitializeOnLoad` で登録するだけで
`Start/StopGlassesEventsReport` は呼ばない。本実装も `CreateSession` 直後に登録するのみ。

### `actionType`（`XREALActionType`）とシグナルの対応

| `actionType` | 値 | 意味 | emit されるシグナル |
|---|---|---|---|
| `ACTION_TYPE_CLICK` | 1 | キー単押し | `key_event(key, ACTION_CLICK)` |
| `ACTION_TYPE_DOUBLE_CLICK` | 2 | ダブルクリック | `key_event(key, ACTION_DOUBLE_CLICK)` |
| `ACTION_TYPE_LONG_PRESS` | 3 | 長押し | `key_event(key, ACTION_LONG_PRESS)` |
| `ACTION_TYPE_INCREASE_BRIGHTNESS` | 6 | 明るさ + | `brightness_changed(level)` |
| `ACTION_TYPE_DECREASE_BRIGHTNESS` | 7 | 明るさ − | `brightness_changed(level)` |
| `ACTION_TYPE_INCREASE_VOLUME` | 8 | 音量 + | `volume_changed(level)` |
| `ACTION_TYPE_DECREASE_VOLUME` | 9 | 音量 − | `volume_changed(level)` |
| `ACTION_TYPE_NEXT_EC_LEVEL` | 12 | EC 調光レベル送り | `ec_level_changed(level)` |
| `ACTION_TYPE_KEY_STATE` | 2023 | キー down/up 生イベント | `key_state_changed(key, state)` |
| `ACTION_TYPE_PROXIMITY_WEARING_STATE` | 2024 | 装着 / 取り外し | `wearing_changed(bool)` |

すべてのイベントは種別を問わず catch-all の `glasses_event(action_type, para, para2, para3)` でも
emit される（未分類イベントの受け皿・デバッグ用）。

### `XrealHeadTracker` の入力シグナル一覧

| シグナル | 引数 | 説明 |
|---|---|---|
| `key_event` | `(key: int, action: int)` | 物理キーの click/double/long。`KEY_*` × `ACTION_*` |
| `key_state_changed` | `(key: int, state: int)` | キーの down/up 生イベント。`KEY_STATE_*` |
| `wearing_changed` | `(wearing: bool)` | 装着センサー（P-sensor）put-on / take-off |
| `brightness_changed` | `(level: int)` | 明るさ変更 |
| `volume_changed` | `(level: int)` | 音量変更 |
| `ec_level_changed` | `(level: int)` | EC 調光レベル変更（One Pro） |
| `glasses_event` | `(action_type: int, para: int, para2: int, para3: float)` | 全イベントの catch-all |

`XrealHeadTracker` の入力定数:

| 定数 | 値 | | 定数 | 値 |
|---|---|---|---|---|
| `KEY_MULTI` | 1 | | `ACTION_CLICK` | 1 |
| `KEY_INCREASE` | 2 | | `ACTION_DOUBLE_CLICK` | 2 |
| `KEY_DECREASE` | 3 | | `ACTION_LONG_PRESS` | 3 |
| `KEY_MENU` | 4 | | `KEY_STATE_DOWN` | 1 |
| | | | `KEY_STATE_UP` | 2 |

---

## ホットプラグ（USB 挿抜）

ネイティブ関数ではなく Java 側 `DisplayListener`（`XrealBridge`）→ JNI アトミックカウンタ →
`process()` でポーリング → emit。ABI RE ではないが入力系の一部として併記。

| シグナル | 説明 |
|---|---|
| `glasses_connected` | グラス（3840×1080 ディスプレイ）接続を検知 |
| `glasses_disconnected` | グラス取り外しを検知 |
| `display_started` | グラス表示 + ヘッドトラッキングが初回稼働。ここで自動 recenter |

**既知のギャップ:** グラス未接続で起動したインスタンスは、後から接続しても `glasses_connected` は
出るがセッションが作られない（要アプリ再起動）。[`hotplug-session-recovery.md`](hotplug-session-recovery.md)。

---

## `libnr_loader.so` — NR レンダリング低レベル API

コンポジタ経路（`NRRendering*` / `NRSwapchain*` / `NRBufferViewport*` / `NRFrame*`、45 シンボル）は
RE 済みだが直叩き経路は袋小路（`reverse-engineering.md` 参照）。GDScript には RE 検証用のプローブのみ公開:

| `XrealSystem` メソッド | 用途 |
|---|---|
| `is_nr_rendering_available() -> bool` | `libnr_loader.so` のシンボルが解決できたか |
| `get_nr_rendering_symbol_count() -> int` | 解決済みシンボル数 |
| `smoke_test_nr_rendering_create_destroy() -> int` | ハンドル生成→破棄のスモークテスト |
| `smoke_test_nr_rendering_start_stop() -> int` | 生成→start→stop→破棄のスモークテスト |

Phase C の `NRController*`（36 個、レガシー `NativeController.cs` からシグネチャ復元可能）は未実装。

---

## GDScript 使用例

```gdscript
extends Node3D  # XrealHeadTracker をルートに持つシーン想定

@onready var tracker: XrealHeadTracker = $XrealHeadTracker

func _ready() -> void:
    var sys := XrealSystem.new()
    if not sys.is_available():
        return  # デスクトップ / 未接続

    print("plugin=", sys.get_plugin_version(), " device=", sys.get_device_type())

    # 入力シグナルを接続
    tracker.key_event.connect(_on_key_event)
    tracker.wearing_changed.connect(_on_wearing_changed)

    # 装着していなくても表示を消さない
    sys.set_display_bypass_psensor(true)

    # roll 調査: 3DoF に切り替えて頭を傾けてみる
    # sys.switch_tracking_type(XrealSystem.TRACKING_3DOF)

func _on_key_event(key: int, action: int) -> void:
    if key == XrealHeadTracker.KEY_MENU and action == XrealHeadTracker.ACTION_LONG_PRESS:
        tracker.recenter()

func _on_wearing_changed(wearing: bool) -> void:
    get_tree().paused = not wearing  # 外したら一時停止、など
```

---

## 意図的に公開していない関数（まとめ）

| 関数 / 系統 | 理由 |
|---|---|
| `CreateSession` / `InitUserDefinedSettings` / `ResumeSession` / `UnityPluginLoad` / `XREALLoadAPI` | 起動順序が固定。任意タイミングで呼ぶと SIGSEGV。拡張内部が 1 回だけ正しく呼ぶ |
| `SetGlassesEventCallback` の生登録 | シグナルとして公開済み。生コールバックを GDScript に渡す手段は持たせない |
| `CreateFrame` / `SubmitCurrentFrame` / `NRFrame*` 直叩き | SDK レンダリングスレッドと競合してクラッシュ。表示は XR ディスプレイプロバイダ経由で実現済み |
| `SetGlassesHardwareEventCallback` | `SetGlassesEventCallback` が正規の funnel。こちらは未使用・未 RE |
| ハンドトラッキング / 6DoF 位置 / 平面・画像・アンカー | One Pro のハード非対応（カメラ無し）。スコープ外 |
