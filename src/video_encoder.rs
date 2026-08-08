//! First-person-view video streaming through the XREAL SDK's hardware encoder
//! `libmedia_codec.so`, whose flat C `HWEncoder*` exports are dlsym'd like the other vendored libs.
//! The encoder is a MediaCodec-backed H.264 encoder and muxer: configure it with a JSON string
//! carrying the resolution, bitrate and fps, a `codecType` of 0 for a local mp4, 1 for RTMP or 2
//! for RTP, and the output path or `rtp://` or `rtmp://` URL, then hand it a GL texture id per frame
//! through `HWEncoderUpdateSurface`. See `docs/develop/plans/fpv-streaming-plan.md`.
//!
//! `HWEncoderUpdateSurface(handle, gl_texture_id, timestamp)` reads the GL texture on the **current
//! EGL context**, so `submit_frame` MUST be called on Godot's render thread, through
//! `crate::unity_plugin::run_render_thread_tick` or `RenderingServer::call_on_render_thread`, and
//! never on the main thread. `codecType` is derived from the output URL scheme.

use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use libloading::Library;

const MEDIA_CODEC_LIB: &str = "libmedia_codec.so";

type FnCreate = unsafe extern "C" fn(*mut u64) -> i32;
type FnSetConfig = unsafe extern "C" fn(u64, *const c_char) -> i32;
/// `HWEncoderSetMediaProjection(handle, media_projection)`. The SDK's
/// `NativeEncoder.SetConfigration` always calls this right after the config, passing a null
/// projection for the RGB-camera and texture path, since only screen capture makes it non-null. It
/// initialises encoder state that `HWEncoderStart` reads, and omitting it left that field null and
/// crashed `HWEncoderStart` on device with a SIGSEGV.
type FnSetMediaProjection = unsafe extern "C" fn(u64, *mut core::ffi::c_void) -> i32;
type FnStart = unsafe extern "C" fn(u64) -> i32;
type FnUpdateSurface = unsafe extern "C" fn(u64, usize, u64) -> i32;
type FnStop = unsafe extern "C" fn(u64) -> i32;
type FnDestroy = unsafe extern "C" fn(u64) -> i32;

/// A live encoder: the loaded library, resolved `HWEncoder*` entry points, and the handle.
struct Encoder {
    _lib: Library,
    update_surface: FnUpdateSurface,
    stop: FnStop,
    destroy: FnDestroy,
    handle: u64,
    /// Whether to inject periodic `request-sync` (the IDR workaround; see [`maybe_request_idr`]),
    /// resolved by [`resolve_idr_hack`] at start.
    idr_hack: bool,
}

// --- Periodic-IDR workaround (docs/develop/archive/codex-idr-analysis.md) -----------------------------
//
// The vendored libmedia_codec.so 3.1.0 configures the H.264 encoder with
// `intra-refresh-period=10`, which replaces periodic IDR key frames with cyclic intra refresh, so
// a late-joining RTP/FLV viewer never gets a decodable starting point (regression from the older
// lib, which emitted an IDR about once a second). The lib exposes no key-frame request and calls
// `AMediaCodec_setParameters` nowhere, but the underlying codec still honours Android's
// `request-sync` parameter. codex located the codec object: from the HWEncoder handle,
// `*(handle + 0x88)` is the video object and `*(+0x08)` of that is the `AMediaCodec*`. We reach it
// through `libmediandk.so` and ask for a sync frame ~once a second.
//
// This depends on that opaque C++ layout, so it is a toggle: the `xreal/idr_workaround`
// ProjectSetting (default ON), overridable at runtime by `debug.xreal.idr_hack` (0/1). It is
// pinned to the analyzed build (GNU Build ID 75a6536f531fa7de046db96609c7e119ad5287f4); an SDK
// bump needs the offsets re-checked. `request-sync` failing or the layout being wrong at
// worst produces no key frame - the pointers are null-checked, so a changed layout degrades to
// the old single-IDR behaviour rather than crashing, as long as +0x88/+0x08 still land on
// readable memory.

