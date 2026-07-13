# godot-xreal

[English](README.md) | 日本語

`godot-xreal` は [XREAL](https://www.xreal.com/) グラスを駆動する Godot 4 用 GDExtension
（[godot-rust](https://godot-rust.github.io/) による Rust 実装）です。Unity 版 `com.xreal.xr` SDK
を、その **ネイティブライブラリを再利用する形で** Godot へ移植したものです。

> **⚠️ 非公式・実験的。** 本プロジェクトは独立したコミュニティ製で、**XREAL 社とは無関係であり、
> 同社の承認・サポートを受けていません**。「XREAL」および SDK は各権利者に帰属します。ネイティブ
> ライブラリは**同梱していません**（[vendoring](#xreal-ランタイムライブラリの-vendoring必須) 参照）。
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

XREAL One Pro で実機確認。以下はすべてコミュニティによるリバースエンジニアリングでの相互運用であり、公式 API ではありません。

| 機能 | 状態 | 補足 |
|---|---|---|
| **ヘッドトラッキング**（姿勢: ピッチ / ヨー / ロール） | ✅ | XR-plugin の表示ポーズ由来。アイカメラを駆動。 |
| **トラッキングモード** 6DoF / 3DoF / 0DoF | ✅ | 選択可（`xreal/tracking_type` / `XrealSystem.set_tracking_type` / `debug.xreal.tracking_type`）。 |
| **ステレオ表示** — ヘッドロックの覗き窓 | ✅ | グラス越しにワールド固定 3D。**Multipass**（両眼）。 |
| **Multiview** ステレオ | 🚧 WIP | 登録・head-lock は動作するが右目が現状黒。 |
| **Recenter** | ✅ | 正面方向をリセット（SDK `NativePerception::Recenter`）。 |
| **RGB カメラ**（Godot `CameraFeed`） | ✅ | フルカラーで 3D シーン内のヘッドロックのクアッドに表示。**3DoF 必須**（6DoF SLAM とカメラを共有するため）。 |
| **グラス入力** — 物理キー（MENU/MULTI: クリック/ダブル/長押し） | ✅ | Godot シグナル（`key_event`, `key_state_changed`）。 |
| **装着センサー / 明るさ / 音量 / 調光 / USB ホットプラグ** | ✅ | シグナル（`wearing_changed`, `brightness_changed`, `glasses_connected` 等）。 |
| **診断** — セッション/トラッキング状態、HMD クロック、プラグイン版 | ✅ | `XrealSystem` 経由。 |

未実装: アプリカメラの 6DoF 位置、ハンド/画像/平面トラッキング、空間アンカー、メッシング、
音声/写真キャプチャ、NRSDK の高レベル知覚機能。

## XREAL ランタイムライブラリの vendoring（必須）

XREAL のネイティブライブラリは本リポジトリに **含まれません**（XREAL の規約に従うため）。**XREAL SDK
for Unity**（`com.xreal.xr` パッケージ。tgz `com.xreal.xr.tar.gz` で提供）から入手し、APK を
エクスポートする前に **8 個の `.so` を `jniLibs/arm64-v8a/` に配置**してください（`jniLibs/` は git 管理外）:

1. `com.xreal.xr.tar.gz` を展開 → `package/` ディレクトリ。
2. **コア 3 個**（`package/Runtime/Plugins/Android/arm64-v8a/` からコピー、または
   `pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <…>/package`）:
   `libXREALNativeSessionManager.so` / `libXREALXRPlugin.so` / `libVulkanSupport.so`。
3. **NR 系 5 個**（各 `.aar` は zip。中の `jni/arm64-v8a/<lib>` を取り出す）:
   - `nr_api.aar` → `libnr_api.so` / `libnr_plugin_6dof.so` / `libnr_rgb_camera.so`
   - `nr_loader.aar` → `libnr_loader.so`
   - `nr_common.aar` → `libnr_libusb.so`

`scripts/build.ps1` / `scripts/build.sh` はエクスポート前にこれらを確認し、欠けていれば同じ入手手順を
表示して終了します。詳細: [`docs/build-and-release.md`](docs/build-and-release.md)。

## 使い方（MVP）

1. 拡張をビルドし XREAL ライブラリを vendoring（[`docs/build-and-release.md`](docs/build-and-release.md)）。
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
src/
  lib.rs        ExtensionLibrary エントリ
  ffi.rs        repr(C) 構造体 / enum / 関数ポインタ型（RE した ABI）
  native.rs     XREAL .so の dlopen/dlsym
  session.rs    安全なライフサイクル + 座標変換
  node.rs       XrealHeadTracker（Node3D）= 3DoF MVP ノード
demo/           最小 Godot シーン
jniLibs/        vendoring した XREAL .so（git 管理外）+ ビルド成果物
scripts/        build.ps1 / build.sh（パイプライン）+ vendor_xreal_libs.ps1（コア lib コピー）
docs/           移植計画 + リバースエンジニアリングメモ
```

## ライセンス

MIT（[LICENSE](LICENSE)）。XREAL のネイティブライブラリは **含まれず**、XREAL の規約に従います。
