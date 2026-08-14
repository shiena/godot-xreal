# docs/user/

User-facing documentation. Start with the repo [README](../../README.md) for installation and
device support.

## Godot's XR workflow, on XREAL glasses

Poses come from `XRCamera3D` and `XRController3D`, hand joints from `XRHandTracker`, buttons from
InputMap actions. The addon supplies the XREAL runtime behind those nodes.

Set the renderer to Compatibility before anything else. The glasses path hands its eye textures to
the XREAL compositor as GL texture names, which only that renderer's context supplies, and under
Forward+ or Mobile the glasses stay black while tracking and the phone display carry on as if all
were well.

The application owns the hierarchy. Add
`addons/godot_xreal/features/xreal_xr_runtime.tscn` under your `XROrigin3D` and it attaches to what
it finds; instance `addons/godot_xreal/xr_origin.tscn` instead if you are starting from nothing:

```
XROrigin3D                     # yours
├── XRCamera3D
├── LeftController  (XRController3D, tracker = left_hand)
├── RightController (XRController3D, tracker = right_hand)
└── XrealXRRuntime             # the XREAL bootstrap, attached to the above
```

Hanging your own nodes off the controllers in `xr_origin.tscn` needs **Editable Children** on that
instance, since they belong to an instanced scene. Without it the node is dropped on load and
nothing reports why. Other Godot XR addons attach to these nodes normally; a stock godot-xr-tools
pointer was checked on device. Leave those addons' plugins disabled if they write to
`xr/openxr/enabled`, as godot-xr-tools does.

Gameplay should read head and controller tracking from those standard XR nodes. On XREAL the
runtime itself polls the native phone IMU and touchpad. Glasses keys and app-owned phone UI controls
flow through the addon, and `trigger_click`/`primary_click`, `grip_click`, and `menu_button` become
`xr_select`, `xr_grab`, and `xr_menu`. The runtime creates missing InputMap actions. The native controller's raw
button bitfield is intentionally not decoded until its layout is device-verified. Hand joints use
Godot's standard `XRHandTracker`.

A glasses key reports a click rather than a press and a release, so the runtime publishes it as a
short button pulse raised partway through the frame. Connect to
[`XRController3D.button_pressed`](https://docs.godotengine.org/en/stable/classes/class_xrcontroller3d.html)
to catch those, or poll `Input.is_action_pressed()`; `is_action_just_pressed()` reports them only
when the node happens to run after the runtime, so it is not reliable for glasses keys.

An app with its own phone UI feeds it without implementing XR/InputMap mapping:

```gdscript
$XrealXRRuntime.set_controller_button(&"trigger_click", pressed)
$XrealXRRuntime.set_controller_axis(touch_position, touching)
$XrealXRRuntime.set_controller_hand(is_right)
```

On an XREAL desktop-preview run, `XrealShared.find_tracking_head()` chooses the preview flycam head
before the inactive runtime camera, so head-locked content follows mouse/WASD movement.

Add the other scenes under `addons/godot_xreal/features/` for camera, planes, anchors, image
tracking, depth mesh, hands, capture, recording, and streaming. They remain self-contained and may
be configured from the Inspector without writing initialization code, and they stay inert off
device so scenes remain runnable in the editor.

## Hands

Hand tracking is an Air 2 Ultra feature. Ask for it in Project Settings under `xreal/input_source`,
set to **Controller And Hands**. The addon leaves that at controller-only because the SDK spends
about 878 ms on the Hands bit while starting up, and it reads the setting once, at boot. A One or a
One Pro pays that time and gets nothing back.

Two components draw hands, and they differ in what they ask of you.

`xreal_hands.tscn` puts a small sphere on each of the 26 joints per hand. It needs no art of your
own and belongs under a world-fixed node. Reach for it to see whether tracking works at all.

`xreal_hand_models.tscn` drives your own skinned hand models, and belongs **under your
`XROrigin3D`**. It builds Godot's standard hand rig for each side: an `XRNode3D` bound to the
tracker, your model under it, and an `XRHandModifier3D` on the model's `Skeleton3D`. The hands sit
on the real hands and disappear when tracking stops, so on glasses without hand tracking they never
appear and nothing has to switch them off.

A tracked hand also aims and clicks. While it is tracked it owns its side's `aim` and `grip` poses,
so `XRController3D` follows the hand instead of the phone, and a thumb-to-index pinch raises
`trigger_click` and so `xr_select`. Gameplay code reads the same nodes and actions it reads on an
OpenXR headset, and needs no branch. Putting the hand down hands the ray back to the phone.
The `XRInputRouter` node inside `xreal_xr_runtime.tscn` exports `hand_aim` to turn this off, and
`shoulder_offset`, `pinch_press_m` and `pinch_release_m` to tune it.

### Wiring up hand models

The addon ships no models, so bring your own. `XRHandModifier3D` matches bones by name alone:
`Left<bone>` and `Right<bone>`, where `<bone>` runs `Palm`, `Hand`, `ThumbMetacarpal` through
`LittleTip`. A model also has to be skinned in the OpenXR joint convention, because the modifier
replaces each bone's pose with the tracker's joint transform outright.

Godot's own demo hands meet both conditions. Copy `LeftHandHumanoid.gltf`, `RightHandHumanoid.gltf`
and their `.bin` files out of
[godot-demo-projects](https://github.com/godotengine/godot-demo-projects), under
`xr/openxr_hand_tracking_demo/assets/gltf/`, into your project. They are MIT-licensed.

1. Set `xreal/input_source` to **Controller And Hands**.
2. Add `addons/godot_xreal/features/xreal_hand_models.tscn` under your `XROrigin3D`, and leave its
   own transform at the identity. Anything between it and the tracking origin displaces the hands.
3. Point `left_model` and `right_model` at the two glTF scenes.
4. Give `material_override` an unshaded material if your scene is dark. The glasses composite
   additively, so scene lighting turns a hand into a dark smear.

```
XROrigin3D                     # yours
├── XRCamera3D
├── XrealXRRuntime
└── XrealHandModels            # left_model / right_model
```

Those demo hands carry no `Palm` bone, so Godot logs `Couldn't obtain bone for LeftPalm` once per
hand at start-up. The modifier skips the bones it cannot find, and the other 25 drive the hand.

godot-xr-tools' hand models do not work here. Their 26 bones correspond one for one, which makes
renaming look sufficient, but their bind pose sits at a fixed rotation from the convention the
modifier assumes: 90° on the fingers and 135° on the thumb, measured against Godot's demo hands.
The skin would deform by that same rotation.

## api/: generated class reference

[api/README.md](api/README.md) lists every class the addon exposes to GDScript, one page each. Those
pages are **generated, not written**: the native classes come from the `///` doc comments in `src/`
(gdext `register-docs`) and the feature components from the `##` doc comments in
`addons/godot_xreal/` (Godot's own `--gdscript-docs` doctool), rendered by `src/api_docs.rs`. Edit a
doc comment, then run `scripts/gen_api_docs.{ps1,sh}` and commit; the pages carry a DO-NOT-EDIT
header for a reason. They are plain CommonMark with explicit anchors, so they read on GitHub as-is
and any static-site generator can build them unchanged.