const MEDIA_NDK_LIB: &str = "libmediandk.so";
/// Offset of the video object within the HWEncoder handle, then of the `AMediaCodec*` within it.
const OFF_VIDEO_OBJ: usize = 0x88;
const OFF_CODEC_PTR: usize = 0x08;

type FnFormatNew = unsafe extern "C" fn() -> *mut c_void;
type FnFormatSetInt32 = unsafe extern "C" fn(*mut c_void, *const c_char, i32);
type FnFormatDelete = unsafe extern "C" fn(*mut c_void);
type FnCodecSetParameters = unsafe extern "C" fn(*mut c_void, *const c_void) -> i32;

struct MediaNdk {
    format_new: FnFormatNew,
    format_set_int32: FnFormatSetInt32,
    format_delete: FnFormatDelete,
    codec_set_parameters: FnCodecSetParameters,
    _lib: Library,
}

unsafe impl Send for MediaNdk {}
unsafe impl Sync for MediaNdk {}

static MEDIA_NDK: OnceLock<Option<MediaNdk>> = OnceLock::new();
/// Frames since the last requested key frame, shared across the GL and Vulkan feed paths (only
/// one encoder is ever live).
static IDR_COUNTER: AtomicU32 = AtomicU32::new(0);
/// Request a sync frame roughly once a second; both feed paths tick at the ~60 Hz render rate.
const IDR_PERIOD_FRAMES: u32 = 60;

fn media_ndk() -> Option<&'static MediaNdk> {
    MEDIA_NDK
        .get_or_init(|| unsafe {
            let lib = Library::new(MEDIA_NDK_LIB).ok()?;
            let format_new = *lib.get::<FnFormatNew>(b"AMediaFormat_new\0").ok()?;
            let format_set_int32 = *lib
                .get::<FnFormatSetInt32>(b"AMediaFormat_setInt32\0")
                .ok()?;
            let format_delete = *lib.get::<FnFormatDelete>(b"AMediaFormat_delete\0").ok()?;
            let codec_set_parameters = *lib
                .get::<FnCodecSetParameters>(b"AMediaCodec_setParameters\0")
                .ok()?;
            Some(MediaNdk {
                format_new,
                format_set_int32,
                format_delete,
                codec_set_parameters,
                _lib: lib,
            })
        })
        .as_ref()
}

/// Called once per fed frame. When the IDR workaround is on, asks the codec for a sync frame every
/// [`IDR_PERIOD_FRAMES`]. Any thread; the encoder feed is already serialized per path.
fn maybe_request_idr(handle: u64, idr_hack: bool) {
    if !idr_hack || handle == 0 {
        return;
    }
    if IDR_COUNTER.fetch_add(1, Ordering::Relaxed) % IDR_PERIOD_FRAMES != IDR_PERIOD_FRAMES - 1 {
        return;
    }
    let Some(ndk) = media_ndk() else { return };
    unsafe {
        // Walk the encoder object layout to the AMediaCodec*, null-checking each hop.
        let video = *((handle as *const u8).add(OFF_VIDEO_OBJ) as *const *mut c_void);
        if video.is_null() {
            return;
        }
        let codec = *((video as *const u8).add(OFF_CODEC_PTR) as *const *mut c_void);
        if codec.is_null() {
            return;
        }
        let fmt = (ndk.format_new)();
        if fmt.is_null() {
            return;
        }
        (ndk.format_set_int32)(fmt, c"request-sync".as_ptr(), 0);
        let r = (ndk.codec_set_parameters)(codec, fmt);
        (ndk.format_delete)(fmt);
        static LOGGED: AtomicU32 = AtomicU32::new(0);
        if LOGGED.fetch_add(1, Ordering::Relaxed) < 3 {
            godot::global::godot_print!(
                "[xreal] IDR workaround: request-sync -> {r} (codec={codec:?})"
            );
        }
    }
}

