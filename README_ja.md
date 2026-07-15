# godot-xreal

[English](README.md) | 日本語

`godot-xreal` は [XREAL](https://www.xreal.com/) グラスを駆動する Godot 4 用 GDExtension
（[godot-rust](https://godot-rust.github.io/) による Rust 実装）です。Unity 版 `com.xreal.xr` SDK
を、その **ネイティブライブラリを再利用する形で** Godot へ移植したものです（動作確認は SDK **3.1.0**）。

> **⚠️ 非公式・実験的。** 本プロジェクトは独立したコミュニティ製で、**XREAL 社とは無関係であり、
> 同社の承認・サポートを受けていません**。「XREAL」および SDK は各権利者に帰属します。ネイティブ
> ライブラリは**同梱しておらず**、ビルドの事前準備として自分で vendoring します（[ビルド](#ビルド) 参照）。
> vendoring した SDK の C ABI をリバースエンジニアリングして相互運用しています。利用は自己責任で。

## なぜ C# 翻訳ではなくネイティブ移植か

Unity SDK は Android の `.so` に薄く C# を被せた構造で、その `.so` がエンジン非依存のフラットな
C ABI をエクスポートしています（`libXREALNativeSessionManager.so` → `XREALGetHeadPoseAtTime` 等、
`libXREALXRPlugin.so` → OpenXR 的なコンポジタ・レイヤ API を含む 274 関数）。よって C# を翻訳せず、
この拡張は `.so` を `dlopen` して Godot に直接つなぎます。下層の難読化された NRSDK proc テーブル
（`libnr_api.so` / `NRGetProcAddr`）は回避します。詳細は
[`docs/reverse-engineering.md`](docs/reverse-engineering.md)。

## 対応プラットフォーム

XREAL のネイティブは **Android arm64 のみ** のため、対応端末（スマホ / Beam 等）に USB-C 接続した
**Godot Android アプリ** が対象です。デスクトップでも拡張はロードされ（シーン編集用）、その場合
ヘッドトラッキングは無効になります。

## 対応機能

**XREAL SDK for Unity 3.1.0** のネイティブライブラリを用いて XREAL One Pro で実機確認。以下はすべて
コミュニティによるリバースエンジニアリングでの相互運用であり、公式 API ではありません。

| 機能 | 状態 | 補足 |
|---|---|---|
| **ヘッドトラッキング**（姿勢: ピッチ / ヨー / ロール） | ✅ | XR-plugin の表示ポーズ由来。アイカメラを駆動。 |
| **トラッキングモード** 6DoF / 3DoF / 0DoF | ✅ | 選択可（`xreal/tracking_type` / `XrealSystem.set_tracking_type` / `debug.xreal.tracking_type`）。 |
| **ステレオ表示** — ヘッドロックの覗き窓 | ✅ | グラス越しにワールド固定 3D。**Multipass**（両眼）。唯一のステレオモード（セレクタなし）。 |
| **Multiview** ステレオ | ❌ 棚上げ | 右目が黒 — NR コンポジタ（`libnr_api`）が我々の client `GL_TEXTURE_2D_ARRAY` を取り込めず、かつ本リグ（2 SubViewport 描画）では性能利得も無い。コードは残すが無効化、開発者用エスケープ `setprop debug.xreal.force_multiview 1` のみ。詳細 `docs/codex-righteye-analysis.md`。 |
| **Recenter** | ✅ | 正面方向をリセット（SDK `NativePerception::Recenter`）。 |
| **RGB カメラ**（Godot `CameraFeed`） | ✅ | フルカラーで 3D シーン内のヘッドロックのクアッドに表示。**3DoF 必須**（6DoF SLAM とカメラを共有するため）。 |
| **グラス入力** — 物理キー（MENU/MULTI: クリック/ダブル/長押し） | ✅ | Godot シグナル（`key_event`, `key_state_changed`）。 |
| **装着センサー / 明るさ / 音量 / 調光 / USB ホットプラグ** | ✅ | シグナル（`wearing_changed`, `brightness_changed`, `glasses_connected` 等）。 |
| **診断** — セッション/トラッキング状態、HMD クロック、プラグイン版 | ✅ | `XrealSystem` 経由。 |
| **オンスクリーン・タッチコントローラ**（スマホ画面） | ✅（デモ） | アプリ層の Godot UI（`demo/touch_controller.gd`）: カスタマイズ可能なタッチパッド+ボタン→シグナル、スマホ振動ハプティクス。スマホにコントローラ・グラスに 3D を表示（画面分離）。ネイティブ非依存で SDK の `XREALVirtualController` に相当。 |
| **スマホ 3D ポインター**（ホスト IMU） | ✅（デモ） | スマホを傾けてグラス内に 3D レイを飛ばす（`demo/phone_pointer.gd`）。姿勢は `XrealSystem.poll_controller()` が露出する NRController の生 IMU（`accel`→ピッチ/ロール, `gyro`→ヨー）を GDScript で融合。本機では NRController の**融合ポーズ**も Godot 内蔵 `Input.get_gyroscope()` も空だったため。レイキャストで当たったオブジェクトをハイライト・トリガーで選択、オンスクリーンの左右手切替でレイの原点を切替、gyro ドリフトはバイアス学習+デッドゾーンで抑制。`recenter` で正面リセット。 |
| **マルチレジューム** — スマホを別アプリに切替えてもグラスのアプリが継続 | ✅ | 実機確認: Home/別アプリ後もグラス側でヘッドトラッキング+カメラが更新継続。manifest 足場（`nr_features=multiResume`+`NRFakeActivity`）で成立。フローティング「戻る」ボタンは**不可**（自前オーバーレイは Godot の GL サーフェスを乱す・NR `FloatingManager` は非 Unity アプリから不可）。 |

未実装: アプリカメラの 6DoF 位置、ハンド/画像/平面トラッキング、空間アンカー、メッシング、
音声/写真キャプチャ、NRSDK の高レベル知覚機能。（平面/画像/アンカー/メッシュは ARCore・AR Foundation
不要で移植可能 — 実現性調査: [`docs/ar-features-plan.md`](docs/ar-features-plan.md)。）

## ビルド

GDExtension 部分は素の godot-rust です。プロジェクト固有の手順は**一度だけ行う事前準備** — XREAL
ネイティブライブラリの vendoring — のみで、これを Android エクスポート前に済ませます。コマンドの詳細
（デスクトップ反復、手動 `cargo ndk` / Gradle、署名）: [`docs/build-and-release.md`](docs/build-and-release.md)。

**デスクトップエディタで開くだけならビルド不要**です: 本拡張は Android 専用で、`.gdextension` の
デスクトップ各プラットフォームはコミット済みの何もしないスタブ（[`dummy/`](dummy/gdext_dummy.c)）を
指しているため、ライブラリ欠落のエラーは出ません（再ビルドは `scripts/build_dummy_libs.ps1` / `.sh`
— ほぼ不要）。

### 事前準備: XREAL ランタイムライブラリの vendoring

XREAL のネイティブライブラリは本リポジトリに **含まれません**（XREAL の規約に従うため）。**XREAL SDK
for Unity**（`com.xreal.xr` パッケージ。tgz `com.xreal.xr.tar.gz` で提供。**動作確認済みは 3.1.0**）
を入手・展開し（→ `package/` ディレクトリ）、次を実行してください:

```powershell
pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <…>/package
```

このスクリプトが Android エクスポートに必要なものをすべて配置します（配置先はすべて git 管理外。
ダウンロードは行わず、パッケージは自分で用意します）:

**コア 3 個の `.so` → `jniLibs/arm64-v8a/`** — `Runtime/Plugins/Android/arm64-v8a/` からコピー。
`godot_xreal.gdextension` の `[dependencies]` 経由で APK に同梱され、起動時に `dlopen` されます:

| `.so` | 役割 |
|---|---|
| `libXREALNativeSessionManager.so` | セッション / ヘッドポーズの C ABI |
| `libXREALXRPlugin.so` | XR-plugin コンポジタ / 表示の C ABI |
| `libVulkanSupport.so` | 上記2つが必要とするサポート lib |

**5 個の `.aar` → `addons/godot_xreal/android/`** — アドオンのエクスポートプラグイン
（`export_plugin.gd`）が APK に取り込みます: グラスに必要な Java/JNI 層+manifest エントリを
担います。さらに NR 系ネイティブ lib（`jni/arm64-v8a/*.so`）も内包しており、Gradle が APK に
マージするため**別途抽出は不要**です。コピー元はすべて `Runtime/Plugins/Android/` 直下:

| `.aar` | 役割 | APK に届くネイティブ lib |
|---|---|---|
| `nr_loader.aar` | NR ローダの Java 層 | `libnr_loader.so` |
| `nr_api.aar` | NR API の Java 層 | `libnr_api.so` / `libnr_plugin_6dof.so` / `libnr_rgb_camera.so` |
| `nr_common.aar` | NR 共通層 | `libnr_libusb.so`（+ QNN/SNPE 系） |
| `GlassesDisplayPlugEvent-2.4.2.aar` | グラス検出 `GlassesInitProvider` | — |
| `Log-Control-1.2.aar` | 上記が参照する `LogControl` — **必須**（欠けると Godot 起動前にクラッシュ） | — |

**XrealBridge の Java ソース** — vendoring も事前コンパイルも*不要*: コミット済みのソース
（`addons/godot_xreal/android/src/`）をアドオンのエクスポートプラグインが gradle ビルド
テンプレートに配置し、エクスポート時の Gradle がコンパイルします。

**`nractivitylife*.aar` はコピー禁止** — ランチャーが Unity 専用のため Godot アプリでは起動不能に
なります。（`nr_common.aar` 内の QNN/SNPE 系 `.so` は本拡張では未使用ですが、`.aar` ごと APK に
入ります。）

### ビルド & インストール

ツールチェーンが `PATH` にある前提（Rust の `aarch64-linux-android` ターゲット、`cargo-ndk`、
`ANDROID_NDK_HOME`、Godot 4.7-stable バイナリ、`adb`）で、`scripts/build.sh`（または `scripts/build.ps1`）
が Android の4段階 — cargo-ndk ビルド → Godot APK エクスポート → `adb install` → 起動 — をまとめて
実行します。実行前に上記の事前準備（コア 3 個の `.so` とアドオンの `.aar`/`.jar` の両方）を再チェックし、
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
addons/godot_xreal/   インストール可能なアドオン（plugin.cfg, plugin.gd, xreal_rig.tscn,
                      export_plugin.gd + android/: ブリッジ Java ソース、vendoring した .aar/.jar は git 管理外）
src/
  lib.rs        ExtensionLibrary エントリ
  ffi.rs        repr(C) 構造体 / enum / 関数ポインタ型（RE した ABI）
  native.rs     XREAL .so の dlopen/dlsym
  session.rs    安全なライフサイクル + 座標変換
  node.rs       XrealHeadTracker（Node3D）= 3DoF MVP ノード
demo/           最小 Godot シーン
jniLibs/        vendoring した XREAL .so（git 管理外）+ ビルド成果物
scripts/        build.ps1 / build.sh（パイプライン）+ vendor_xreal_libs.ps1（ランタイム一式の配置）
docs/           移植計画 + リバースエンジニアリングメモ
```

## ライセンス

以下のいずれかのライセンスを選択できます:

* Apache License, Version 2.0（[LICENSE-APACHE](LICENSE-APACHE) または http://www.apache.org/licenses/LICENSE-2.0 ）
* MIT license（[LICENSE-MIT](LICENSE-MIT) または http://opensource.org/licenses/MIT ）
