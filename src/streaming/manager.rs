//! StreamManagerHandle, StreamCommand and StreamTask (real I/O via
//! ffmpeg-next).
//!
//! `StreamManagerHandle` exposes a simple API (open/close stream, shut down)
//! implemented over a command channel consumed by an orchestrator loop
//! (`run_manager_loop`). Each open stream runs its own `StreamTask`: a
//! dedicated thread (via `tokio::task::spawn_blocking`) that opens the RTSP
//! connection with `ffmpeg-next`, decodes packets, scales each frame down to
//! the display size requested by the UI, converts it to RGBA8 and delivers it
//! through its own `mpsc::Sender<CameraFrame>` channel. The connection status
//! is published separately, in a `watch::Sender<ConnectionStatus>`, which
//! always holds the latest value (Requirements 3.1, 3.2, 3.4, 3.5).
//!
//! Cooperative cancellation uses `tokio_util::sync::CancellationToken`: the
//! command loop keeps one token per stream (keyed by `rtsp_url`) and cancels it
//! when processing `RemoveStream`/`Shutdown` (Requirements 3.6, 6.6).

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::audio::AudioSink;
use crate::model::{CameraFrame, ConnectionStatus, DisplaySize};

use super::audio_decode::AudioDecoder;
use super::scale::output_size;
use super::state::{
    backoff_delay, next_connection_state, ConnectionEvent, DEFAULT_MAX_BACKOFF_DELAY,
};

/// Pause before retrying the read when FFmpeg answers `EAGAIN` (no packet
/// available right now), avoiding a busy loop on the camera thread.
const EAGAIN_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Environment variable that turns GPU decoding off when set to `0`, `false` or
/// `off` (useful to compare CPU usage or to work around a faulty driver).
pub const GPU_DECODING_ENV: &str = "RTSP_VIEWER_GPU_DECODE";

/// Video packets accepted by the GPU without producing any frame before giving
/// up on it. A camera sends a keyframe every few seconds; 150 packets (~7 to 10
/// s at 15–20 fps) is enough headroom for that.
const GPU_FIRST_FRAME_PACKET_LIMIT: u32 = 150;

/// Resampling filter of the pixel converter. Measured on 2560x1440 frames
/// (`scaler_benchmark` in `scale.rs`): bilinear keeps only 75% of the fine
/// detail when halving the size, bicubic 98%, for about 1.5 ms more per frame
/// (3.3 ms to 5.2 ms at 1440p to 1080p). It also interpolates the chroma
/// planes smoothly, which area averaging does not. The "accurate rounding"
/// and "full chroma interpolation" variants cost 4 to 5 times more for no
/// visible gain, so they are not used.
const SCALER_FLAGS: ffmpeg_next::software::scaling::Flags =
    ffmpeg_next::software::scaling::Flags::BICUBIC;

/// GPU decoding on, unless `GPU_DECODING_ENV` turns it off.
fn gpu_decoding_enabled() -> bool {
    !matches!(
        std::env::var(GPU_DECODING_ENV).as_deref().map(str::trim),
        Ok("0" | "false" | "off")
    )
}

/// Channels a `StreamTask` uses to talk to the UI.
pub struct StreamChannels {
    /// Decoded frames (dropped when the channel is full).
    pub frames: mpsc::Sender<CameraFrame>,
    /// Connection status; the `watch` always holds the latest value.
    pub status: watch::Sender<ConnectionStatus>,
    /// Maximum size at which the UI shows the video (`None` = source
    /// resolution). Read on every frame, so it follows window resizes.
    pub display_size: watch::Receiver<Option<DisplaySize>>,
    /// Where to play this stream's sound: `Some` while the camera is enlarged
    /// and not muted, `None` otherwise. With `None` the audio packets are
    /// dropped without being decoded.
    pub audio: watch::Receiver<Option<AudioSink>>,
    /// Whether the stream has an audio track the app can decode. Published
    /// after each connection, for the UI to show the sound controls.
    pub has_audio: watch::Sender<bool>,
}

/// Command sent to the `Stream_Manager` orchestrator loop through the internal
/// channel of `StreamManagerHandle`.
pub enum StreamCommand {
    /// Opens `rtsp_url` and starts decoding it, talking to the UI through
    /// `channels`.
    AddStream {
        rtsp_url: String,
        channels: StreamChannels,
    },
    /// Stops (without reconnecting) the `StreamTask` for `rtsp_url`, if any.
    RemoveStream { rtsp_url: String },
    /// Stops (without reconnecting) every running `StreamTask` and confirms
    /// through `done` after signaling the cancellation of all of them.
    Shutdown { done: oneshot::Sender<()> },
}