// The fn pointers borrow from `_lib`, which is kept alive alongside them. Moving them across
// threads is safe, and it happens: `submit_frame` runs on the render thread while `start` and
// `stop` run on the main thread.
unsafe impl Send for Encoder {}

static ENCODER: Mutex<Option<Encoder>> = Mutex::new(None);

/// `codecType` for the output path: 2 for `rtp://`, 1 for `rtmp://`, otherwise 0 for a local file.
fn codec_type(output: &str) -> i32 {
    if output.starts_with("rtp://") {
        2
    } else if output.starts_with("rtmp://") {
        1
    } else {
        0
    }
}

/// Fallback output rate of the AAC track when we are not feeding app audio, matching the XREAL Unity
/// SDK's `RECORD_AUDIO_SAMPLERATE_DEFAULT` (its `monophonic` variant is 16000).
///
/// **The encoder does not resample.** Measured on the X4000: pushing 44100 Hz PCM while the config
/// said 48000 produced an audio track 0.914x the video's length, exactly 44100/48000. The rate
/// passed per push to `HWEncoderNotifyAudioData` labels the samples, but the *config* rate is what
/// the track is written at, so the two have to agree. The Unity SDK never hits this, because
/// Android runs Unity's mixer at 48000 too and its constant already matches.
const AUDIO_SAMPLE_RATE: i32 = 48_000;

/// Build the encoder config JSON, in the SDK format from `EncodeTypes.cs`. `with_mic` captures the
/// microphone natively, and `with_internal` makes it capture app audio too, which needs a
/// MediaProjection. `with_alpha` sets `useAlpha`, and the encoder then packs the frame's RGB and
/// alpha top-and-bottom for the ObserverView MRC composite; the input texture has to carry a real
/// alpha channel, meaning a transparent-background viewport. See
/// docs/develop/plans/observer-view-notes.md.
#[allow(clippy::too_many_arguments)]
fn config_json(
    output: &str,
    width: i32,
    height: i32,
    bitrate: i32,
    fps: i32,
    with_mic: bool,
    with_internal: bool,
    with_alpha: bool,
    audio_rate: Option<i32>,
) -> String {
    // Escape the path or URL for JSON, since a `"` or `\` in `output` would otherwise break the
    // config. codec_type() still sees the raw string, and it matches on the scheme and extension, so
    // escaping does not affect it.
    let output_json = output.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        concat!(
            "{{\"width\":{},\"height\":{},\"bitRate\":{},\"fps\":{},\"codecType\":{},",
            "\"outPutPath\":\"{}\",\"useStepTime\":0,\"useAlpha\":{},\"useLinnerTexture\":true,",
            "\"addMicphoneAudio\":{},\"addInternalAudio\":{},\"audioSampleRate\":{},",
            "\"audioBitRate\":128000}}"
        ),
        width,
        height,
        bitrate,
        fps,
        codec_type(output),
        output_json,
        with_alpha,
        with_mic,
        with_internal,
        audio_rate.unwrap_or(AUDIO_SAMPLE_RATE)
    )
}

/// Whether a stream is currently active (either the GL-path encoder or any non-idle state of
/// the Vulkan lifecycle, StartPending and StopPending included, so mutual exclusion and the
/// recorder's finalize-wait both observe the truth).
pub fn is_active() -> bool {
    ENCODER.lock().expect("encoder mutex").is_some() || vk_is_active()
}

/// The encoder configuration, owned by the stage-4 Vulkan state machine while a start is
/// pending (the tick thread performs the actual native start; see [`vk_tick`]).
#[derive(Clone)]
struct EncoderConfig {
    output: String,
    width: i32,
    height: i32,
    bitrate: i32,
    fps: i32,
    with_mic: bool,
    with_internal: bool,
    with_alpha: bool,
    audio_rate: Option<i32>,
    /// Whether to inject the periodic-IDR workaround; resolved on the main thread in [`start`].
    idr_hack: bool,
}

