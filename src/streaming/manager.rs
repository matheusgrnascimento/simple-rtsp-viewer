//! StreamManagerHandle, StreamCommand and StreamTask (real I/O via ffmpeg-next).
//!
//! `StreamManagerHandle` exposes a simple async API (add/remove camera, shut down)
//! implemented over a command channel consumed by an orchestrator loop
//! (`run_manager_loop`). Each added camera runs its own `StreamTask`: a dedicated
//! thread (via `tokio::task::spawn_blocking`) that opens the RTSP connection with
//! `ffmpeg-next`, decodes packets, converts each frame to RGBA8 and delivers the
//! result through the camera's own `mpsc::Sender<StreamEvent>` channel
//! (Requirements 3.1, 3.2, 3.4, 3.5).
//!
//! Cooperative cancellation uses `tokio_util::sync::CancellationToken`: the command
//! loop keeps one token per camera (keyed by `rtsp_url`) and cancels it when
//! processing `RemoveCamera`/`Shutdown` (Requirements 3.6, 6.6).

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::model::{Camera, CameraFrame, ConnectionStatus, StreamEvent};

use super::state::{
    backoff_delay, next_connection_state, ConnectionEvent, DEFAULT_MAX_BACKOFF_DELAY,
};

/// Capacity of the `StreamManagerHandle` command channel. Commands (add/remove
/// camera, shut down) are rare and always processed quickly by the orchestrator
/// loop, so a small capacity is enough.
const COMMAND_CHANNEL_CAPACITY: usize = 32;

/// Command sent to the `Stream_Manager` orchestrator loop through the internal
/// channel of `StreamManagerHandle`.
pub enum StreamCommand {
    /// Starts decoding `camera`, delivering `StreamEvent`s through `sender`.
    AddCamera {
        camera: Camera,
        sender: mpsc::Sender<StreamEvent>,
    },
    /// Stops (without reconnecting) the `StreamTask` for `rtsp_url`, if any.
    RemoveCamera { rtsp_url: String },
    /// Stops (without reconnecting) every running `StreamTask`.
    Shutdown,
}

/// Handle of the `Stream_Manager`, used by the UI to add/remove cameras and shut
/// down all streams. Cheap to clone: internally it is just an
/// `mpsc::Sender<StreamCommand>` to the orchestrator loop.
#[derive(Clone)]
pub struct StreamManagerHandle {
    commands: mpsc::Sender<StreamCommand>,
}

impl StreamManagerHandle {
    /// Creates a new `Stream_Manager`, spawning its orchestrator loop as a
    /// background tokio task, and returns the handle used to send it commands.
    pub fn new() -> Self {
        let (commands, receiver) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        tokio::spawn(run_manager_loop(receiver));
        StreamManagerHandle { commands }
    }

    /// Requests decoding of `camera` to start; the produced `StreamEvent`s
    /// (frames/status) are delivered through `sender` (Requirement 3.1).
    pub async fn add_camera(&self, camera: Camera, sender: mpsc::Sender<StreamEvent>) {
        let _ = self
            .commands
            .send(StreamCommand::AddCamera { camera, sender })
            .await;
    }

    /// Requests the shutdown (without reconnecting) of the `StreamTask` of the
    /// camera with the given `rtsp_url`, releasing its resources (Requirements
    /// 3.6, 6.6).
    ///
    /// Part of the Stream_Manager API planned in the design
    /// (`StreamCommand::RemoveCamera`); the UI does not remove cameras
    /// interactively yet at this stage, hence the `allow(dead_code)`.
    #[allow(dead_code)]
    pub async fn remove_camera(&self, rtsp_url: String) {
        let _ = self
            .commands
            .send(StreamCommand::RemoveCamera { rtsp_url })
            .await;
    }

    /// Requests the shutdown (without reconnecting) of every running
    /// `StreamTask`, releasing its resources (Requirement 3.6).
    pub async fn shutdown(&self) {
        let _ = self.commands.send(StreamCommand::Shutdown).await;
    }
}

