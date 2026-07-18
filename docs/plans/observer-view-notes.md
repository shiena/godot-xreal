# XREAL StreamingReceiver ObserverView reverse-engineering notes

Analysis date: 2026-07-19  
Target: StreammingReceiver_v1.2.0 (Assembly-CSharp.dll, Unity assets, and native decoder), cross-checked against the vendored com.xreal.xr SDK.

## Conclusion

ObserverView is a composited external/spectator view, not another name for the glasses' head POV. The glasses side is expected to render the AR scene from an observer camera, stream the virtual image with alpha, and let the PC receiver composite it over the PC's **local webcam**. The receiver supplies the observer camera's projection/FOV so that the virtual render matches that webcam.

The receiver does **not** send an observer pose. Its ObserverView code has no position, orientation, transform, marker, or anchor message. The observer camera pose must therefore be established on the glasses/sender side, for example as an app-defined world-space camera or an aligned anchor. The exact pose/alignment policy cannot be recovered from this receiver because the SDK ships no ObserverView sender sample.

## Concrete receiver evidence

### Rendering and composition

- ObserverViewController.OnFirstFrameReady() creates a Blender with BlendMode.ObserverView.
- Blender.SetTextureByBlendMode(ObserverView) assigns the decoded RTP texture to _MainTex, creates a WebCamTexture at the decoded texture's width/height and 30 fps, and assigns it to the misspelled shader property _BcakGroundTex.
- FirstPersonView selects FirstPersonViewBlend and assigns only _MainTex; it never opens a webcam.
- StreammingEncoder uses the same NRNativeDecoder in both modes. It uploads decoded video as TextureFormat.RGB24, so there is no ordinary fourth alpha channel in the decoded Unity texture.

The Unity asset Shaders/ObserverViewBlend contains shader NRToolKit/ObserverViewBlend. Its compiled pixel shader samples _MainTex twice, once from each half-height UV range, and samples _BcakGroundTex once. Its effective operation is:

~~~text
foreground_rgb = main texture, one vertical half
alpha          = main texture, other vertical half, first channel
output_rgb     = alpha * (foreground_rgb - webcam_rgb) + webcam_rgb
output_a       = 1
~~~

Thus the H.264 image carries a **top/bottom packed RGB + alpha mask**, and the receiver reconstructs the virtual foreground and alpha-composites it over its webcam. ObserverView is specifically designed for a PC-camera spectator shot.

The SDK supports the matching sender format. NativeEncodeConfig.useAlpha is serialized into the libmedia_codec configuration, while FirstPersonView uses useAlpha:false. The SDK derives it from CameraParameters.hologramOpacity == 0; however, XREALVideoCapture.StartVideoMode() forcibly resets hologramOpacity to 1. An Observer sender using this SDK version therefore needs either a lower-level capture path or a direct native encoder config with useAlpha:true. The native-first Godot port can set that field directly.

### FOV flow and meaning

The Observer page contains a DebugPanel; the FirstPerson page does not. On a successful room join, ObserverViewController.SwitchScreen(RTPScreen) calls both _Encoder.Play() and m_DebugPanel.UpdateCurrentConfig().

UpdateCurrentConfig() selects one of six PC-camera calibration presets (1-65, 1-78, 1-90, 2-65, 2-78, 2-90) and invokes OnConfigChanged. The call chain is:

~~~text
DebugPanel.OnConfigChanged(Fov4f)
  -> ObserverViewController.UpdateCameraParam(Fov4f)
  -> NetWorkServer.UpdateCameraParam(activeClient, fov)
  -> Client.Send(MessageType.UpdateCameraParam, serialized CameraParam)
~~~

This proves the direction: **PC receiver/server -> glasses sender/client**. The TCP message has type UpdateCameraParam = 6 and a JSON-serialized payload equivalent to:

~~~json
{"fov":{"left":0.0,"right":0.0,"top":0.0,"bottom":0.0}}
~~~

