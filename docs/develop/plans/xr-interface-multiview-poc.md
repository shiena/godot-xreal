# XRInterfaceExtension multiview PoC

実装日: 2026-08-03  
ブランチ: `experiment/xr-interface-multiview-poc`（`origin/main`から作成）

## 目的

現在の左右2 `SubViewport`を使うscene Multipassと、XREAL SDK compositorのsubmission方式を分離して
評価する。Godot側だけを標準XR rendererへ接続し、1回のscene drawで2-layer targetを生成する。
XREAL側は実績のある左右別eye targetを維持する。

```text
root Viewport (phone 2D UI, use_xr=false)

one offscreen SubViewport (use_xr=true, shared World3D)
  -> XrealXrInterface (view_count=2)
  -> Godot standard XR multiview render
  -> one VkImage, array layer 0/1
  -> existing Vulkan bridge copy
  -> existing XREAL left/right compositor targets
```

## 有効化

Project Settingsの`xreal/xr_multiview_poc`を有効にして、Mobile rendererを使う。addonとしての
既定値は`false`である。Mobile rendererは`Android Vulkan`プリセットの`--rendering-method mobile`が
選択する。
実機比較用にAndroid property `debug.xreal.xr_multiview=0/1`がProject Settingを上書きする。

GodotはXR用shader groupをrenderer初期化時にだけ有効化するため、`xr/shaders/enabled=true`も必要になる。
Androidの`Regular`プリセットは`--xr-mode off`を強制追加してこの設定を無視するため、PoCの
`Android Vulkan`プリセットはXR Modeを`OpenXR`にする。一方で`xr/openxr/enabled=false`を明示し、
OpenXR interfaceとruntimeは起動させない。これはAndroid exporterがXR shaderを無効化するのを避けるための
PoC上のworkaroundである。

Vulkan glasses bridgeの既存kill switchも必要である。

```powershell
adb shell setprop debug.xreal.vulkan_glasses 1
```

起動ログで次を確認する。

- `XRInterfaceExtension initialized: ..., views=2`
- `XR multiview PoC active: one offscreen XR SubViewport`
- `vk_bridge eye-src probe: ... layers=0/1 same_image=true`

最後の行は、左右別のSubViewport imageではなく、同じ2-layer imageを左右sourceとして使っている証拠になる。

## 実装範囲

- `XrealXrInterface: XRInterfaceExtension`を`XRServer`へ登録しprimary interfaceに設定。
- 共有World3Dを持つ単一のoffscreen XR SubViewportを生成し、従来の左右2 SubViewportを抑止。
- root Viewportは非XRのままスマホ用2D UIを描画する。既存設定に従ってrootの3Dだけを無効化する。
- XREAL frame descriptorの左右eye offsetと非対称projectionをXR virtualから返す。
- app Camera3Dのglobal transform、near/far、FOV fallback、cull mask、environmentをoffscreen XR renderで維持。
- `_post_draw_viewport()`でGodot内部RD textureの`VkImage`を取得。
- `EyeSource.array_layer`を追加し、bridgeのbarrier/copy/blit対象をlayer 0/1から選択。
- render scaleの縮小はbridgeのlinear blitが対応するときだけ縮小targetを使う。sRGB型の縮小target
  をrender threadで検出したときは、multipass同様にfull-size target+Godot bilinear scalingへ
  フォールバックする。
- node teardown時にViewportとXRServerの状態を復元。

## Beam Pro smoke test（2026-08-03）

Androidの`Regular`プリセットではinterface自体と2-layer image取得は成功したが、
`!variants_enabled[p_variant]`と`shader.is_null()`が毎frame発生し、スマホはGodot logoで停止した。
上記のXR shader起動時設定により両errorは0件となった。

次にroot XR viewportのlayer 0をスマホ全面へmirrorしたところ、スマホのaspect ratioへ引き伸ばされ、
2D controllerを上書きした。そのためrootのXR化とmirrorを廃止し、単一のoffscreen XR SubViewportと
非XR root Viewportに分離した。

device logで次を確認済み。

