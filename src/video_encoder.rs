//! First-person-view video streaming via the XREAL SDK's hardware encoder `libmedia_codec.so` (flat C
//! `HWEncoder*` exports, dlsym'd like the other vendored libs). The encoder is a MediaCodec-backed
//! H.264 encoder + muxer: configure it with a JSON string (resolution / bitrate / fps + `codecType`
//! 0=local mp4 / 1=RTMP / 2=RTP and the output path or `rtp://`/`rtmp://` URL), then hand it a GL
//! texture id per frame via `HWEncoderUpdateSurface`. See `docs/plans/fpv-streaming-plan.md`.
//!
//! `HWEncoderUpdateSurface(handle, gl_texture_id, timestamp)` reads the GL texture on the **current
//! EGL context**, so `submit_frame` MUST be called on Godot's render thread (see
//! `crate::unity_plugin::run_render_thread_tick` / `RenderingServer::call_on_render_thread`), not the
//! main thread. `codecType` is derived from the output URL scheme.

use std::ffi::{c_char, CString};
use std::sync::Mutex;

use libloading::Library;

const MEDIA_CODEC_LIB: &str = "libmedia_codec.so";

type FnCreate = unsafe extern "C" fn(*mut u64) -> i32;
type FnSetConfig = unsafe extern "C" fn(u64, *const c_char) -> i32;
type FnStart = unsafe extern "C" fn(u64) -> i32;
type FnUpdateSurface = unsafe extern "C" fn(u64, usize, u64) -> i32;
/// `HWEncoderNotifyAudioData(handle, samples, nSamples, nBytesPerSample, nChannels, sampleRate, fmt)`
/// — `fmt` 0 = s16 / 8 = float. Feeds app ("internal") audio; the mic is captured natively when
/// enabled in the config.
type FnNotifyAudio = unsafe extern "C" fn(u64, *const u8, i32, i32, i32, i32, i32) -> i32;
type FnStop = unsafe extern "C" fn(u64) -> i32;
type FnDestroy = unsafe extern "C" fn(u64) -> i32;

/// A live encoder: the loaded library, resolved `HWEncoder*` entry points, and the handle.
struct Encoder {
    _lib: Library,
    update_surface: FnUpdateSurface,
    notify_audio: Option<FnNotifyAudio>,
    stop: FnStop,
    destroy: FnDestroy,
    handle: u64,
}

// The fn pointers borrow from `_lib` (kept alive alongside them); safe to move across threads —
// `submit_frame` runs on the render thread while `start`/`stop` run on the main thread.
unsafe impl Send for Encoder {}

static ENCODER: Mutex<Option<Encoder>> = Mutex::new(None);

/// `codecType` for the output path: `rtp://` → 2, `rtmp://` → 1, else 0 (local file).
fn codec_type(output: &str) -> i32 {
    if output.starts_with("rtp://") {
        2
    } else if output.starts_with("rtmp://") {
        1
    } else {
        0
    }
}

/// Build the encoder config JSON (SDK format from `EncodeTypes.cs`). `with_mic` captures the microphone
/// natively; `with_internal` mixes app audio fed via [`push_audio`].
fn config_json(
    output: &str,
    width: i32,
    height: i32,
    bitrate: i32,
    fps: i32,
    with_mic: bool,
    with_internal: bool,
) -> String {
    format!(
        concat!(
            "{{\"width\":{},\"height\":{},\"bitRate\":{},\"fps\":{},\"codecType\":{},",
            "\"outPutPath\":\"{}\",\"useStepTime\":0,\"useAlpha\":false,\"useLinnerTexture\":true,",
            "\"addMicphoneAudio\":{},\"addInternalAudio\":{},\"audioSampleRate\":16000,",
            "\"audioBitRate\":128000}}"
        ),
        width,
        height,
        bitrate,
        fps,
        codec_type(output),
        output,
        with_mic,
        with_internal
    )
}

/// Whether a stream is currently active.
pub fn is_active() -> bool {
    ENCODER.lock().expect("encoder mutex").is_some()
}

