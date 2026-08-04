# XREAL native encoder app/microphone audio mixing contract

Date: 2026-07-22. Binary examined: SDK 3.1.0 arm64-v8a
`jniLibs/arm64-v8a/libmedia_codec.so` (9,121,256 bytes, SHA-256
`4A5EC33467DD1ECC73E868E168C502A090316E99AAC3764271C590E131A43E5F`), using NDK
27.2.12479018 `llvm-objdump`, `llvm-nm`, `llvm-readelf`, and `llvm-strings`. This is static
analysis, cross-checked against the supplied Unity C# and the device measurements in the task.

`libmedia_codec.so` has only dynamic symbols, despite its flat `HWEncoder*` exports being named.
The encoder implementation was recovered through the concrete object's vtable at `0x8155c0`:
`SetMediaProjection @ 0x21435c`, `SetConfigration @ 0x214534`, `Start @ 0x214b14`,
`AdjustVolume @ 0x215e3c`, and `NotifyAudioData @ 0x217354`.

## Verdict

**CONFIRMED — one AAC track can contain native playback-capture audio and native microphone audio.**
The intended two-source path is:

1. obtain a real, non-null Android `MediaProjection` after user consent;
2. configure `addMicphoneAudio:true`, `addInternalAudio:true`, and
   `audioUseExternalData:false`/absent;
3. call `HWEncoderSetConfigration`, then `HWEncoderSetMediaProjection(handle, projection)`, then
   `HWEncoderStart`;
4. do **not** call `HWEncoderNotifyAudioData`;
5. stop and destroy normally.

The mic callback is the primary producer. Before it queues a PCM block to the one AAC encoder, the
native mixer pulls the corresponding app block from the playback recorder and performs saturating
sample addition at `0x217c70..0x217cac` (with a vectorized version beginning at `0x217d20`). There
is one downstream audio-encoder queue at encoder-object `+0x90`, not one AAC track per source.

**CONFIRMED — the current combination, native mic plus `HWEncoderNotifyAudioData`, is not a supported
two-source combination.** `NotifyAudioData` enters the exact same internal source-0/mic callback path
as the native mic recorder. It does not identify its input as `REC_APP`. Thus native mic callbacks
and pushed callbacks independently enqueue blocks to the same AAC producer queue. They are not mixed
with each other. The observed 1.7934 audio/video duration and recurrent near-silent mic blocks are the
expected result of those two producers being appended/interleaved.

**CONFIRMED — there is no source selector in the exported `NotifyAudioData` ABI.** If a non-null
`MediaProjection` is not acceptable and Godot must supply app PCM, the reliable contract is to
capture/mix app and mic on our side and submit exactly one PCM stream. The binary contains an
otherwise-unexposed JSON switch for this mode: `"audioUseExternalData":true`. With both native capture
flags false, that switch creates the AAC path without starting either native recorder. This field is
**RE / unverified by the shipped C# config type** even though its native parsing and start behavior
are confirmed in this binary.

## What `addInternalAudio` does

**CONFIRMED — it starts Android playback capture; it does not mean “expect pushed app samples.”**
The JSON parser resolves `addInternalAudio` at string address `0xd7a8e` and stores it at encoder byte
`+0x59` (`0x214978..0x2149bc`). `Start @ 0x214b14` tests that byte at
`0x21519c..0x2151e8` and calls the internal-recorder constructor at `0x2157cc`.

The constructor uses these Android/JNI types and methods:

- `android/media/AudioPlaybackCaptureConfiguration$Builder` at string `0x105973`;
- constructor signature `(Landroid/media/projection/MediaProjection;)V` at `0x1059ab`;
- `addMatchingUsage` at `0xf22eb`, called with integer `1` at `0x20eba8..0x20ebcc`;
- `build`, returning `AudioPlaybackCaptureConfiguration`, at `0x20ebd0..0x20ebf4`;
- `android/media/AudioRecord$Builder.setAudioPlaybackCaptureConfig` at strings `0x1059f3` and
  `0xe0b25`, used in the AudioRecord-builder path around `0x20f1e0..0x20f210`.

Integer usage `1` is Android `AudioAttributes.USAGE_MEDIA`. The binary also gates this builder on
Android API level 29 at `0x20eb5c..0x20eb64`.

The decisive builder sequence is:

```text
0x20eb70  add x1, x1, #0x973     ; AudioPlaybackCaptureConfiguration$Builder
0x20eb94  add x2, x2, #0x9ab     ; (MediaProjection;)V
0x20eba0  mov x3, x21            ; supplied MediaProjection jobject
0x20ebb0  add x1, x1, #0x2eb     ; addMatchingUsage
0x20ebc8  mov w2, #1             ; USAGE_MEDIA
```

