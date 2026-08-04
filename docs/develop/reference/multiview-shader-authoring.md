# Multiview で shader を書くときの注意点（Godot 4 / Vulkan）

XRInterface 経由の真の multiview（[`xr-interface-multiview-poc.md`](../plans/xr-interface-multiview-poc.md)）
を有効にしたとき、**どの shader / content が「片目しか描かれない」「両目に同じ絵が出て視差が消える」に
なるか**の調査記録。Unity の `unity_StereoEyeIndex` / `UNITY_VERTEX_OUTPUT_STEREO` 相当の対処が
Godot で必要かどうかが出発点。調査日 2026-08-04、Godot 4.x / Mobile renderer（Vulkan）前提。

## 結論

**Unity のような宣言・マクロは不要。** Godot の `shader_type spatial` は engine が multiview variant を
自動生成し、`MODEL_MATRIX` / `VIEW_MATRIX` / `PROJECTION_MATRIX` は `gl_ViewIndex` によって eye ごとに
自動で切り替わる。素直に書いた spatial shader はそのまま両目に出る。

ただし下表のカテゴリは自動化の外にあり、multiview で壊れる。

| # | カテゴリ | 症状 | 対処 |
|---|---|---|---|
| 1 | 起動設定（`xr/shaders/enabled`） | 描画自体が落ちる | プロジェクト設定（下記） |
| 2 | CPU からカメラ姿勢を uniform で渡す shader | 両目が同じ絵 = 視差なし | `EYE_OFFSET` / `INV_VIEW_MATRIX` |
| 3 | 3DGS / gsplat の自前 view-proj | 右目がズレる or 消える | vertex で `PROJECTION_MATRIX` を使う |
| 4 | `CompositorEffect` / 自前 RD compute | 右目だけ効果が未適用 | `get_view_count()` ループ |
| 5 | 自前 `sampler2D` へ渡す `ViewportTexture` | 常に mono | 設計で回避 |
| 6 | `canvas_item` / SubViewport 貼り付け / clip 座標直書き | 常に mono（立体にならない） | 仕様。UI はこれで良い |
| 7 | screen-space 系エフェクト（SSR 等） | 黒画面 / Vulkan エラー | Mobile renderer では該当なし |

`hint_screen_texture` / `hint_depth_texture` は **自動対応済み**（後述）。

## 1. 前提設定

- `xr/shaders/enabled=true` — multiview variant のコンパイルを有効にする。抜けると毎フレーム
  `!variants_enabled[p_variant]` と `shader.is_null()` が出て 3D が描かれない（PoC で実際に踏んだ。
  経緯は [`xr-interface-multiview-poc.md`](../plans/xr-interface-multiview-poc.md) の「有効化」節）。
- Vulkan（Mobile / Forward+）限定。Compatibility renderer は従来の Multipass 経路。

## 2. CPU 側からカメラ姿勢を uniform で渡す shader ← 最頻出

`set_shader_parameter("camera_pos", cam.global_position)` のような実装は、GPU multiview では
**両目とも同じ mono 値**を受け取る。描画自体は両目に出るが、billboard の向きや視差計算が片目基準に
なり、「右目が左目のコピー」に見える。shader 内の built-in を使うのが正解。

```glsl
// multiview では CAMERA_POSITION_WORLD は「両目の中点」になる（Godot 公式仕様）。
// 実際の eye 位置はこう復元する。
vec3 eye_world = CAMERA_POSITION_WORLD + (INV_VIEW_MATRIX * vec4(EYE_OFFSET, 0.0)).xyz;
```

| built-in | 意味 |
|---|---|
| `VIEW_INDEX` | 現在の view。`VIEW_MONO_LEFT`(0) = mono または左目、`VIEW_RIGHT`(1) = 右目。Unity の `unity_StereoEyeIndex` 相当 |
| `EYE_OFFSET` | view 空間での eye オフセット（vec3）。**multiview 時のみ有効** |
| `CAMERA_POSITION_WORLD` | multiview では**両目の中点** |

`VIEW_INDEX` / `EYE_OFFSET` は vertex / fragment の両方で使える。

## 3. 3DGS / godot-gsplat ← このリポジトリの本命リスク

compute depth sort と splat 投影を「1 本の view/projection 行列」に対して行う実装は、multiview では
片目分しか正しくならない。

- **sort**: 両目で sort 順を共有するのは実用上許容（中点カメラで sort する）。
- **投影**: vertex 側で `PROJECTION_MATRIX` / `MODELVIEW_MATRIX` を使っていれば両目とも正しく出る。
  自前で `mat4 u_view_proj` を uniform として渡していると**右目がズレる、または画面外へ飛んで消える**。

PoC の実機 Go / No-Go 項目 2「3DGS の右 eye」はここを見ることになる。

**実機判定（2026-08-05、godot_gsplat + scene_v3.gsplatpack 100k budget）**: multiview で右 layer が
正しく描画された（`demo/multiview_verify_3dgs.tscn`、両 eye に視差付き・multipass と同構図）。
godot_gsplat の投影は built-in 経由で multiview-safe、sort は `head_center` 共有（上の許容と整合）。