/// Handle of the `Stream_Manager`, used by the UI to open/close streams and
/// shut them all down. Cheap to clone: internally it is just the sending side
/// of the orchestrator loop's command channel.
///
/// The command channel is unbounded, so sends are synchronous and reach the
/// loop in exactly the order the UI made them. This matters when switching a
/// camera's stream (closing the substream and opening the main one): with
/// concurrent sends, a quick back-and-forth switch could leave a stream open
/// with nobody showing it.
#[derive(Clone)]
pub struct StreamManagerHandle {
    commands: mpsc::UnboundedSender<StreamCommand>,
}

impl StreamManagerHandle {
    /// Creates a new `Stream_Manager`, spawning its orchestrator loop as a
    /// background tokio task, and returns the handle used to send it commands.
    pub fn new() -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        tokio::spawn(run_manager_loop(receiver));
        StreamManagerHandle { commands }
    }

    /// Requests `rtsp_url` to be opened. The decoded frames, already scaled
    /// down to the display size, and every connection status change are
    /// delivered through `channels` (Requirement 3.1). If there is already a
    /// `StreamTask` for the same URL, it is replaced.
    pub fn add_stream(&self, rtsp_url: String, channels: StreamChannels) {
        let _ = self
            .commands
            .send(StreamCommand::AddStream { rtsp_url, channels });
    }

    /// Requests the shutdown (without reconnecting) of the `StreamTask` of
    /// `rtsp_url`, releasing its resources (Requirements 3.6, 6.6).
    pub fn remove_stream(&self, rtsp_url: String) {
        let _ = self.commands.send(StreamCommand::RemoveStream { rtsp_url });
    }

    /// Requests the shutdown (without reconnecting) of every running
    /// `StreamTask`, releasing its resources (Requirement 3.6).
    ///
    /// Only returns after the orchestrator loop has signaled the cancellation
    /// of every `StreamTask`. Without that confirmation, the caller could shut
    /// down the tokio runtime before the loop processed the command: the loop
    /// would be dropped without cancelling anything and the camera threads
    /// would keep running.
    pub async fn shutdown(&self) {
        let (done, confirmed) = oneshot::channel();
        if self.commands.send(StreamCommand::Shutdown { done }).is_ok() {
            // An error here means the loop already ended: there is nothing to
            // wait for.
            let _ = confirmed.await;
        }
    }
}

impl Default for StreamManagerHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Orchestrator loop of the `Stream_Manager`: consumes `StreamCommand`s and keeps
/// one `CancellationToken` per active stream (keyed by `rtsp_url`) so it can stop
/// `StreamTask`s individually (`RemoveStream`) or all at once (`Shutdown`).
async fn run_manager_loop(mut commands: mpsc::UnboundedReceiver<StreamCommand>) {
    let mut tasks: HashMap<String, CancellationToken> = HashMap::new();

    while let Some(command) = commands.recv().await {
        match command {
            StreamCommand::AddStream { rtsp_url, channels } => {
                // A new add of the same URL replaces (cancels) the previous StreamTask,
                // if any, avoiding two concurrent tasks for the same stream.
                if let Some(previous_token) = tasks.remove(&rtsp_url) {
                    previous_token.cancel();
                }

                let token = CancellationToken::new();
                tasks.insert(rtsp_url.clone(), token.clone());

                tokio::task::spawn_blocking(move || run_stream_task(rtsp_url, channels, token));
            }
            StreamCommand::RemoveStream { rtsp_url } => {
                if let Some(token) = tasks.remove(&rtsp_url) {
                    token.cancel();
                }
            }
            StreamCommand::Shutdown { done } => {
                for (_, token) in tasks.drain() {
                    token.cancel();
                }
                let _ = done.send(());
                break;
            }
        }
    }

    // If the command channel closes without an explicit `Shutdown` (all handles
    // dropped), the remaining StreamTasks are cancelled too, instead of running
    // on with nobody left to stop them.
    for (_, token) in tasks.drain() {
        token.cancel();
    }
}

