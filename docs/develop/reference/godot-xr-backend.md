# Godot 標準 XR backend 統合

## 目的

XREAL 本家 SDK は OpenXR 非対応である。
そこでアプリのシーンとゲームロジックだけを、通常の OpenXR アプリと同じ形にそろえる。
アプリは backend 固有ノードではなく、Godot 標準の `XROrigin3D`、`XRCamera3D`、`XRController3D`、`XRHandTracker`、`InputMap` を参照する。

このアドオンが動くのは XREAL 実機とデスクトップだけである。
Quest などの OpenXR 機では OpenXR vendor plugin を使い、このアドオンのコンポーネントは置かない。
共通化されるのはシーンとコードであって、アドオンそのものではない。

抽象化の優先順位は次のとおり。

1. OpenXR と同じ Godot XR API へ公開する。
2. XR API に自然な表現がない入力は Godot `Input` へ公開する。
3. XREAL 固有のデバイス機能だけ既存シグナルと API を維持する。

## ユーザー向け境界

`addons/godot_xreal/features/xreal_xr_runtime.tscn` がコード不要の bootstrap である。
階層はアプリが所有し、この component は既存の `XROrigin3D` を探して取り付く。
解決順は `xr_origin_path`、祖先、自身の子孫、tree 全体である。

```
XROrigin3D                 # アプリが所有。OpenXR ビルドでは bootstrap だけ外す
├── XRCamera3D
├── LeftController  (left_hand / aim)
├── RightController (right_hand / aim)
└── XrealXRRuntime         # bootstrap。XRInputRouter を子に持つ
```

controller は node 名ではなく `tracker` で照合する。
Quest 流の `LeftHand` / `LeftAim` のような命名や、aim と grip を別ノードに分ける構成でもそのまま繋がる。

controller tracker が publish する pose は `aim`、`grip`、`default` の 3 つである。
`default` は `aim` と同じ姿勢を運ぶ。
Godot の OpenXR が全 interaction profile で `default_pose` を `.../input/aim/pose` に bind し、tracker 上では `default` へ改名するためである（`openxr_interface.cpp`）。
`XRNode3D.pose` のデフォルト値が `default` なので、これが無いとアプリが素の `XRController3D` を置いたときだけ pose が来ない。
同梱の `xr_origin.tscn` は `pose = &"aim"` を明示しているため、どちらでも動く。
見つけた `XRCamera3D` は `xreal_shared_xr_camera` group へ登録するので、feature 群はアプリが名前を知らせなくても head を引ける。
ゼロから始める場合は `addons/godot_xreal/xr_origin.tscn` を配置する。
標準階層に bootstrap を入れたものである。

Godot の `xr/openxr/enabled` はこのアドオンでは有効にしない。
起動時に一度だけ読まれる設定で、runtime が見つからないビルドでは startup alert（modal dialog）が出て停止する。
OpenXR 機向けのビルドでは、そちら側のプロジェクト設定として有効にする。
なお `xreal/xr_multiview_poc` は自前の `XrealXrInterface` を使うため、この設定とは無関係である。

## 他の Godot XR アドオンとの併用

標準ノードを使うため、他の Godot XR アドオンはそのまま取り付く。
2026-08-08 に godot-xr-tools 4.5.1 の `function_pointer.tscn` を無改造で実機検証した。
`XRHelpers` が tracker 名 `right_hand` から我々の `XRController3D` を見つけ、デフォルトの `active_button_action = "trigger_click"` も公開名と一致し、両眼に視差付きでレーザーが描画された。

2 点の罠がある。

godot-xr-tools の **plugin は有効化しない**。
`plugin.gd` が `xr/openxr/enabled = true` を書き込むためである。
scene と script は plugin 無効のままで動く。

`xr_origin.tscn` を instance した場合、その controller に子を足すには **Editable Children**（`.tscn` では `[editable path="XROrigin3D"]`）が要る。
無いと editor は受け付けるが読み込み時に捨てられ、error も warning も出ない。
実機診断で `fp: found=false` として初めて分かった。

## ヘッドトラッキング

XREAL では `XrealHeadTracker` が backend driver として残る。
driver はネイティブ DISP pose を `XrealXrInterface` の tracking-space camera transform として公開し、標準 `XRCamera3D` が受け取る。
driver 自身の transform 更新は旧 child-camera rig 向けの互換動作である。

GLES Multipass では従来の XREAL compositor 向け 2-eye `SubViewport` を維持しつつ、`XrealXrInterface` を標準ノードへの pose 供給に使う。
Vulkan multiview を有効にした場合は同じ interface が Godot の 2-view render target も供給する。

`XrealShared.find_tracking_head()` は実行環境に応じて次の順に head を選ぶ。

