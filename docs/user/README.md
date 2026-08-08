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

## api/: generated class reference

[api/README.md](api/README.md) lists every class the addon exposes to GDScript, one page each. Those
pages are **generated, not written**: the native classes come from the `///` doc comments in `src/`
(gdext `register-docs`) and the feature components from the `##` doc comments in
`addons/godot_xreal/` (Godot's own `--gdscript-docs` doctool), rendered by `src/api_docs.rs`. Edit a
doc comment, then run `scripts/gen_api_docs.{ps1,sh}` and commit; the pages carry a DO-NOT-EDIT
header for a reason. They are plain CommonMark with explicit anchors, so they read on GitHub as-is
and any static-site generator can build them unchanged.