/// Body of the `StreamTask` of a single stream, run on a dedicated thread via
/// `spawn_blocking` (Requirement 3.5). Opens the RTSP connection, decodes frames
/// and delivers them via `channels.frames`, publishing every status change to
/// `channels.status`; on failure, applies the reconnection state machine
/// (`next_connection_state`/`backoff_delay`) and retries indefinitely unless
/// `cancel_token` is cancelled (Requirements 3.2, 3.3, 6.1, 6.2, 6.3, 6.4, 6.5).
fn run_stream_task(rtsp_url: String, channels: StreamChannels, cancel_token: CancellationToken) {
    let mut status = ConnectionStatus::Disconnected;
    let mut attempt: u32 = 0;
    // Starts by trying to decode on the GPU; `decode_stream` turns the GPU off
    // for the next attempts of this stream if it fails.
    let mut use_gpu = gpu_decoding_enabled();

    loop {
        if cancel_token.is_cancelled() {
            return;
        }

        match decode_stream(
            &rtsp_url,
            &channels,
            &cancel_token,
            &mut status,
            &mut attempt,
            &mut use_gpu,
        ) {
            StopReason::Cancelled => {
                // Shutdown requested (removal/shutdown): no transition to
                // Reconnecting and no new attempt scheduled (Requirements 3.6,
                // 6.6).
                status = next_connection_state(status, ConnectionEvent::UserRequestedStop);
                channels.status.send_replace(status);
                return;
            }
            StopReason::Failure => {
                status = next_connection_state(status, ConnectionEvent::DecodeFailure);
                channels.status.send_replace(status);

                let delay = backoff_delay(attempt, DEFAULT_MAX_BACKOFF_DELAY);
                attempt = attempt.saturating_add(1);

                if wait_or_cancelled(delay, &cancel_token) {
                    return;
                }
            }
        }
    }
}

/// Replaces the "yuvj*" formats with the equivalent "yuv*" one and tells
/// whether the video uses the full value range (0–255, "JPEG") instead of the
/// limited one (16–235).
///
/// The "yuvj*" formats are the old, deprecated way of marking full-range YUV.
/// swscale still accepts them, but every converter it creates prints
/// "deprecated pixel format used, make sure you did set range correctly" to the
/// console. With the current format and the range set explicitly
/// (`set_full_range_source`), the conversion is the same and the warning goes
/// away. "yuv*" frames marked as full range are also converted with the right
/// range.
fn normalize_source_format(
    format: ffmpeg_next::format::Pixel,
    range: ffmpeg_next::util::color::Range,
) -> (ffmpeg_next::format::Pixel, bool) {
    use ffmpeg_next::format::Pixel;

    match format {
        Pixel::YUVJ420P => (Pixel::YUV420P, true),
        Pixel::YUVJ422P => (Pixel::YUV422P, true),
        Pixel::YUVJ444P => (Pixel::YUV444P, true),
        Pixel::YUVJ440P => (Pixel::YUV440P, true),
        Pixel::YUVJ411P => (Pixel::YUV411P, true),
        other => (other, range == ffmpeg_next::util::color::Range::JPEG),
    }
}

/// Configures the converter to treat the source as full-range YUV, keeping the
/// other color parameters swscale chose when creating it.
fn set_full_range_source(scaler: &mut ffmpeg_next::software::scaling::Context) {
    use std::ffi::c_int;

    let mut inv_table: *mut c_int = std::ptr::null_mut();
    let mut table: *mut c_int = std::ptr::null_mut();
    let (mut src_range, mut dst_range): (c_int, c_int) = (0, 0);
    let (mut brightness, mut contrast, mut saturation): (c_int, c_int, c_int) = (0, 0, 0);

    // SAFETY: the pointer comes from a valid `SwsContext`, owned by `scaler`
    // for the whole call; the tables read by `sws_getColorspaceDetails` belong
    // to the context itself and are passed back unchanged.
    unsafe {
        let ctx = scaler.as_mut_ptr();
        let read = ffmpeg_next::ffi::sws_getColorspaceDetails(
            ctx,
            &mut inv_table,
            &mut src_range,
            &mut table,
            &mut dst_range,
            &mut brightness,
            &mut contrast,
            &mut saturation,
        );
        if read >= 0 {
            ffmpeg_next::ffi::sws_setColorspaceDetails(
                ctx, inv_table, 1, table, dst_range, brightness, contrast, saturation,
            );
        }
    }
}

/// Configuration a pixel converter (`sws_scale`) was created for. When any
/// field changes, the converter must be recreated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScalerConfig {
    /// Source format, already normalized (no "yuvj*").
    source_format: ffmpeg_next::format::Pixel,
    /// Full-range YUV source (0–255).
    full_range: bool,
    source_width: u32,
    source_height: u32,
    out_width: u32,
    out_height: u32,
}

/// Why the decoding loop of a connection attempt exited.
enum StopReason {
    /// `cancel_token` was cancelled during decoding.
    Cancelled,
    /// The connection/decoding failed (network, timeout, decoding error, stream EOF).
    Failure,
}