impl Default for StreamManagerHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Orchestrator loop of the `Stream_Manager`: consumes `StreamCommand`s and keeps
/// one `CancellationToken` per active camera (keyed by `rtsp_url`) so it can stop
/// `StreamTask`s individually (`RemoveCamera`) or all at once (`Shutdown`).
async fn run_manager_loop(mut commands: mpsc::Receiver<StreamCommand>) {
    let mut tasks: HashMap<String, CancellationToken> = HashMap::new();

    while let Some(command) = commands.recv().await {
        match command {
            StreamCommand::AddCamera { camera, sender } => {
                // A new add of the same URL replaces (cancels) the previous StreamTask,
                // if any, avoiding two concurrent tasks for the same camera.
                if let Some(previous_token) = tasks.remove(&camera.rtsp_url) {
                    previous_token.cancel();
                }

                let token = CancellationToken::new();
                tasks.insert(camera.rtsp_url.clone(), token.clone());

                tokio::task::spawn_blocking(move || run_stream_task(camera, sender, token));
            }
            StreamCommand::RemoveCamera { rtsp_url } => {
                if let Some(token) = tasks.remove(&rtsp_url) {
                    token.cancel();
                }
            }
            StreamCommand::Shutdown => {
                for (_, token) in tasks.drain() {
                    token.cancel();
                }
                break;
            }
        }
    }
}

