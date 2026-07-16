# godot-xreal

[English](README.md) | 日本語

`godot-xreal` は [XREAL](https://www.xreal.com/) グラスを駆動する Godot 4 用 GDExtension
（[godot-rust](https://godot-rust.github.io/) による Rust 実装）です。Unity 版 `com.xreal.xr` SDK
を、その **ネイティブライブラリを再利用する形で** Godot へ移植したものです（動作確認は SDK **3.1.0**）。

> **⚠️ 非公式・実験的。** 本プロジェクトは独立したコミュニティ製で、**XREAL 社とは無関係であり、
> 同社の承認・サポートを受けていません**。「XREAL」および SDK は各権利者に帰属します。ネイティブ
> ライブラリは**同梱しておらず**、ビルドの事前準備として自分で vendoring します（[事前準備](#事前準備-xreal-ランタイムライブラリの-vendoring) 参照）。
> vendoring した SDK の C ABI をリバースエンジニアリングして相互運用しています。利用は自己責任で。

## なぜ C# 翻訳ではなくネイティブ移植か

Unity SDK は Android の `.so` に薄く C# を被せた構造で、その `.so` がエンジン非依存のフラットな
C ABI をエクスポートしています（`libXREALNativeSessionManager.so` → `XREALGetHeadPoseAtTime` 等、
`libXREALXRPlugin.so` → OpenXR 的なコンポジタ・レイヤ API を含む 274 関数）。よって C# を翻訳せず、
この拡張は `.so` を `dlopen` して Godot に直接つなぎます。下層の難読化された NRSDK proc テーブル
（`libnr_api.so` / `NRGetProcAddr`）は回避します。詳細は
[`docs/reference/reverse-engineering.md`](docs/reference/reverse-engineering.md)。

## 対応プラットフォーム

XREAL のネイティブは **Android arm64 のみ** のため、対応端末（スマホ / Beam 等）に USB-C 接続した
**Godot Android アプリ** が対象です。デスクトップでも拡張はロードされ（シーン編集用）、その場合
ヘッドトラッキングは無効になります。

## 対応機能

**XREAL SDK for Unity 3.1.0** のネイティブライブラリを用いて XREAL One Pro（ハンドトラッキングは
XREAL Air 2 Ultra）で実機確認。以下はすべてコミュニティによるリバースエンジニアリングでの相互運用で
あり、公式 API ではありません。

| 機能 | 状態 | 補足 |
|---|---|---|
| **ヘッドトラッキング**（姿勢: ピッチ / ヨー / ロール） | ✅ | XR-plugin の表示ポーズ由来。アイカメラを駆動。 |
| **トラッキングモード** 6DoF / 3DoF / 0DoF | ✅ | 選択可（`xreal/tracking_type` / `XrealSystem.set_tracking_type` / `debug.xreal.tracking_type`）。 |
| **ステレオ表示** — ヘッドロックの覗き窓 | ✅ | グラス越しにワールド固定 3D。**Multipass**（両眼）。唯一のステレオモード（セレクタなし）。 |
| **Multiview** ステレオ | ❌ 棚上げ | 右目が黒 — NR コンポジタ（`libnr_api`）が我々の client `GL_TEXTURE_2D_ARRAY` を取り込めず、かつ本リグ（2 SubViewport 描画）では性能利得も無い。コードは残すが無効化、開発者用エスケープ `setprop debug.xreal.force_multiview 1` のみ。詳細 `docs/archive/codex-righteye-analysis.md`。 |
| **Recenter** | ✅ | 正面方向をリセット（SDK `NativePerception::Recenter`）。 |
| **ハンドトラッキング**（両手26関節）→ Godot `XRHandTracker` | ✅（Air 2 Ultra） | 手の関節を2つの `XRServer` ハンドトラッカ（`/user/hand_tracker/{left,right}`）へライブ供給。デモは world-lock した関節球を描画。**Air 2 Ultra 専用** — One Pro は外向きカメラが無く `IsHandTrackingSupported()==false`。有効化は内部 `SetHandTrackingEnabled`+`input_source=3`。詳細 [`docs/plans/hand-tracking-plan.md`](docs/plans/hand-tracking-plan.md)。 |
| **RGB カメラ**（Godot `CameraFeed`） | ✅ | フルカラーで 3D シーン内のヘッドロックのクアッドに表示。**3DoF 必須**（6DoF SLAM とカメラを共有するため）。 |
| **レンダーメトリクス** — present FPS / dropped / early / latency | ✅ | コンポジタの実測値を `NRMetrics*` API で直接取得（Unity の `UpdateMetrics` sink は使わない）。`XrealSystem`（`get_present_fps()`, `get_dropped_frame_count()` 等）。詳細 [`docs/plans/render-metrics-gdscript-plan.md`](docs/plans/render-metrics-gdscript-plan.md)。 |
| **グラス入力** — 物理キー（MENU/MULTI: クリック/ダブル/長押し） | ✅ | Godot シグナル（`key_event`, `key_state_changed`）。 |
| **装着センサー / 明るさ / 音量 / 調光 / USB ホットプラグ** | ✅ | シグナル（`wearing_changed`, `brightness_changed`, `glasses_connected` 等）。 |
| **診断** — セッション/トラッキング状態、HMD クロック、プラグイン版 | ✅ | `XrealSystem` 経由。 |
| **オンスクリーン・タッチコントローラ**（スマホ画面） | ✅（デモ） | アプリ層の Godot UI（`demo/touch_controller.gd`）: カスタマイズ可能なタッチパッド+ボタン→シグナル、スマホ振動ハプティクス。スマホにコントローラ・グラスに 3D を表示（画面分離）。ネイティブ非依存で SDK の `XREALVirtualController` に相当。 |
| **スマホ 3D ポインター**（ホスト IMU） | ✅（デモ） | スマホを傾けてグラス内に 3D レイを飛ばす（`demo/phone_pointer.gd`）。姿勢は `XrealSystem.poll_controller()` が露出する NRController の生 IMU（`accel`→ピッチ/ロール, `gyro`→ヨー）を GDScript で融合。本機では NRController の**融合ポーズ**も Godot 内蔵 `Input.get_gyroscope()` も空だったため。レイキャストで当たったオブジェクトをハイライト・トリガーで選択、オンスクリーンの左右手切替でレイの原点を切替、gyro ドリフトはバイアス学習+デッドゾーンで抑制。`recenter` で正面リセット。 |
| **マルチレジューム** — スマホを別アプリに切替えてもグラスのアプリが継続（描画も） | ✅ | **Unity SDK がフローティングウインドウ（復帰ボタン）で行う所を、本移植では代わりに auto-enter Picture-in-Picture で実装。** 背景化するとアプリはスマホ隅の小タイル（pause だが可視）になり、Godot の GL スレッド + Surface が生存 → グラスがライブ描画継続。タイルタップで全画面復帰。`XrealBridge.enableAutoEnterPiP`（`demo/main.gd` から駆動）、manifest 足場 `nr_features=multiResume`+`NRFakeActivity`。**実機検証済み**: submit カウンタが背景化後も進行（PiP 前は凍結）。なぜ PiP か（フローティングウインドウ / foreground service / SurfaceView 付け替えでない理由）は `docs/plans/background-render-plan.md`。（先にフローティング「戻る」ボタンを実装したが*復帰*専用で描画維持できず、PiP に差し替えて除去: `docs/archive/codex-floatingmanager-analysis.md`） |
| **平面検出** → GDScript | ✅ 移植済み（実機検証待ち） | 水平/垂直の平面検出を `XrealSystem.set_plane_detection_mode()` + `poll_planes()`（追加/更新/削除、ポーズ・サイズ・alignment 付き）+ `get_plane_boundary()` で提供。`libXREALXRPlugin.so` のフラット C export（追加 AAR 不要）、6DoF 必須。4 つの AR 機能の C ABI は RE 確定済み — [`docs/plans/ar-features-plan.md`](docs/plans/ar-features-plan.md)。 |
| **空間アンカー** → GDScript | ✅ 移植済み（実機検証待ち） | ワールドアンカーの作成/永続化/復元を `XrealSystem.acquire_anchor()` / `poll_anchors()` / `save_anchor()` / `load_anchor()` / `estimate_anchor_quality()` 等で提供。フラット C export（`XRTrackedAnchor` レイアウトは実機確定）+ 同梱の `nr_spatial_anchor.aar` バックエンド、6DoF 必須。併せて `is_camera_supported()` / `is_hmd_feature_supported()`（SDK のデバイス別判定 — Air 2 Ultra は RGB カメラ非搭載）も追加。 |

未実装: アプリカメラの 6DoF 位置、画像トラッキング、メッシング、音声/写真キャプチャ。（画像/
メッシュは ARCore・AR Foundation 不要で移植可能、C ABI も RE 済み —
[`docs/plans/ar-features-plan.md`](docs/plans/ar-features-plan.md)。）

## 事前準備: XREAL ランタイムライブラリの vendoring

XREAL のネイティブライブラリは本リポジトリに **含まれません**（XREAL の規約に従うため）。**XREAL SDK
for Unity**（`com.xreal.xr` パッケージ。tgz `com.xreal.xr.tar.gz` で提供。**動作確認済みは 3.1.0**）
を入手し、その中のライブラリを次の**3つのいずれか**の方法で配置します（どれも**同じファイル**を同じ
git 管理外の配置先に置きます。詳細は下の表）:

1. **推奨 — エディタ拡張（dock）。** アドオンを有効化し（プロジェクト → プロジェクト設定 → プラグイン
   → 「Godot XREAL」）、左パネルの **`XREAL Import`** dock を開いて *Select package…* をクリック、
   `com.xreal.xr(.tgz|.tar.gz)`（または展開済みの `package/` フォルダ）を選ぶだけ。システムの `tar` で
   展開して一式を配置し、再スキャンまで行います（ターミナル不要）。
2. **代替 — スクリプト。** ターミナルから:
   ```powershell
   pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <…>/com.xreal.xr.tar.gz   # または展開済みの …/package
   ```
   （macOS / Linux は `./scripts/vendor_xreal_libs.sh <…>`。）
3. **代替 — 手動展開。** tgz を自分で展開し、下の表のファイルをリポジトリ内の各配置先へコピー。

vendoring が扱うのは XREAL 純正ライブラリのみ — アドオン本体の `libgodot_xreal.so` は従来どおり
`cargo ndk` ビルド（またはプリビルト）から入ります。配置内容:

**`.so` 4 個 → `jniLibs/arm64-v8a/`** — `godot_xreal.gdextension` の `[dependencies]` 経由で APK に
同梱され、起動時に `dlopen` されます。先頭3つは `Runtime/Plugins/Android/arm64-v8a/` からコピー:

| `.so` | 役割 |
|---|---|
| `libXREALNativeSessionManager.so` | セッション / ヘッドポーズの C ABI |
| `libXREALXRPlugin.so` | XR-plugin コンポジタ / 表示の C ABI |
| `libVulkanSupport.so` | 上記2つが必要とするサポート lib |
| `libmedia_codec.so` | FPV H.264 エンコーダ（`Runtime/Scripts/…/Camera Features/…/arm64/` から） |

**7 個の `.aar` → `addons/godot_xreal/android/`** — アドオンのエクスポートプラグイン
（`export_plugin.gd`）が APK に取り込みます: グラスに必要な Java/JNI 層+manifest エントリを
担います。さらに NR 系ネイティブ lib（`jni/arm64-v8a/*.so`）も内包しており、Gradle が APK に
マージするため**別途抽出は不要**です。コピー元はすべて `Runtime/Plugins/Android/` 直下:

| `.aar` | 役割 | APK に届くネイティブ lib |
|---|---|---|
| `nr_loader.aar` | NR ローダの Java 層 | `libnr_loader.so` |
| `nr_api.aar` | NR API の Java 層 | `libnr_api.so` / `libnr_plugin_6dof.so` / `libnr_rgb_camera.so` |
| `nr_common.aar` | NR 共通層 | `libnr_libusb.so`（+ QNN/SNPE 系） |
| `nr_spatial_anchor.aar` | 空間アンカーのバックエンド | `libnr_spatial_anchor.so` |
| `nr_image_tracking.aar` | 画像トラッキングのバックエンド | `libnr_image_tracking.so` |
| `GlassesDisplayPlugEvent-2.4.2.aar` | グラス検出 `GlassesInitProvider` | — |
| `Log-Control-1.2.aar` | 上記が参照する `LogControl` — **必須**（欠けると Godot 起動前にクラッシュ） | — |

**XrealBridge の Java ソース** — vendoring も事前コンパイルも*不要*: コミット済みのソース
（`addons/godot_xreal/android/src/`）をアドオンのエクスポートプラグインが gradle ビルド
テンプレートに配置し、エクスポート時の Gradle がコンパイルします。

**`nractivitylife*.aar` はコピー禁止** — ランチャーが Unity 専用のため Godot アプリでは起動不能に
なります。（`nr_common.aar` 内の QNN/SNPE 系 `.so` は本拡張では未使用ですが、`.aar` ごと APK に
入ります。）

## ビルド

GDExtension 部分は素の godot-rust です。先に上の**事前準備**（XREAL ネイティブライブラリの vendoring）
を済ませてからビルドします。コマンドの詳細（デスクトップ反復、手動 `cargo ndk` / Gradle、署名）:
[`docs/guides/build-and-release.md`](docs/guides/build-and-release.md)。

**デスクトップエディタ**でライブラリ欠落エラーを出さずに開くには、クローン後に一度だけ何もしない
スタブをビルドします: `pwsh scripts/build_dummy_libs.ps1`（または `./scripts/build_dummy_libs.sh`）
— 必要なのは clang + lld のみで、どのホストからでも全デスクトップターゲットをクロスコンパイル
できます。本拡張は Android 専用ですが Godot にはそれを宣言する手段が無いため、`.gdextension` の
デスクトップ各プラットフォームはこのスタブ（[`dummy/gdext_dummy.c`](dummy/gdext_dummy.c)）を指して
います。スタブは何も登録せず、コミットもしません。

### ビルド & インストール

ツールチェーンが `PATH` にある前提（Rust の `aarch64-linux-android` ターゲット、`cargo-ndk`、
`ANDROID_NDK_HOME`、Godot 4.7-stable バイナリ、`adb`）で、`scripts/build.sh`（または `scripts/build.ps1`）
が Android の4段階 — cargo-ndk ビルド → Godot APK エクスポート → `adb install` → 起動 — をまとめて
実行します。実行前に上記の事前準備（`.so` 4 個とアドオンの `.aar`/`.jar` の両方）を再チェックし、
欠けていれば同じ入手手順を表示します。

```bash
./scripts/build.sh --all      # ビルド + エクスポート + インストール + グラスで起動
```

## 使い方（MVP）

1. ライブラリを vendoring してビルド・デプロイ（上記 [ビルド](#ビルド) 参照）。
2. シーンに `XrealHeadTracker` ノードを追加し、その子に `Camera3D` を置く。
3. 実機ではカメラが頭の動きに追従します（3DoF）。

同梱の `demo/main.tscn` がボックスのリングでこれを実演します。

```
XrealHeadTracker (Node3D)   # ネイティブのヘッドポーズで回転
└── Camera3D                # current = true
```

API:

| メンバ | 説明 |
|---|---|
| `is_tracking() -> bool` | 直前フレームでネイティブのポーズが適用されたか |
| `recenter()` | 3DoF の正面方向をリセット（`RecenterGlasses`） |

## 構成

```
godot_xreal.gdextension  GDExtension マニフェスト（Android .so + デスクトップスタブ + dlopen 依存）
addons/godot_xreal/      インストール可能なアドオン
  plugin.cfg/.gd         EditorPlugin — エディタ dock も登録
  export_plugin.gd       Android エクスポート: manifest・権限・.aar/assets ステージング
  xreal_rig.tscn         XrealHeadTracker + Camera3D リグ
  editor/                dock: vendor_import_dock.gd（SDK 取込）, image_db_dock.gd
  android/               ブリッジ Java ソース + nr_plugins.json（vendoring した .aar は git 管理外）
src/                     Rust GDExtension 本体
  lib.rs                 ExtensionLibrary エントリ
  ffi.rs / native.rs     RE した ABI（repr(C) 構造体）+ XREAL .so の dlopen/dlsym
  session.rs/jni_bridge.rs  セッションのライフサイクル + Android Activity 取得
  signal_guard.rs        null-NativeGlasses teardown クラッシュ回避
  node.rs                XrealHeadTracker（Node3D）
  system.rs              XrealSystem（RefCounted）+ XrealAR（Node — AR 変化シグナル）
  camera_feed.rs         XrealCameraFeed（CameraFeed）= RGB カメラ
  hand_tracking.rs       XrealHandTracker（Node）→ XRHandTracker
  depth_mesh.rs · metrics.rs · video_encoder.rs · controller_probe.rs
                         AR メッシュ · レンダーメトリクス · FPV H.264 配信 · スマホ IMU ポインタ
  gl.rs / unity_plugin.rs   GLES + Unity ネイティブプラグイン emulation（表示パス）
  glasses_events.rs / native_error.rs   キャッシュ型イベント funnel
demo/                    AR デモ（main.tscn + 各 manager: hand/anchor/image/mesh/stream/
                         capture/blend + スマホタッチコントローラ）
dummy/                   デスクトップ GDExtension スタブ（gdext_dummy.c）= エディタ用
jniLibs/                 vendoring した XREAL .so（git 管理外）+ ビルド成果物 libgodot_xreal.so
scripts/                 build + vendor_xreal_libs + build_dummy_libs + build_image_db（.ps1/.sh）
.github/workflows/       CI（fmt/clippy/test/build）+ Release（プリビルトアドオン）
docs/                    guides / reference / plans / archive — 目次は docs/README.md
```

## ライセンス

以下のいずれかのライセンスを選択できます:

* Apache License, Version 2.0（[LICENSE-APACHE](LICENSE-APACHE) または http://www.apache.org/licenses/LICENSE-2.0 ）
* MIT license（[LICENSE-MIT](LICENSE-MIT) または http://opensource.org/licenses/MIT ）