**CONFIRMED — `HWEncoderSetMediaProjection` must run before the playback recorder starts.** The
implementation stores a JNI global reference wrapper at process globals `0x952e58/0x952e60`
(`0x21435c..0x214418`). The internal-recorder setup retrieves that global reference at
`0x2158a0..0x2158b0`. The binary contains the diagnostic “Internal recorder is running, set
mediaProjection fail” at `0xaf786` and refuses to replace a running internal recorder at
`0x21442c..0x214464`.

**LIKELY — passing null makes `addInternalAudio` ineffective as an actual playback source.**
`SetMediaProjection(null)` still creates a wrapper, so `Start`'s shallow presence check at
`0x215080..0x215094` leaves `+0x59` enabled. Later, however, the null JNI object is passed to the
`AudioPlaybackCaptureConfiguration.Builder(MediaProjection)` constructor. There is no alternate
non-projection app-capture implementation. The task's internal-only-plus-push recording has normal
duration rather than two producers' duration, which is consistent with the native playback recorder
not producing data. The precise exception/failure status on this device remains unresolved because
the JNI exception path was not observed live.

**CONFIRMED — a valid `MediaProjection` is necessary but not by itself sufficient for every sound.**
The capture configuration includes only matching usage `USAGE_MEDIA`; Android playback-capture
policy, the producing app's manifest/policy, and audio usage still determine what is capturable.

## What `HWEncoderNotifyAudioData` does

The true exported signature in this binary is:

```c
// Exported ABI, confirmed from AArch64 register use.
int HWEncoderNotifyAudioData(
    uint64_t handle,
    const void *data,
    int32_t n_samples,
    int32_t bytes_per_sample,
    int32_t channels,
    int32_t sample_rate,
    int32_t sample_fmt);
```

**CONFIRMED — all seven arguments have the meanings above.** At implementation entry
`0x217354`, `x1` is saved as data, `w2` as sample count, `w3` as bytes per sample, `w4` as channels,
`w5` as sample rate, and `w6` as sample format (`0x217380..0x2173a0`). The fast path verifies:

- pushed channels equal configured channels at `0x2173ac..0x2173b8`;
- `sample_fmt == 1` at `0x217400..0x217404`;
- pushed sample rate equals configured sample rate at `0x217408..0x217410`.

When all three match, it calls the common input routine at `0x2177a0..0x2177bc`. Otherwise it creates
or reuses a sample converter at `0x217414..0x21768c`, then sends converted s16 data through the same
routine at `0x2176d0..0x2176ec`. For the implementation contract, use matching mono s16 and avoid
depending on this conversion path.

**CONFIRMED — `n_samples` is the total number of scalar samples, not bytes and not per-channel
frames.** The common path allocates/copies `n_samples * bytes_per_sample` bytes at
`0x216950..0x21699c`; it does not multiply the payload size by `channels`. This matches the C# call
`audioData.Length / bytePerSample`.

**CONFIRMED — `sample_fmt=1` is the native s16 contract, despite the stale C# DllImport comment saying
“0:s16.”** The shipped C# caller passes `1`; the native direct-path check requires `1`; and the mixer
loads, scales, adds, and saturates 16-bit signed elements. `fmt` is therefore not a hidden recorder
index.

**CONFIRMED — pushed blocks are hard-wired to internal source 0, the mic side.** The exported method
eventually calls `0x2177f8` with its private `w5` source argument forced to zero at
`0x216738..0x216758`. Source 0 uses volume slot `encoder+0x200`, corresponding to
`RecorderIndex.REC_MIC=0`, and logs “OutputStream::MixAudio mic scale=%f” (string `0xf5a6d`) at
`0x217864..0x21789c`. The application slot is `encoder+0x204`, matching `REC_APP=1` and
`AdjustVolume`'s indexed store at `0x215e8c..0x215e9c`.

```text
0x216738  mov w27, w5            ; preserve public sample_rate
0x216744  mov w5, wzr            ; private source selector = 0
0x216758  bl  0x2177f8           ; scale/mix source-0 block

0x217818  tbz w5, #0, 0x217864  ; zero selects mic-side branch
0x21789c  ldr s0, [x21,#0x200]  ; REC_MIC volume slot
```

**CONFIRMED — native mic data and notified data converge before the queue but not before mixing with
each other.** Native mic setup at `0x215524` installs callback `0x216710` at
`0x21569c..0x2156d8`. `NotifyAudioData` also reaches `0x216710`. Each invocation allocates a block and
links it into the single queue owned by the AAC encoder at object `+0x90`
(`0x216924..0x216aa0`). Consequently, two concurrent source-0 producers append two timelines.

