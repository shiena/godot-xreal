> Provenance: codex (codex-cli 0.144.4), 2026-07-31, disassembly of the vendored
> `libmedia_codec.so` (GNU Build ID `75a6536f531fa7de046db96609c7e119ad5287f4`). Verdict driving
> our fix: the lib sets `intra-refresh-period=10`, replacing periodic IDR with cyclic intra
> refresh; no in-lib or JSON knob disables it. We took the runtime `AMediaCodec_setParameters(
> request-sync)` route (the "unofficial escape"), gated by `debug.xreal.idr_hack`.

# XREAL SDK 3.1.0 HW encoder IDR 調査報告

## 結論

SDK 3.1.0 の `libmedia_codec.so` は、H.264 encoder に次の相反する設定を渡しています。

```text
i-frame-interval     = 1   // 1秒ごとのキーフレーム要求
intra-refresh-period = 10  // IDRの代わりに10フレーム周期の巡回intra refresh
```

原因は後者です。Android の仕様でも intra refresh は、キーフレームを挿入する方式に代えてフレームの一部を順次 intra 符号化するストリーミング向け機能です。[MediaFormat.KEY_INTRA_REFRESH_PERIOD](https://developer.android.com/reference/android/media/MediaFormat#KEY_INTRA_REFRESH_PERIOD)

したがって：

- `i-frame-interval` は `0`、`-1`、巨大値ではなく、正しく **1秒**。
- しかし `intra-refresh-period=10` が無条件に有効化され、端末の codec がこれを採用した結果、先頭以外の IDR が消えた。
- JSON からこの設定を変更・無効化するフィールドはない。
- ライブラリ内に `request-sync` 経路もない。
- サポートされた API の範囲では、**ライブラリ変更なしに周期 IDR を復活させる方法はない**。

対象：

- SHA-256: `4a5ec33467dd1ecc73e868e168c502a090316e99aac3764271c590e131a43e5f`
- GNU Build ID: `75a6536f531fa7de046db96609c7e119ad5287f4`
- [libmedia_codec.so](C:/Users/shien/dev/godot-xreal/addons/godot_xreal/jniLibs/arm64-v8a/libmedia_codec.so)

## MediaCodec の設定内容

映像 encoder 設定関数は `0x20D8CC` から始まります。引数は以下です。

```c
configure_video(
    encoder,
    width,       // w1
    height,      // w2
    bitrate,     // w3
    frame_rate,  // w4
    use_alpha    // w5
);
```

主要部分：

```asm
20D9C0  adrp x1, ...             ; "width"
20D9C4  ldr  w2, [x19,#72]
20D9D0  bl   AMediaFormat_setInt32

20D9D4  adrp x1, ...             ; "height"
20D9D8  ldr  w2, [x19,#76]
20D9E4  bl   AMediaFormat_setInt32

20D9E8  ldr  w8, [x19,#80]       ; requested bitrate
20D9EC  mov  w9, #0xcccd
20D9F0  movk w9, #0xcccc,lsl#16  ; 0xCCCCCCCD
20DA00  add  w8,w8,w8,lsl#3      ; bitrate * 9
20DA04  umull x8,w8,w9
20DA08  lsr  x2,x8,#35           ; bitrate * 9 / 10
20DA0C  bl   AMediaFormat_setInt32 ; "bitrate"

20DA10  adrp x1, ...             ; "frame-rate"
20DA14  ldr  w2, [x19,#84]
20DA20  bl   AMediaFormat_setInt32
```

従って実効 bitrate は JSON `bitRate` の **90%** です。

その後：

```asm
20DA24  ... x1 = "profile"
20DA30  mov  w2,#8
20DA34  bl   AMediaFormat_setInt32

20DA38  ... x1 = "level"
20DA44  mov  w2,#0x8000
20DA48  bl   AMediaFormat_setInt32

20DA4C  adrp x1,0x121000
20DA54  add  x1,x1,#0x419        ; "i-frame-interval"
20DA58  mov  w2,#1
20DA5C  bl   AMediaFormat_setInt32

20DA60  adrp x1,0xDC000
20DA68  add  x1,x1,#0xDD8        ; "intra-refresh-period"
20DA6C  mov  w2,#10
20DA70  bl   AMediaFormat_setInt32
```

復号した文字列：

| アドレス | キー | 値 |
|---:|---|---:|
| `0x109BD8` | `width` | JSON width |
| `0x0C5DC6` | `height` | JSON height |
| `0x0CE9DE` | `bitrate` | JSON bitRate × 0.9 |
| `0x0AF695` | `frame-rate` | 既定値25 |
| `0x0EE8F1` | `profile` | 8 |
| `0x0EE8F9` | `level` | `0x8000` |
| `0x121419` | `i-frame-interval` | **1秒** |
| `0x0DCDD8` | `intra-refresh-period` | **10フレーム** |
| `0x109BDE` | `color-format` | vendor format、fallback 21/19 |

`bitrate-mode` 文字列はバイナリ内に存在せず、設定されません。

`i-frame-interval=1` は Android 仕様上「1秒ごと」です。負なら先頭以降なし、ゼロなら全フレームをキーフレームにする指定ですが、本ライブラリはどちらでもありません。[MediaFormat.KEY_I_FRAME_INTERVAL](https://developer.android.com/reference/android/media/MediaFormat#KEY_I_FRAME_INTERVAL)

configure 後も `AMediaFormat_getInt32` で値を読み返しています。

```asm
20DD80 ... "i-frame-interval"
20DD90 bl AMediaFormat_getInt32
...
20DE70 ... log:
         "Video encoder encoder successfully configured.
          size=%ix%i, i-frame-interval=%d"

20DDAC ... "frame-rate"
20DDBC bl AMediaFormat_getInt32
```

## JSON パーサ

`HWEncoderSetConfigration` は vtable `+0x20` を呼び、実装は `0x214534–0x214B13` です。

読み取る JSON フィールドは次の15個だけです。

| 最初の参照 | JSON キー |
|---:|---|
| `0x214678` | `useAlpha` |
| `0x2146A8` | `width` |
| `0x2146D4` | `height` |
| `0x214714` | `bitRate` |
| `0x214774` | `codecType` |
| `0x2147A0` | `outPutPath` |
| `0x2147D4` | `useStepTime` |
| `0x214804` | `useLinnerTexture` |
| `0x214834` | `addMicphoneAudio` |
| `0x21487C` | `audioChannels` |
| `0x2148A8` | `audioBitRate` |
| `0x2148D4` | `audioSampleRate` |
| `0x214900` | `audioStepRecord` |
| `0x214930` | `audioUseExternalData` |
| `0x214978` | `addInternalAudio` |

以下は存在しません。

```text
iFrameInterval
keyFrameInterval
idrInterval
gopSize
intraRefreshPeriod
requestSync
fps
frameRate
```

特に現在送っている `fps` は **SDK 3.1.0 では無視されます**。constructor は `0x137E08` の64ビット定数 `{2048000, 25}` を `0x213EC8–0x213EE0` で bitrate/fps の既定値として格納します。start 側は：

```asm
214E74  ldp w1,w2,[x19,#56]  ; width, height
214E78  ldp w3,w4,[x19,#64]  ; bitrate, fps
214E84  mov w5,#1
214E88  bl  0x20D8CC
```

したがって MediaCodec の frame rate は、JSON の `fps` にかかわらず通常 **25 fps** です。ただし I-frame interval は秒単位なので、intra refresh を除けば約25フレームごとの IDR が期待値です。

また `0x214574` 付近に `"config fail, encoder already start"` があり、開始後に `SetConfigration` を再適用することもできません。

## runtime sync-frame 経路

存在しません。

- `request-sync`
- `request-sync-frame`
- `video-bitrate`
- `AMEDIACODEC_KEY_REQUEST_SYNC_FRAME`
- `AMediaCodec_setParameters`

はいずれも import/文字列にありません。

全 `AMediaCodec_*` callsite を列挙しても、create/configure/start/stop/flush/buffer access/input surface のみでした。`UpdateSurface` 実装を含め、`setParameters` 呼び出しはありません。

バイナリ内にある唯一の `"setParameters"` は次の JNI 音声処理です。

```text
0x20F244  "android/media/AudioSystem"
0x20F254  "setParameters"
0x20F278  "screen_record=true"

0x20FFA4  "android/media/AudioSystem"
0x20FFB4  "setParameters"
0x20FFD0  "screen_record=false"
```

MediaCodec とは無関係です。

隠し関数にも sync 要求実装はなく、動的シンボルにも載っていないため、`dlsym` 可能な近傍 entry point はありません。

## 正式な修正

最小修正は `intra-refresh-period` の設定を削除することです。`i-frame-interval=1` は既に正しいため、追加フィールドは不要です。

この特定ビルドに対する最小バイナリパッチは：

```text
VA/file offset: 0x20DA70
before: 10 16 18 94    // bl AMediaFormat_setInt32
after:  1F 20 03 D5    // nop
```

これで `"intra-refresh-period",10` の一回だけの call を除去できます。この文字列へのコード参照も `0x20DA60–0x20DA70` の一箇所だけです。端末確認は必要ですが、静的解析上は周期 IDR を抑制する唯一の設定を除去し、既存の1秒間隔指定をそのまま有効にする修正です。

## ライブラリを変更しない非公式な逃げ道

RE/未検証の ABI hack なら、外部から NDK の `AMediaCodec_setParameters` を呼べる可能性があります。

実体ポインタは現ビルドでは：

```c
video = *(void **)(handle + 0x88); // main object +136
codec = *(AMediaCodec **)(video + 0x08);
```

開始後、1秒ごとに概念上：

```c
AMediaFormat *p = AMediaFormat_new();
AMediaFormat_setInt32(p, "request-sync", 0);
AMediaCodec_setParameters(codec, p);
AMediaFormat_delete(p);
```

とします。NDK API は API 26 以降で、codec running 中に使用可能です。[Android NDK Media API](https://developer.android.com/ndk/reference/group/media#amediacodec_setparameters) `request-sync=0` は「soon に sync frame を生成せよ」という正式なパラメータです。[MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME](https://developer.android.com/reference/android/media/MediaCodec#PARAMETER_KEY_REQUEST_SYNC_FRAME)

ただしこれは opaque な C++ object layout に依存し、SDK 更新で即破損し得ます。正式修正ではなく、必ず RE/unverified として扱うべきです。

## パッチもABI hackもしない場合の上限

その場合、ライブの late join は修復不能です。

- viewer を encoder start 前に接続する
- viewer 接続時に encoder 全体を stop/start して新しい先頭 IDR を出す

のどちらかが上限です。先頭 IDR/SPS/PPSだけを保存して後から再送しても、現在の P-frame はその間の参照チェーンに依存するため、late join のライブ復旧にはなりません。