/// Waits `delay`, returning `true` right away (without waiting the full interval) if
/// `cancel_token` is cancelled in the meantime. Uses `std::thread::sleep` in small
/// steps since this function runs inside a `spawn_blocking` thread.
fn wait_or_cancelled(delay: Duration, cancel_token: &CancellationToken) -> bool {
    const POLL_INTERVAL: Duration = Duration::from_millis(100);
    let mut remaining = delay;

    while remaining > Duration::ZERO {
        if cancel_token.is_cancelled() {
            return true;
        }
        let step = remaining.min(POLL_INTERVAL);
        std::thread::sleep(step);
        remaining -= step;
    }

    cancel_token.is_cancelled()
}

/// Opens the RTSP connection via `ffmpeg-next` and decodes frames until a failure
/// or cancellation. Each decoded frame is scaled down to the display size
/// requested by the UI (`channels.display_size`), converted to RGBA8 and
/// delivered via `channels.frames.try_send` (silently dropped when the channel is
/// full, see the note below); when the first frame is decoded successfully, moves
/// `status` to Connected, publishes it to `channels.status` and resets `attempt`
/// (Requirement 6.4).
///
/// With `use_gpu`, decoding goes to the GPU (D3D11VA) when FFmpeg and the driver
/// support the video; otherwise FFmpeg itself carries on with the CPU. If the GPU
/// fails before the first frame, `use_gpu` is turned off and the next attempts of
/// this stream use only the CPU.
///
/// The status does not go through the frame channel: if it shared a small channel
/// with frames using `try_send`, a status change that happened while the channel
/// was full would be dropped and the UI would show the wrong indicator until the
/// next transition. The `watch` always holds the latest value, regardless of free
/// space.
///
/// Note on the drop policy (Requirement 3.2, design.md): the
/// `mpsc::Sender<CameraFrame>` channel is created by the UI with a small capacity
/// (a few slots). When `try_send` fails because it is full, the frame that was
/// just decoded is dropped (not the ones already pending in the channel). In
/// practice, since the UI drains the channel on every render frame (~60Hz) and
/// the capacity is small, the oldest pending frame is consumed by the UI before
/// the next send attempt, so the observed effect is equivalent to always keeping
/// the latest frame available without piling up memory.
fn decode_stream(
    rtsp_url: &str,
    channels: &StreamChannels,
    cancel_token: &CancellationToken,
    status: &mut ConnectionStatus,
    attempt: &mut u32,
    use_gpu: &mut bool,
) -> StopReason {
    // URL without credentials for logging (avoids leaking the username/password
    // embedded in the RTSP_URL in plain text).
    let safe_url = crate::model::redact_rtsp_credentials(rtsp_url);

    // Demuxer options: forces RTSP over TCP (more tolerant of packet loss) and
    // sets a socket timeout (`stimeout`, in microseconds). Without that timeout,
    // a network connection that hangs (half-open socket) would keep
    // `av_read_frame` blocked indefinitely, preventing both automatic
    // reconnection (Requirement 6.1) and cooperative shutdown via
    // `CancellationToken` (Requirements 3.6, 6.6), since cancellation is only
    // checked between packets.
    let mut options = ffmpeg_next::Dictionary::new();
    options.set("rtsp_transport", "tcp");
    // Socket timeout in microseconds. The option name changed between FFmpeg
    // versions (`stimeout` up to 4.x/5.x, `timeout` from 6.x/7.x on for the RTSP
    // demuxer); setting both guarantees the effect regardless of the linked libav
    // version. Unrecognized options are simply ignored.
    options.set("stimeout", "5000000");
    options.set("timeout", "5000000");

    let mut input_ctx = match ffmpeg_next::format::input_with_dictionary(rtsp_url, options) {
        Ok(ctx) => ctx,
        Err(err) => {
            tracing::error!(rtsp_url = %safe_url, error = %err, "failed to open the RTSP stream");
            return StopReason::Failure;
        }
    };

    let video_stream_index = {
        let stream = match input_ctx.streams().best(ffmpeg_next::media::Type::Video) {
            Some(stream) => stream,
            None => {
                tracing::error!(rtsp_url = %safe_url, "no video stream found");
                return StopReason::Failure;
            }
        };
        stream.index()
    };

    let mut gpu_attached = false;
    let mut decoder = {
        let stream = input_ctx
            .stream(video_stream_index)
            .expect("valid stream index");
        let mut context_decoder = match ffmpeg_next::codec::context::Context::from_parameters(
            stream.parameters(),
        ) {
            Ok(ctx) => ctx,
            Err(err) => {
                tracing::error!(rtsp_url = %safe_url, error = %err, "failed to create the decoding context");
                return StopReason::Failure;
            }
        };
        if *use_gpu {
            gpu_attached = attach_gpu_device(&mut context_decoder);
            if !gpu_attached {
                // No D3D11 device (Windows without support, driver or another
                // system): no point in trying again on every reconnection.
                tracing::warn!(rtsp_url = %safe_url, "GPU unavailable for decoding; using the CPU");
                *use_gpu = false;
            }
        }
        match context_decoder.decoder().video() {
            Ok(decoder) => decoder,
            Err(err) => {
                tracing::error!(rtsp_url = %safe_url, error = %err, "failed to open the video decoder");
                return StopReason::Failure;
            }
        }
    };

    // The audio track, if the session has one we can decode. Its packets are
    // read together with the video; they are only decoded while the UI hands
    // this stream a sink (the camera is enlarged and not muted).
    let audio_stream = input_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Audio)
        .map(|stream| (stream.index(), stream.parameters()));
    let audio_index = audio_stream.as_ref().map(|(index, _)| *index);
    let mut audio = audio_stream
        .as_ref()
        .and_then(|(_, parameters)| AudioDecoder::new(parameters));
    channels.has_audio.send_replace(audio.is_some());

    // Current pixel converter and the configuration it was created for: source
    // format and size, output size.
    let mut scaler: Option<(ScalerConfig, ffmpeg_next::software::scaling::Context)> = None;
    // Decoded frame and, when it is in GPU memory, its copy in system memory.
    // Reused for every frame.
    let mut decoded = ffmpeg_next::util::frame::video::Video::empty();
    let mut downloaded = ffmpeg_next::util::frame::video::Video::empty();
    let mut decoded_any = false;
    let mut packets_without_frame: u32 = 0;

    loop {
        if cancel_token.is_cancelled() {
            return StopReason::Cancelled;
        }

        // Manual read instead of `input_ctx.packets()`: the ffmpeg-next
        // iterator only ends on EOF and discards any other error (`Err(..) =>
        // ()`), retrying the read indefinitely. So a socket timeout or I/O
        // error would never get here: automatic reconnection (Requirement 6.1)
        // would not fire and `cancel_token` would no longer be checked
        // (Requirements 3.6, 6.6). A new `Packet` on every read, as the
        // iterator itself does, makes sure the previous packet's buffer is
        // released by `Drop` (`av_packet_unref`).
        let mut packet = ffmpeg_next::Packet::empty();
        match packet.read(&mut input_ctx) {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Eof) => {
                // The server ended the stream or the connection: treated as a
                // failure to trigger automatic reconnection (Requirement 6.1).
                tracing::warn!(rtsp_url = %safe_url, "RTSP stream ended (EOF)");
                return StopReason::Failure;
            }
            Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
                // No packet available right now, without a connection failure:
                // brief pause so the CPU is not kept busy, then try again
                // (cancellation is checked at the start of the next iteration).
                std::thread::sleep(EAGAIN_RETRY_DELAY);
                continue;
            }
            Err(err) => {
                tracing::error!(rtsp_url = %safe_url, error = %err, "failed to read a packet from the RTSP stream");
                return StopReason::Failure;
            }
        }

        if Some(packet.stream()) == audio_index {
            if let Some(audio) = audio.as_mut() {
                // Cloning the `Option<AudioSink>` is just an `Arc` clone; the
                // borrow of the watch value must not outlive the call.
                let sink = channels.audio.borrow().clone();
                audio.process(&packet, sink.as_ref());
            }
            continue;
        }

        if packet.stream() != video_stream_index {
            continue;
        }

        if let Err(err) = decoder.send_packet(&packet) {
            if gpu_attached && !decoded_any {
                tracing::warn!(rtsp_url = %safe_url, "the GPU refused the video; the next attempts will use the CPU");
                *use_gpu = false;
            }
            tracing::error!(rtsp_url = %safe_url, error = %err, "failed to send a packet to the decoder");
            return StopReason::Failure;
        }

        while decoder.receive_frame(&mut decoded).is_ok() {
            if cancel_token.is_cancelled() {
                return StopReason::Cancelled;
            }

            if !decoded_any {
                decoded_any = true;
                let on_gpu = is_hardware_frame(&decoded);
                tracing::info!(
                    rtsp_url = %safe_url,
                    decoder = if on_gpu { "GPU (D3D11VA)" } else { "CPU" },
                    "first frame decoded"
                );
            }

            if *status != ConnectionStatus::Connected {
                *status = next_connection_state(*status, ConnectionEvent::FirstFrameDecoded);
                *attempt = 0;
                channels.status.send_replace(*status);
            }

            // Channel full: the UI has not shown the previous frame yet. This
            // one is dropped right here, before the GPU download and the RGBA
            // conversion, which are the most expensive work after decoding.
            if channels.frames.capacity() == 0 {
                continue;
            }

            let source = if is_hardware_frame(&decoded) {
                if !download_hardware_frame(&decoded, &mut downloaded) {
                    tracing::warn!(rtsp_url = %safe_url, "failed to copy the frame from the GPU; the next attempts will use the CPU");
                    *use_gpu = false;
                    return StopReason::Failure;
                }
                &downloaded
            } else {
                &decoded
            };

            // Scales the frame down to the size the UI will show it at during
            // the RGBA conversion: converting and copying a 2560x1440 frame to
            // show it in a 600x340 tile would waste ~94% of the work.
            let (out_width, out_height) = output_size(
                source.width(),
                source.height(),
                *channels.display_size.borrow(),
            );
            // The color range comes from the decoded frame: the GPU copy does
            // not carry that metadata.
            let (source_format, full_range) =
                normalize_source_format(source.format(), decoded.color_range());
            let config = ScalerConfig {
                source_format,
                full_range,
                source_width: source.width(),
                source_height: source.height(),
                out_width,
                out_height,
            };

            // Recreates the converter when the configuration changes: the UI
            // asked for another size (window resized, camera enlarged) or the
            // camera itself changed resolution.
            if scaler.as_ref().map(|(current, _)| *current) != Some(config) {
                match ffmpeg_next::software::scaling::Context::get(
                    config.source_format,
                    config.source_width,
                    config.source_height,
                    ffmpeg_next::format::Pixel::RGBA,
                    config.out_width,
                    config.out_height,
                    SCALER_FLAGS,
                ) {
                    Ok(mut new_scaler) => {
                        if config.full_range {
                            set_full_range_source(&mut new_scaler);
                        }
                        scaler = Some((config, new_scaler));
                    }
                    Err(err) => {
                        tracing::error!(rtsp_url = %safe_url, error = %err, "failed to create the pixel converter");
                        return StopReason::Failure;
                    }
                }
            }
            let (_, scaler_ctx) = scaler.as_mut().expect("converter created right above");

            let Some(image) = scale_into_image(scaler_ctx, source, out_width, out_height) else {
                tracing::error!(rtsp_url = %safe_url, "failed to convert the frame to RGBA8");
                return StopReason::Failure;
            };

            // Drop policy for the oldest frame: see the note in this function's docs.
            let _ = channels.frames.try_send(image);
        }

        // The GPU accepts the packets but returns no frames (a profile or
        // resolution the driver does not decode): reconnect using the CPU.
        if !decoded_any {
            packets_without_frame += 1;
            if gpu_attached && packets_without_frame > GPU_FIRST_FRAME_PACKET_LIMIT {
                tracing::warn!(rtsp_url = %safe_url, "the GPU produced no frames; the next attempts will use the CPU");
                *use_gpu = false;
                return StopReason::Failure;
            }
        }
    }
}