- `views=2` / one offscreen XR SubViewport
- `layers=0/1 same_image=true`
- `direct linear scale blit 1476x851 -> 1968x1134`
- `targets=2 filled=2` / `submit=Some(0)`
- `origin/main`同期経路での`Project FPS: 51 (19.60 mspf)`（既知の約52 FPSと一致）

OpenXRに関する起動ログはAndroid側の`--xr_mode_openxr`選択のみで、Godot OpenXR interfaceの
初期化やruntime errorは発生していない。左右の実画像、parallax、3DGS materialの右layerは引き続き
目視確認が必要である。

## Multiview vs Multipass benchmark（2026-08-03）

`feat/vk-sync-v2`を一時的にmergeし、custom Godotで`VK_KHR_external_semaphore_fd`を有効化した。
device logの`sync_fd=available`を確認し、両経路とも`debug.xreal.vk_sync=2`、render scale 75%
（1476x851 -> 1968x1134）で比較した。

`demo/multiview_benchmark.tscn`は独立した`MeshInstance3D`を格子状に並べる。`high_geometry=true`で
Sphereを使うgeometry workload、`false`で小さなBoxを使うdraw-call workloadになる。tracking確立後に
8秒warm-upし、15秒ずつ複数windowを収集する。計測時は`run/main_scene`をこのsceneへ切り替える。
同一APKを`debug.xreal.xr_multiview=0/1`で再起動したため、asset、resolution、sync、
native compositorは同一である。

| instances | path | FPS | frame p50 | process monitor | draw calls | primitives |
|---:|---|---:|---:|---:|---:|---:|
| 1,152 | Multiview | 59.73 | 16.667 ms | 21.21 ms | 743 | 0.808M |
| 1,152 | Multipass | 59.70 | 16.667 ms | 22.05 ms | 1,436 | 1.562M |
| 3,840 | Multiview | 30.22 | 33.333 ms | 40.06 ms | 2,764 | 3.007M |
| 3,840 | Multipass | 30.79 | 33.333 ms | 46.38 ms | 5,384 | 5.857M |
| 1,800 | Multiview | **50.9** | 19.607 ms | 34.1 ms | 1,212 | 1.319M |
| 1,800 | Multipass | **58.5** | 16.7-16.9 ms | 27-29 ms | 2,354 | 2.560M |
| 6,000 low-poly Box | Multiview | **42.2** | 23.3-24.1 ms | 34.9 ms | 4,111 | 49.3K |
| 6,000 low-poly Box | Multipass | **39.8** | 25.000 ms | 35.4 ms | 7,990 | 95.9K |

1,800 instanceはA-B-A順で再測定し、Multiviewの50.98-51.00 FPSとMultipassの58.21-58.86 FPSは
起動順に関わらず再現した。同じ定常期間のAndroid `top` 5 sampleはMultiviewが平107%、Multipassが
平127%で、GPU busyはそれぞれ平96.3%と97.1%だった。

6,000 BoxもA-B-A順で再測定し、Multiview 42.1-42.4 FPS、Multipass 39.7-40.0 FPSが再現した。
CPU draw-call律速ではMultiviewが**5.9%**高速である。

### 判定

MultiviewはGodot統計上draw callsとprimitivesをほぼ半減し、process CPU使用も約16%下げた。
しかし、GPUが飽和する境界ではBeam Pro / Adreno 710のlayered multiview経路がMultipassより遅く、
FPSは約13%低かった。Godotのprimitives値はAPI drawの論理統計であり、2 view分の実GPU workが
半分になったことを意味しない。

一方、low-poly Boxを6,000個並べたCPU draw-call律速sceneでは、draw calls半減が支配的になり
Multiviewが5.9%高速だった。よって、Multiviewの効果は**content特性依存**である。

- 多数の独立したlow-poly Spatial drawでCPU submissionが重い: Multiviewが有利。
- 3DGSやhigh-poly meshのようにGPUが重い: Beam ProではMultipassが有利。

「重い3DGS contentを動かす」という当初目的に対する現在の採用判定は**No-Go**のままで、
Multipassを既定に維持するのが良い。MultiviewはCPU draw-bound content用のopt-in候補としてのみ価値がある。

## 意図的な制限

