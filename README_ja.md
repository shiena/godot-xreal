# godot-xreal

[English](README.md) | 日本語

`godot-xreal` は [XREAL](https://www.xreal.com/) グラスを制御する Godot 4 用 GDExtension です（[godot-rust](https://godot-rust.github.io/) による Rust 実装）。
Unity 版 `com.xreal.xr` SDK を、そのネイティブライブラリを再利用する形で Godot へ移植したものです（動作確認は SDK 3.1.0）。

開発は Godot 標準の XR ワークフローで行います。
`XROrigin3D` の下に `XRCamera3D` と `XRController3D` を配置します。
手の関節は `XRHandTracker`、ボタンは InputMap アクションで受け取ります。
アドオンはこの階層を置き換えません。既存の階層に組み込まれます。

> **⚠️ 非公式かつ実験的。**
> 本プロジェクトは独立したコミュニティ製で、XREAL 社とは無関係であり、同社の承認もサポートも受けていません。
> 「XREAL」および SDK は各権利者に帰属します。
> ネイティブライブラリは同梱していません。ビルドの事前準備として自分で vendoring します（[XREAL ランタイムライブラリの vendoring](#xreal-ランタイムライブラリの-vendoring) を参照）。
> vendoring した SDK の C ABI をリバースエンジニアリングして相互運用しています。
> 利用は自己責任でお願いします。

## なぜ C# 翻訳ではなくネイティブ移植か

Unity SDK は Android の `.so` に薄く C# を被せた構造です。
その `.so` が、エンジン非依存のフラットな C ABI をエクスポートしています。
`libXREALNativeSessionManager.so` は `XREALGetHeadPoseAtTime` などを、`libXREALXRPlugin.so` は OpenXR 的なコンポジタレイヤ API を含む 274 関数を公開します。
そこでこの拡張は、C# を翻訳せずに `.so` を `dlopen` し、Godot から直接呼び出します。
下層の難読化された NRSDK proc テーブル（`libnr_api.so` の `NRGetProcAddr`）は使用しません。
ABI の導出過程と RE 済み関数の一覧は、開発者向けドキュメント（目次 [`docs/develop/README.md`](docs/develop/README.md)）にあります。

## 対応プラットフォーム

XREAL のネイティブライブラリは Android arm64 のみに対応しています。
そのため、対応端末（スマホや Beam）に USB-C 接続した Godot Android アプリが対象です。
デスクトップでも拡張は読み込まれますが（シーン編集用）、ヘッドトラッキングは動作しません。

## 対応機能

XREAL SDK for Unity 3.1.0 のネイティブライブラリを用い、XREAL One Pro で実機確認しました（「Air 2 Ultra」と記した行は XREAL Air 2 Ultra での確認です）。
以下はすべてコミュニティによるリバースエンジニアリングでの相互運用であり、公式 API ではありません。
各行の背景にある設計ノートと計測は、[開発者向けドキュメント](docs/develop/README.md)にあります。

| 機能 | 状態 | 補足 |
|---|---|---|
| **Godot 標準 XR ワークフロー**（`XROrigin3D`、`XRCamera3D`、`XRController3D`、`XRHandTracker`、InputMap） | ✅ | シーンの階層はアプリが所有し、アドオンはそこに組み込まれます。コントローラの入力名は Godot 標準の OpenXR アクション名（`trigger_click`、`grip_click`、`menu_button`、`primary_click`、`trigger`、`grip`、`primary`）です。以下の XREAL 固有機能は、本アドオン独自のコンポーネントで提供します。 |
| **ヘッドトラッキング**（6DoF、回転と位置の world-lock） | ✅ | XR-plugin の表示ポーズ（フル姿勢と並進）を使用して、アイカメラの位置と向きを更新します。 |
| **トラッキングモード**（6DoF / 3DoF / 0DoF） | ✅ | `xreal/tracking_type` または `XrealSystem.set_tracking_type` で選択します。 |
| **ステレオ表示**（ヘッドロックの表示領域） | ✅ | グラス越しにワールド固定の 3D を表示します。デフォルトは Multipass（両眼）です。 |
| **Multiview** ステレオ（single-pass-instanced） | ✅ Vulkan Mobile のみ、利得はコンテンツ依存 | デフォルトは Multipass で、どちらのレンダラーでも動作します。真の single-pass multiview に対応するのは Vulkan Mobile レンダラーだけです。プロジェクト設定 `xreal/xr_multiview_poc`（または `setprop debug.xreal.xr_multiview 1`）を有効にすると、自前の `XRInterfaceExtension` を通じて Godot が両眼を 1 パスで 2-layer ターゲットへ描画します。実機では draw call が半減し、draw call 律速のシーンは 5.9% 高速、100k splat の 3DGS シーンもわずかに高速でした。GPU 律速のシーンは Adreno 710 で 13% 低速です。有効化には `xr/shaders/enabled=true` と、エクスポートプリセットの XR Mode を `OpenXR` にする設定が必要です（詳細は[アドオンの README](addons/godot_xreal/README.md#project-settings)）。Compatibility（GL）レンダラーではこの設定を警告付きで無視し、Multipass で動作します。 |
| **Vulkan Mobile レンダラー** | ✅ 実機検証済み、デフォルト | 第 2 のエクスポートプリセット「Android Vulkan」が、移植全体を Godot の Forward Mobile Vulkan レンダラーで動作させます。出荷版の Compatibility ビルドと同居してインストールできます。グラス描画、RGB カメラ、FPV 配信と録画のいずれも実機で動作し、色は Compatibility ビルドと一致します。グラスはティアリングなしで描画されます。同期方式は起動時に自動選択され、`VK_KHR_external_semaphore_fd` デバイス拡張が有効なら 60 FPS、無い場合は約 52 FPS で動作します。この拡張は `rendering/rendering_device/vulkan/additional_device_extensions` プロジェクト設定から要求し、`project.godot` に記載済みです。60 FPS で動作させるには、この設定を備えた Godot のエクスポートテンプレートが必要です。本リポジトリではこれがデフォルトのレンダリング方式で、Android のエクスポートプリセットは両方ともこれを出力します。Compatibility レンダラーにはない `RenderingDevice` と GPU コンピュートが使用でき、描画経路が Android XR や Project Aura と揃います。 |
| **Recenter** | ✅ | 正面方向をリセットします（SDK の `NativePerception::Recenter`）。 |
| **レンダーメトリクス**（present FPS、dropped、early、latency） | ✅ | コンポジタの実測値を `NRMetrics*` API で直接取得します（Unity の `UpdateMetrics` sink は使用しません）。`XrealSystem` の `get_present_fps()` や `get_dropped_frame_count()` などで読み取れます。 |
| **フォーカス平面**（コンポジタの再投影） | ✅ 実機検証待ち | コンポジタは VSync のたびに、直前のフレームを最新の頭部ポーズへワープします。その基準となる平面を SDK は 1.4 m に固定しています。そのため、平面から離れた表示ほど残像が残り、二重像が発生します。`XrealSystem.set_focus_plane()` に頭部ローカル座標を渡せば、毎フレーム移動できます。`XrealFocusPlane` コンポーネントは、SDK の `FocusManager` と同じく前方レイキャストの結果で更新します。`SetFocusPlane` export の引数は値渡しの `UnityXRVector3` 2 個（点と法線）です。Unity 側のラッパーが取る 3 個目の velocity は、ここへ届く前に破棄されます。 |
| **グラス入力**（物理キー MENU/MULTI のクリック、ダブル、長押し） | ✅ | Godot シグナル `key_event` と `key_state_changed` で受け取ります。 |
| **装着センサー、明るさ、音量、調光、USB ホットプラグ** | ✅ | `wearing_changed`、`brightness_changed`、`glasses_connected` などのシグナルで受け取ります。 |
| **診断**（セッションとトラッキングの状態、HMD クロック、プラグイン版） | ✅ | `XrealSystem` 経由で取得します。`get_capabilities()` は、接続中のグラスが対応している機能を `bool` の `Dictionary` 1 個で返します。そのため、メニュー構築のように全体像が必要なコードは、サブシステムごとの getter を呼ばずに済みます。 |
| **マルチレジューム**（スマホを別アプリに切替えてもグラスのアプリが描画ごと継続） | ✅ | Unity SDK がフローティングウィンドウ（復帰ボタン）で実現している部分を、本移植では auto-enter Picture-in-Picture で実装しています。背景化するとアプリはスマホ隅の小タイル（pause 状態だが可視）になります。Godot の GL スレッドと Surface は破棄されずに維持されるため、グラスへの描画は継続します。タイルをタップすると全画面に復帰します。`XrealBridge.enableAutoEnterPiP` を `demo/main.gd` から呼び出し、manifest には `nr_features=multiResume` と `NRFakeActivity` を追加しています。実機では、submit カウンタが背景化後も進むことを確認しました。設計比較では、フローティングウィンドウ、foreground service、SurfaceView 付け替えのいずれよりも PiP が優れていました。 |
| **キャプチャの音声**（マイクとアプリ音声） | ✅ | 録画にも FPV 配信にも、両方の音声を追加できます。SDK のエンコーダが native に録音してミックスする方式で、Godot 自身のミキサーは経路に入りません。各キャプチャ機能の `audio_state` で選択します。マイクには `RECORD_AUDIO`、アプリ音声（内部音声）には Android の MediaProjection が必要です（`addInternalAudio` はエンコーダに `AudioPlaybackCapture` を開かせるため）。そのため、アプリ音声を要求する最初のキャプチャで画面キャプチャの同意ダイアログが表示されます。その回はマイクのみ、次回から両方が入ります。マイクに適用される DSP については[マイクが拾う音と拾わない音](#マイクが拾う音と拾わない音)を参照してください。 |
| **RGB カメラ**（Godot `CameraFeed`） | ✅（One シリーズ） | フルカラーで、3D シーン内のヘッドロックのクアッドに表示します。6DoF と同時に使用できます（SLAM は別系統のグレースケールカメラを使用するため）。 |
| **ハンドトラッキング**（両手 26 関節、Godot `XRHandTracker` へ） | ✅（Air 2 Ultra） | 手の関節を 2 つの `XRServer` ハンドトラッカ（`/user/hand_tracker/{left,right}`）へ毎フレーム送信します。デモは world-lock した関節球を描画します。One Pro は外向きカメラがなく `IsHandTrackingSupported()==false` を返すため、Air 2 Ultra 専用です。有効化は内部 `SetHandTrackingEnabled` と `input_source=3`。 |
| **平面検出**（GDScript へ） | ✅（Air 2 Ultra） | 水平と垂直の平面検出を `XrealSystem.set_plane_detection_mode()` と `poll_planes()`（追加、更新、削除をポーズ、サイズ、alignment 付きで返す）、`get_plane_boundary()` で提供します。`libXREALXRPlugin.so` のフラット C export で動作するため追加 AAR は不要ですが、6DoF が必須です。4 つの AR 機能の C ABI は RE 確定済みです。 |
| **空間アンカー**（GDScript へ） | ✅（Air 2 Ultra） | ワールドアンカーの作成、永続化、復元を `XrealSystem.acquire_anchor()`、`poll_anchors()`、`save_anchor()`、`load_anchor()`、`estimate_anchor_quality()` などで提供します。フラット C export（`XRTrackedAnchor` レイアウトは実機確定）と同梱の `nr_spatial_anchor.aar` バックエンドで動作し、6DoF が必須です。併せて `is_camera_supported()` と `is_hmd_feature_supported()`（SDK のデバイス別判定。Air 2 Ultra は RGB カメラ非搭載）も追加しています。 |
| **オンスクリーンタッチコントローラ**（スマホ画面） | ✅（デモ） | アプリ層の Godot UI です（`demo/touch_controller.gd`）。カスタマイズ可能なタッチパッドとボタンがシグナルを送信し、スマホの振動でハプティクスを返します。スマホにコントローラ、グラスに 3D を表示する画面分離の構成で、ネイティブには依存しません。SDK の `XREALVirtualController` に相当します。 |
| **スマホコントローラ → Godot XR/Input** | ✅ | `XrealXRRuntime` が NRController の生 IMU とタッチパッドを取得し、`XrealXRInputRouter` が標準 `XRControllerTracker` のポーズへ変換します。各 tracker が publish するポーズは `aim`、`grip`、`default` で、OpenXR ランタイムと同じ構成です。そのため、素の `XRController3D` を配置するだけで、`pose` プロパティを設定しなくてもレイが正しい向きになります。グラスの物理キーとアプリ側のスマホ UI ボタンも、同じアドオン内のブリッジ経由で `XRController3D` と `xr_select`/`xr_grab`/`xr_menu` に送られます。デモは標準ポーズをレイとして表示するだけです。ネイティブの生ボタンビットは、実機で割り当てを確認するまで変換しません。 |

このほか画像トラッキング、マーカートラッキング、深度メッシュ、写真と合成のキャプチャ、FPV 配信も移植済みです。
深度メッシュは SDK の頂点ごとの意味分類を保持します。
グラスで保存したスキャンは、エディタ dock で `ArrayMesh` や `.glb` に変換できます。
一部は実機検証待ちです。

## インストール（プリビルト）

プリビルトのアドオンを入手して XREAL 純正ライブラリを vendoring すれば、自分でビルドせずに動作します。

1. [Releases](https://github.com/shiena/godot-xreal/releases) から `godot-xreal-<version>.zip` をダウンロードし、Godot 4.7 プロジェクトのルートに展開します。
   `godot_xreal.gdextension`、Android arm64 の `.so`、デスクトップエディタ用スタブ、`addons/godot_xreal/` が同梱されているため、Rust、cargo-ndk、clang はいずれも不要です。
2. ［プロジェクト］＞［プロジェクト設定］＞［プラグイン］で「Godot XREAL」を有効にします。
3. XREAL ランタイムライブラリを vendoring します（[XREAL ランタイムライブラリの vendoring](#xreal-ランタイムライブラリの-vendoring) を参照。「XREAL Import」dock ならワンクリックです）。
   これらのライブラリは XREAL の規約に従うため同梱していません。

拡張そのものを改造する場合は、[ソースからビルド](#ビルドソースから)します。

## XREAL ランタイムライブラリの vendoring

XREAL のネイティブライブラリは、XREAL の規約に従うため本リポジトリに含まれません。
**XREAL SDK for Unity**（`com.xreal.xr` パッケージ）を入手します。
tgz `com.xreal.xr.tar.gz` で提供され、動作確認済みのバージョンは 3.1.0 です。
その中のライブラリを、次の 3 つのいずれかの方法で配置します。
どの方法も、同じファイルを同じ git 管理外の配置先（下の表）に置きます。

1. **エディタ拡張（dock）**：推奨。［プロジェクト］＞［プロジェクト設定］＞［プラグイン］でアドオンを有効にし、左パネルの「XREAL Import」dock を開いて［Select package…］をクリックします。
   `com.xreal.xr(.tgz|.tar.gz)`（または展開済みの `package/` フォルダ）を選択すると、システムの `tar` で展開して一式を配置し、再スキャンまで実行します。ターミナルでの操作は不要です。
2. **スクリプト**：ターミナルから次を実行します。
   ```powershell
   pwsh scripts/vendor_xreal_libs.ps1 -XrealPackage <…>/com.xreal.xr.tar.gz   # または展開済みの …/package
   ```
   （macOS / Linux は `./scripts/vendor_xreal_libs.sh <…>`。）
3. **手動展開**：tgz を自分で展開し、下の表のファイルをリポジトリ内の各配置先へコピーします。

vendoring が扱うのは XREAL 純正ライブラリだけです。
アドオン本体の `libgodot_xreal.so` は、従来どおり `cargo ndk` ビルド（またはプリビルト）から入ります。

`.so` 4 個は `addons/godot_xreal/jniLibs/arm64-v8a/` へ配置します。
`godot_xreal.gdextension` の `[dependencies]` 経由で APK に同梱され、起動時に `dlopen` されます。
先頭 3 つのコピー元は `Runtime/Plugins/Android/arm64-v8a/` です。

| `.so` | 役割 |
|---|---|
| `libXREALNativeSessionManager.so` | セッションとヘッドポーズの C ABI |
| `libXREALXRPlugin.so` | XR-plugin のコンポジタと表示の C ABI |
| `libVulkanSupport.so` | 上記 2 つが必要とするサポート lib |
| `libmedia_codec.so` | FPV H.264 エンコーダ（`Runtime/Scripts/…/Camera Features/…/arm64/` から） |

`.aar` 7 個は `addons/godot_xreal/android/` へ配置します。
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

XrealBridge の Java ソースは、vendoring も事前コンパイルも不要です。
コミット済みのソース（`addons/godot_xreal/android/src/`）を、アドオンのエクスポートプラグインが gradle ビルドテンプレートに配置します。
エクスポート時の Gradle が、それをコンパイルします。

> **注意**: `nractivitylife*.aar` はコピーしないでください。ランチャーが Unity 専用のため、Godot アプリが起動できなくなります。

（`nr_common.aar` 内の QNN/SNPE 系 `.so` は本拡張では使用しませんが、`.aar` ごと APK に入ります。）

## ビルド（ソースから）

GDExtension 部分は素の godot-rust です。
先に XREAL ネイティブライブラリの vendoring を済ませてからビルドします。
コマンドの詳細（デスクトップ反復、手動の `cargo ndk` と Gradle、署名）は、[開発者向けドキュメント](docs/develop/README.md)にあります。

デスクトップエディタでライブラリ欠落エラーを出さずに開くには、クローン後に一度だけ、何もしないスタブをビルドします。
`pwsh scripts/build_dummy_libs.ps1`（または `./scripts/build_dummy_libs.sh`）を実行するだけです。
必要なのは clang と lld のみで、どのホストからでも全デスクトップターゲットをクロスコンパイルできます。
本拡張は Android 専用ですが、Godot にはそれを宣言する手段がありません。
そのため、`.gdextension` のデスクトップ各プラットフォームは、このスタブ（[`dummy/gdext_dummy.c`](dummy/gdext_dummy.c)）を指しています。
スタブは何も登録せず、コミットもしません。

### ビルドから起動までをまとめるスクリプト

`scripts/build.sh`（または `scripts/build.ps1`）が、Android の 4 段階をまとめて実行します。
4 段階は cargo-ndk ビルド、Godot APK エクスポート、`adb install`、起動です。
実行にはツールチェーンが `PATH` にあることが前提です（Rust の `aarch64-linux-android` ターゲット、`cargo-ndk`、`ANDROID_NDK_HOME`、Godot 4.7-stable バイナリ、`adb`）。
スクリプトは実行前に vendoring の状態（`.so` 4 個とアドオンの `.aar`/`.jar` の両方）を再チェックし、欠けていれば同じ入手手順を表示します。

```bash
./scripts/build.sh --all      # ビルド + エクスポート + インストール + グラスで起動
```

## 使い方

1. アドオンを導入し（[プリビルト](#インストールプリビルト) か [ソースからビルド](#ビルドソースから)）、ライブラリを vendoring します。
2. レンダラーを選びます。
   デフォルトは **Mobile** で、グラスをティアリングなしで描画します。
   同期方式は起動時に自動選択されます。
   `VK_KHR_external_semaphore_fd` デバイス拡張が有効なら 60 FPS、無い場合は約 52 FPS で動作します。
   この拡張は `rendering/rendering_device/vulkan/additional_device_extensions` プロジェクト設定から要求します。
   60 FPS で動作させるには、この設定を備えた Godot のエクスポートテンプレートが必要です。
   Forward+ は選べません。セッション、ヘッドトラッキング、スマホ側の表示は動作しますが、グラスは黒いままになります。
3. `XROrigin3D` の下に `XRCamera3D` とコントローラを配置します。
   ノード名は任意です。階層はアプリ側が所有します。
4. 同じ階層に `addons/godot_xreal/features/xreal_xr_runtime.tscn` を追加します。
   このコンポーネントは、見つけた階層に自動的に組み込まれます。初期化コードは不要です。
   ゼロから始める場合は、手順 3 と 4 の構成をあらかじめ組んだ `addons/godot_xreal/xr_origin.tscn` を配置します。
5. PC で確認する場合は、`addons/godot_xreal/xreal_desktop_preview.tscn` も追加します。
   デスクトップ実行にはグラス向けの描画先がないため、このコンポーネントが 2 枚目のウィンドウを開いて 3D を描画します。
   右ドラッグで見回し、WASD で移動できます。
   実機では自動的に削除されるため、そのまま残せます。
   操作の一覧は[アドオンの README](addons/godot_xreal/README.md#previewing-the-glasses-view-on-desktop)にあります。

同梱の `demo/main.tscn` が、ボックスのリングとオンスクリーンタッチコントローラでこれを実演します。
アドオンの input router が XR 入力を標準のコントローラノードへ公開し、主要ボタンを InputMap の `xr_select`、`xr_grab`、`xr_menu` に変換します。

```
XROrigin3D                     # アプリ側が所有
├── XRCamera3D
├── LeftController  (XRController3D, tracker = left_hand)
├── RightController (XRController3D, tracker = right_hand)
└── XrealXRRuntime             # XREAL の bootstrap。上の階層に組み込まれる
```

`XrealHeadTracker` の主なメンバは次の 2 つです。

| メンバ | 説明 |
|---|---|
| `is_tracking() -> bool` | 直前フレームでネイティブのポーズが適用されたか |
| `recenter()` | 正面方向をリセットする（`RecenterGlasses`） |

全クラスのリファレンス（メソッド、シグナル、プロパティ、定数、GDScript の機能コンポーネントも含む）は、doc コメントから生成した[クラスリファレンス](docs/user/api/README.md)にあります。

## 機能ごとのセットアップ

ほとんどの機能は、サブシーンを配置するだけで動作します（`addons/godot_xreal/features/` を参照）。
追加の手順が必要なのは次の 2 つで、いずれも XREAL 純正ツールを使用します。
背景は XREAL の[開発者ドキュメント](https://docs.xreal.com/)を参照してください。

### 画像トラッキングの参照画像データベース

画像トラッキングは、実行時にコンパイル済みの参照画像**データベース blob**（`.bin`）を読み込みます。
Unity の `XREALImageLibraryBuildProcessor` に相当する処理を、vendoring した `trackableImageTools` CLI で自分の画像から実行します。
この CLI は SDK パッケージの `Tools~/` 由来で、Windows と macOS のホストでのみ動作します。
[vendoring](#xreal-ランタイムライブラリの-vendoring)で `addons/godot_xreal/tools/` に配置されます。

推奨は「XREAL Image DB」エディタ dock です（アドオン有効化時に左パネルに表示）。

1. マニフェストを選択するか、デフォルト（`res://demo/image_tracking/reference.json`）のまま使用します。
   1 つのマニフェストは 1 つ以上の**セット**を持ちます。各セットが、実行時にアクティブ化されて巡回される 1 つのトラッキング DB になります。
2. 各参照画像について［Add image］をクリックし、ファイルを選択して実物の印刷幅（メートル）を入力します（GUID は自動生成）。
   1 セットあたり最大 5 枚が SDK の上限です。
   特徴点が少ない画像や自己相似が高い画像は、警告ではなく拒否されます。
   クラッシュしやすい DB が作られないようにしているためです。
3. ［Build blob］をクリックします。
   ツールが実行され、マニフェストの隣に `.bin` を書き出します。

ターミナルから実行する場合は `pwsh scripts/build_image_db.ps1` です（デフォルト `demo/image_tracking/reference.json`）。

`demo/image_tracking/` の中身はコミットされません。
マニフェストも参照画像もビルドした `.bin` も git 管理外で、プロジェクトごとに用意します。
マニフェストは、最初の［Add image］で dock が書き出します。
実行時は `xreal_image_tracking` 機能の `manifest_path` にマニフェストを指定します。
すると、`XrealSystem.init_image_database` で全セットを登録します。

### FPV 配信の受信アプリ

デモの［Stream］は、一人称視点を H.264/RTP で配信します。
配信内容は AR シーンで、RGB カメラが ON のときはカメラと AR の合成です。
音声は機能の `audio_state` に応じて、マイクとアプリ音声のいずれか、または両方が含まれます（[対応機能](#対応機能)の「キャプチャの音声」行を参照）。

受信方法は 2 つあります。
XREAL 公式の PC アプリを使用するか、本リポジトリのスクリプトを使用するかです。

> **注意**: どちらの場合も、受信側は同一 LAN の PC で動作させ、［Stream］をクリックする前に起動してください。後から起動した受信側はハンドシェイクを取り逃すため、順序を入れ替えることはできません。

アプリがブロードキャストし、待ち受けている受信側が応答して配信が始まります。
そのため、アドレスの入力は不要です。

#### 1. XREAL 公式の StreamingReceiver

XREAL の [First Person View](https://docs.xreal.com/Tools/First%20Person%20View) ページで配布されている PC アプリです。
起動して［Stream］をクリックするだけで、本移植は Unity SDK と同じ手順でペアリングします。

#### 2. 本リポジトリ同梱の受信サーバー

`scripts/stream_server/` に置いてあり、オープンソースのみで動作するためベンダー製のソフトウェアは不要です。
映像（RFC 6184 H.264）も音声（RFC 3016 LATM で AAC-LC 16 kHz モノラル）も、ごく標準的な形式であることが分かりました。
そのため、復号に独自実装は不要です。
用途に応じて 2 通りあります。

ブラウザで見る場合は [`fpv_server.py`](scripts/stream_server/fpv_server.py) を使用します。

```bash
python scripts/stream_server/fpv_server.py       # 起動後 http://localhost:8080 を開く
```

必要なのは python 3 だけで、`pip install` も ffmpeg も不要です。
サーバーは一切デコードしません。RTP を FLV に変換して WebSocket で送信するだけで、復号はブラウザ内蔵の H.264/AAC デコーダが実行します。
視聴者はいつでも接続、切断できます。

ffplay で見る場合と録画する場合は、別途 ffmpeg が `PATH` に必要です。

```powershell
pwsh scripts/stream_server/receive.ps1           # ffplay のライブウィンドウ
pwsh scripts/stream_server/receive.ps1 -Record   # 同フォルダに .mkv で録画
```

（macOS / Linux は `scripts/stream_server/receive.sh [--record]`。）

オプション、ディスカバリのプロトコル、静かな部屋で音声が無音になるのが不具合ではなく仕様である理由は、[`scripts/stream_server/README.md`](scripts/stream_server/README.md) にまとめています。

配信するのはカメラではなく自前のレンダーターゲットです。
そのため RGB カメラは不要で、カメラ非搭載の Air 2 Ultra でも動作します。

#### マイクが拾う音と拾わない音

エンコーダはマイクを `AUDIO_SOURCE_VOICE_COMMUNICATION` で開き、Acoustic Echo Canceler と Noise Suppression を適用しています（`adb shell dumpsys audio` の `RecordActivityMonitor` で確認できます）。
素の録音ではなく通話用のフロントエンドのため、挙動が直感と食い違います。
故障と早合点しないために、実機で測定して分かったことを挙げます。

- 静かな部屋は「ノイズフロア」ではなく完全なデジタル無音になります。
  全サンプルが同一値で、`mean == max == -91 dB` でした。
  音声経路が機能していないと判断する前に、何か音を出してください。
- 定常音は抑制されます。
  1 kHz の連続トーンは、鳴り始めでレベルが 60 dB 跳ね上がった後、抑制されました。
  定常信号に対するノイズサプレッサの正常な動作です。
  試験には音声や音楽を使用してください。テストトーンは、この経路にとって最悪に近いプローブです。
- ハウリングは発生しません。
  配信を再生しているスピーカーの隣にグラスを置いてもループしませんでした。
  エコーキャンセラが戻ってきたアプリ音声をエコーとみなし、サプレッサがループで増幅される定常音を抑制するためです。
- 両方を有効にすると、アプリ音声が支配的になります。
  BGM が -22 dBFS 前後に対し、マイクの寄与は -38 dBFS 未満でした。
  そのため、マイクが入っていないように見えがちです。

以前は Godot のミキサーから `AudioEffectCapture` と `HWEncoderNotifyAudioData` でアプリ音声を送っており、本 README も「エンジン側の制約でアプリ音声は不可能」と記載していました。
どちらも誤りでした。
`HWEncoderNotifyAudioData` は、アプリ音声用ではなくマイク側のパイプラインに直結しています。
そのため、ネイティブのマイクと併用すると、同一トラックに 2 つの producer が並びます。
その結果、音声トラックは映像の 1.79 倍の長さになり、その 35% が無音になっていました。
SDK が本来想定しているのは、上記の MediaProjection 経路です。

## 構成

```
godot_xreal.gdextension  GDExtension マニフェスト（Android .so + デスクトップスタブ + dlopen 依存）
addons/godot_xreal/      インストール可能なアドオン
  plugin.cfg/.gd         EditorPlugin: プロジェクト設定とエディタ dock を登録
  export_plugin.gd       Android エクスポート: manifest、権限、.aar/assets ステージング
  xr_origin.tscn         Godot 標準 XR ノードの共通階層
  xreal_rig.tscn         旧 XrealHeadTracker + Camera3D リグ
  xreal_desktop_preview.tscn/.gd   デスクトッププレビュー（実機では自動的に削除）
  xreal_android_bridge.gd   XrealBridge Java ヘルパの起動役（PiP、Activity 取得）
  features/              配置するだけで動作する機能コンポーネント: XR runtime、カメラ、平面、
                         アンカー、画像トラッキング、深度メッシュ、ハンド、
                         フォーカス平面、写真と合成のキャプチャ、配信、録画
  shaders/               各コンポーネントが共有する YCbCr とカメラ+AR 合成シェーダー
  editor/                dock: vendor_import_dock.gd（SDK 取込）, image_db_dock.gd,
                         mesh_snapshot_dock.gd（深度メッシュのスキャン → ArrayMesh/.glb）
  android/               ブリッジ Java ソース（nr_plugins.json と .aar は vendoring 対象で git 管理外）
  tools/                 vendoring した trackableImageTools CLI（git 管理外）
  bin/                   ビルド済みライブラリ（git 管理外）: android/libgodot_xreal.so + デスクトップ dummy スタブ
src/                     Rust GDExtension 本体
  lib.rs                 ExtensionLibrary エントリ
  ffi.rs / native.rs     RE した ABI（repr(C) 構造体）+ XREAL .so の dlopen/dlsym
  session.rs/jni_bridge.rs  セッションのライフサイクル + Android Activity 取得
  signal_guard.rs        null-NativeGlasses teardown クラッシュ回避
  node.rs                XrealHeadTracker（Node3D）
  system.rs              XrealSystem（RefCounted）+ XrealAR（Node、AR 変化シグナル）
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
                         + gen_stub_classes / gen_docs / gen_api_docs（.ps1/.sh の対応版）
  stream_server/         FPV 受信サーバー: fpv_server.py（ブラウザ）+ receive.ps1/.sh（ffplay/録画）
.github/workflows/       CI（fmt/clippy/test/build）+ Release（プリビルトアドオン）
docs/                    user/（生成クラスリファレンス api/ とその目次）と develop/（開発
                         ドキュメント: guides / reference / plans / archive、目次は各 README）
```

## ライセンス

以下のいずれかのライセンスを選択できます。

* Apache License, Version 2.0（[LICENSE-APACHE](LICENSE-APACHE) または http://www.apache.org/licenses/LICENSE-2.0 ）
* MIT license（[LICENSE-MIT](LICENSE-MIT) または http://opensource.org/licenses/MIT ）