/// Body of the `StreamTask` of a single camera, run on a dedicated thread via
/// `spawn_blocking` (Requirement 3.5). Opens the RTSP connection, decodes frames
/// and delivers them through `sender`; on failure, applies the reconnection
/// state machine (`next_connection_state`/`backoff_delay`) and retries
/// indefinitely unless `cancel_token` is cancelled (Requirements 3.2, 3.3, 6.1,
/// 6.2, 6.3, 6.4, 6.5).
fn run_stream_task(
    camera: Camera,
    sender: mpsc::Sender<StreamEvent>,
    cancel_token: CancellationToken,
) {
    let mut status = ConnectionStatus::Disconnected;
    let mut attempt: u32 = 0;

    loop {
        if cancel_token.is_cancelled() {
            return;
        }

        match decode_stream(
            &camera.rtsp_url,
            &sender,
            &cancel_token,
            &mut status,
            &mut attempt,
        ) {
            StopReason::Cancelled => {
                // Shutdown requested (removal/shutdown): no transition to
                // Reconnecting and no new attempt scheduled (Requirements 3.6,
                // 6.6).
                status = next_connection_state(status, ConnectionEvent::UserRequestedStop);
                let _ = sender.try_send(StreamEvent::Status(status));
                return;
            }
            StopReason::Failure => {
                status = next_connection_state(status, ConnectionEvent::DecodeFailure);
                let _ = sender.try_send(StreamEvent::Status(status));

                let delay = backoff_delay(attempt, DEFAULT_MAX_BACKOFF_DELAY);
                attempt = attempt.saturating_add(1);

                if wait_or_cancelled(delay, &cancel_token) {
                    return;
                }
            }
        }
    }
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
/// or cancellation. Each decoded frame is converted to RGBA8 and delivered via
/// `sender.try_send` (silently dropped when the channel is full, see the note
/// below); when the first frame is decoded successfully, moves `status` to
/// Connected and resets `attempt` (Requirement 6.4).
///
/// Note on the drop policy (Requirement 3.2, design.md): the
/// `mpsc::Sender<StreamEvent>` channel is created by the UI with a small capacity
/// (a few slots). When `try_send` fails because it is full, the frame that was
/// just decoded is dropped (not the ones already pending in the channel). In
/// practice, since the UI drains the channel on every render frame (~60Hz) and
/// the capacity is small, the oldest pending frame is consumed by the UI before
/// the next send attempt, so the observed effect is equivalent to always keeping
/// the latest frame available without piling up memory.
fn decode_stream(
    rtsp_url: &str,
    sender: &mpsc::Sender<StreamEvent>,
    cancel_token: &CancellationToken,
    status: &mut ConnectionStatus,
    attempt: &mut u32,
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

    let mut decoder = {
        let stream = input_ctx
            .stream(video_stream_index)
            .expect("valid stream index");
        let context_decoder = match ffmpeg_next::codec::context::Context::from_parameters(
            stream.parameters(),
        ) {
            Ok(ctx) => ctx,
            Err(err) => {
                tracing::error!(rtsp_url = %safe_url, error = %err, "failed to create the decoding context");
                return StopReason::Failure;
            }
        };
        match context_decoder.decoder().video() {
            Ok(decoder) => decoder,
            Err(err) => {
                tracing::error!(rtsp_url = %safe_url, error = %err, "failed to open the video decoder");
                return StopReason::Failure;
            }
        }
    };

    let mut scaler: Option<ffmpeg_next::software::scaling::Context> = None;

    for (stream, packet) in input_ctx.packets() {
        if cancel_token.is_cancelled() {
            return StopReason::Cancelled;
        }

        if stream.index() != video_stream_index {
            continue;
        }

        if let Err(err) = decoder.send_packet(&packet) {
            tracing::error!(rtsp_url = %safe_url, error = %err, "failed to send a packet to the decoder");
            return StopReason::Failure;
        }

        let mut decoded = ffmpeg_next::util::frame::video::Video::empty();
        while decoder.receive_frame(&mut decoded).is_ok() {
            if cancel_token.is_cancelled() {
                return StopReason::Cancelled;
            }

            let scaler_ctx = match &mut scaler {
                Some(existing) => existing,
                None => {
                    let new_scaler = match ffmpeg_next::software::scaling::Context::get(
                        decoder.format(),
                        decoder.width(),
                        decoder.height(),
                        ffmpeg_next::format::Pixel::RGBA,
                        decoder.width(),
                        decoder.height(),
                        ffmpeg_next::software::scaling::Flags::BILINEAR,
                    ) {
                        Ok(s) => s,
                        Err(err) => {
                            tracing::error!(rtsp_url = %safe_url, error = %err, "failed to create the pixel converter");
                            return StopReason::Failure;
                        }
                    };
                    scaler = Some(new_scaler);
                    scaler.as_mut().expect("scaler just inserted")
                }
            };

            let mut rgba_frame = ffmpeg_next::util::frame::video::Video::empty();
            if let Err(err) = scaler_ctx.run(&decoded, &mut rgba_frame) {
                tracing::error!(rtsp_url = %safe_url, error = %err, "failed to convert the frame to RGBA8");
                return StopReason::Failure;
            }

            let width = rgba_frame.width();
            let height = rgba_frame.height();
            let rgba = copy_rgba_plane(&rgba_frame, width, height);

            if *status != ConnectionStatus::Connected {
                *status = next_connection_state(*status, ConnectionEvent::FirstFrameDecoded);
                *attempt = 0;
                let _ = sender.try_send(StreamEvent::Status(*status));
            }

            // Drop policy for the oldest frame: see the note in this function's docs.
            let _ = sender.try_send(StreamEvent::Frame(CameraFrame {
                width,
                height,
                rgba,
            }));
        }
    }

    // The packet loop ended without cancellation: the stream reached its end
    // (EOF) or the network connection was closed. Treated as a failure to
    // trigger automatic reconnection (Requirement 6.1).
    StopReason::Failure
}

/// Copies the RGBA8 pixel plane of `frame` into a compact `Vec<u8>` (without
/// stride padding), in the layout `CameraFrame` expects (row by row, `width *
/// 4` bytes per row, `height` rows).
fn copy_rgba_plane(
    frame: &ffmpeg_next::util::frame::video::Video,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let row_bytes = width as usize * 4;
    let stride = frame.stride(0);
    let data = frame.data(0);

    let mut rgba = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = row * stride;
        rgba.extend_from_slice(&data[start..start + row_bytes]);
    }
    rgba
}
