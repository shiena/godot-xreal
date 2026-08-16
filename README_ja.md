# godot-xreal

[English](README.md) | 日本語

`godot-xreal` は [XREAL](https://www.xreal.com/) グラスを駆動する Godot 4 用 GDExtension です（[godot-rust](https://godot-rust.github.io/) による Rust 実装）。
Unity 版 `com.xreal.xr` SDK を、そのネイティブライブラリを再利用する形で Godot へ移植したものです（動作確認は SDK 3.1.0）。

開発は Godot 標準の XR ワークフローで行います。
`XROrigin3D` の下に `XRCamera3D` と `XRController3D` を置き、手の関節は `XRHandTracker`、ボタンは InputMap アクションで受け取ります。
アドオンはこの階層を置き換えず、取り付きます。

> **⚠️ 非公式かつ実験的。**
> 本プロジェクトは独立したコミュニティ製で、XREAL 社とは無関係であり、同社の承認もサポートも受けていません。
> 「XREAL」および SDK は各権利者に帰属します。
> ネイティブライブラリは同梱しておらず、ビルドの事前準備として自分で vendoring します（[XREAL ランタイムライブラリの vendoring](#xreal-ランタイムライブラリの-vendoring) 参照）。
> vendoring した SDK の C ABI をリバースエンジニアリングして相互運用しています。
> 利用は自己責任で。

## なぜ C# 翻訳ではなくネイティブ移植か

Unity SDK は Android の `.so` に薄く C# を被せた構造で、その `.so` がエンジン非依存のフラットな C ABI をエクスポートしています。
`libXREALNativeSessionManager.so` は `XREALGetHeadPoseAtTime` などを、`libXREALXRPlugin.so` は OpenXR 的なコンポジタレイヤ API を含む 274 関数を公開します。
よって C# を翻訳せず、この拡張は `.so` を `dlopen` して Godot に直接つなぎます。
下層の難読化された NRSDK proc テーブル（`libnr_api.so` の `NRGetProcAddr`）は回避します。
ABI の導出過程と RE 済み関数の一覧は、開発者向けドキュメント（目次 [`docs/develop/README.md`](docs/develop/README.md)）にあります。

## 対応プラットフォーム

XREAL のネイティブは Android arm64 のみのため、対応端末（スマホや Beam）に USB-C 接続した Godot Android アプリが対象です。
デスクトップでも拡張はロードされますが（シーン編集用）、ヘッドトラッキングは無効になります。

## 対応機能

XREAL SDK for Unity 3.1.0 のネイティブライブラリを用いて、XREAL One Pro（「Air 2 Ultra」表記の行は XREAL Air 2 Ultra）で実機確認しました。
以下はすべてコミュニティによるリバースエンジニアリングでの相互運用であり、公式 API ではありません。
各行の背景にある設計ノートと計測は、開発者向けドキュメント（目次 [`docs/develop/README.md`](docs/develop/README.md)）にあります。

| 機能 | 状態 | 補足 |
|---|---|---|
| **Godot 標準 XR ワークフロー**（`XROrigin3D`、`XRCamera3D`、`XRController3D`、`XRHandTracker`、InputMap） | ✅ | シーンの階層はアプリが所有し、アドオンがそこへ取り付きます。コントローラの入力名は Godot 標準の OpenXR アクション名（`trigger_click`、`grip_click`、`menu_button`、`primary_click`、`trigger`、`grip`、`primary`）です。以下の XREAL 固有機能は本アドオン独自のコンポーネントで提供します。 |
| **ヘッドトラッキング**（6DoF、回転と位置の world-lock） | ✅ | XR-plugin の表示ポーズ（フル姿勢と並進）でアイカメラを駆動します。 |
| **トラッキングモード**（6DoF / 3DoF / 0DoF） | ✅ | `xreal/tracking_type`、`XrealSystem.set_tracking_type`、`debug.xreal.tracking_type` で選択します。 |
| **ステレオ表示**（ヘッドロックの覗き窓） | ✅ | グラス越しにワールド固定の 3D を表示します。既定は Multipass（両眼）です。 |
| **Multiview** ステレオ（single-pass-instanced） | ✅ Vulkan Mobile のみ、利得はコンテンツ依存 | Multipass はどちらのレンダラーでも動作し、既定です。真の single-pass multiview があるのは Vulkan Mobile レンダラーだけです。プロジェクト設定 `xreal/xr_multiview_poc`（または `setprop debug.xreal.xr_multiview 1`）を有効にすると、自前の `XRInterfaceExtension` を通じて Godot が両眼を1パスで 2-layer ターゲットへ描画します。実機では draw call が半減し、draw call 律速のシーンで 5.9% 高速、100k splat の 3DGS シーンでもわずかに高速でした。一方 GPU 律速のシーンは Adreno 710 で 13% 低速で、利得はコンテンツに依存します。有効化には `xr/shaders/enabled=true` とエクスポートプリセットの XR Mode を `OpenXR` にする設定が必要です。設定の詳細は[アドオンの README](addons/godot_xreal/README.md#project-settings)にあります。Compatibility（GL）レンダラーではこの設定は警告付きで無視され、Multipass 経路になります。 |
| **Vulkan Mobile レンダラー**（実験的） | ✅ 実機検証済み、オプトイン | 第2のエクスポートプリセット「Android Vulkan」が、移植全体を Godot の Forward Mobile Vulkan レンダラーで動かします。出荷版の Compatibility ビルドと同居してインストールできます。グラス描画、RGB カメラ、FPV 配信と録画のいずれも実機で動作し、色は Compatibility ビルドと一致します。グラスは既定でティアリングのない `vkQueueWaitIdle` 同期で約 52 FPS です。より速いパイプライン方式は 58〜60 FPS に達しますが、速い首振りで視野の下部がずれます。SDK コンポジタが再投影でアイ画像をサンプルする際、Vulkan から GL へのコピーとの間に GPU フェンスがないためです。`debug.xreal.vk_sync 1` でティアリングを許容して 58〜60 FPS に切り替えられます。60 FPS でティアリングを消す修正には、Godot が有効化していない Vulkan 拡張が必要です。各アイと各エンコードフレームは、自前で確保した `VkImage` を opaque-fd の GL テクスチャとして共有することで Vulkan から SDK の GL コンポジタへ渡ります。そのためコンポジタは従来どおり素の GL テクスチャ名を受け取ります。Compatibility レンダラーにはない `RenderingDevice` と GPU コンピュートが使え、描画経路が Android XR や Project Aura と統一されます。サーマル soak が完了するまでは `debug.xreal.vulkan_glasses` の後ろに置かれ、既定は Compatibility のままです。 |
| **Recenter** | ✅ | 正面方向をリセットします（SDK の `NativePerception::Recenter`）。 |
| **レンダーメトリクス**（present FPS、dropped、early、latency） | ✅ | コンポジタの実測値を `NRMetrics*` API で直接取得します（Unity の `UpdateMetrics` sink は使いません）。`XrealSystem` の `get_present_fps()` や `get_dropped_frame_count()` などで読めます。 |
| **フォーカス平面**（コンポジタの再投影） | ✅ 実機検証待ち | コンポジタは VSync のたびに直前のフレームを最新の頭部ポーズへワープします。その基準となる平面を SDK は 1.4 m に固定しており、そこから離れた表示ほど尾を引いて二重に見えます。`XrealSystem.set_focus_plane()` が頭部ローカル座標で毎フレーム動かせます。`XrealFocusPlane` コンポーネントは SDK の `FocusManager` と同じく前方レイキャストから駆動します。`SetFocusPlane` export の引数は値渡しの `UnityXRVector3` 2 個（点と法線）で、Unity 側のラッパーが取る 3 個目の velocity はここへ届く前に捨てられます。 |
| **グラス入力**（物理キー MENU/MULTI のクリック、ダブル、長押し） | ✅ | Godot シグナル `key_event` と `key_state_changed` で受け取ります。 |
| **装着センサー、明るさ、音量、調光、USB ホットプラグ** | ✅ | `wearing_changed`、`brightness_changed`、`glasses_connected` などのシグナルで受け取ります。 |
| **診断**（セッションとトラッキングの状態、HMD クロック、プラグイン版） | ✅ | `XrealSystem` 経由で取得します。`get_capabilities()` は接続中のグラスが何に対応しているかを `bool` の `Dictionary` 1 個で返すので、メニュー構築のように全体像が要るコードはサブシステムごとの getter を呼ばずに済みます。 |
| **マルチレジューム**（スマホを別アプリに切替えてもグラスのアプリが描画ごと継続） | ✅ | Unity SDK がフローティングウインドウ（復帰ボタン）で行う所を、本移植では auto-enter Picture-in-Picture で実装しています。背景化するとアプリはスマホ隅の小タイル（pause だが可視）になり、Godot の GL スレッドと Surface が生存するため、グラスはライブ描画を続けます。タイルをタップすると全画面に復帰します。`XrealBridge.enableAutoEnterPiP` を `demo/main.gd` から駆動し、manifest 足場として `nr_features=multiResume` と `NRFakeActivity` を置いています。実機では submit カウンタが背景化後も進むことを確認しました。設計比較では、フローティングウインドウ、foreground service、SurfaceView 付け替えのいずれよりも PiP が優れていました。 |
| **キャプチャの音声**（マイクとアプリ音声） | ✅ | 録画も FPV 配信も両方載せられます。SDK のエンコーダが native に録音してミックスする方式で、Godot 自身のミキサーは経路に入りません。各キャプチャ機能の `audio_state` で選択します。マイクは `RECORD_AUDIO`、アプリ音声（内部音声）は Android の MediaProjection が必要です（`addInternalAudio` はエンコーダに `AudioPlaybackCapture` を開かせるため）。そのため、アプリ音声を要求する最初のキャプチャで画面キャプチャの同意ダイアログが出て、その回はマイクのみ、次回から両方入ります。マイクが通る DSP については[マイクが拾う音と拾わない音](#マイクが拾う音と拾わない音)を参照。 |
| **RGB カメラ**（Godot `CameraFeed`） | ✅（One シリーズ） | フルカラーで 3D シーン内のヘッドロックのクアッドに表示します。6DoF と同時に使えます（SLAM は別系統のグレースケールカメラを使うため）。 |
| **ハンドトラッキング**（両手26関節、Godot `XRHandTracker` へ） | ✅（Air 2 Ultra） | 手の関節を2つの `XRServer` ハンドトラッカ（`/user/hand_tracker/{left,right}`）へライブ供給します。デモは world-lock した関節球を描画します。One Pro は外向きカメラが無く `IsHandTrackingSupported()==false` を返すため、Air 2 Ultra 専用です。有効化は内部 `SetHandTrackingEnabled` と `input_source=3`。 |
| **平面検出**（GDScript へ） | ✅（Air 2 Ultra） | 水平と垂直の平面検出を `XrealSystem.set_plane_detection_mode()` と `poll_planes()`（追加、更新、削除をポーズ、サイズ、alignment 付きで返す）、`get_plane_boundary()` で提供します。`libXREALXRPlugin.so` のフラット C export で動くため追加 AAR は不要で、6DoF が必須です。4 つの AR 機能の C ABI は RE 確定済みです。 |
| **空間アンカー**（GDScript へ） | ✅（Air 2 Ultra） | ワールドアンカーの作成、永続化、復元を `XrealSystem.acquire_anchor()`、`poll_anchors()`、`save_anchor()`、`load_anchor()`、`estimate_anchor_quality()` などで提供します。フラット C export（`XRTrackedAnchor` レイアウトは実機確定）と同梱の `nr_spatial_anchor.aar` バックエンドで動き、6DoF が必須です。併せて `is_camera_supported()` と `is_hmd_feature_supported()`（SDK のデバイス別判定。Air 2 Ultra は RGB カメラ非搭載）も追加しています。 |
| **オンスクリーンタッチコントローラ**（スマホ画面） | ✅（デモ） | アプリ層の Godot UI です（`demo/touch_controller.gd`）。カスタマイズ可能なタッチパッドとボタンがシグナルを出し、スマホ振動でハプティクスを返します。スマホにコントローラ、グラスに 3D を表示する画面分離の構成で、ネイティブには依存しません。SDK の `XREALVirtualController` に相当します。 |
| **スマホコントローラー → Godot XR/Input** | ✅ | `XrealXRRuntime` が NRController の生 IMU とタッチパッドを取得し、`XrealXRInputRouter` が標準 `XRControllerTracker` のポーズへ変換します。各 tracker が publish するポーズは `aim`、`grip`、`default` で、OpenXR ランタイムと同じ構成です。そのため素の `XRController3D` を置くだけで、`pose` プロパティを設定しなくてもレイが向きます。グラスの物理キーとアプリ側のスマホ UI ボタンも、同じアドオン内のブリッジから `XRController3D` と `xr_select`/`xr_grab`/`xr_menu` へ届きます。デモは標準ポーズをレイとして表示するだけです。ネイティブの生ボタンビットは、実機で割り当てを確認するまで変換しません。 |

このほか画像トラッキング、マーカートラッキング、深度メッシュ、写真と合成のキャプチャ、FPV 配信も移植済みです。
深度メッシュは SDK の頂点ごとの意味分類を保持し、グラスで保存したスキャンはエディタ dock で `ArrayMesh` や `.glb` に変換できます。
一部は実機検証待ちです。

## インストール（プリビルト）

多くのユーザーはビルド不要です。
プリビルトのアドオンを入手して、XREAL 純正ライブラリを vendoring するだけで動きます。

1. [Releases](https://github.com/shiena/godot-xreal/releases) から `godot-xreal-<version>.zip` をダウンロードし、Godot 4.7 プロジェクトのルートに展開します。
   `godot_xreal.gdextension`、Android arm64 の `.so`、デスクトップエディタ用スタブ、`addons/godot_xreal/` が同梱されているので、Rust も cargo-ndk も clang も要りません。
2. プラグインを有効化します（プロジェクト → プロジェクト設定 → プラグイン →「Godot XREAL」）。
3. XREAL ランタイムライブラリを vendoring します（[XREAL ランタイムライブラリの vendoring](#xreal-ランタイムライブラリの-vendoring) 参照。`XREAL Import` dock ならワンクリックです）。
   これらは XREAL の規約に従うため同梱されません。

拡張を改造するのでなければ、ソースからのビルドは要りません（改造する場合は [ビルド（ソースから）](#ビルドソースから)）。

## XREAL ランタイムライブラリの vendoring

XREAL のネイティブライブラリは、XREAL の規約に従うため本リポジトリに含まれません。
**XREAL SDK for Unity**（`com.xreal.xr` パッケージ）を入手してください。
tgz `com.xreal.xr.tar.gz` で提供され、動作確認済みのバージョンは 3.1.0 です。
その中のライブラリを、次の3つのいずれかの方法で配置します。
どれも同じファイルを同じ git 管理外の配置先に置きます（配置先は下の表）。

1. **エディタ拡張（dock）**：推奨。アドオンを有効化し（プロジェクト → プロジェクト設定 → プラグイン →「Godot XREAL」）、左パネルの `XREAL Import` dock を開いて *Select package…* をクリックし、`com.xreal.xr(.tgz|.tar.gz)`（または展開済みの `package/` フォルダ）を選びます。
   システムの `tar` で展開して一式を配置し、再スキャンまで行うため、ターミナルは要りません。
2. **スクリプト**：ターミナルから次を実行します。
   ```powershell
   pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <…>/com.xreal.xr.tar.gz   # または展開済みの …/package
   ```
   （macOS / Linux は `./scripts/vendor_xreal_libs.sh <…>`。）
3. **手動展開**：tgz を自分で展開し、下の表のファイルをリポジトリ内の各配置先へコピーします。

vendoring が扱うのは XREAL 純正ライブラリだけです。
アドオン本体の `libgodot_xreal.so` は従来どおり `cargo ndk` ビルド（またはプリビルト）から入ります。

`.so` 4 個は `addons/godot_xreal/jniLibs/arm64-v8a/` へ置きます。
`godot_xreal.gdextension` の `[dependencies]` 経由で APK に同梱され、起動時に `dlopen` されます。
先頭3つのコピー元は `Runtime/Plugins/Android/arm64-v8a/` です。

| `.so` | 役割 |
|---|---|
| `libXREALNativeSessionManager.so` | セッションとヘッドポーズの C ABI |
| `libXREALXRPlugin.so` | XR-plugin のコンポジタと表示の C ABI |
| `libVulkanSupport.so` | 上記2つが必要とするサポート lib |
| `libmedia_codec.so` | FPV H.264 エンコーダ（`Runtime/Scripts/…/Camera Features/…/arm64/` から） |

`.aar` 7 個は `addons/godot_xreal/android/` へ置きます。
アドオンのエクスポートプラグイン（`export_plugin.gd`）がこれらを APK に取り込み、グラスに必要な Java/JNI 層と manifest エントリを供給します。
さらに NR 系ネイティブ lib（`jni/arm64-v8a/*.so`）も内包しており、Gradle が APK にマージするため別途抽出は不要です。
コピー元はすべて `Runtime/Plugins/Android/` 直下です。

| `.aar` | 役割 | APK に届くネイティブ lib |
|---|---|---|
| `nr_loader.aar` | NR ローダの Java 層 | `libnr_loader.so` |
| `nr_api.aar` | NR API の Java 層 | `libnr_api.so`、`libnr_plugin_6dof.so`、`libnr_rgb_camera.so` |
| `nr_common.aar` | NR 共通層 | `libnr_libusb.so`（と QNN/SNPE 系） |
| `nr_spatial_anchor.aar` | 空間アンカーのバックエンド | `libnr_spatial_anchor.so` |
| `nr_image_tracking.aar` | 画像トラッキングのバックエンド | `libnr_image_tracking.so` |
| `GlassesDisplayPlugEvent-2.4.2.aar` | グラス検出 `GlassesInitProvider` | なし |
| `Log-Control-1.2.aar` | 上記が参照する `LogControl`。欠けると Godot 起動前にクラッシュするため必須 | なし |

XrealBridge の Java ソースは、vendoring も事前コンパイルも要りません。
コミット済みのソース（`addons/godot_xreal/android/src/`）をアドオンのエクスポートプラグインが gradle ビルドテンプレートに配置し、エクスポート時の Gradle がコンパイルします。

`nractivitylife*.aar` はコピーしないでください。
ランチャーが Unity 専用のため、Godot アプリでは起動できなくなります。
（`nr_common.aar` 内の QNN/SNPE 系 `.so` は本拡張では未使用ですが、`.aar` ごと APK に入ります。）

## ビルド（ソースから）

必要になるのは拡張を改造する場合だけで、多くのユーザーはプリビルトを使います。
GDExtension 部分は素の godot-rust です。
先に XREAL ネイティブライブラリの vendoring を済ませてからビルドします。
コマンドの詳細（デスクトップ反復、手動の `cargo ndk` と Gradle、署名）は、開発者向けドキュメント（目次 [`docs/develop/README.md`](docs/develop/README.md)）にあります。

デスクトップエディタでライブラリ欠落エラーを出さずに開くには、クローン後に一度だけ何もしないスタブをビルドします。
`pwsh scripts/build_dummy_libs.ps1`（または `./scripts/build_dummy_libs.sh`）を実行するだけで、必要なのは clang と lld のみ、どのホストからでも全デスクトップターゲットをクロスコンパイルできます。
本拡張は Android 専用ですが、Godot にはそれを宣言する手段が無いため、`.gdextension` のデスクトップ各プラットフォームはこのスタブ（[`dummy/gdext_dummy.c`](dummy/gdext_dummy.c)）を指しています。
スタブは何も登録せず、コミットもしません。

### ビルド & インストール

ツールチェーンが `PATH` にある前提で（Rust の `aarch64-linux-android` ターゲット、`cargo-ndk`、`ANDROID_NDK_HOME`、Godot 4.7-stable バイナリ、`adb`）、`scripts/build.sh`（または `scripts/build.ps1`）が Android の4段階をまとめて実行します。
4段階は cargo-ndk ビルド、Godot APK エクスポート、`adb install`、起動です。
実行前に vendoring の状態（`.so` 4 個とアドオンの `.aar`/`.jar` の両方）を再チェックし、欠けていれば同じ入手手順を表示します。

```bash
./scripts/build.sh --all      # ビルド + エクスポート + インストール + グラスで起動
```

## 使い方

1. アドオンを導入し（[プリビルト](#インストールプリビルト) か [ソースからビルド](#ビルドソースから)）、ライブラリを vendoring します。
2. レンダラーを **Compatibility** にします。グラスへの表示は eye テクスチャを GL のテクスチャ名として XREAL コンポジタへ渡す方式で、これを供給できるコンテキストを持つのがこのレンダラーだけだからです。Forward+ や Mobile でもセッション、ヘッドトラッキング、スマホ側の表示は立ち上がるため、症状はグラスが黒いままという形でしか現れません。
3. Godot XR の作法どおりにシーンを組みます。`XROrigin3D` の下に `XRCamera3D` とコントローラを、好きな名前で置いてください。階層はアプリ側が所有します。その下に `addons/godot_xreal/features/xreal_xr_runtime.tscn` を追加すると、見つけた階層に自動で取り付きます。初期化コードは不要です。ゼロから始めるなら `addons/godot_xreal/xr_origin.tscn` を配置してください。同じ階層にこのコンポーネントを入れたものです。
4. PC で確認するときは `addons/godot_xreal/xreal_desktop_preview.tscn` も追加します。デスクトップ実行にはグラス向けの描画先が無いため、このコンポーネントが 2 枚目のウィンドウを開いてそこに 3D を描きます。右ドラッグで見回し、WASD で移動できます。実機では自分を破棄するので、そのまま残せます。操作の一覧は [アドオンの README](addons/godot_xreal/README.md#previewing-the-glasses-view-on-desktop) にあります。

同梱の `demo/main.tscn` が、ボックスのリングとオンスクリーンタッチコントローラでこれを実演します。アドオンの input router が XR 入力を標準のコントローラノードへ公開し、主要ボタンを InputMap の `xr_select`、`xr_grab`、`xr_menu` に変換します。

```
XROrigin3D                     # アプリ側が所有
├── XRCamera3D
├── LeftController  (XRController3D, tracker = left_hand)
├── RightController (XRController3D, tracker = right_hand)
└── XrealXRRuntime             # XREAL の bootstrap。上の階層に取り付く
```

`XrealHeadTracker` の主なメンバは次の2つです。

| メンバ | 説明 |
|---|---|
| `is_tracking() -> bool` | 直前フレームでネイティブのポーズが適用されたか |
| `recenter()` | 正面方向をリセットする（`RecenterGlasses`） |

全クラスのリファレンス（メソッド、シグナル、プロパティ、定数、GDScript の機能コンポーネントも含む）は、doc コメントから生成した [クラスリファレンス](docs/user/api/README.md) にあります。

## 機能ごとのセットアップ

ほとんどの機能は、サブシーンを配置するだけで動きます（`addons/godot_xreal/features/` 参照）。
追加の手順が要るのは次の2つで、いずれも XREAL 純正ツールを使います。
背景は XREAL の[開発者ドキュメント](https://docs.xreal.com/)を参照してください。

### 画像トラッキングの参照画像データベース

画像トラッキングは、実行時にコンパイル済みの参照画像**データベース blob**（`.bin`）を読み込みます。
Unity の `XREALImageLibraryBuildProcessor` に相当する処理を、vendoring した `trackableImageTools` CLI で自分の画像から行います。
この CLI は SDK パッケージの `Tools~/` 由来で、Windows と macOS のホストでのみ動き、[vendoring](#xreal-ランタイムライブラリの-vendoring)で `addons/godot_xreal/tools/` に配置されます。

推奨は「XREAL Image DB」エディタ dock です（アドオン有効化時に左パネルに表示）。

1. マニフェストを選ぶか、既定（`res://demo/image_tracking/reference.json`）のまま使います。
   1つのマニフェストは1つ以上の**セット**を持ち、各セットが実行時にアクティブ化されて巡回される1つのトラッキング DB になります。
2. 各参照画像について **Add image** を押し、ファイルを選んで実物の印刷幅（メートル）を入力します（GUID は自動生成）。
   1セット最大5枚が SDK の上限です。
   特徴点が少ない画像や自己相似が高い画像は、警告ではなく拒否されます。クラッシュしやすい DB が作られないようにしているためです。
3. **Build blob** を押すとツールが走り、マニフェストの隣に `.bin` を書き出します。

ターミナルからは `pwsh scripts/build_image_db.ps1`（既定 `demo/image_tracking/reference.json`）。

`demo/image_tracking/` の中身はコミットされません。
マニフェストも参照画像もビルドした `.bin` も git 管理外で、プロジェクトごとに用意します。
マニフェストは最初の **Add image** で dock が書き出します。
実行時は `xreal_image_tracking` 機能の `manifest_path` にマニフェストを指定すると、`XrealSystem.init_image_database` で全セットを登録します。

### FPV 配信の受信アプリ

デモの **Stream** ボタンは、一人称視点を H.264/RTP で配信します。
配信内容は AR シーンで、RGB カメラが ON のときはカメラと AR の合成です。
音声は機能の `audio_state` に応じて、マイクとアプリ音声のいずれか、または両方が載ります（[対応機能](#対応機能)の「キャプチャの音声」行を参照）。

受信方法は2つあります。
XREAL 公式の PC アプリを使うか、本リポジトリのスクリプトを使うかです。

どちらの場合も、受信側は同一 LAN の PC で、Stream を押す前に起動してください。
アプリがブロードキャストし、待ち受けている受信側が応答して配信が始まるため、アドレス入力は不要です。
順序は重要で、後から起動した受信側はハンドシェイクを取り逃しています。

#### 1. XREAL 公式の StreamingReceiver

XREAL の [First Person View](https://docs.xreal.com/Tools/First%20Person%20View) ページで配布されている PC アプリです。
起動して Stream を押すだけで、本移植は Unity SDK と同じ手順でペアリングします。

#### 2. 本リポジトリ同梱の受信サーバー

`scripts/stream_server/` に置いてあり、オープンソースのみで動くのでベンダー製ソフトは要りません。
映像（RFC 6184 H.264）も音声（RFC 3016 LATM で AAC-LC 16 kHz モノラル）もごく標準的な形式であることが分かったため、復号に独自実装は不要です。
用途に応じて2通りあります。

ブラウザで見るなら [`fpv_server.py`](scripts/stream_server/fpv_server.py) を使います。

```bash
python scripts/stream_server/fpv_server.py       # 起動後 http://localhost:8080 を開く
```

必要なのは python 3 だけで、`pip install` も ffmpeg も要りません。
サーバーは一切デコードせず、RTP を FLV に詰め替えて WebSocket で送るだけで、復号はブラウザ内蔵の H.264/AAC デコーダが行います。
視聴者はいつでも接続、切断できます。

ffplay で見る場合と録画する場合は、別途 ffmpeg が `PATH` に必要です。

```powershell
pwsh scripts/stream_server/receive.ps1           # ffplay のライブウインドウ
pwsh scripts/stream_server/receive.ps1 -Record   # 同フォルダに .mkv で録画
```

（macOS / Linux は `scripts/stream_server/receive.sh [--record]`。）

オプション、ディスカバリのプロトコル、静かな部屋で音声が無音になるのが不具合ではなく仕様である理由は、[`scripts/stream_server/README.md`](scripts/stream_server/README.md) にまとめています。

配信するのはカメラではなく自前のレンダーターゲットなので、RGB カメラは不要です。
カメラ非搭載の Air 2 Ultra でも動作します。

#### マイクが拾う音と拾わない音

エンコーダはマイクを `AUDIO_SOURCE_VOICE_COMMUNICATION` で開き、Acoustic Echo Canceler と Noise Suppression を付けています（`adb shell dumpsys audio` の `RecordActivityMonitor` で確認できます）。
これは素の録音ではなく通話用のフロントエンドなので、挙動が直感と食い違います。
故障と早合点する前に知っておく価値のある点を、すべて実機実測に基づいて挙げます。

- 静かな部屋は「ノイズフロア」ではなく完全なデジタル無音になります。
  全サンプルが同一値で、`mean == max == -91 dB` でした。
  音声経路が死んでいると判断する前に、何か音を出してください。
- 定常音は抑圧されます。
  1 kHz の連続トーンは、鳴り始めでレベルが 60 dB 跳ね上がった後、押し戻されました。
  定常信号に対するノイズサプレッサの正常動作です。
  試験には音声や音楽を使ってください。テストトーンは、この経路にとって最悪に近いプローブです。
- ハウリングしません。
  配信を再生しているスピーカーの隣にグラスを置いてもループしませんでした。
  エコーキャンセラが戻ってきたアプリ音声をエコーとみなし、サプレッサがループの育てる定常音を潰すためです。
- 両方有効時はアプリ音声が支配的です。
  BGM が -22 dBFS 前後に対し、マイクの寄与は -38 dBFS 未満でした。
  マイクが入っていないように見えがちです。

以前は Godot のミキサーから `AudioEffectCapture` と `HWEncoderNotifyAudioData` でアプリ音声を流し込んでおり、本 README も「エンジン側の制約でアプリ音声は不可能」と記載していました。
どちらも誤りでした。
`HWEncoderNotifyAudioData` はアプリ音声用ではなくマイク側のパイプラインに直結しているため、ネイティブのマイクと併用すると同一トラックに2つの producer が並びます。
その結果、音声トラックは映像の 1.79 倍の長さになり、その 35% が無音になっていました。
SDK が本来想定しているのは、上記の MediaProjection 経路です。

## 構成

```
godot_xreal.gdextension  GDExtension マニフェスト（Android .so + デスクトップスタブ + dlopen 依存）
addons/godot_xreal/      インストール可能なアドオン
  plugin.cfg/.gd         EditorPlugin — プロジェクト設定とエディタ dock を登録
  export_plugin.gd       Android エクスポート: manifest・権限・.aar/assets ステージング
  xr_origin.tscn         Godot 標準 XR ノードの共通階層
  xreal_rig.tscn         旧 XrealHeadTracker + Camera3D リグ
  xreal_desktop_preview.tscn/.gd   デスクトッププレビュー（実機では自分を破棄）
  xreal_android_bridge.gd   XrealBridge Java ヘルパの起動役（PiP、Activity 取得）
  features/              置くだけで動く機能コンポーネント: XR runtime、カメラ、平面、
                         アンカー、画像トラッキング、深度メッシュ、ハンド、
                         フォーカス平面、写真と合成のキャプチャ、配信、録画
  shaders/               各コンポーネントが共有する YCbCr とカメラ+AR 合成シェーダー
  editor/                dock: vendor_import_dock.gd（SDK 取込）, image_db_dock.gd,
                         mesh_snapshot_dock.gd（深度メッシュのスキャン → ArrayMesh/.glb）
  android/               ブリッジ Java ソース（nr_plugins.json と .aar は vendoring・git 管理外）
  tools/                 vendoring した trackableImageTools CLI（git 管理外）
  bin/                   ビルド済みライブラリ（git 管理外）: android/libgodot_xreal.so + デスクトップ dummy スタブ
src/                     Rust GDExtension 本体
  lib.rs                 ExtensionLibrary エントリ
  ffi.rs / native.rs     RE した ABI（repr(C) 構造体）+ XREAL .so の dlopen/dlsym
  session.rs/jni_bridge.rs  セッションのライフサイクル + Android Activity 取得
  signal_guard.rs        null-NativeGlasses teardown クラッシュ回避
  node.rs                XrealHeadTracker（Node3D）
  system.rs              XrealSystem（RefCounted）+ XrealAR（Node — AR 変化シグナル）
  camera_feed.rs         XrealCameraFeed（CameraFeed）= RGB カメラ
  hand_tracking.rs       XrealHandTracker（Node）→ XRHandTracker
  xr_interface.rs        XrealXrInterface = 標準 XR pose 経路 + オプトインの Vulkan multiview
  depth_mesh.rs · metrics.rs · video_encoder.rs · controller_probe.rs
                         AR メッシュ · レンダーメトリクス · FPV H.264 配信 · スマホ IMU ポインタ
  gl.rs / unity_plugin.rs   GLES + Unity ネイティブプラグイン emulation（表示パス）
  vk_bridge.rs / egl_context.rs   Vulkan グラスブリッジ（opaque-fd の VkImage → GL テクスチャ共有)
                         + 専用 EGL コンテキスト。ahb_probe.rs はその stage-0 共有プローブ
  glasses_events.rs / native_error.rs   キャッシュ型イベント funnel
  doc_gen.rs / api_docs.rs   doc 生成器（F1 スタブと docs/user/api）。ホストの cargo test で実行
demo/                    AR デモ（main.tscn + 各 manager: hand/anchor/image/mesh/stream/
                         capture/blend + スマホタッチコントローラ）
dummy/                   デスクトップ GDExtension スタブのソース（gdext_dummy.c + 生成物
                         stub_*.inc: クラス一覧、メンバ、F1 doc）= ビルド先は addons/godot_xreal/bin/
addons/godot_xreal/jniLibs/  vendoring した XREAL コア .so（git 管理外）
scripts/                 build + vendor_xreal_libs + build_dummy_libs + build_image_db
                         + gen_stub_classes / gen_docs / gen_api_docs（.ps1/.sh の双子）
  stream_server/         FPV 受信サーバー: fpv_server.py（ブラウザ）+ receive.ps1/.sh（ffplay/録画）
.github/workflows/       CI（fmt/clippy/test/build）+ Release（プリビルトアドオン）
docs/                    user/（生成クラスリファレンス api/ とその目次）と develop/（開発
                         ドキュメント: guides / reference / plans / archive、目次は各 README）
```

## ライセンス

以下のいずれかのライセンスを選択できます。

* Apache License, Version 2.0（[LICENSE-APACHE](LICENSE-APACHE) または http://www.apache.org/licenses/LICENSE-2.0 ）
* MIT license（[LICENSE-MIT](LICENSE-MIT) または http://opensource.org/licenses/MIT ）