1. XREAL desktop preview では preview flycam の head
2. `xreal_shared_xr_camera` group の標準 `XRCamera3D`
3. 旧 `xreal_head_tracker` group の driver
4. 上記以外で利用可能な desktop preview の head

XREAL desktop では runtime camera が identity のままなので、preview を優先して head-locked content を flycam へ追従させる。
実機では `XROrigin3D` の world transform を capture、focus plane、stream へ反映しながら、旧シーンとの互換性を保つ。

`xr_origin.tscn` は `XRCamera3D` を `current` にしているため、desktop でも root viewport が原点固定のカメラから 3D を 1 パス描いてしまう。
実機では eye viewport が立った時点で driver が root viewport の 3D を止めるので起きない。
desktop でも同じ `xreal/disable_host_viewport_3d` に従って止めるが、desktop preview が tree にあるときに限る。
preview が無ければ root viewport がアプリ唯一の 3D 表示先だからである。
止めるのは描画だけで `current` は保つ。
driver が eye カメラの FOV と near/far を viewport のカメラから読むためである。

## コントローラー入力

`XrealXRInputRouter` が OpenXR runtime と同名の `left_hand` / `right_hand` `XRControllerTracker` を作る。
runtime が `XrealSystem.start_controller()` / `poll_controller()` を呼び、生 IMU を相補 filter で 3DoF pose へ変換し、native touch state と axis も tracker へ公開する。
この poll と fusion は addon 内にあり、demo へ依存しない。
アプリは OpenXR 機と同じく `XRController3D` の pose と input 値を読む。

phone 画面の trigger、grip、menu など、アプリが描画する virtual controller は `XrealXRRuntime.set_controller_button/axis/hand()` を呼ぶ。
XR tracker 更新と InputMap 変換は addon 側で完結し、demo はこの公開 API を使う入力 source の一例にすぎない。
現行 device の `poll_controller()` が返す raw button bitfield は割当未検証のため推測で decode しない。

公開する input 名は Godot 標準の OpenXR action 名に一致させてある。
実在の Quest 3 プロジェクトの action map と突き合わせ、`trigger_click`、`grip_click`、`menu_button`、`primary_click`、`trigger`、`grip`、`primary` がすべて含まれることを確認した（2026-08-08）。

標準化した主要ボタンは Godot `Input` にも集約する。

| XR input | InputMap action |
|---|---|
| `trigger_click` | `xr_select` |
| `primary_click` | `xr_select` |
| `grip_click` | `xr_grab` |
| `menu_button` | `xr_menu` |

XREAL グラスの物理キーは click 単位のイベントしか提供しないため、runtime は単押し（`ACTION_CLICK`）だけを対応する tracker button の pulse として公開する。
long-press recenter などの XREAL 固有動作は互換シグナルにも残す。

pulse は press を 2 回の `process_frame` にまたがって保持する。
glasses callback 由来の click は backend driver の `_process` 中に発行されるため、tree 上で先に処理されるノードはその frame の `_process` を終えている。
1 frame で release すると、そうしたノードは `is_action_pressed()` すら一度も true として観測できない。
2 frame にすれば、どのノードも必ず 1 回は press 状態の `_process` を通る。
ただし press が input phase ではなく process 途中で始まる以上、`is_action_just_pressed()` はノード順に依存する。
glasses キーを取りこぼしなく受けるには `XRController3D.button_pressed` を購読する。
demo はこの経路を使う。

## ハンドトラッキング

XREAL の `XrealHandTracker` は `/user/hand_tracker/left` と `/user/hand_tracker/right` の標準 `XRHandTracker` を XRServer へ登録する。
OpenXR 機でも同じ予約 tracker 名なので、consumer 側のコードはそのまま通る。
`xreal_hands.tscn` などの consumer は XRServer だけを参照する。

## XREAL 固有機能

RGB camera、plane、anchor、image tracking、depth mesh、capture、stream などは OpenXR に一律で置き換えられる API ではないため、既存の feature scene を維持する。
`XrealShared.is_native_runtime()` は Android 実行かつ拡張がロード済みのときだけ true となり、desktop ではこれらの scene を配置したままでも inert になる。

## 実装ファイル

- `addons/godot_xreal/features/xreal_xr_runtime.gd/.tscn`：backend lifecycle
- `addons/godot_xreal/xr_origin.tscn`：共通の標準 XR 階層
- `addons/godot_xreal/xr_input_router.gd`：tracker と InputMap の bridge
- `src/xr_interface.rs`：Godot `XRInterfaceExtension`
- `src/node.rs`：XREAL pose 取得、compositor driver、interface への pose 公開

## 検証境界

desktop Godot で scene と script の解析、Rust test、Clippy、Android arm64 compile までは自動検証する。
XREAL 実機では head pose、両眼表示、phone controller、hand tracker を確認する必要がある。
OpenXR 機側は本アドオンの対象外であり、検証はアプリのシーンとコードが移植できることに限られる。