## 4. CompositorEffect / 自前 RenderingDevice compute

`get_view_count()` をループしていない実装は **layer 0 しか処理せず、右目だけエフェクトが未適用**に
なる。Godot 公式サンプルの形:

```gdscript
for view in range(render_scene_buffers.get_view_count()):
    var color_image = render_scene_buffers.get_color_layer(view)
    # ...
```

公式ドキュメントは「post processing で multiview を使う性能上の利点はない。この形で view を個別に
扱っても GPU の並列化は効く」としている。

## 5. 自動で面倒を見てくれるもの（対処不要）

- `hint_screen_texture` / `hint_depth_texture` — multiview 時に engine が `sampler2DArray` へ差し替え、
  `texture(samp, vec2 uv)` を `texture(samp, vec3(uv, ViewIndex))` に書き換える。
  ユーザー shader は `texture(screen_tex, SCREEN_UV)` のままで良い（[PR #71455]）。
- 通常の PBR / `unshaded` material、`SCREEN_UV`、light 関数。

**例外**: 自分で `ViewportTexture` を `sampler2D` uniform として渡した場合は自動化の対象外で、常に mono。

## 6. 構造的に mono になるもの（片目欠けではないが立体にならない）

- `shader_type canvas_item`（2D）— multiview 非対応。root の 2D UI はこれで問題ない。
- SubViewport を Quad に貼る方式 — 両目に同じ平面画像が出る。
- vertex で `POSITION` に clip 座標を直書きする full-screen quad / skybox hack — `PROJECTION_MATRIX` を
  無視するので両目同じ絵になる。

## 7. Screen-space 系エフェクト（Mobile renderer なので実害は小さい）

- SSR は stereo で壊れる（[#86987]）。
- SSR / SSS + TAA + XR で全ジオメトリが黒くなる（[#66998]）。
- VoxelGI / LightmapGI / SDFGI / SSR は XR shader 有効時に Vulkan エラーを吐く（[#84999]）。
- そもそも Mobile renderer には SSR / SSAO / SSIL / volumetric fog が無いため、この PoC の構成では
  踏みにくい。

## このリポジトリの shader の判定

| shader | 種別 | 判定 |
|---|---|---|
| [`xreal_ycbcr.gdshader`](../../../addons/godot_xreal/shaders/xreal_ycbcr.gdshader) | spatial | 安全（`UV` のみ） |
| [`xreal_image_marker.gdshader`](../../../demo/xreal_image_marker.gdshader) | spatial | 安全（`UV` / `FRONT_FACING` のみ） |
| [`xreal_blend_2d.gdshader`](../../../addons/godot_xreal/shaders/xreal_blend_2d.gdshader) | canvas_item | 無関係（MR 合成用の 2D） |
| [`xreal_ycbcr_2d.gdshader`](../../../addons/godot_xreal/shaders/xreal_ycbcr_2d.gdshader) | canvas_item | 無関係 |

現状 addon / demo 側に multiview で壊れる shader は無い。リスクは今後載せる 3DGS 経路と、
`CompositorEffect` を導入した場合に限られる。

## Go / No-Go 項目 1 用の診断 material

計画書の「左右で異なる色を出す `VIEW_INDEX` 診断 material」はこれで足りる。

```glsl
shader_type spatial;
render_mode unshaded;

void fragment() {
	// 左目 = 赤 / 右目 = 青。両目とも赤なら multiview が効いていない。
	ALBEDO = (VIEW_INDEX == VIEW_RIGHT) ? vec3(0.0, 0.3, 1.0) : vec3(1.0, 0.2, 0.0);
}
```

`vk_bridge` の `layers=0/1 same_image=true` ログと突き合わせれば、「2-layer image は取れているが
layer 1 に描画されていない」のか「描画はされているが bridge の copy 元 / 先が違う」のかを切り分けられる。

## 出典

- [Spatial shaders — Godot docs](https://docs.godotengine.org/en/stable/tutorials/shaders/shader_reference/spatial_shader.html)
  — `VIEW_INDEX` / `VIEW_MONO_LEFT` / `VIEW_RIGHT` / `EYE_OFFSET`、`CAMERA_POSITION_WORLD` が
  multiview では両目の中点であること。
- [Compositor effects — Godot docs](https://docs.godotengine.org/en/stable/tutorials/rendering/compositor.html)
  — `get_view_count()` ループと `get_color_layer(view)`。
- [PR #71455 Make screen texture and depth texture work in Multiview][PR #71455]
- [PR #48011 Add VIEW_INDEX variable](https://github.com/godotengine/godot/pull/48011)
- [#86987 SSR is broken in stereo rendering][#86987]
- [#84999 Some GI effects prevent 3D rendering if XR shaders are enabled][#84999]
- [#66998 SSR/SSS with TAA and XR results in black geometry][#66998]

[PR #71455]: https://github.com/godotengine/godot/pull/71455
[#86987]: https://github.com/godotengine/godot/issues/86987
[#84999]: https://github.com/godotengine/godot/issues/84999
[#66998]: https://github.com/godotengine/godot/issues/66998