/// Turns on GPU decoding in the decoder context, not opened yet, by creating a
/// D3D11VA device. With the device present, FFmpeg picks the GPU format on its
/// own when the video is supported and falls back to the CPU when it is not.
/// Returns `false` if the device could not be created.
fn attach_gpu_device(context: &mut ffmpeg_next::codec::context::Context) -> bool {
    use ffmpeg_next::ffi;

    let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
    // SAFETY: `context` is a valid `AVCodecContext` that is not open yet. The
    // reference created by `av_hwdevice_ctx_create` becomes owned by the
    // context, which releases it in `avcodec_free_context`.
    unsafe {
        let created = ffi::av_hwdevice_ctx_create(
            &mut device,
            ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        );
        if created < 0 || device.is_null() {
            return false;
        }
        (*context.as_mut_ptr()).hw_device_ctx = device;
    }
    true
}

/// The frame is in GPU memory (and must be copied before converting).
fn is_hardware_frame(frame: &ffmpeg_next::util::frame::video::Video) -> bool {
    // SAFETY: reads a field of a valid `AVFrame`.
    unsafe { !(*frame.as_ptr()).hw_frames_ctx.is_null() }
}

/// Copies a frame from GPU memory to `target`, in system memory (usually in
/// NV12), reusing `target` across frames.
fn download_hardware_frame(
    hardware: &ffmpeg_next::util::frame::video::Video,
    target: &mut ffmpeg_next::util::frame::video::Video,
) -> bool {
    // SAFETY: two valid `AVFrame`s; `av_frame_unref` releases the previous
    // content of `target` before `av_hwframe_transfer_data` allocates the new
    // one.
    unsafe {
        ffmpeg_next::ffi::av_frame_unref(target.as_mut_ptr());
        ffmpeg_next::ffi::av_hwframe_transfer_data(target.as_mut_ptr(), hardware.as_ptr(), 0) >= 0
    }
}