/// Resolve the periodic-IDR workaround setting, MAIN THREAD ONLY (it reads `ProjectSettings`).
/// Priority: the `debug.xreal.idr_hack` system property overrides at runtime (0/1); otherwise the
/// `xreal/idr_workaround` ProjectSetting; otherwise ON by default. See
/// docs/develop/archive/codex-idr-analysis.md.
fn resolve_idr_hack() -> bool {
    if let Some(v) = crate::session::android_prop_i32(b"debug.xreal.idr_hack\0") {
        return v == 1;
    }
    use godot::classes::ProjectSettings;
    use godot::obj::Singleton;
    let ps = ProjectSettings::singleton();
    if ps.has_setting("xreal/idr_workaround") {
        ps.get_setting_with_override("xreal/idr_workaround")
            .try_to::<bool>()
            .unwrap_or(true)
    } else {
        true
    }
}

/// Start streaming the FPV to `output`, which is an `rtp://ip:port`, an `rtmp://…` or a local file
/// path. It creates, configures and starts the HW encoder, and returns `false` on any failure,
/// whether a missing library or symbol or a non-zero `HWEncoder*` status. Feed frames with
/// [`submit_frame`] from the render thread.
///
/// Under the Vulkan renderer with the stage-2 bridge active, this instead QUEUES an asynchronous
/// start: every `HWEncoder*` lifecycle call must run on the Vulkan tick with the private EGL
/// context bound (the context whose share group carries the bundle GL names; where the encoder's
/// worker context latches on is unknown, so all of them go there). `true` then means "accepted";
/// the next tick starts the encoder and a failure lands in the `Failed` state, observable through
/// [`is_active`] going false.
#[allow(clippy::too_many_arguments)]
pub fn start(
    output: &str,
    width: i32,
    height: i32,
    bitrate: i32,
    fps: i32,
    with_mic: bool,
    with_internal: bool,
    with_alpha: bool,
    audio_rate: Option<i32>,
) -> bool {
    // Resolve the IDR workaround here, on the main thread (stream_start is a #[func]): the Vulkan
    // path's native start runs on the render tick, where ProjectSettings must not be read.
    let config = EncoderConfig {
        output: output.to_string(),
        width,
        height,
        bitrate,
        fps,
        with_mic,
        with_internal,
        with_alpha,
        audio_rate,
        idr_hack: resolve_idr_hack(),
    };
    if !crate::gl::renderer_is_gl() {
        // The stage-4 Vulkan path needs the bridge machinery (the tick, the private EGL context,
        // the opaque-fd bundles), but NOT the glasses kill switch: ensure_init brings the bridge
        // up on demand, so the encoder works in encoder-only mode too (glasses rendering off).
        if !crate::vk_bridge::ensure_init() {
            godot::global::godot_warn!(
                "[xreal] FPV encoder unavailable: Vulkan bridge failed to initialize"
            );
            return false;
        }
        return vk_request_start(config);
    }
    let mut guard = ENCODER.lock().expect("encoder mutex");
    if guard.is_some() {
        return true; // already streaming
    }
    match unsafe { start_native(&config) } {
        Some(enc) => {
            *guard = Some(enc);
            true
        }
        None => false,
    }
}