/// Start streaming the FPV to `output` (`rtp://ip:port`, `rtmp://…`, or a local file path). Creates,
/// configures, and starts the HW encoder. Returns `false` on any failure (library/symbol absent or an
/// `HWEncoder*` non-zero status). Feed frames with [`submit_frame`] from the render thread.
pub fn start(
    output: &str,
    width: i32,
    height: i32,
    bitrate: i32,
    fps: i32,
    with_mic: bool,
    with_internal: bool,
) -> bool {
    let mut guard = ENCODER.lock().expect("encoder mutex");
    if guard.is_some() {
        return true; // already streaming
    }
    unsafe {
        let Ok(lib) = Library::new(MEDIA_CODEC_LIB) else {
            godot::global::godot_warn!("[xreal] dlopen {MEDIA_CODEC_LIB} failed");
            return false;
        };
        let create: FnCreate = match lib.get::<FnCreate>(b"HWEncoderCreate\0") {
            Ok(s) => *s,
            Err(_) => return false,
        };
        let set_config: FnSetConfig = match lib.get::<FnSetConfig>(b"HWEncoderSetConfigration\0") {
            Ok(s) => *s,
            Err(_) => return false,
        };
        let start_fn: FnStart = match lib.get::<FnStart>(b"HWEncoderStart\0") {
            Ok(s) => *s,
            Err(_) => return false,
        };
        let update_surface: FnUpdateSurface =
            match lib.get::<FnUpdateSurface>(b"HWEncoderUpdateSurface\0") {
                Ok(s) => *s,
                Err(_) => return false,
            };
        let stop: FnStop = match lib.get::<FnStop>(b"HWEncoderStop\0") {
            Ok(s) => *s,
            Err(_) => return false,
        };
        let destroy: FnDestroy = match lib.get::<FnDestroy>(b"HWEncoderDestroy\0") {
            Ok(s) => *s,
            Err(_) => return false,
        };
        let notify_audio: Option<FnNotifyAudio> = lib
            .get::<FnNotifyAudio>(b"HWEncoderNotifyAudioData\0")
            .ok()
            .map(|s| *s);

        let mut handle: u64 = 0;
        if create(&mut handle) != 0 || handle == 0 {
            godot::global::godot_warn!("[xreal] HWEncoderCreate failed");
            return false;
        }
        let cfg = config_json(output, width, height, bitrate, fps, with_mic, with_internal);
        let Ok(cfg_c) = CString::new(cfg.as_str()) else {
            destroy(handle);
            return false;
        };
        if set_config(handle, cfg_c.as_ptr()) != 0 {
            godot::global::godot_warn!("[xreal] HWEncoderSetConfigration failed: {cfg}");
            destroy(handle);
            return false;
        }
        if start_fn(handle) != 0 {
            godot::global::godot_warn!("[xreal] HWEncoderStart failed");
            destroy(handle);
            return false;
        }
        godot::global::godot_print!(
            "[xreal] FPV stream started -> {output} ({width}x{height} @{fps} {bitrate}bps codecType={})",
            codec_type(output)
        );
        *guard = Some(Encoder {
            _lib: lib,
            update_surface,
            notify_audio,
            stop,
            destroy,
            handle,
        });
        true
    }
}

/// Feed one buffer of app ("internal") audio to the stream via `HWEncoderNotifyAudioData`. `samples` is
/// raw PCM; `bytes_per_sample`/`channels`/`sample_rate`/`fmt` (0=s16, 8=float) describe it. Returns the
/// encoder status (`-1` if not streaming / the export is absent). The mic, if enabled, is captured
/// natively — this is only for app audio.
pub fn push_audio(
    samples: &[u8],
    n_samples: i32,
    bytes_per_sample: i32,
    channels: i32,
    sample_rate: i32,
    fmt: i32,
) -> i32 {
    let guard = ENCODER.lock().expect("encoder mutex");
    match guard
        .as_ref()
        .and_then(|e| e.notify_audio.map(|f| (e.handle, f)))
    {
        Some((handle, f)) => unsafe {
            f(
                handle,
                samples.as_ptr(),
                n_samples,
                bytes_per_sample,
                channels,
                sample_rate,
                fmt,
            )
        },
        None => -1,
    }
}

/// Feed one frame: `gl_texture_id` is the GL texture name to encode (from
/// `RenderingServer.texture_get_native_handle` on a viewport texture), `timestamp` in nanoseconds.
/// **Render thread only.** Returns the encoder status (`0` = ok, `-1` if not streaming).
pub fn submit_frame(gl_texture_id: usize, timestamp: u64) -> i32 {
    let guard = ENCODER.lock().expect("encoder mutex");
    match guard.as_ref() {
        Some(enc) => unsafe { (enc.update_surface)(enc.handle, gl_texture_id, timestamp) },
        None => -1,
    }
}

/// Stop + destroy the encoder (idempotent).
pub fn stop() {
    let mut guard = ENCODER.lock().expect("encoder mutex");
    if let Some(enc) = guard.take() {
        unsafe {
            (enc.stop)(enc.handle);
            (enc.destroy)(enc.handle);
        }
        godot::global::godot_print!("[xreal] FPV stream stopped");
    }
}
