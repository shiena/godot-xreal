# godot-xreal

[English](README.md) | 日本語

`godot-xreal` は [XREAL](https://www.xreal.com/) グラスを駆動する Godot 4 用 GDExtension
（[godot-rust](https://godot-rust.github.io/) による Rust 実装）です。Unity 版 `com.xreal.xr` SDK
を、その **ネイティブライブラリを再利用する形で** Godot へ移植したものです。

> **ステータス: 初期スケルトン。** 現在のマイルストーンは **3DoF（頭の回転）で画面に映す** こと。
> ステレオ表示／コンポジタ経路は未実装です。[`docs/port-plan.md`](docs/port-plan.md) を参照。

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

## XREAL ランタイムライブラリの vendoring（必須）

XREAL のネイティブライブラリは本リポジトリに **含まれません**（XREAL の規約に従うため）。**XREAL SDK
for Unity**（`com.xreal.xr` パッケージ。tgz `com.xreal.xr.tar.gz` で提供）から入手し、APK を
エクスポートする前に **8 個の `.so` を `jniLibs/arm64-v8a/` に配置**してください（`jniLibs/` は git 管理外）:

1. `com.xreal.xr.tar.gz` を展開 → `package/` ディレクトリ。
2. **コア 3 個**（`package/Runtime/Plugins/Android/arm64-v8a/` からコピー、または
   `pwsh tools/vendor_xreal_libs.ps1 -XrealPackage <…>/package`）:
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
scripts/        build.ps1 / build.sh — build→export→install→run パイプライン
tools/          vendor_xreal_libs.ps1
docs/           移植計画 + リバースエンジニアリングメモ
```

## ライセンス

MIT（[LICENSE](LICENSE)）。XREAL のネイティブライブラリは **含まれず**、XREAL の規約に従います。