/// The native open-configure-start flow, shared by the GL path (called inline on the main
/// thread) and the Vulkan tick. Returns the live encoder or `None` with the reason logged.
unsafe fn start_native(config: &EncoderConfig) -> Option<Encoder> {
    let EncoderConfig {
        output,
        width,
        height,
        bitrate,
        fps,
        with_mic,
        with_internal,
        with_alpha,
        audio_rate,
        idr_hack: _, // read from `config` at the end, after `config.clone()` above moved the rest
    } = config.clone();
    let output = output.as_str();
    {
        let Ok(lib) = Library::new(MEDIA_CODEC_LIB) else {
            godot::global::godot_warn!("[xreal] dlopen {MEDIA_CODEC_LIB} failed");
            return None;
        };
        let create: FnCreate = match lib.get::<FnCreate>(b"HWEncoderCreate\0") {
            Ok(s) => *s,
            Err(_) => return None,
        };
        let set_config: FnSetConfig = match lib.get::<FnSetConfig>(b"HWEncoderSetConfigration\0") {
            Ok(s) => *s,
            Err(_) => return None,
        };
        let set_media_projection: FnSetMediaProjection =
            match lib.get::<FnSetMediaProjection>(b"HWEncoderSetMediaProjection\0") {
                Ok(s) => *s,
                Err(_) => return None,
            };
        let start_fn: FnStart = match lib.get::<FnStart>(b"HWEncoderStart\0") {
            Ok(s) => *s,
            Err(_) => return None,
        };
        let update_surface: FnUpdateSurface =
            match lib.get::<FnUpdateSurface>(b"HWEncoderUpdateSurface\0") {
                Ok(s) => *s,
                Err(_) => return None,
            };
        let stop: FnStop = match lib.get::<FnStop>(b"HWEncoderStop\0") {
            Ok(s) => *s,
            Err(_) => return None,
        };
        let destroy: FnDestroy = match lib.get::<FnDestroy>(b"HWEncoderDestroy\0") {
            Ok(s) => *s,
            Err(_) => return None,
        };

        let mut handle: u64 = 0;
        if create(&mut handle) != 0 || handle == 0 {
            godot::global::godot_warn!("[xreal] HWEncoderCreate failed");
            return None;
        }
        let cfg = config_json(
            output,
            width,
            height,
            bitrate,
            fps,
            with_mic,
            with_internal,
            with_alpha,
            audio_rate,
        );
        let Ok(cfg_c) = CString::new(cfg.as_str()) else {
            destroy(handle);
            return None;
        };
        if set_config(handle, cfg_c.as_ptr()) != 0 {
            godot::global::godot_warn!("[xreal] HWEncoderSetConfigration failed: {cfg}");
            destroy(handle);
            return None;
        }
        // This must come right after the config, as the SDK does: without it HWEncoderStart dereferenced
        // a null field and hit a SIGSEGV. The projection is not decoration. With `addInternalAudio` the
        // encoder builds an AudioPlaybackCaptureConfiguration from it and opens its own AudioRecord for
        // app sound, then adds those blocks to the microphone's (docs/develop/archive/codex-audio-mix-
        // analysis.md). Null is still correct when app audio was not asked for, or consent was declined:
        // the capture simply does not start.
        let projection = if with_internal {
            crate::jni_bridge::media_projection_ptr()
        } else {
            std::ptr::null_mut()
        };
        if with_internal && projection.is_null() {
            godot::global::godot_warn!(
                "[xreal] app audio requested but no MediaProjection was granted - \
                 recording microphone only"
            );
        }
        let mp = set_media_projection(handle, projection);
        if mp != 0 {
            godot::global::godot_warn!(
                "[xreal] HWEncoderSetMediaProjection returned {mp} (continuing)"
            );
        }
        if start_fn(handle) != 0 {
            godot::global::godot_warn!("[xreal] HWEncoderStart failed");
            destroy(handle);
            return None;
        }
        godot::global::godot_print!(
            "[xreal] FPV stream started -> {output} ({width}x{height} @{fps} {bitrate}bps codecType={})",
            codec_type(output)
        );
        if config.idr_hack {
            IDR_COUNTER.store(0, Ordering::Relaxed);
            godot::global::godot_print!(
                "[xreal] IDR workaround enabled: request-sync every {IDR_PERIOD_FRAMES} frames"
            );
        }
        Some(Encoder {
            _lib: lib,
            update_surface,
            stop,
            destroy,
            handle,
            idr_hack: config.idr_hack,
        })
    }
}