The two routes converge as follows:

```text
native mic setup:  0x21569c  adrp x10, 0x216000
                   0x2156a4  add  x10, x10, #0x710  ; callback 0x216710
Notify fast path:  0x2177bc  bl   0x216710
queue insertion:   0x216924..0x2169e0
```

**CONFIRMED — the real two-source mixer is asymmetric.** Internal playback setup at `0x2157cc`
installs callback `0x2160a0` at `0x215954..0x215994`. When mic capture is enabled, the playback
callback does not enqueue its own AAC input block (`0x2160d0..0x216110`); it leaves app data available
to the mixer. The source-0/mic path obtains that buffered playback data at
`0x217af0..0x217c1c` and combines it sample-by-sample at `0x217c24..0x217cac`. When mic capture is not
enabled, the playback callback instead becomes the sole queue producer at `0x216408..0x21658c`.

The scalar mixer itself is unambiguous signed-16 saturating addition:

```text
0x217c70  ldr   h1, [x11]        ; buffered app sample
0x217c74  ldrsh w14, [x9], #2    ; source-0/mic sample
0x217c8c  fadd  d1, d1, d2
0x217c94  cmp   w14, #0x7fff
0x217c9c  cmn   w14, #0x8000
0x217ca8  strh  w14, [x11], #2   ; mixed sample replaces app sample
```

**CONFIRMED — `audioUseExternalData` is the intended single external-producer enable.** The JSON key
exists at string `0x11d610`; parsing stores it at encoder byte `+0x5b` at
`0x214930..0x214974`. The overall audio-enabled byte `+0x4b` is the OR of mic, internal, and external
flags. `Start` creates the AAC encoder when `+0x4b` is set, starts native recorders only for `+0x5a`
and `+0x59`, and preserves audio enable when external `+0x5b` alone is set
(`0x2151ec..0x215208`). This is the clean mode for one pre-mixed `NotifyAudioData` stream.

The external-only keep-alive test in `Start` is:

```text
0x2151ec  ldrb w8, [x19,#0x5a]  ; addMicphoneAudio
0x2151f4  ldrb w8, [x19,#0x59]  ; addInternalAudio
0x2151fc  ldrb w8, [x19,#0x5b]  ; audioUseExternalData
0x215200  cbnz w8, 0x215208     ; keep the AAC path enabled
0x215204  strb wzr, [x19,#0x4b]; otherwise disable audio
```

## Per-question findings

### 1. What `addInternalAudio` actually does

- **CONFIRMED:** starts an API-29 `AudioPlaybackCaptureConfiguration`/`AudioRecord` path for
  `USAGE_MEDIA`.
- **CONFIRMED:** that path consumes the `MediaProjection` passed to
  `HWEncoderSetMediaProjection`; it is not driven by `NotifyAudioData`.
- **LIKELY:** a null projection leaves the flag superficially enabled but yields no functioning
  playback source. There is no fallback capture path.
- **CONFIRMED:** set the projection before `Start`; replacing it after internal capture starts is
  rejected.

### 2. How `NotifyAudioData` reaches the track

- **CONFIRMED:** validates/converts the described PCM, enters the source-0/mic input routine, then
  adds an independently allocated block to the one AAC input queue at object `+0x90`.
- **CONFIRMED:** native mic callbacks enter the same routine and same queue.
- **CONFIRMED:** notified blocks and native mic blocks are not mixed with each other. They are two
  producers on one encoded timeline, explaining the malformed duration.
- **CONFIRMED:** source-0 blocks can be mixed with buffered native source-1 playback capture, which
  is why the native mixer exists; that does not make notified app PCM source 1.

### 3. Whether a per-source selector is missing

- **CONFIRMED:** no selector exists in the exported seven-argument function. `sample_fmt` is really a
  format and must be `1` for the s16 fast path.
- **CONFIRMED:** `REC_MIC`/`REC_APP` selects only the two volume slots in
  `HWEncoderAdjustVolume(handle, index, float)`; it is not part of `NotifyAudioData`.
- **CONFIRMED:** the private mixer has a source selector, but `NotifyAudioData` fixes it to source 0.
  No exported call exposes source 1 injection.

### 4. Supported combinations

- **CONFIRMED (static contract):** native mic + native app capture: both flags true, valid projection,
  no notify calls. Native code mixes to one queue/track.
- **CONFIRMED (static contract):** pushed custom mic + native app capture: mic flag false, internal
  flag true, valid projection, and one notify producer. Pushed data occupies source 0 and native
  playback occupies source 1. This is not useful when the pushed data is actually app audio.