- **Vulkan限定。** Godot 4.xの`XRInterfaceExtension.get_render_target_texture()`はRendererRD専用で、
  Compatibility rendererの内部GL array textureを返せない。GLESでは既存Multipassへフォールバックする。
- dynamic render scaleは未接続。`xreal/render_scale`を初期化時に一度だけ読み、XR target sizeへ反映する。
- XREAL compositor側はMultipassのまま。真のmultiviewはGodot scene rendererに限定され、layer copyは2回残る。
- #114940はmultiview生成自体には不要。`origin/main`の既存Vulkan同期では約52 FPSの
  `vkQueueWaitIdle`経路、または既知のtearingがあるpipelined経路を使う。
- 既存のprimary XR interfaceがある場合は上書きせず、PoCを無効化する。

## 実機Go / No-Go

1. 左右で異なる色を出す`VIEW_INDEX`診断materialにより、両layerの実行を確認する。診断shaderと、
   multiviewで壊れるshaderの分類は[`multiview-shader-authoring.md`](../reference/multiview-shader-authoring.md)。
2. 通常meshと3DGSの両方で右eyeが描かれ、parallaxが正しいことを確認する。
3. ログが`layers=0/1 same_image=true`となることを確認する。
4. 現行Multipassと同じsceneでCPU/GPU/frame timeを比較し、scene workloadが減ることを確認する。
5. head-lock、roll、6DoF position、near/far、非対称projectionに回帰がないことを確認する。

項目1・2・5とencoder経路は`demo/multiview_verify.tscn`で確認する。`run/main_scene`をこのsceneへ
切り替え、同一APKを`debug.xreal.xr_multiview=0/1`で起動し直して比較する。検証・ベンチマーク用の
scene一式（`demo/multiview_benchmark.*`、`demo/multiview_verify*`、`demo/multiview_view_index.gdshader`）
はローカル専用で、リポジトリには含めない。VIEW_INDEX診断球が項目1、
1/2/4/8 mのpillarと床gridが項目2と5、Near clip toggleがapp cameraのnear/far追従、Recordボタンが
録画encoder経路をカバーする。録画したmp4はgalleryへ発行される。

右eye欠落、projection不整合、またはGodot内部targetの実layoutがbridge前提と一致しない場合はNo-Goとし、
`xreal/xr_multiview_poc=false`で従来経路へ戻す。

## 検証シーン実機結果（2026-08-05、Beam Pro + One Pro）

`demo/multiview_verify.tscn`を`debug.xreal.xr_multiview=0/1`のA/Bで実行し、glasses display
（`screencap -d <display-id>`、左右eyeがside-by-sideで写る）で直接確認した。

- 項目1: VIEW_INDEX診断球はmultiviewで左=赤/右=青、multipassで両目赤。layer 1の実行を確認。
- 項目2: pillarと床gridはmultiviewの両eyeに視差付きで描画され、右eye欠落なし。
- near/far追従: Near clip 0.5 mトグルで0.35 mのプローブが両pathで消えた。
- encoder経路: multiview動作中の10秒録画がgalleryへ発行された（`record_20260805_010134.mp4`）。
- scale path: 有効化直後はgodot-bilinear、bridge init後に
  `multiview scale path upgraded to bridge-linear (1476x851)`、eye-src probeは
  `1476x851 layers=0/1 same_image=true srgb=false`。
- Project FPS 57-60（`vk_sync`既定=wait-idle、軽いscene）。

3DGSは`demo/multiview_verify_3dgs.tscn`で確認した（2026-08-05、godot_gsplat +
`samples/3dgs/scene_v3.gsplatpack` 100k budget。いずれもlocal-only assetで、無ければmarkerのみ描画）。
multiviewで右layerに視差付きで正しく描画され、multipassと同構図
（[`multiview-shader-authoring.md`](../reference/multiview-shader-authoring.md)の3DGS節に判定を記録）。
FPSはmultiview 13-14（mean 76-80 ms、draws=5）、multipass 12（mean 84.5 ms、draws=7）で、
この100k splat workloadではmultiviewが僅かに速い。
装着状態でのparallax・快適性も両pathで問題なしを確認した（2026-08-05、ユーザー目視）。
これでGo / No-Goの全項目を消化した。