/// Feed one frame. `gl_texture_id` is the GL texture name to encode, taken from
/// `RenderingServer.texture_get_native_handle` on the actual viewport color-texture RID that
/// `viewport_get_texture` returns, not on the `ViewportTexture` proxy RID, and `timestamp` is in
/// nanoseconds. **Render thread only.** It returns the encoder status: `0` for ok, `-1` when not
/// streaming, `-2` under the Vulkan renderer, whose components publish through
/// `stream_publish_viewport` instead (the integer here would be a `VkImage`, never a GL name).
pub fn submit_frame(gl_texture_id: usize, timestamp: u64) -> i32 {
    if !crate::gl::renderer_is_gl() {
        return -2;
    }
    let guard = ENCODER.lock().expect("encoder mutex");
    match guard.as_ref() {
        Some(enc) => {
            let status = unsafe { (enc.update_surface)(enc.handle, gl_texture_id, timestamp) };
            maybe_request_idr(enc.handle, enc.idr_hack);
            status
        }
        None => -1,
    }
}

/// Stop and destroy the encoder. Idempotent. Under Vulkan this only *requests* the stop; the
/// tick performs the native teardown, and [`is_active`] turns false when it is done (which is
/// what the recorder waits on before publishing the mp4).
pub fn stop() {
    if !crate::gl::renderer_is_gl() {
        vk_request_stop();
        return;
    }
    let mut guard = ENCODER.lock().expect("encoder mutex");
    if let Some(enc) = guard.take() {
        unsafe {
            (enc.stop)(enc.handle);
            (enc.destroy)(enc.handle);
        }
        godot::global::godot_print!("[xreal] FPV stream stopped");
    }
}

// ---------------------------------------------------------------------------------------------
// Stage-4 Vulkan lifecycle state machine. Every native HWEncoder* call runs on the Vulkan tick
// with the private EGL context bound: the encoder's worker context shares against SOME context
// it observes during its lifecycle (which call latches it is unknown, so all of them get the
// same one), and the bundle GL names only exist in the private context's share group. See
// docs/develop/archive/codex-vulkan-stage4-design.md.
// ---------------------------------------------------------------------------------------------

enum VkEncState {
    Idle,
    /// `stream_start` accepted; the next tick performs the native start.
    StartPending(EncoderConfig),
    Running(Encoder),
    /// `stop()` requested; the next tick stops + destroys, then releases the bundles.
    StopPending(Encoder),
    /// The native start failed; cleared by the next start request.
    Failed,
}

static VK_ENC: Mutex<VkEncState> = Mutex::new(VkEncState::Idle);

fn vk_request_start(config: EncoderConfig) -> bool {
    let mut st = VK_ENC.lock().expect("vk enc mutex");
    match *st {
        VkEncState::Idle | VkEncState::Failed => {
            godot::global::godot_print!(
                "[xreal] FPV encoder start queued (Vulkan tick will run it) -> {}",
                config.output
            );
            *st = VkEncState::StartPending(config);
            true
        }
        VkEncState::Running(_) | VkEncState::StartPending(_) => true, // already streaming
        VkEncState::StopPending(_) => false, // teardown in flight; retry next frame
    }
}

fn vk_request_stop() {
    let mut st = VK_ENC.lock().expect("vk enc mutex");
    *st = match std::mem::replace(&mut *st, VkEncState::Idle) {
        VkEncState::Running(enc) => VkEncState::StopPending(enc),
        VkEncState::StartPending(_) => VkEncState::Idle, // never started
        other => other,
    };
}

/// Whether the bridge should copy stream frames into the encoder bundles this tick.
pub fn vk_wants_frames() -> bool {
    matches!(
        *VK_ENC.lock().expect("vk enc mutex"),
        VkEncState::Running(_)
    )
}