/// Converts `frame` to RGBA at `width`x`height`, writing straight into the egui
/// image buffer: no intermediate RGBA frame, no copy to strip the row padding
/// and no pixel conversion on the UI thread.
fn scale_into_image(
    scaler: &mut ffmpeg_next::software::scaling::Context,
    frame: &ffmpeg_next::util::frame::video::Video,
    width: u32,
    height: u32,
) -> Option<CameraFrame> {
    use std::ffi::c_int;

    let (columns, rows) = (width as usize, height as usize);
    let pixel_count = columns.checked_mul(rows)?;
    let mut pixels: Vec<egui::Color32> = Vec::with_capacity(pixel_count);
    let row_bytes = c_int::try_from(columns.checked_mul(4)?).ok()?;
    let expected_rows = c_int::try_from(rows).ok()?;

    let destination: [*mut u8; 4] = [
        pixels.as_mut_ptr().cast::<u8>(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    ];
    let destination_stride: [c_int; 4] = [row_bytes, 0, 0, 0];

    // SAFETY: the converter was created for the format and size of `frame` and
    // for RGBA output of `width`x`height` (see `ScalerConfig`). The destination
    // has capacity for exactly `width * height` 4-byte pixels (`Color32` is
    // `[u8; 4]` in RGBA order), with rows of `width * 4` bytes.
    let written_rows = unsafe {
        let source = frame.as_ptr();
        ffmpeg_next::ffi::sws_scale(
            scaler.as_mut_ptr(),
            (*source).data.as_ptr().cast::<*const u8>(),
            (*source).linesize.as_ptr(),
            0,
            (*source).height,
            destination.as_ptr(),
            destination_stride.as_ptr(),
        )
    };
    if written_rows != expected_rows {
        return None;
    }

    // SAFETY: the `sws_scale` above wrote all `rows` complete rows,
    // initializing the `pixel_count` pixels. RGBA output from a YUV source has
    // alpha 255, so the pixels are already in `Color32`'s premultiplied format.
    unsafe { pixels.set_len(pixel_count) };
    Some(egui::ColorImage {
        size: [columns, rows],
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Address with no RTSP server listening: the connection is refused and the
    /// `StreamTask` enters the reconnection cycle. Uses real FFmpeg, but only
    /// the loopback interface.
    const UNREACHABLE_RTSP_URL: &str = "rtsp://127.0.0.1:1/test";

    /// Generous timeout: on Windows, a refused connection on localhost takes
    /// about 2s because of SYN retransmissions.
    const STATUS_TIMEOUT: Duration = Duration::from_secs(15);

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().expect("failed to create the test runtime")
    }

    /// Frame channel already full, simulating a UI that did not drain it in
    /// time.
    fn full_frame_channel() -> (mpsc::Sender<CameraFrame>, mpsc::Receiver<CameraFrame>) {
        let (frame_tx, frame_rx) = mpsc::channel(1);
        frame_tx
            .try_send(CameraFrame::new([1, 1], egui::Color32::BLACK))
            .expect("a newly created channel has room for one frame");
        (frame_tx, frame_rx)
    }

    /// Channels of a test `StreamTask`, plus the ends the test uses to watch
    /// the status. The display size `Sender` is returned so it stays alive
    /// during the test.
    fn test_channels(
        frames: mpsc::Sender<CameraFrame>,
    ) -> (
        StreamChannels,
        watch::Receiver<ConnectionStatus>,
        watch::Sender<Option<DisplaySize>>,
    ) {
        let (status_tx, status_rx) = watch::channel(ConnectionStatus::Disconnected);
        let (display_tx, display_rx) = watch::channel(None);
        // No sound in these tests: the audio sender can go away, the receiver
        // keeps returning `None`.
        let (_audio_tx, audio_rx) = watch::channel(None);
        let (has_audio_tx, _has_audio_rx) = watch::channel(false);
        let channels = StreamChannels {
            frames,
            status: status_tx,
            display_size: display_rx,
            audio: audio_rx,
            has_audio: has_audio_tx,
        };
        (channels, status_rx, display_tx)
    }

    /// Waits for the `StreamTask` to publish the first status (the connection
    /// failure).
    async fn wait_first_status(status_rx: &mut watch::Receiver<ConnectionStatus>) {
        tokio::time::timeout(STATUS_TIMEOUT, status_rx.changed())
            .await
            .expect("the status should change after the connection failure")
            .expect("the StreamTask should still be alive");
    }

    /// Waits for the `StreamTask` to finish: when it exits, it drops the status
    /// `Sender` and `changed` starts returning an error.
    async fn task_finishes(status_rx: &mut watch::Receiver<ConnectionStatus>) -> bool {
        tokio::time::timeout(STATUS_TIMEOUT, async {
            while status_rx.changed().await.is_ok() {}
        })
        .await
        .is_ok()
    }

    #[test]
    fn deprecated_jpeg_formats_become_full_range_yuv() {
        use ffmpeg_next::format::Pixel;
        use ffmpeg_next::util::color::Range;

        assert_eq!(
            normalize_source_format(Pixel::YUVJ420P, Range::Unspecified),
            (Pixel::YUV420P, true)
        );
        assert_eq!(
            normalize_source_format(Pixel::YUVJ422P, Range::MPEG),
            (Pixel::YUV422P, true)
        );
    }

    #[test]
    fn regular_formats_keep_format_and_follow_the_frame_range() {
        use ffmpeg_next::format::Pixel;
        use ffmpeg_next::util::color::Range;

        assert_eq!(
            normalize_source_format(Pixel::YUV420P, Range::MPEG),
            (Pixel::YUV420P, false)
        );
        assert_eq!(
            normalize_source_format(Pixel::YUV420P, Range::Unspecified),
            (Pixel::YUV420P, false)
        );
        assert_eq!(
            normalize_source_format(Pixel::YUV420P, Range::JPEG),
            (Pixel::YUV420P, true)
        );
    }

    #[test]
    fn status_change_reaches_ui_even_with_full_frame_channel() {
        let runtime = test_runtime();
        runtime.block_on(async {
            let handle = StreamManagerHandle::new();
            // `_frame_rx` must stay alive so the channel stays open and full.
            let (frame_tx, _frame_rx) = full_frame_channel();
            let (channels, mut status_rx, _display_tx) = test_channels(frame_tx);

            handle.add_stream(UNREACHABLE_RTSP_URL.to_string(), channels);

            wait_first_status(&mut status_rx).await;
            assert_eq!(*status_rx.borrow(), ConnectionStatus::Reconnecting);

            handle.shutdown().await;
        });
        runtime.shutdown_timeout(Duration::from_secs(2));
    }

    #[test]
    fn shutdown_stops_stream_task_in_reconnect_cycle() {
        let runtime = test_runtime();
        runtime.block_on(async {
            let handle = StreamManagerHandle::new();
            let (frame_tx, _frame_rx) = mpsc::channel(1);
            let (channels, mut status_rx, _display_tx) = test_channels(frame_tx);

            handle.add_stream(UNREACHABLE_RTSP_URL.to_string(), channels);
            wait_first_status(&mut status_rx).await;

            handle.shutdown().await;

            assert!(
                task_finishes(&mut status_rx).await,
                "the StreamTask should finish after the shutdown"
            );
        });
        runtime.shutdown_timeout(Duration::from_secs(2));
    }

    #[test]
    fn remove_stream_stops_only_that_stream() {
        let runtime = test_runtime();
        runtime.block_on(async {
            let handle = StreamManagerHandle::new();
            let (removed_frames, _removed_frames_rx) = mpsc::channel(1);
            let (kept_frames, _kept_frames_rx) = mpsc::channel(1);
            let (removed, mut removed_status, _removed_display) = test_channels(removed_frames);
            let (kept, mut kept_status, _kept_display) = test_channels(kept_frames);

            // Two distinct URLs that do not respond, like the substream and the
            // main stream of the same camera.
            let removed_url = UNREACHABLE_RTSP_URL.to_string();
            let kept_url = format!("{UNREACHABLE_RTSP_URL}2");
            handle.add_stream(removed_url.clone(), removed);
            handle.add_stream(kept_url, kept);
            wait_first_status(&mut removed_status).await;
            wait_first_status(&mut kept_status).await;

            handle.remove_stream(removed_url);

            assert!(
                task_finishes(&mut removed_status).await,
                "the removed StreamTask should finish"
            );
            assert!(
                kept_status.has_changed().is_ok(),
                "the other StreamTask should stay alive"
            );

            handle.shutdown().await;
        });
        runtime.shutdown_timeout(Duration::from_secs(2));
    }
}