- **CONFIRMED (static contract):** entirely external/pre-mixed PCM: both native flags false,
  `audioUseExternalData:true`, and exactly one notify producer.
- **UNSUPPORTED by the exported ABI:** native mic + pushed app PCM. Both occupy source 0; there is no
  public way to label the pushed data `REC_APP`.

The native-native and external-only sequences are statically complete, but a valid-projection run and
an `audioUseExternalData` run have not yet been device-verified in this repository.

## What we must change

Choose exactly one of these implementations.

### Option A — let XREAL capture and mix both sources

- [ ] Add an Android bridge that requests screen-capture consent and obtains a non-null
  `android.media.projection.MediaProjection` object.
- [ ] Keep `RECORD_AUDIO` runtime permission for the mic. Ensure app output uses capturable
  `USAGE_MEDIA` and is allowed by Android playback-capture policy.
- [ ] Configure `addMicphoneAudio:true`, `addInternalAudio:true`, and omit/set false
  `audioUseExternalData`.
- [ ] Call `Create -> SetConfigration -> SetMediaProjection(non_null_jobject) -> AdjustVolume
  (optional) -> Start` in that order.
- [ ] Stop calling `HWEncoderNotifyAudioData` in this mode. Do not install/pump `XrealAudioTap`.
- [ ] Use `REC_MIC=0` and `REC_APP=1` only for the respective native volume factors.
- [ ] Stop and destroy as today; release the Java `MediaProjection` according to Android lifecycle
  ownership after capture.

### Option B — mix both sources ourselves and push one stream

- [ ] Capture the microphone ourselves; do not enable XREAL's native mic recorder.
- [ ] Convert app and mic to one common rate/channel layout, align them by sample time, sum with
  saturation/limiting, and produce one continuous mono s16 stream.
- [ ] Add the native-only config field `"audioUseExternalData":true` and set
  `addMicphoneAudio:false`, `addInternalAudio:false`. Mark this config field RE/unverified in code and
  the reverse-engineering reference when implemented.
- [ ] Retain the existing required call order, including `SetMediaProjection(handle, null)` before
  `Start`; no playback capture is requested in this mode.
- [ ] Make exactly one `NotifyAudioData` call stream with
  `(n_samples=byte_len/2, bytes_per_sample=2, channels=1, sample_rate=config_rate,
  sample_fmt=1)`.
- [ ] Do not use `addInternalAudio:true` with null projection merely as an AAC-track enable. That is
  an accidental side effect; `audioUseExternalData` is the native switch for this purpose.
- [ ] Treat `REC_MIC` volume as the volume slot applied to the whole external stream, or leave it at
  1.0 and perform source gains before mixing. `REC_APP` cannot address notified PCM.

In either option, update the current comments that describe `NotifyAudioData` as an “app/internal
audio” API. It is an external PCM input hard-wired to the mic-side/source-0 pipeline.

## Unresolved / what a device experiment would settle

- **UNRESOLVED — valid-projection end-to-end behavior on the target device.** Record loud BGM plus
  spoken mic with both flags true, a non-null projection, and zero notify calls. A one-duration AAC
  track containing both signals would validate the complete intended path.
- **UNRESOLVED — exact null-projection failure mode.** Run internal-only, no pushed data, with null
  projection while capturing logcat/JNI exceptions. This distinguishes constructor failure,
  `AudioRecord` preparation failure, and a running recorder that yields no samples.
- **UNRESOLVED — `audioUseExternalData` device behavior.** Run both native flags false, external true,
  and push a timestamped tone. Verify one AAC track, correct duration, and no opened `AudioRecord`.
- **UNRESOLVED — playback policy for Godot's output on this build.** With a valid projection, verify
  that Godot/OpenSL output is tagged `USAGE_MEDIA` and permitted for playback capture. If not, the
  native-native route cannot supply app sound even though the mixer contract is correct.
- **UNRESOLVED — mixer alignment under mismatched recorder scheduling.** The code pulls available app
  samples when a mic block arrives, but static analysis does not establish its long-run drift/drop
  policy. A dual-tone recording and sample-level correlation would quantify alignment.
- **UNRESOLVED — converter edge cases.** Although the mismatch path is present, keep pushed channels,
  rate, and format equal to the configured AAC input until multi-rate/stereo device tests verify its
  duration and channel semantics.

The highest-value next experiment is the external-only config. It directly validates the safe
fallback contract without requiring `MediaProjection`, and it should remove the second producer that
caused the measured 1.7934-duration track.