/// The Vulkan-side "encoder busy" flag folded into [`is_active`].
fn vk_is_active() -> bool {
    !matches!(
        *VK_ENC.lock().expect("vk enc mutex"),
        VkEncState::Idle | VkEncState::Failed
    )
}

/// One encoder step on the Vulkan tick. MUST run with the private EGL context bound and after
/// `vk_bridge::wait_entry_fence()`, which is what proves the ready bundle's copy complete.
pub fn vk_tick() {
    let mut st = VK_ENC.lock().expect("vk enc mutex");
    match std::mem::replace(&mut *st, VkEncState::Idle) {
        VkEncState::Idle => {}
        VkEncState::Failed => *st = VkEncState::Failed,
        VkEncState::StartPending(config) => match unsafe { start_native(&config) } {
            Some(enc) => {
                godot::global::godot_print!("[xreal] FPV encoder started on the Vulkan tick");
                *st = VkEncState::Running(enc);
            }
            None => {
                godot::global::godot_warn!(
                    "[xreal] FPV encoder start FAILED on the Vulkan tick (state=Failed)"
                );
                *st = VkEncState::Failed;
            }
        },
        VkEncState::Running(enc) => {
            if let Some((gl_name, ts)) = crate::vk_bridge::encoder_take_ready() {
                let status = unsafe { (enc.update_surface)(enc.handle, gl_name as usize, ts) };
                let n = crate::vk_bridge::ENC_FED.fetch_add(1, Ordering::Relaxed);
                if n < 3 || n.is_multiple_of(300) || status != 0 {
                    godot::global::godot_print!(
                        "[xreal] vk encoder fed #{n}: gl={gl_name} ts={ts} status={status}"
                    );
                }
                // GL-side completion release before the bundle's next Vulkan reuse: external
                // memory sharing supplies no GL->Vulkan execution ordering by itself.
                crate::vk_bridge::gl_finish();
                maybe_request_idr(enc.handle, enc.idr_hack);
            }
            *st = VkEncState::Running(enc);
        }
        VkEncState::StopPending(enc) => {
            unsafe {
                (enc.stop)(enc.handle);
                (enc.destroy)(enc.handle);
            }
            crate::vk_bridge::encoder_release_bundles();
            godot::global::godot_print!("[xreal] FPV encoder stopped on the Vulkan tick");
            *st = VkEncState::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{codec_type, config_json};

    #[test]
    fn codec_type_from_url_scheme() {
        assert_eq!(codec_type("rtp://1.2.3.4:5000"), 2);
        assert_eq!(codec_type("rtmp://host/app"), 1);
        assert_eq!(codec_type("/sdcard/out.mp4"), 0);
        assert_eq!(codec_type("clip.mp4"), 0);
    }

    #[test]
    fn config_json_embeds_params_and_rtp_codec_type() {
        let j = config_json(
            "rtp://10.0.0.2:6000",
            1280,
            720,
            4_000_000,
            30,
            true,
            false,
            false,
            None,
        );
        for needle in [
            "\"width\":1280",
            "\"height\":720",
            "\"bitRate\":4000000",
            "\"fps\":30",
            "\"codecType\":2", // rtp -> 2
            "\"outPutPath\":\"rtp://10.0.0.2:6000\"",
            "\"addMicphoneAudio\":true",
            "\"addInternalAudio\":false",
            "\"audioSampleRate\":48000",
        ] {
            assert!(j.contains(needle), "missing {needle} in {j}");
        }
    }

    #[test]
    fn config_json_local_file_is_codec_type_zero() {
        let j = config_json(
            "/sdcard/clip.mp4",
            640,
            480,
            1_000_000,
            24,
            false,
            true,
            true,
            Some(44_100),
        );
        assert!(j.contains("\"codecType\":0"));
        assert!(j.contains("\"addMicphoneAudio\":false"));
        assert!(j.contains("\"addInternalAudio\":true"));
        assert!(j.contains("\"useAlpha\":true"));
        assert!(j.contains("\"audioSampleRate\":44100"));
    }
}