The four values are not degrees. DebugPanel.CalculateFOV() passes calibrated webcam intrinsics to ProjectMatrixUtility.CalculateFOVByFCC(), which calculates normalized image-plane extents/tangents:

~~~text
left   = cx / fx
right  = (width - cx) / fx
top    = cy / fy
bottom = (height - cy) / fy
~~~

The calibration size is fixed at 1920x1080. Unequal left/right or top/bottom preserves an off-centre principal point. The DLL also contains PerspectiveOffCenter(left,right,bottom,top,near,far), documenting the intended projection-matrix interpretation.

The SDK client's NetWorkClient.UpdateCameraParamResponse() deserializes this payload and raises OnCameraParamUpdate. Its UpdateCameraParamRequest() name is misleading/vestigial for this receiver version: NetWorkServer does **not** register an inbound type-6 handler, so an empty type-6 request from the glasses is ignored. The receiver pushes type 6 unsolicited after room entry and again whenever the PC user changes the preset.

### No pose or alignment path in the receiver

CameraParam contains only Fov4f fov. ObserverViewController neither subscribes to NetWorkServer.OnReceived nor sends MessageSynchronization (type 7), and it has no transform/pose fields. NetWorkServer's only Observer-specific outbound data is UpdateCameraParam.

The serialized ObserverViewPage prefab was also inspected. Its non-UI logic consists of ObserverViewController, DebugPanel, the RTP RawImage, and the idle AVPro video player. It has no ArUco, marker, calibration, Vuforia anchor, or tracking component. The assembly bundles OpenCV/Aruco and Vuforia types as unrelated package/sample baggage, but none is attached to the ObserverView page. Blender simply starts the default WebCamTexture; it does not estimate that camera's pose.

The preliminary hypothesis is therefore only half correct: the PC dictates the observer **projection/FOV**, but this app does not dictate its **pose**. A fixed transform, manual placement, world anchor, or marker alignment would have to be implemented by the sender application, and the receiver DLL cannot distinguish which XREAL intended.

## Protocol comparison with FirstPersonView

| Aspect | FirstPersonView | ObserverView |
|---|---|---|
| PC controller | FirstPersonViewController | ObserverViewController |
| Discovery | UDP FIND-SERVER on 6001 | Same |
| TCP control | Fixed server port 6000; same framing and room/heartbeat flow | Same |
| RTP video | H.264 to receiver default 5555 | Same decoder and default 5555 |
| Initial app message | Type 7 {"useAudio":bool}, then waits for {"success":true} | No type-7 handler or acknowledgement |
| Camera parameters | None | PC sends type 6 CameraParam/Fov4f after join and on preset changes |
| Glasses render | Head/capture-camera POV, suitable for direct display | Dedicated external observer camera; virtual content with alpha |
| PC display | Decoded _MainTex only | Decoded RGB+alpha composited over a local WebCamTexture |
| Audio | Negotiated by type 7 | Intended default is off; no Observer audio negotiation |

BroadCastMsgOprator always replies "<local-ip>:6000". Both controllers use the same StreammingEncoder/NRNativeDecoder. The managed wrapper declares RTPDecoderSetConfigration(port) but never calls it; media_enc.dll embeds the default 5555 and an SDP with H.264 payload type 96. There is no separate Observer port or transport flow. The common decoder supports audio, but ObserverView has no intended way to enable it.

The mode is selected out-of-band by the page chosen in the PC UI; there is no mode field in the handshake. Sending the FPV type-7 useAudio request while the receiver is on its Observer page will time out because that controller never subscribes to OnReceived.

## What the Godot glasses sender would need

Relative to the working FPV path:

1. Reuse FIND-SERVER, TCP connect, EnterRoom, ExitRoom, and heartbeats. Do not require the FPV useAudio request/response when targeting the Observer page.
2. Register type 6 and deserialize CameraParam.fov. Accept an unsolicited initial update after room entry and later live updates. Do not depend on UpdateCameraParamRequest().
3. Add a dedicated observer render camera. Convert the four tangent extents to an off-centre Godot projection, checking Unity/Godot handedness and vertical orientation on device.
4. Give that camera an explicit pose in the glasses app's world. To align with the PC webcam, provide an application-level placement/calibration mechanism such as a fixed scene anchor, manual alignment, or marker/anchor workflow. Nothing in the receiver protocol supplies this transform.
5. Render virtual content only from that observer camera, preserving alpha; do not render the glasses RGB camera/head POV into this target.
6. Start the same native H.264/RTP encoder to rtp://<receiver-ip>:5555, but configure useAlpha:true. The receiver expects the native encoder's top/bottom RGB+alpha packing. If the port cannot use that native mode, it must reproduce the same packing before encoding.
7. Keep Observer audio disabled unless both sides are deliberately extended with a new negotiation.

The FPV implementation is reusable at the discovery, TCP framing, RTP, H.264, and frame-submission layers. The new work is type-6 handling, off-axis projection, an observer pose/alignment policy, virtual-only rendering, and alpha-packed encoding.

## Remaining unknowns and useful captures

The receiver DLL proves the FOV direction and absence of a PC-sent pose, but cannot recover the missing sender's pose policy. These checks would settle the remaining questions:

- Obtain/decompile an actual XREAL ObserverView sender or instrument an app that implements OnCameraParamUpdate; inspect where it places its capture camera and how it applies Fov4f.
- Capture TCP while entering the Observer page to record the exact initial type-6 JSON values/timing. This validates, rather than changes, the recovered protocol.
- Decode one Observer RTP frame before PC composition. It should visibly contain vertically packed foreground colour and alpha; this validates native useAlpha:true and the half ordering.
- Test a known world-space object while moving the glasses and PC webcam independently. Since no pose crosses TCP, this reveals whether the sender uses a fixed world transform, a head-relative offset, or a separately configured anchor.

## Live capture (2026-07-19) — validates the recovered protocol on device

Ran a UDP+TCP client (Python, acting as the glasses/sender) against the running ObserverView receiver:

- `FIND-SERVER` (UDP 6001) -> reply `192.168.0.8:6000` — identical discovery + TCP control port to FPV.
- `EnterRoom`(4) -> `{"result":true}`, then the receiver **pushed exactly one** `UpdateCameraParam`(6):
  `{"fov":{"left":0.558311641216278,"right":0.585904240608215,"top":0.328167080879211,"bottom":0.314368575811386}}`.
  Interpreting the four as tangents: H FOV ≈ atan(.558)+atan(.586) ≈ 59.6°, V FOV ≈ atan(.328)+atan(.314)
  ≈ 35.6°, aspect ≈ 1.78 (16:9) — consistent with `left=cx/fx … bottom=(h-cy)/fy` from a webcam preset.
- **No pose, and no further messages** over 22 s of passive listening (`UpdateCameraParam count = 1`).
- Confirmed the useAudio caveat: sending the FPV `{"useAudio":true}`(7) request while the receiver was on
  its Observer page made it **drop the connection** — the Observer controller never handles type 7.

So the on-device capture matches codex's DLL/shader analysis: **PC -> glasses, FOV only (off-centre,
webcam-derived), no pose**; discovery/TCP/RTP:5555 shared with FPV; type-7 useAudio unhandled.

## Bottom line

ObserverView **is** an MRC/LIV-style spectator composite (virtual holograms + alpha over the PC's
webcam), not a simpler pose-controlled camera. For this repo it is implementable by reusing the FPV
pairing/RTP path plus: handle `UpdateCameraParam`(6), build an off-centre projection from the FOV, render
**virtual-only with alpha**, encode with `useAlpha:true` in the top/bottom packing the receiver expects
(our `src/video_encoder.rs` currently hardcodes `useAlpha:false`), and choose an observer-camera pose +
webcam-alignment policy ourselves (the protocol carries none).
