//! SimpleRtspViewerApp: eframe/egui integration.
//!
//! This module implements the `Viewing_Grid`: it builds the cameras from the
//! app's library (`store`), drains without blocking the events produced by the
//! `StreamTask`s of the `Stream_Manager`, draws the command bar and the grid of
//! `Camera_Tile`s, and handles drag-and-drop reordering and the camera
//! management dialogs, saving the library when it changes.
//!
//! No network I/O, decoding or file write happens directly on the UI thread:
//! communication with the `Stream_Manager` goes through channels
//! (`tokio::sync::mpsc`) and the library write is triggered with `tokio::spawn`
//! (fire-and-forget), as in design.md. The exceptions are the Windows
//! credential vault, written right when the user saves a login, and the
//! "Import" file picker.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use crate::audio::{self, AudioOutput, AudioSink};
use crate::credentials;
use crate::dialogs::{CameraForm, Dialog, DialogAction, DialogEnv, LoginForm, ScanDialog};
use crate::grid;
use crate::import;
use crate::model::{Camera, CameraFrame, ConnectionStatus, DisplaySize};
use crate::pixel::{self, border_rect, hairline_stroke, snap_pos, snap_rect};
use crate::probe::Probe;
use crate::sidebar::{self, AudioControls, CameraEntry, SidebarAction, SidebarInput, Viewing};
use crate::store::{self, CameraLibrary};
use crate::streaming::{StreamChannels, StreamManagerHandle};
use crate::theme;
use crate::widgets;
use crate::zoom::{self, Zoom};

/// How long a toast (the short confirmation popup) stays on screen, counting
/// its fade in and fade out. Error messages are not toasts: they stay in the
/// notice bar until closed.
const TOAST_DURATION: Duration = Duration::from_millis(2800);
const TOAST_FADE_IN: Duration = Duration::from_millis(150);
const TOAST_FADE_OUT: Duration = Duration::from_millis(350);

/// Maximum width of a toast; longer messages wrap.
const TOAST_MAX_WIDTH: f32 = 420.0;

/// Opacity (0 to 1) of a toast `elapsed` after it appeared, or `None` once its
/// duration is over: it fades in, stays solid, then fades out.
fn toast_opacity(elapsed: Duration) -> Option<f32> {
    if elapsed >= TOAST_DURATION {
        return None;
    }
    let fade_in = (elapsed.as_secs_f32() / TOAST_FADE_IN.as_secs_f32()).min(1.0);
    let remaining = TOAST_DURATION - elapsed;
    let fade_out = (remaining.as_secs_f32() / TOAST_FADE_OUT.as_secs_f32()).min(1.0);
    Some(fade_in.min(fade_out))
}

/// Space between the camera tiles and around the grid, as between Windows 11
/// cards.
const GAP: f32 = 8.0;

/// Corner radius of the camera tiles (the same as Windows 11 cards).
const TILE_ROUNDING: f32 = 8.0;

/// How long the "double-click or Esc to go back" hint shows after enlarging a
/// camera; it fades out during the last half second.
const ENLARGED_HINT_DURATION: Duration = Duration::from_secs(3);
const ENLARGED_HINT_FADE: Duration = Duration::from_millis(500);

/// Capacity of each camera's frame channel (`CameraFrame`). A single frame:
/// only the latest one matters to the UI, which drains the channel on every
/// render frame. While the previous frame has not been consumed, the
/// `StreamTask` drops new ones even before converting them to RGBA, without
/// spending CPU on frames that would not be shown (Requirement 7.2, see
/// design.md).
const FRAME_CHANNEL_CAPACITY: usize = 1;

/// Size, in whole physical pixels, at which a tile of `size_points` (egui
/// points) appears on screen, floored to even numbers.
///
/// It is the exact size of the tile, never larger: the decoder scales every
/// frame to fit in it, and the frame is then drawn 1:1 (one texel per screen
/// pixel, see `pixel::video_rect`) with no resampling on the GPU, which is what
/// keeps the video sharp. Following every pixel of a window resize only makes
/// each camera recreate its pixel converter, which costs well under a
/// millisecond.
fn display_size_for(size_points: egui::Vec2, pixels_per_point: f32) -> DisplaySize {
    let (width, height) = pixel::physical_size(size_points, pixels_per_point);
    DisplaySize {
        width: (width & !1).max(2),
        height: (height & !1).max(2),
    }
}

/// Stream currently open for a camera and the UI's ends of the matching
/// `StreamTask` channels.
struct ActiveStream {
    /// Open URL: the substream, or the main one when the enlarged camera is in
    /// high resolution.
    url: String,
    frames: mpsc::Receiver<CameraFrame>,
    status: watch::Receiver<ConnectionStatus>,
    /// Size at which the camera is being shown, read by the `StreamTask`.
    display_size: watch::Sender<Option<DisplaySize>>,
    /// Where the `StreamTask` plays this stream's sound: `Some` only while the
    /// camera is enlarged and not muted.
    audio: watch::Sender<Option<AudioSink>>,
    /// The stream has an audio track the app can decode.
    has_audio: watch::Receiver<bool>,
}

/// Sound of the enlarged camera: the output device, opened the first time it
/// is needed, and the user's mute and volume choices (kept while the app is
/// open, across cameras).
struct AudioState {
    output: Option<AudioOutput>,
    /// Opens the output device. A field so tests do not touch a real device.
    open: fn() -> Result<AudioOutput, String>,
    /// Opening the device failed; remembered so the error shows once, not on
    /// every enlarge.
    open_failed: bool,
    /// The device is running (a camera is enlarged and not muted).
    playing: bool,
    muted: bool,
    /// Volume from 0 to 1.
    volume: f32,
}

impl AudioState {
    fn new() -> Self {
        Self {
            output: None,
            open: AudioOutput::open,
            open_failed: false,
            playing: false,
            muted: false,
            volume: audio::DEFAULT_VOLUME,
        }
    }
}

/// What the user did on a grid tile in this frame.
enum BlockAction {
    None,
    /// Dropped tile `from` onto tile `to` (drag and drop).
    Drop {
        from: usize,
        to: usize,
    },
    /// Double-clicked the tile (or used the menu) to enlarge it.
    Enlarge,
    /// Asked to edit the camera from the context menu.
    Edit,
    /// Asked to remove the camera from the context menu.
    Remove,
}

/// How a tile is drawn in this frame.
#[derive(Clone, Copy)]
struct TileState {
    /// The pointer is over it.
    hovered: bool,
    /// A dragged tile would be dropped here.
    drop_target: bool,
    /// It is the tile being dragged: stays in place, dimmed.
    dimmed: bool,
    /// Part of the picture shown, in normalized texture coordinates.
    view: egui::Rect,
}

/// Rectangles of the overlays drawn on a tile (`None` when not drawn).
#[derive(Clone, Copy)]
struct Overlays {
    /// Connection status pill, centered.
    status: Option<egui::Rect>,
    /// Camera name label, bottom-left.
    name: Option<egui::Rect>,
}

/// Action requested from the sidebar.
enum Command {
    AddCamera,
    Scan,
    Import,
    SharedLogin,
    Refresh,
}

/// Error message in the notice bar, below the command bar. It stays until the
/// user closes it.
struct Notice {
    text: String,
}

/// Short confirmation popup at the bottom of the window. It goes away on its
/// own after `TOAST_DURATION`.
struct Toast {
    text: String,
    /// When the toast appeared.
    since: Instant,
}

/// How a message is shown to the user.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NoticeKind {
    /// Something worked: a short toast that disappears by itself.
    Success,
    /// Something failed: the notice bar, which stays until closed.
    Error,
}

/// Applies a drag-and-drop position swap and tells whether the result actually
/// differs from the original list, and therefore whether the layout persistence
/// should be triggered. Pure function (no I/O), used both by the real UI and by
/// the unit tests of the wiring logic (Requirements 4.1, 4.2, 4.4).
fn reorder_and_should_persist(cameras: &[Camera], from: usize, to: usize) -> (Vec<Camera>, bool) {
    let reordered = grid::reorder(cameras, from, to);
    let changed = reordered != cameras;
    (reordered, changed)
}

/// Windows dark theme; without information from the system, assumes dark.
fn is_dark(system_theme: Option<eframe::Theme>) -> bool {
    system_theme.is_none_or(|theme| theme == eframe::Theme::Dark)
}

/// Pill-shaped status notice, centered in the tile: an indicator (spinner or
/// colored dot) and the text. Returns the pill's rectangle, or `None` when the
/// tile is too small for it (nothing is drawn then, rather than overflowing).
fn paint_status_pill(
    ui: &egui::Ui,
    painter: &egui::Painter,
    tile: egui::Rect,
    label: &str,
    color: Option<egui::Color32>,
    spinner: bool,
) -> Option<egui::Rect> {
    const MARKER: f32 = 14.0;
    let ppp = ui.ctx().pixels_per_point();
    let marker_space = if spinner || color.is_some() {
        MARKER + 8.0
    } else {
        0.0
    };

    // The pill must fit in the tile, with a margin; the label shrinks (with an
    // ellipsis) before the pill outgrows it.
    let max_label = tile.width() - 16.0 - 28.0 - marker_space;
    if max_label < 24.0 || tile.height() < 48.0 {
        return None;
    }
    let label = widgets::single_line(
        ui,
        label,
        egui::FontId::proportional(13.0),
        egui::Color32::WHITE,
        max_label,
    );
    let pill = snap_rect(
        egui::Rect::from_center_size(
            tile.center(),
            egui::vec2(marker_space + label.size().x + 28.0, 32.0),
        ),
        ppp,
    );
    painter.rect_filled(pill, 16.0, theme::OVERLAY_CHIP);

    let mut x = pill.left() + 14.0;
    if marker_space > 0.0 {
        let marker = egui::Rect::from_center_size(
            egui::pos2(x + MARKER / 2.0, pill.center().y),
            egui::vec2(MARKER, MARKER),
        );
        if spinner {
            egui::Spinner::new()
                .size(MARKER)
                .color(color.unwrap_or(egui::Color32::WHITE))
                .paint_at(ui, marker);
        } else if let Some(color) = color {
            painter.circle_filled(snap_pos(marker.center(), ppp), 4.0, color);
        }
        x += marker_space;
    }
    widgets::paint_text_left(ui, label, x, pill.center().y, egui::Color32::WHITE);
    Some(pill)
}

/// Label with the camera name in the bottom-left corner of the tile, with a
/// green dot when the video is live. The name is cut with an ellipsis to fit
/// the tile; the label is omitted when even a short name would not fit, or
/// when it would touch `avoid` (the status pill, which takes priority).
/// Returns the label's rectangle when drawn.
fn paint_name_chip(
    ui: &egui::Ui,
    painter: &egui::Painter,
    tile: egui::Rect,
    name: &str,
    live: bool,
    avoid: Option<egui::Rect>,
) -> Option<egui::Rect> {
    let ppp = ui.ctx().pixels_per_point();
    let dot_space = if live { 14.0 } else { 0.0 };
    let max_label = tile.width() - 16.0 - 16.0 - dot_space;
    if max_label < 24.0 || tile.height() < 56.0 {
        return None;
    }
    let label = widgets::single_line(
        ui,
        name,
        egui::FontId::proportional(13.0),
        egui::Color32::WHITE,
        max_label,
    );
    let size = egui::vec2(dot_space + label.size().x + 16.0, 24.0);
    let chip = snap_rect(
        egui::Rect::from_min_size(
            egui::pos2(tile.left() + 8.0, tile.bottom() - 8.0 - size.y),
            size,
        ),
        ppp,
    );
    if avoid.is_some_and(|other| other.expand(4.0).intersects(chip)) {
        return None;
    }
    painter.rect_filled(chip, 4.0, theme::OVERLAY_CHIP);

    let mut x = chip.left() + 8.0;
    if live {
        painter.circle_filled(
            snap_pos(egui::pos2(x + 3.0, chip.center().y), ppp),
            3.0,
            theme::SUCCESS,
        );
        x += dot_space;
    }
    widgets::paint_text_left(ui, label, x, chip.center().y, egui::Color32::WHITE);
    Some(chip)
}

/// `SimpleRtspViewerApp`: integration of the `Viewing_Grid` with
/// `eframe`/`egui`.
pub struct SimpleRtspViewerApp {
    /// Current in-memory camera order (reflected in the grid).
    cameras: Vec<Camera>,
    /// Stream open for each camera, keyed by the camera `id` (which identifies
    /// it even when the open one is the substream or the login changed). The
    /// status comes in a `watch`, which always holds the latest value, so no
    /// status change is lost even with the frame channel full.
    streams: HashMap<String, ActiveStream>,
    /// Last GPU texture loaded for each camera, keyed by `id`.
    textures: HashMap<String, egui::TextureHandle>,
    /// Last `ConnectionStatus` read for each camera, keyed by `id`.
    statuses: HashMap<String, ConnectionStatus>,
    /// Index of the tile being dragged (drag and drop), if any, to draw the
    /// preview that follows the pointer and dim the tile it came from.
    dragging: Option<usize>,
    /// The sidebar shows only icons.
    sidebar_collapsed: bool,
    /// Area of the window the video fills (everything right of the sidebar),
    /// from the last frame; the toast is placed in it.
    content_rect: egui::Rect,
    /// Where each component was drawn, recorded for the layout tests.
    probe: Probe,
    /// Enlarged camera (filling the content area), by `id`; `None` shows the
    /// grid.
    enlarged: Option<String>,
    /// When the current camera was enlarged, to show the hint on how to go back
    /// for a few seconds.
    enlarged_since: Option<Instant>,
    /// The enlarged camera shows its main stream (higher resolution) instead of
    /// the substream. Turned off whenever the enlarged camera changes.
    high_resolution: bool,
    /// Zoom of the enlarged camera. Reset whenever the enlarged camera changes.
    zoom: Zoom,
    /// Handle to the `Stream_Manager`, used to open and close streams.
    stream_manager_handle: StreamManagerHandle,
    /// Camera library (stored list and logins), source of `cameras`.
    library: CameraLibrary,
    /// Open dialog, if any.
    dialog: Option<Dialog>,
    /// Error message in the notice bar, if any.
    notice: Option<Notice>,
    /// Confirmation toast on screen, if any.
    toast: Option<Toast>,
    /// Sound of the enlarged camera.
    audio: AudioState,
    /// Windows 11 look in use.
    look: Look,
}

/// Look state: palette of the current theme, Mica backdrop and Windows fonts.
struct Look {
    palette: theme::Palette,
    /// Windows accent color, read at startup.
    accent: egui::Color32,
    /// Mica was requested (not turned off by the environment variable).
    mica_requested: bool,
    /// Mica is actually applied to the window (Windows 11).
    mica: bool,
    /// The Windows icon font was loaded.
    icons: bool,
    /// Theme (dark?) the style was last applied for.
    applied_dark: Option<bool>,
}

impl Look {
    fn new(dark: bool) -> Self {
        let accent = theme::system_accent();
        Self {
            palette: theme::Palette::new(dark, accent),
            accent,
            mica_requested: false,
            mica: false,
            icons: false,
            applied_dark: None,
        }
    }
}

impl SimpleRtspViewerApp {
    /// Builds the initial app state from the camera library, starting the
    /// decoding of each camera through `stream_manager_handle` (Requirements
    /// 2.1, 2.3, 3.1).
    ///
    /// A stream (the substream, if any) is opened for each camera with
    /// dedicated channels; the UI's ends are kept in `streams`. The initial
    /// `statuses` are all `Disconnected`, until the matching `StreamTask`
    /// publishes the first status.
    ///
    /// `StreamManagerHandle::new` needs an active tokio runtime on the current
    /// thread; the commands sent here are synchronous.
    pub fn new(library: CameraLibrary, stream_manager_handle: StreamManagerHandle) -> Self {
        let mut app = Self {
            cameras: Vec::new(),
            streams: HashMap::new(),
            textures: HashMap::new(),
            statuses: HashMap::new(),
            dragging: None,
            sidebar_collapsed: false,
            content_rect: egui::Rect::NOTHING,
            probe: Probe::default(),
            enlarged: None,
            enlarged_since: None,
            zoom: Zoom::default(),
            high_resolution: false,
            stream_manager_handle,
            library,
            dialog: None,
            notice: None,
            toast: None,
            audio: AudioState::new(),
            look: Look::new(true),
        };
        app.rebuild_cameras();
        app
    }

    /// Rebuilds the cameras from the library (after adding, editing, removing
    /// or changing a login): drops the state of the cameras that left and opens
    /// or switches the streams of what changed.
    fn rebuild_cameras(&mut self) {
        self.cameras = self.library.cameras();
        let ids: HashSet<String> = self
            .cameras
            .iter()
            .map(|camera| camera.id.clone())
            .collect();

        let removed: Vec<String> = self
            .streams
            .keys()
            .filter(|id| !ids.contains(*id))
            .cloned()
            .collect();
        for id in removed {
            if let Some(stream) = self.streams.remove(&id) {
                self.stream_manager_handle.remove_stream(stream.url);
            }
        }
        self.textures.retain(|id, _| ids.contains(id));
        self.statuses.retain(|id, _| ids.contains(id));
        for camera in &self.cameras {
            self.statuses
                .entry(camera.id.clone())
                .or_insert(ConnectionStatus::Disconnected);
        }
        if self.enlarged.as_ref().is_some_and(|id| !ids.contains(id)) {
            self.enlarged = None;
        }
        self.sync_streams();
        self.sync_audio();
    }

    /// Applies the Windows 11 look when the window is created: system fonts,
    /// the current Windows light/dark theme, accent color and Mica backdrop.
    pub fn with_windows_look(mut self, cc: &eframe::CreationContext<'_>) -> Self {
        let dark = is_dark(cc.integration_info.system_theme);

        self.look.icons = theme::install_fonts(&cc.egui_ctx);
        self.look.mica_requested = theme::mica_requested();
        self.look.mica = self.look.mica_requested && theme::apply_mica_on_start(cc, dark);
        self.apply_theme(&cc.egui_ctx, dark);
        self
    }

    /// Follows Windows light/dark theme changes while the app is open.
    fn sync_theme(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let dark = is_dark(frame.info().system_theme);
        if self.look.applied_dark == Some(dark) {
            return;
        }
        if self.look.mica_requested {
            self.look.mica = theme::apply_mica_on_frame(frame, dark);
        }
        self.apply_theme(ctx, dark);
    }

    fn apply_theme(&mut self, ctx: &egui::Context, dark: bool) {
        self.look.palette = theme::Palette::new(dark, self.look.accent);
        self.look.applied_dark = Some(dark);
        theme::apply_style(ctx, &self.look.palette, self.look.mica);
    }

    /// Makes sure each camera has the right stream open for the current mode:
    /// the main one for the enlarged camera in high resolution (or for a camera
    /// without a substream), the substream for the others. Only switches what
    /// changed.
    fn sync_streams(&mut self) {
        let changes: Vec<(String, String)> = self
            .cameras
            .iter()
            .filter_map(|camera| {
                let key = &camera.id;
                let main = self.high_resolution && self.enlarged.as_ref() == Some(key);
                let wanted = camera.stream_url(main);
                let current = self.streams.get(key).map(|stream| stream.url.as_str());
                (current != Some(wanted)).then(|| (key.clone(), wanted.to_string()))
            })
            .collect();

        for (key, url) in changes {
            self.start_stream(&key, url);
        }
    }

    /// Reconnects every camera: closes each open stream and opens it again,
    /// which also resets the reconnection backoff. The last frame stays on
    /// screen until the new connection delivers one.
    fn refresh_streams(&mut self) {
        let open: Vec<(String, String)> = self
            .streams
            .iter()
            .map(|(key, stream)| (key.clone(), stream.url.clone()))
            .collect();
        for (key, url) in open {
            self.start_stream(&key, url);
        }
    }

    /// Opens `url` to show camera `key`, first closing the stream that was open
    /// for it, if any.
    fn start_stream(&mut self, key: &str, url: String) {
        // The new stream starts with the camera's current status and the last
        // known display size. So, when switching between substream and main
        // stream, the last frame stays on screen, without flashing
        // "Disconnected", while the new stream connects; if the connection
        // fails, the StreamTask publishes "Reconnecting".
        let current_status = self
            .statuses
            .get(key)
            .copied()
            .unwrap_or(ConnectionStatus::Disconnected);
        let current_display = self
            .streams
            .get(key)
            .and_then(|stream| *stream.display_size.borrow());
        // The sound controls stay on screen while the stream switches.
        let had_audio = self
            .streams
            .get(key)
            .is_some_and(|stream| *stream.has_audio.borrow());

        if let Some(previous) = self.streams.remove(key) {
            self.stream_manager_handle.remove_stream(previous.url);
        }

        let (frame_tx, frame_rx) = mpsc::channel(FRAME_CHANNEL_CAPACITY);
        let (status_tx, status_rx) = watch::channel(current_status);
        let (display_tx, display_rx) = watch::channel(current_display);
        // The new stream of the enlarged camera keeps playing its sound.
        let (audio_tx, audio_rx) = watch::channel(self.current_audio_sink(key));
        let (has_audio_tx, has_audio_rx) = watch::channel(had_audio);
        self.stream_manager_handle.add_stream(
            url.clone(),
            StreamChannels {
                frames: frame_tx,
                status: status_tx,
                display_size: display_rx,
                audio: audio_rx,
                has_audio: has_audio_tx,
            },
        );
        self.streams.insert(
            key.to_string(),
            ActiveStream {
                url,
                frames: frame_rx,
                status: status_rx,
                display_size: display_tx,
                audio: audio_tx,
                has_audio: has_audio_rx,
            },
        );
    }

    /// Where camera `key` plays its sound: the output's sink for the enlarged
    /// camera while it is not muted, nowhere for all the others.
    fn current_audio_sink(&self, key: &str) -> Option<AudioSink> {
        if self.audio.muted || self.enlarged.as_deref() != Some(key) {
            return None;
        }
        self.audio.output.as_ref().map(AudioOutput::sink)
    }

    /// Opens the audio output if it is not open yet. A failure (no output
    /// device) shows an error once and leaves the cameras without sound.
    fn ensure_audio_output(&mut self) {
        if self.audio.output.is_some() {
            return;
        }
        match (self.audio.open)() {
            Ok(output) => {
                output.set_volume(self.audio.volume);
                self.audio.output = Some(output);
                self.audio.open_failed = false;
            }
            Err(err) => {
                tracing::warn!(error = %err, "audio output unavailable");
                if !self.audio.open_failed {
                    self.show_notice(
                        NoticeKind::Error,
                        format!("The camera sound is unavailable: {err}"),
                    );
                }
                self.audio.open_failed = true;
            }
        }
    }

    /// Brings the sound in line with what is on screen: the device runs, and
    /// the enlarged camera's stream gets the sink, only while a camera is
    /// enlarged and not muted. Every other stream gets none, so its audio
    /// packets are dropped without decoding.
    fn sync_audio(&mut self) {
        let playing = self.enlarged.is_some() && !self.audio.muted;
        if playing {
            self.ensure_audio_output();
        }
        if let Some(output) = &self.audio.output {
            if playing != self.audio.playing {
                output.set_active(playing);
            }
            self.audio.playing = playing;
        }
        for (key, stream) in &self.streams {
            stream.audio.send_replace(self.current_audio_sink(key));
        }
    }

    fn set_muted(&mut self, muted: bool) {
        if self.audio.muted != muted {
            self.audio.muted = muted;
            self.sync_audio();
        }
    }

    fn set_volume(&mut self, volume: f32) {
        self.audio.volume = volume.clamp(0.0, 1.0);
        if let Some(output) = &self.audio.output {
            output.set_volume(self.audio.volume);
        }
    }

    /// Enlarges camera `key` (or goes back to the grid, with `None`) and
    /// switches the open streams accordingly.
    fn set_enlarged(&mut self, key: Option<String>) {
        if self.enlarged != key {
            self.enlarged_since = key.as_ref().map(|_| Instant::now());
            self.enlarged = key;
            self.high_resolution = false;
            self.zoom = Zoom::default();
            self.sync_streams();
            self.sync_audio();
        }
    }

    /// Enlarges the camera at index (0-based, in grid order). Pressing the
    /// number of the camera that is already enlarged goes back to the grid.
    /// Does nothing when there is no camera at that position.
    fn toggle_enlarged_at(&mut self, index: usize) {
        let Some(id) = self.cameras.get(index).map(|camera| camera.id.clone()) else {
            return;
        };
        if self.enlarged.as_deref() == Some(id.as_str()) {
            self.set_enlarged(None);
        } else {
            self.set_enlarged(Some(id));
        }
    }

    /// Keys 1 to 9 open the camera at that position in the grid. Ignored
    /// while a dialog is open or a text field has focus.
    fn handle_number_shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() || ctx.wants_keyboard_input() {
            return;
        }
        const KEYS: [egui::Key; 9] = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        let pressed = ctx.input(|input| {
            if input.modifiers.any() {
                return None;
            }
            KEYS.iter().position(|key| input.key_pressed(*key))
        });
        if let Some(index) = pressed {
            self.toggle_enlarged_at(index);
        }
    }
    /// Switches the enlarged camera between its substream and its main stream.
    fn set_high_resolution(&mut self, high: bool) {
        if self.high_resolution != high {
            self.high_resolution = high;
            self.sync_streams();
        }
    }

    /// Index, in `cameras`, of the enlarged camera.
    fn enlarged_index(&self) -> Option<usize> {
        let key = self.enlarged.as_ref()?;
        self.cameras.iter().position(|camera| &camera.id == key)
    }

    /// Tells the `StreamTask` of camera `key` the size at which it is shown, so
    /// the frames already arrive at that size. Only sends when the (rounded)
    /// size changes.
    fn report_display_size(&self, key: &str, size_points: egui::Vec2, pixels_per_point: f32) {
        let Some(stream) = self.streams.get(key) else {
            return;
        };
        let requested = Some(display_size_for(size_points, pixels_per_point));
        if *stream.display_size.borrow() != requested {
            stream.display_size.send_replace(requested);
        }
    }

    /// Drains without blocking (`try_recv`) the pending frames of each camera,
    /// updating the associated GPU texture, and reads the latest connection
    /// status published in the matching `watch` (Requirements 2.4, 2.7, 3.5).
    /// No network I/O or decoding happens here, only reads from channels
    /// already filled by the background `StreamTask`s. Returns `true` if at
    /// least one frame arrived or some status changed in this cycle, so the
    /// caller can adjust the repaint rate.
    fn drain_camera_events(&mut self, ctx: &egui::Context) -> bool {
        let mut received_any = false;
        for camera in &self.cameras {
            let key = &camera.id;
            let Some(stream) = self.streams.get_mut(key) else {
                continue;
            };

            // Only the latest frame becomes a texture; uploading to the GPU
            // frames that would be replaced within the same screen frame would
            // be wasted work.
            let mut newest = None;
            while let Ok(frame) = stream.frames.try_recv() {
                newest = Some(frame);
            }
            if let Some(frame) = newest {
                received_any = true;
                Self::update_texture(&mut self.textures, ctx, key, frame);
            }

            // Compares with the last value read: works the same before and
            // after the `StreamTask` ends and drops the `Sender`.
            let latest = *stream.status.borrow();
            if self.statuses.get(key) != Some(&latest) {
                self.statuses.insert(key.clone(), latest);
                received_any = true;
            }
        }
        received_any
    }

    /// Updates (or creates) the camera's `egui::TextureHandle` with the frame.
    /// The frame already arrives in egui's format and is moved into the
    /// texture, without a copy.
    fn update_texture(
        textures: &mut HashMap<String, egui::TextureHandle>,
        ctx: &egui::Context,
        url: &str,
        frame: CameraFrame,
    ) {
        if let Some(texture) = textures.get_mut(url) {
            texture.set(frame, egui::TextureOptions::LINEAR);
        } else {
            let texture = ctx.load_texture(url.to_string(), frame, egui::TextureOptions::LINEAR);
            textures.insert(url.to_string(), texture);
        }
    }

    // -----------------------------------------------------------------------
    // Sidebar
    // -----------------------------------------------------------------------

    /// Draws the sidebar and applies what the user did in it.
    fn draw_sidebar(&mut self, ctx: &egui::Context) {
        let actions = {
            let entries: Vec<CameraEntry<'_>> = self
                .cameras
                .iter()
                .zip(&self.library.data.cameras)
                .map(|(camera, stored)| {
                    debug_assert_eq!(camera.id, stored.id);
                    CameraEntry {
                        id: &camera.id,
                        name: camera.name.as_deref().unwrap_or(&stored.host),
                        address: stored.address(),
                        status: self
                            .statuses
                            .get(&camera.id)
                            .copied()
                            .unwrap_or(ConnectionStatus::Disconnected),
                    }
                })
                .collect();
            let input = SidebarInput {
                palette: &self.look.palette,
                icons: self.look.icons,
                mica: self.look.mica,
                collapsed: self.sidebar_collapsed,
                enabled: self.dialog.is_none(),
                cameras: &entries,
                enlarged: self.enlarged.as_deref(),
                viewing: self.viewing(),
            };
            sidebar::show(ctx, &input, &self.probe)
        };

        for action in actions {
            self.apply_sidebar_action(action);
        }
    }

    /// What the sidebar shows about the enlarged camera, if any.
    fn viewing(&self) -> Option<Viewing<'_>> {
        let index = self.enlarged_index()?;
        let camera = &self.cameras[index];
        let stored = self.library.data.cameras.get(index)?;
        let has_substream = camera.substream_url.is_some();
        let main_open = !has_substream || self.high_resolution;
        let has_audio = !self.audio.open_failed
            && self
                .streams
                .get(&camera.id)
                .is_some_and(|stream| *stream.has_audio.borrow());

        Some(Viewing {
            name: camera.name.as_deref().unwrap_or(&stored.host),
            detail: format!(
                "{} · {}",
                stored.address(),
                if main_open {
                    "Main stream"
                } else {
                    "Substream"
                }
            ),
            high_resolution: has_substream.then_some(self.high_resolution),
            audio: has_audio.then_some(AudioControls {
                muted: self.audio.muted,
                volume: self.audio.volume,
                default_volume: audio::DEFAULT_VOLUME,
            }),
        })
    }

    fn apply_sidebar_action(&mut self, action: SidebarAction) {
        match action {
            SidebarAction::AddCamera => self.run_command(Command::AddCamera),
            SidebarAction::Scan => self.run_command(Command::Scan),
            SidebarAction::Import => self.run_command(Command::Import),
            SidebarAction::Refresh => self.run_command(Command::Refresh),
            SidebarAction::SharedLogin => self.run_command(Command::SharedLogin),
            SidebarAction::ToggleCollapsed => self.sidebar_collapsed = !self.sidebar_collapsed,
            SidebarAction::Select(id) => {
                // Selecting the camera that is already enlarged goes back to
                // the grid.
                if self.enlarged.as_deref() == Some(id.as_str()) {
                    self.set_enlarged(None);
                } else {
                    self.set_enlarged(Some(id));
                }
            }
            SidebarAction::Edit(id) => self.open_edit_dialog(&id),
            SidebarAction::Remove(id) => {
                let label = self.camera_label(&id);
                self.dialog = Some(Dialog::ConfirmRemove { id, label });
            }
            SidebarAction::BackToGrid => self.set_enlarged(None),
            SidebarAction::SetHighResolution(high) => self.set_high_resolution(high),
            SidebarAction::SetMuted(muted) => self.set_muted(muted),
            SidebarAction::SetVolume(volume) => self.set_volume(volume),
        }
    }

    // -----------------------------------------------------------------------
    // Content: grid or enlarged camera
    // -----------------------------------------------------------------------

    /// Draws everything right of the sidebar: the empty screen, the enlarged
    /// camera or the grid of `Camera_Tile`s (Requirements 2.1, 2.2, 2.3).
    fn draw_content(&mut self, ctx: &egui::Context) {
        // With Mica, the panel is transparent so the effect shows around and
        // between the tiles.
        let panel = egui::Frame::none()
            .fill(self.panel_fill())
            .inner_margin(egui::Margin::same(GAP));

        // With a dialog open, the content is inactive (as in a modal window).
        let interactive = self.dialog.is_none();
        egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
            ui.add_enabled_ui(interactive, |ui| self.content(ui));
        });
    }

    fn content(&mut self, ui: &mut egui::Ui) {
        let ppp = ui.ctx().pixels_per_point();
        let area = ui.available_rect_before_wrap();
        self.content_rect = area;
        probe!(self.probe, "container:content", area);

        if self.cameras.is_empty() {
            self.paint_empty_state(ui, area);
            return;
        }

        if let Some(index) = self.enlarged_index() {
            self.enlarged_tile(ui, index, snap_rect(area, ppp));
            return;
        }

        self.grid_tiles(ui, area, ppp);
    }

    /// The grid: every tile laid out by `grid::tile_rects` (equal size, edges
    /// on whole pixels, never overlapping), with drag-and-drop reordering
    /// (Requirements 4.1, 4.2, 4.3).
    fn grid_tiles(&mut self, ui: &mut egui::Ui, area: egui::Rect, ppp: f32) {
        let rects = grid::tile_rects(self.cameras.len(), area, GAP, ppp);

        let mut action: Option<(usize, BlockAction)> = None;
        let mut dragging = None;
        for (index, rect) in rects.iter().copied().enumerate() {
            let (block_action, is_dragged) = self.camera_tile(ui, index, rect);
            if is_dragged {
                dragging = Some(index);
            }
            if !matches!(block_action, BlockAction::None) {
                action = Some((index, block_action));
            }
        }
        self.dragging = dragging;
        self.paint_drag_preview(ui);

        let Some((index, action)) = action else {
            return;
        };
        let id = self.cameras[index].id.clone();
        match action {
            BlockAction::None => {}
            BlockAction::Drop { from, to } => self.apply_reorder(from, to),
            BlockAction::Enlarge => self.set_enlarged(Some(id)),
            BlockAction::Edit => self.open_edit_dialog(&id),
            BlockAction::Remove => {
                let label = self.camera_label(&id);
                self.dialog = Some(Dialog::ConfirmRemove { id, label });
            }
        }
    }

    /// One grid tile, as both a drag-and-drop source and target (Requirements
    /// 2.1, 4.1, 4.2, 4.3). Returns what the user did on it and whether it is
    /// being dragged.
    fn camera_tile(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        rect: egui::Rect,
    ) -> (BlockAction, bool) {
        let id = ui
            .id()
            .with(("camera_tile", self.cameras[index].id.as_str()));
        let response = ui.interact(rect, id, egui::Sense::click_and_drag());
        // Stores the payload the moment the drag starts.
        response.dnd_set_drag_payload(index);

        let is_dragged = ui.ctx().is_being_dragged(id);
        if is_dragged {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }

        let drop_target = response
            .dnd_hover_payload::<usize>()
            .is_some_and(|from| *from != index);
        let overlays = self.paint_camera(
            ui,
            index,
            rect,
            TileState {
                hovered: response.hovered(),
                drop_target,
                dimmed: is_dragged,
                view: zoom::full_view(),
            },
        );
        probe!(self.probe, format!("tile:{index}"), rect);
        if let Some(status) = overlays.status {
            probe!(self.probe, format!("tile:{index}:status"), status);
        }
        if let Some(name) = overlays.name {
            probe!(self.probe, format!("tile:{index}:name"), name);
        }

        if response.double_clicked() {
            return (BlockAction::Enlarge, is_dragged);
        }

        // Context menu (right button), as on Windows items.
        let mut menu_action = None;
        response.context_menu(|ui| {
            ui.set_min_width(160.0);
            if ui.button("Enlarge").clicked() {
                menu_action = Some(BlockAction::Enlarge);
                ui.close_menu();
            }
            if ui.button("Edit…").clicked() {
                menu_action = Some(BlockAction::Edit);
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Remove…").clicked() {
                menu_action = Some(BlockAction::Remove);
                ui.close_menu();
            }
        });
        if let Some(action) = menu_action {
            return (action, is_dragged);
        }

        if let Some(from) = response.dnd_release_payload::<usize>() {
            if *from != index {
                return (
                    BlockAction::Drop {
                        from: *from,
                        to: index,
                    },
                    is_dragged,
                );
            }
        }

        (BlockAction::None, is_dragged)
    }

    /// Small preview of the tile being dragged, following the pointer.
    fn paint_drag_preview(&self, ui: &egui::Ui) {
        let Some(index) = self.dragging else {
            return;
        };
        let Some(pointer) = ui.ctx().pointer_interact_pos() else {
            return;
        };
        let camera = &self.cameras[index];
        let palette = &self.look.palette;
        let size = egui::vec2(176.0, 100.0);

        egui::Area::new(egui::Id::new("simple_rtsp_viewer_drag_preview"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pointer - size / 2.0)
            .interactable(false)
            .show(ui.ctx(), |ui| {
                ui.set_opacity(0.92);
                let ppp = ui.ctx().pixels_per_point();
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                let rect = snap_rect(rect, ppp);
                ui.painter()
                    .rect_filled(rect, TILE_ROUNDING, palette.tile_background);
                if let Some(texture) = self.textures.get(&camera.id) {
                    let video = pixel::fit_preserving_aspect(rect, texture.size_vec2());
                    let mut shape = egui::epaint::RectShape::filled(
                        snap_rect(video, ppp),
                        0.0,
                        egui::Color32::WHITE,
                    );
                    shape.fill_texture_id = texture.id();
                    shape.uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    ui.painter().add(shape);
                }
                ui.painter().rect_stroke(
                    border_rect(rect, ppp),
                    TILE_ROUNDING,
                    egui::Stroke::new(2.0 / ppp, palette.accent),
                );
                if let Some(name) = camera.name.as_deref() {
                    paint_name_chip(ui, ui.painter(), rect, name, false, None);
                }
            });
    }

    /// The enlarged camera filling the content area. Double-click or Esc
    /// returns to the grid; the resolution and the sound are controlled from
    /// the sidebar, so nothing floats over the picture except the name, the
    /// connection status and a temporary hint.
    fn enlarged_tile(&mut self, ui: &mut egui::Ui, index: usize, rect: egui::Rect) {
        let id = ui
            .id()
            .with(("enlarged_tile", self.cameras[index].id.as_str()));
        let response = ui.interact(rect, id, egui::Sense::click());
        self.zoom_with_scroll(ui, index, rect, &response);

        let overlays = self.paint_camera(
            ui,
            index,
            rect,
            TileState {
                hovered: false,
                drop_target: false,
                dimmed: false,
                view: self.zoom.view(),
            },
        );
        let hint = self.paint_enlarged_hint(ui, rect, &[overlays.status, overlays.name]);

        probe!(self.probe, "tile:enlarged", rect);
        if let Some(status) = overlays.status {
            probe!(self.probe, "tile:enlarged:status", status);
        }
        if let Some(name) = overlays.name {
            probe!(self.probe, "tile:enlarged:name", name);
        }
        if let Some(hint) = hint {
            probe!(self.probe, "tile:enlarged:hint", hint);
        }

        // With a dialog open the area is inactive and Esc belongs to the
        // dialog.
        let escape = ui.is_enabled() && ui.input(|input| input.key_pressed(egui::Key::Escape));
        if response.double_clicked() || escape {
            self.set_enlarged(None);
        }
    }

    /// Scrolling over the enlarged picture zooms around the pointer. Does
    /// nothing while a dialog is open.
    fn zoom_with_scroll(
        &mut self,
        ui: &egui::Ui,
        index: usize,
        rect: egui::Rect,
        response: &egui::Response,
    ) {
        if !ui.is_enabled() || !response.hovered() {
            return;
        }
        let Some(texture) = self.textures.get(&self.cameras[index].id) else {
            return;
        };
        let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
            return;
        };
        let scroll_y = ui.input(|input| input.smooth_scroll_delta.y);
        let video = pixel::video_rect(rect, texture.size_vec2(), ui.ctx().pixels_per_point());
        self.zoom.scroll(scroll_y, pointer, video);
    }

    /// Hint on how to go back to the grid, in the top-right corner of the
    /// enlarged camera, during the first seconds after enlarging. Uses the
    /// longest wording that fits, and is skipped when it would touch another
    /// overlay (`avoid`). Returns its rectangle while visible.
    fn paint_enlarged_hint(
        &self,
        ui: &egui::Ui,
        rect: egui::Rect,
        avoid: &[Option<egui::Rect>],
    ) -> Option<egui::Rect> {
        let since = self.enlarged_since?;
        let elapsed = since.elapsed();
        if elapsed >= ENLARGED_HINT_DURATION {
            return None;
        }
        // Keeps the fade-out animation running even without new frames.
        ui.ctx().request_repaint_after(Duration::from_millis(50));

        let ppp = ui.ctx().pixels_per_point();
        let remaining = ENLARGED_HINT_DURATION - elapsed;
        let opacity = (remaining.as_secs_f32() / ENLARGED_HINT_FADE.as_secs_f32()).min(1.0);
        let text_color = egui::Color32::WHITE.gamma_multiply(opacity);

        let icon_space = if self.look.icons { 22.0 } else { 0.0 };
        let painter = ui.painter().with_clip_rect(rect);
        let wordings = [
            "Double-click or Esc to go back to the grid",
            "Esc to go back",
        ];
        let max_width = rect.width() - 24.0 - 24.0 - icon_space;
        let label = wordings.iter().find_map(|wording| {
            let galley = widgets::single_line(
                ui,
                wording,
                egui::FontId::proportional(13.0),
                text_color,
                f32::INFINITY,
            );
            (galley.size().x <= max_width).then(|| {
                widgets::single_line(
                    ui,
                    wording,
                    egui::FontId::proportional(13.0),
                    text_color,
                    max_width,
                )
            })
        })?;

        let size = egui::vec2(icon_space + label.size().x + 24.0, 32.0);
        let chip = snap_rect(
            egui::Rect::from_min_size(
                egui::pos2(rect.right() - 12.0 - size.x, rect.top() + 12.0),
                size,
            ),
            ppp,
        );
        if avoid
            .iter()
            .flatten()
            .any(|other| other.expand(4.0).intersects(chip))
        {
            return None;
        }
        painter.rect_filled(chip, 6.0, theme::OVERLAY_CHIP.gamma_multiply(opacity));

        let mut x = chip.left() + 12.0;
        if self.look.icons {
            widgets::paint_icon(
                ui,
                theme::ICON_BACK_TO_WINDOW,
                14.0,
                egui::pos2(x + 7.0, chip.center().y),
                text_color,
            );
            x += icon_space;
        }
        widgets::paint_text_left(ui, label, x, chip.center().y, text_color);
        Some(chip)
    }

    /// Screen without cameras: icon, title and where to add them.
    fn paint_empty_state(&self, ui: &egui::Ui, area: egui::Rect) {
        let palette = &self.look.palette;
        let painter = ui.painter();
        let center_x = area.center().x;
        let mut y = area.center().y - 48.0;

        if self.look.icons {
            painter.text(
                snap_pos(egui::pos2(center_x, y), ui.ctx().pixels_per_point()),
                egui::Align2::CENTER_CENTER,
                theme::ICON_VIDEO,
                theme::icon_font(40.0),
                palette.text_secondary,
            );
            y += 48.0;
        }
        painter.text(
            egui::pos2(center_x, y),
            egui::Align2::CENTER_CENTER,
            "No cameras yet",
            theme::semibold(20.0),
            palette.text,
        );
        y += 32.0;
        painter.text(
            egui::pos2(center_x, y),
            egui::Align2::CENTER_CENTER,
            "Use \"Scan network\" in the sidebar to find your cameras,",
            egui::FontId::proportional(14.0),
            palette.text_secondary,
        );
        y += 22.0;
        painter.text(
            egui::pos2(center_x, y),
            egui::Align2::CENTER_CENTER,
            "or \"Add camera\" to enter the address of one.",
            egui::FontId::proportional(14.0),
            palette.text_secondary,
        );
    }

    /// Draws camera `index` in `rect` and tells its `StreamTask` the size at
    /// which it is shown. Returns where the overlays were drawn.
    fn paint_camera(
        &self,
        ui: &egui::Ui,
        index: usize,
        rect: egui::Rect,
        state: TileState,
    ) -> Overlays {
        let camera = &self.cameras[index];
        let key = &camera.id;
        let status = self
            .statuses
            .get(key)
            .copied()
            .unwrap_or(ConnectionStatus::Disconnected);

        let overlays = self.paint_tile(
            ui,
            rect,
            self.textures.get(key),
            status,
            camera.name.as_deref(),
            state,
        );
        self.report_display_size(key, rect.size(), ui.ctx().pixels_per_point());
        overlays
    }

    /// Paints a `Camera_Tile` as a Windows 11 card: dark background with
    /// rounded corners, the latest video drawn 1:1 when it matches the tile
    /// (see `pixel::video_rect`) or fitted keeping its aspect ratio, the camera
    /// name in a label in the bottom-left corner and the connection status
    /// notice (Requirements 2.4, 2.5, 2.6).
    fn paint_tile(
        &self,
        ui: &egui::Ui,
        rect: egui::Rect,
        texture: Option<&egui::TextureHandle>,
        status: ConnectionStatus,
        name: Option<&str>,
        state: TileState,
    ) -> Overlays {
        let palette = &self.look.palette;
        let ppp = ui.ctx().pixels_per_point();
        let painter = ui.painter().with_clip_rect(rect.expand(1.0));
        painter.rect_filled(rect, TILE_ROUNDING, palette.tile_background);

        if let Some(texture) = texture {
            let video = pixel::video_rect(rect, texture.size_vec2(), ppp);
            // The picture only has rounded corners when it reaches the tile's
            // corners; with bars it sits inside them.
            let covers_tile =
                video.width() >= rect.width() - 2.0 && video.height() >= rect.height() - 2.0;
            let mut shape = egui::epaint::RectShape::filled(
                video,
                if covers_tile { TILE_ROUNDING } else { 0.0 },
                egui::Color32::WHITE,
            );
            shape.fill_texture_id = texture.id();
            shape.uv = state.view;
            painter.add(shape);
        }

        // Status notices: distinct from each other and from normal video, which
        // gets no notice (Requirements 2.5, 2.6). Over a frozen video, the
        // video is darkened so the notice stands out.
        let has_video = texture.is_some();
        let dim = |painter: &egui::Painter| {
            painter.rect_filled(rect, TILE_ROUNDING, egui::Color32::from_black_alpha(140));
        };
        let pill = match grid::status_indicator(status) {
            grid::IndicatorKind::None => {
                if has_video {
                    None
                } else {
                    // Connected, but the first frame has not arrived yet.
                    paint_status_pill(ui, &painter, rect, "Connecting…", None, true)
                }
            }
            grid::IndicatorKind::Reconnecting => {
                if has_video {
                    dim(&painter);
                }
                paint_status_pill(
                    ui,
                    &painter,
                    rect,
                    "Reconnecting…",
                    Some(theme::CAUTION),
                    true,
                )
            }
            grid::IndicatorKind::Disconnected if has_video => {
                dim(&painter);
                paint_status_pill(
                    ui,
                    &painter,
                    rect,
                    "Disconnected",
                    Some(theme::CRITICAL),
                    false,
                )
            }
            grid::IndicatorKind::Disconnected => {
                // Initial state: the first connection is still in progress.
                paint_status_pill(ui, &painter, rect, "Connecting…", None, true)
            }
        };

        let name_chip = name.and_then(|name| {
            let live = has_video && status == ConnectionStatus::Connected;
            paint_name_chip(ui, &painter, rect, name, live, pill)
        });

        // The tile being dragged stays in place, dimmed.
        if state.dimmed {
            painter.rect_filled(rect, TILE_ROUNDING, egui::Color32::from_black_alpha(120));
        }

        // Border: exactly one physical pixel, a little more visible under the
        // pointer, and in the accent color on the tile a drag would drop on.
        if state.drop_target {
            painter.rect_filled(rect, TILE_ROUNDING, palette.accent.gamma_multiply(0.18));
            painter.rect_stroke(
                snap_rect(rect, ppp).shrink(1.0 / ppp),
                TILE_ROUNDING,
                egui::Stroke::new(2.0 / ppp, palette.accent),
            );
        } else {
            let color = if state.hovered {
                palette.tile_border_hover
            } else {
                palette.tile_border
            };
            painter.rect_stroke(
                border_rect(rect, ppp),
                TILE_ROUNDING,
                hairline_stroke(ppp, color),
            );
        }

        Overlays {
            status: pill,
            name: name_chip,
        }
    }

    /// Applies the `from`/`to` position swap to `self.cameras` and, if the
    /// order actually changed, takes the new order to the library and saves it
    /// (Requirements 4.4, 5.1). If `to` is invalid or equal to `from`,
    /// `grid::reorder` already returns the original list unchanged and no write
    /// is triggered.
    fn apply_reorder(&mut self, from: usize, to: usize) {
        let (reordered, changed) = reorder_and_should_persist(&self.cameras, from, to);
        self.cameras = reordered;

        if changed {
            let position: HashMap<&str, usize> = self
                .cameras
                .iter()
                .enumerate()
                .map(|(index, camera)| (camera.id.as_str(), index))
                .collect();
            self.library.data.cameras.sort_by_key(|camera| {
                position
                    .get(camera.id.as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            self.persist_library();
        }
    }

    /// Triggers the async (fire-and-forget) write of the camera library,
    /// without blocking the UI thread (Requirement 5.1). The disk write runs on
    /// a thread of tokio's blocking pool via `spawn_blocking`; failures are
    /// logged, without interrupting the app (Requirement 5.7).
    fn persist_library(&self) {
        let path = self.library.path.clone();
        let data = self.library.data.clone();

        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || store::save(&path, &data)).await;

            match result {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::error!(error = %err, "failed to save the camera list");
                }
                Err(join_err) => {
                    tracing::error!(error = %join_err, "failed to run the background camera list write");
                }
            }
        });
    }

    /// Camera name for messages (or the address, if it has no name).
    fn camera_label(&self, id: &str) -> String {
        self.library
            .data
            .cameras
            .iter()
            .find(|camera| camera.id == id)
            .map(|camera| camera.name.clone().unwrap_or_else(|| camera.address()))
            .unwrap_or_default()
    }

    fn open_edit_dialog(&mut self, id: &str) {
        if let Some(camera) = self
            .library
            .data
            .cameras
            .iter()
            .find(|camera| camera.id == id)
        {
            let own = self.library.own.get(id);
            self.dialog = Some(Dialog::Camera(CameraForm::edit(camera, own)));
        }
    }

    /// Shows a message: a success as a short toast, an error in the notice bar.
    /// A new toast replaces the one on screen.
    fn show_notice(&mut self, kind: NoticeKind, text: impl Into<String>) {
        let text = text.into();
        match kind {
            NoticeKind::Success => {
                self.toast = Some(Toast {
                    text,
                    since: Instant::now(),
                });
            }
            NoticeKind::Error => self.notice = Some(Notice { text }),
        }
    }

    /// Runs a command from the command bar.
    fn run_command(&mut self, command: Command) {
        let defaults = &self.library.data.scan;
        match command {
            Command::AddCamera => self.dialog = Some(Dialog::Camera(CameraForm::new(defaults))),
            Command::Scan => self.dialog = Some(Dialog::Scan(ScanDialog::new(defaults))),
            Command::SharedLogin => {
                self.dialog = Some(Dialog::SharedLogin(LoginForm::new(
                    self.library.shared.as_ref(),
                )));
            }
            Command::Import => self.import_file(),
            Command::Refresh => {
                self.refresh_streams();
                self.show_notice(NoticeKind::Success, "Reconnecting all cameras.");
            }
        }
    }

    /// "Import": picks a camera file (the `cameras.json` format) and merges its
    /// cameras into the library.
    fn import_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import cameras")
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        match import::load_import_file(&path) {
            Ok(outcome) => {
                let had_shared = self.library.shared.is_some();
                let merge = store::merge_imported(
                    &mut self.library.data,
                    &outcome.cameras,
                    &mut self.library.shared,
                );
                let mut problems = Vec::new();
                if merge.shared_defined {
                    if let Some(shared) = &self.library.shared {
                        if let Err(err) = credentials::save_shared(shared) {
                            problems.push(err);
                        }
                    }
                }
                for (id, login) in merge.own {
                    if let Err(err) = credentials::save_for_camera(&id, &login) {
                        problems.push(err);
                    }
                    self.library.own.insert(id, login);
                }
                self.persist_library();
                self.rebuild_cameras();

                if let Some(problem) = problems.first() {
                    self.show_notice(
                        NoticeKind::Error,
                        format!("Cameras imported, but the login was not saved: {problem}"),
                    );
                } else {
                    let mut text = format!("{} camera(s) imported", merge.added);
                    if merge.duplicates > 0 {
                        text.push_str(&format!(", {} were already in the list", merge.duplicates));
                    }
                    if outcome.rejected_count > 0 {
                        text.push_str(&format!(", {} invalid entry(ies)", outcome.rejected_count));
                    }
                    if merge.shared_defined && !had_shared {
                        text.push_str(". The login from the URLs became the default login");
                    }
                    text.push('.');
                    self.show_notice(NoticeKind::Success, text);
                }
            }
            Err(err) => self.show_notice(NoticeKind::Error, format!("Could not import: {err}")),
        }
    }

    /// Applies what the user confirmed in a dialog.
    fn apply_dialog_action(&mut self, action: DialogAction) {
        match action {
            DialogAction::None => return,
            DialogAction::Close => {}
            DialogAction::SaveCamera { camera, own } => {
                let id = camera.id.clone();
                let label = camera.name.clone().unwrap_or_else(|| camera.address());
                let login_saved = match &own {
                    Some(login) => credentials::save_for_camera(&id, login),
                    None => credentials::delete_for_camera(&id),
                };
                match own {
                    Some(login) => {
                        self.library.own.insert(id.clone(), login);
                    }
                    None => {
                        self.library.own.remove(&id);
                    }
                }

                let cameras = &mut self.library.data.cameras;
                let edited = match cameras.iter_mut().find(|existing| existing.id == id) {
                    Some(existing) => {
                        let moved = !existing.same_stream(&camera);
                        *existing = camera;
                        Some(moved)
                    }
                    None => {
                        cameras.push(camera);
                        None
                    }
                };
                // New address: the last frame and status belonged to the old
                // camera.
                if edited == Some(true) {
                    self.textures.remove(&id);
                    self.statuses.remove(&id);
                }
                self.persist_library();
                self.rebuild_cameras();

                match login_saved {
                    Err(err) => self.show_notice(
                        NoticeKind::Error,
                        format!("\"{label}\" was saved, but the login was not: {err}"),
                    ),
                    Ok(()) if edited.is_some() => {
                        self.show_notice(NoticeKind::Success, format!("\"{label}\" updated."))
                    }
                    Ok(()) => self.show_notice(NoticeKind::Success, format!("\"{label}\" added.")),
                }
            }
            DialogAction::SaveSharedLogin(login) => match credentials::save_shared(&login) {
                Ok(()) => {
                    self.library.shared = Some(login);
                    self.rebuild_cameras();
                    self.show_notice(
                        NoticeKind::Success,
                        "Default login saved to the Windows Credential Manager.",
                    );
                }
                Err(err) => {
                    self.show_notice(NoticeKind::Error, format!("The login was not saved: {err}"));
                    return;
                }
            },
            DialogAction::AddCameras { cameras, scan } => {
                let added = cameras.len();
                self.library.data.cameras.extend(cameras);
                self.library.data.scan = scan;
                self.persist_library();
                self.rebuild_cameras();
                self.show_notice(NoticeKind::Success, format!("{added} camera(s) added."));
            }
            DialogAction::RemoveCamera(id) => {
                let label = self.camera_label(&id);
                self.library.data.cameras.retain(|camera| camera.id != id);
                if self.library.own.remove(&id).is_some() {
                    if let Err(err) = credentials::delete_for_camera(&id) {
                        tracing::warn!(error = %err, "the removed camera's own login was not deleted from the vault");
                    }
                }
                self.persist_library();
                self.rebuild_cameras();
                self.show_notice(NoticeKind::Success, format!("\"{label}\" removed."));
            }
        }
        self.dialog = None;
    }

    /// Notice bar (Windows 11 InfoBar) at the top of the content area: the
    /// missing default login or the last error. It stays until closed;
    /// confirmations are toasts instead (see `draw_toast`). The text wraps
    /// inside the space left by the icon and the button, so nothing overlaps at
    /// any window width.
    fn draw_notice_bar(&mut self, ctx: &egui::Context) {
        let palette = self.look.palette;
        let icons = self.look.icons;
        let missing_login = self.library.missing_shared_login();
        if self.notice.is_none() && !missing_login {
            return;
        }

        let ppp = ctx.pixels_per_point();
        let skin = widgets::Skin {
            palette: &palette,
            icons,
        };
        let mut define_login = false;
        let mut dismiss = false;
        let _bar = egui::TopBottomPanel::top("notice_bar")
            .frame(
                egui::Frame::none()
                    .fill(self.panel_fill())
                    .inner_margin(egui::Margin {
                        left: GAP,
                        right: GAP,
                        top: GAP,
                        bottom: 0.0,
                    }),
            )
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.add_enabled_ui(self.dialog.is_none(), |ui| {
                    let (text, fill, icon, icon_color) = match &self.notice {
                        Some(notice) => (
                            notice.text.as_str(),
                            palette.notice_error,
                            theme::ICON_WARNING,
                            palette.critical_text,
                        ),
                        None => (
                            "Set the default login so the app can connect to the cameras.",
                            palette.notice_info,
                            theme::ICON_INFO,
                            palette.accent,
                        ),
                    };
                    let has_error = self.notice.is_some();
                    egui::Frame::none()
                        .fill(fill)
                        .stroke(hairline_stroke(ppp, palette.control_stroke))
                        .rounding(4.0)
                        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.horizontal(|ui| {
                                if icons {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(16.0, 16.0),
                                        egui::Sense::hover(),
                                    );
                                    widgets::paint_icon(ui, icon, 16.0, rect.center(), icon_color);
                                }
                                // The button on the right has a known width; the
                                // text gets all the rest and wraps inside it.
                                let button_width = if has_error { 32.0 } else { 104.0 };
                                let text_width =
                                    (ui.available_width() - button_width - 8.0).max(40.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(text_width, 0.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        ui.set_width(text_width);
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(text).color(palette.text),
                                            )
                                            .wrap(),
                                        );
                                    },
                                );
                                if has_error {
                                    let close = widgets::icon_button(
                                        ui,
                                        &skin,
                                        theme::ICON_CLOSE,
                                        "x",
                                        32.0,
                                        false,
                                        false,
                                    )
                                    .on_hover_text("Close");
                                    if close.clicked() {
                                        dismiss = true;
                                    }
                                } else if ui
                                    .add_sized([button_width, 32.0], egui::Button::new("Set login"))
                                    .clicked()
                                {
                                    define_login = true;
                                }
                            });
                        });
                });
            });

        probe!(self.probe, "notice", _bar.response.rect);
        if dismiss {
            self.notice = None;
        }
        if define_login {
            self.run_command(Command::SharedLogin);
        }
    }

    /// Toast: a short confirmation popup, centered at the bottom of the
    /// content area (not of the window: it never covers the sidebar), over
    /// everything else. It fades in, stays for `TOAST_DURATION` and fades out
    /// by itself; it does not take clicks, so it never gets in the way of the
    /// tiles under it.
    fn draw_toast(&mut self, ctx: &egui::Context) {
        let Some(toast) = &self.toast else {
            return;
        };
        let Some(opacity) = toast_opacity(toast.since.elapsed()) else {
            self.toast = None;
            return;
        };

        let palette = self.look.palette;
        let icons = self.look.icons;
        let ppp = ctx.pixels_per_point();
        let content = self.content_rect;
        // Never wider than the content area, whatever the window size.
        let max_width = TOAST_MAX_WIDTH.min((content.width() - 4.0 * GAP).max(120.0));
        let _shown = egui::Area::new(egui::Id::new("simple_rtsp_viewer_toast"))
            .order(egui::Order::Tooltip)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(snap_pos(
                egui::pos2(content.center().x, content.bottom() - 2.0 * GAP),
                ppp,
            ))
            .interactable(false)
            .show(ctx, |ui| {
                // One opacity for everything drawn in the toast (background,
                // shadow, icon and text).
                ui.set_opacity(opacity);
                egui::Frame::none()
                    .fill(palette.dialog_background)
                    .stroke(hairline_stroke(ppp, palette.control_stroke))
                    .rounding(8.0)
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 4.0),
                        blur: 16.0,
                        spread: 0.0,
                        color: egui::Color32::from_black_alpha(70),
                    })
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0))
                    .show(ui, |ui| {
                        ui.set_max_width(max_width);
                        ui.horizontal(|ui| {
                            if icons {
                                ui.label(
                                    egui::RichText::new(theme::ICON_SUCCESS)
                                        .font(theme::icon_font(16.0))
                                        .color(palette.success_text),
                                );
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&toast.text).color(palette.text),
                                )
                                .wrap(),
                            );
                        });
                    });
            });
        probe!(self.probe, "toast", _shown.response.rect);

        // Keeps the fade animation running while nothing else repaints.
        ctx.request_repaint_after(Duration::from_millis(16));
    }

    /// Open dialog, over the darkened rest of the window.
    fn draw_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };

        // Darkens the window behind the dialog, above the panels and below the
        // egui windows.
        let screen = ctx.screen_rect();
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::PanelResizeLine,
            egui::Id::new("dialog_scrim"),
        ))
        .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(110));

        let env = DialogEnv {
            palette: &self.look.palette,
            shared: self.library.shared.as_ref(),
            cameras: &self.library.data.cameras,
        };
        let action = dialog.show(ctx, &env);
        self.apply_dialog_action(action);
    }

    /// Panel background: transparent with Mica, the theme color without it.
    fn panel_fill(&self) -> egui::Color32 {
        if self.look.mica {
            egui::Color32::TRANSPARENT
        } else {
            self.look.palette.window_background
        }
    }
}

impl eframe::App for SimpleRtspViewerApp {
    /// Color the window is cleared with before each frame: transparent with
    /// Mica (so the effect shows), the theme background color without it.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.look.mica {
            [0.0; 4]
        } else {
            self.look
                .palette
                .window_background
                .to_normalized_gamma_f32()
        }
    }

    /// Main UI loop, called on every `eframe` frame. Drains the pending events
    /// of each camera, redraws the interface (handling drag-and-drop) and
    /// requests an immediate repaint, to keep the video display responsive even
    /// without user interaction (Requirements 2.4, 2.7, 3.5).
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.sync_theme(ctx, frame);
        let received_update = self.ui(ctx);
        self.schedule_repaint(ctx, received_update);
    }
}

impl SimpleRtspViewerApp {
    /// Draws one frame of the interface, from the cameras' events to the
    /// dialogs. Returns whether a frame or a status change arrived. Separate
    /// from `update` (which also follows the system theme) so the layout tests
    /// can run it without a window.
    ///
    /// Order matters: the panels take their space from the window in the order
    /// they are drawn (the sidebar first, then the notice bar, then what is
    /// left for the content), and the dialog and the toast are drawn last, over
    /// everything.
    fn ui(&mut self, ctx: &egui::Context) -> bool {
        let received_update = self.drain_camera_events(ctx);
        self.handle_number_shortcuts(ctx);
        self.draw_sidebar(ctx);
        self.draw_notice_bar(ctx);
        self.draw_content(ctx);
        // After the content, which sees the open dialog and stays inactive in
        // this frame even if the dialog closes now (e.g. Esc belongs only to the
        // dialog).
        self.draw_dialog(ctx);
        self.draw_toast(ctx);
        received_update
    }

    fn schedule_repaint(&self, ctx: &egui::Context, received_update: bool) {
        // Avoids a continuous repaint busy-loop (which would keep CPU/GPU
        // always busy, against Requirements 7.2/7.3). egui already schedules an
        // immediate repaint in response to user interaction (e.g.
        // drag-and-drop); here we only make sure the video channels are drained
        // periodically: if some event arrived in this frame, schedule an
        // immediate repaint to keep up with the stream; otherwise, poll at a
        // modest rate (slower when no camera is connected).
        if received_update {
            ctx.request_repaint();
        } else {
            let any_connected = self
                .statuses
                .values()
                .any(|status| *status == ConnectionStatus::Connected);
            let poll_interval = if any_connected {
                std::time::Duration::from_millis(33)
            } else {
                std::time::Duration::from_millis(250)
            };
            ctx.request_repaint_after(poll_interval);
        }
    }
}

#[cfg(test)]
mod layout_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::CameraAuth;

    fn camera(url: &str) -> Camera {
        Camera {
            id: url.to_string(),
            rtsp_url: url.to_string(),
            name: None,
            substream_url: None,
        }
    }

    fn stored(id: &str, main_path: &str, sub_path: Option<&str>) -> store::StoredCamera {
        // Port 1 on localhost: without a server, the StreamTasks just keep
        // trying to connect.
        store::StoredCamera {
            id: id.to_string(),
            name: None,
            host: "127.0.0.1".to_string(),
            port: 1,
            main_path: main_path.to_string(),
            sub_path: sub_path.map(str::to_string),
            auth: CameraAuth::None,
        }
    }

    /// Test app with the given library, inside a tokio runtime.
    fn with_app(cameras: Vec<store::StoredCamera>, test: impl FnOnce(&mut SimpleRtspViewerApp)) {
        let runtime = tokio::runtime::Runtime::new().expect("failed to create the test runtime");
        let guard = runtime.enter();
        let handle = StreamManagerHandle::new();
        let library = CameraLibrary {
            path: std::env::temp_dir().join("library_ui_test_unused.json"),
            data: store::StoreData {
                cameras,
                scan: store::ScanSettings::default(),
            },
            shared: None,
            own: HashMap::new(),
        };
        let mut app = SimpleRtspViewerApp::new(library, handle.clone());
        // Never open the real audio device in tests.
        app.audio.open = || Ok(AudioOutput::detached(48_000));
        test(&mut app);
        drop(app);
        runtime.block_on(handle.shutdown());
        drop(guard);
        runtime.shutdown_timeout(Duration::from_secs(2));
    }

    #[test]
    fn display_size_is_the_exact_tile_size_in_even_physical_pixels() {
        // 300x170 points on a screen with 1.5 scale = 450x255 pixels: never
        // larger than the tile, even numbers -> 450x254.
        assert_eq!(
            display_size_for(egui::vec2(300.0, 170.0), 1.5),
            DisplaySize {
                width: 450,
                height: 254
            }
        );
        // An exact size does not change.
        assert_eq!(
            display_size_for(egui::vec2(640.0, 352.0), 1.0),
            DisplaySize {
                width: 640,
                height: 352
            }
        );
        // A tile slightly short of a whole pixel does not round up: the frame
        // would then be larger than the tile and have to be resampled.
        assert_eq!(
            display_size_for(egui::vec2(640.9, 360.9), 1.0),
            DisplaySize {
                width: 640,
                height: 360
            }
        );
        // An empty tile (minimized window) still asks for at least 2x2.
        assert_eq!(
            display_size_for(egui::vec2(0.0, 0.0), 1.0),
            DisplaySize {
                width: 2,
                height: 2
            }
        );
    }

    fn open_url(app: &SimpleRtspViewerApp, id: &str) -> String {
        app.streams[id].url.clone()
    }

    #[test]
    fn enlarged_camera_keeps_the_substream_until_high_resolution_is_asked() {
        let cameras = vec![
            stored("a", "/main-a", Some("/sub-a")),
            stored("b", "/main-b", None),
            stored("c", "/main-c", Some("/sub-c")),
        ];
        with_app(cameras, |app| {
            // In the grid: substream when there is one, main stream when not.
            assert_eq!(open_url(app, "a"), "rtsp://127.0.0.1:1/sub-a");
            assert_eq!(open_url(app, "b"), "rtsp://127.0.0.1:1/main-b");

            // Enlarged: still on the substream.
            app.set_enlarged(Some("a".to_string()));
            assert_eq!(app.enlarged_index(), Some(0));
            assert_eq!(open_url(app, "a"), "rtsp://127.0.0.1:1/sub-a");

            // High resolution: only camera A switches to the main stream.
            app.set_high_resolution(true);
            assert_eq!(open_url(app, "a"), "rtsp://127.0.0.1:1/main-a");
            assert_eq!(open_url(app, "c"), "rtsp://127.0.0.1:1/sub-c");
            app.set_high_resolution(false);
            assert_eq!(open_url(app, "a"), "rtsp://127.0.0.1:1/sub-a");

            // Leaving the enlarged view turns high resolution off.
            app.set_high_resolution(true);
            app.set_enlarged(None);
            assert_eq!(app.enlarged_index(), None);
            assert_eq!(open_url(app, "a"), "rtsp://127.0.0.1:1/sub-a");
            app.set_enlarged(Some("c".to_string()));
            assert_eq!(open_url(app, "c"), "rtsp://127.0.0.1:1/sub-c");
        });
    }

    #[test]
    fn number_positions_open_switch_and_close_the_enlarged_camera() {
        let cameras = vec![stored("a", "/main-a", None), stored("b", "/main-b", None)];
        with_app(cameras, |app| {
            app.toggle_enlarged_at(0);
            assert_eq!(app.enlarged_index(), Some(0));
            // Another number goes straight to that camera.
            app.toggle_enlarged_at(1);
            assert_eq!(app.enlarged_index(), Some(1));
            // The same number again goes back to the grid.
            app.toggle_enlarged_at(1);
            assert_eq!(app.enlarged_index(), None);
            // A position without a camera does nothing.
            app.toggle_enlarged_at(5);
            assert_eq!(app.enlarged_index(), None);
        });
    }
    #[test]
    fn zoom_is_reset_when_the_enlarged_camera_changes() {
        let cameras = vec![stored("a", "/main-a", None), stored("b", "/main-b", None)];
        with_app(cameras, |app| {
            let video = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 200.0));
            app.set_enlarged(Some("a".to_string()));
            app.zoom.scroll(300.0, egui::pos2(100.0, 50.0), video);
            assert!(!app.zoom.is_reset());
            app.set_enlarged(Some("b".to_string()));
            assert!(app.zoom.is_reset());
            app.zoom.scroll(300.0, egui::pos2(100.0, 50.0), video);
            app.set_enlarged(None);
            assert!(app.zoom.is_reset());
        });
    }

    #[test]
    fn library_changes_open_close_and_switch_streams() {
        with_app(vec![stored("a", "/main-a", None)], |app| {
            // New camera: opens its stream.
            app.library.data.cameras.push(stored("b", "/main-b", None));
            app.rebuild_cameras();
            assert_eq!(open_url(app, "b"), "rtsp://127.0.0.1:1/main-b");

            // Edited camera (another path): switches the stream, same `id`.
            app.library.data.cameras[1].main_path = "/new-b".to_string();
            app.rebuild_cameras();
            assert_eq!(open_url(app, "b"), "rtsp://127.0.0.1:1/new-b");

            // Removed enlarged camera: closes the stream and goes back to the
            // grid.
            app.set_enlarged(Some("a".to_string()));
            app.library.data.cameras.retain(|camera| camera.id != "a");
            app.rebuild_cameras();
            assert!(!app.streams.contains_key("a"));
            assert!(!app.statuses.contains_key("a"));
            assert_eq!(app.enlarged, None);
            assert_eq!(app.cameras.len(), 1);
        });
    }

    fn sink_of(app: &SimpleRtspViewerApp, id: &str) -> Option<AudioSink> {
        app.streams[id].audio.borrow().clone()
    }

    #[test]
    fn sound_follows_the_enlarged_camera_and_the_mute_button() {
        let cameras = vec![
            stored("a", "/main-a", Some("/sub-a")),
            stored("b", "/main-b", None),
        ];
        with_app(cameras, |app| {
            // In the grid nobody plays, and the device is not even opened.
            assert!(sink_of(app, "a").is_none() && sink_of(app, "b").is_none());
            assert!(app.audio.output.is_none());
            assert!(!app.audio.playing);

            // Enlarging starts only that camera's sound.
            app.set_enlarged(Some("a".to_string()));
            assert!(sink_of(app, "a").is_some());
            assert!(sink_of(app, "b").is_none());
            assert!(app.audio.playing);

            // Muting stops it (the stream drops the audio packets); unmuting
            // brings it back.
            app.set_muted(true);
            assert!(sink_of(app, "a").is_none());
            assert!(!app.audio.playing);
            app.set_muted(false);
            assert!(sink_of(app, "a").is_some());
            assert!(app.audio.playing);

            // Switching to the main stream opens a new stream that keeps the
            // sound, and so does a refresh.
            let before = app.streams["a"].url.clone();
            app.set_high_resolution(true);
            assert_ne!(app.streams["a"].url, before);
            assert!(sink_of(app, "a").is_some());
            app.refresh_streams();
            assert!(sink_of(app, "a").is_some());

            // Enlarging another camera moves the sound to it.
            app.set_enlarged(Some("b".to_string()));
            assert!(sink_of(app, "a").is_none());
            assert!(sink_of(app, "b").is_some());

            // Back to the grid: silence, and the device is paused.
            app.set_enlarged(None);
            assert!(sink_of(app, "a").is_none() && sink_of(app, "b").is_none());
            assert!(!app.audio.playing);
        });
    }

    #[test]
    fn mute_and_volume_are_kept_across_cameras() {
        with_app(
            vec![stored("a", "/a", None), stored("b", "/b", None)],
            |app| {
                app.set_enlarged(Some("a".to_string()));
                app.set_muted(true);
                app.set_volume(0.4);

                // Still muted on the next camera, and the volume stays.
                app.set_enlarged(Some("b".to_string()));
                assert!(sink_of(app, "b").is_none());
                assert_eq!(app.audio.volume, 0.4);

                // Out-of-range volumes are clamped.
                app.set_volume(3.0);
                assert_eq!(app.audio.volume, 1.0);
            },
        );
    }

    #[test]
    fn removing_the_enlarged_camera_stops_the_sound() {
        with_app(
            vec![stored("a", "/a", None), stored("b", "/b", None)],
            |app| {
                app.set_enlarged(Some("a".to_string()));
                assert!(app.audio.playing);

                app.library.data.cameras.retain(|camera| camera.id != "a");
                app.rebuild_cameras();
                assert_eq!(app.enlarged, None);
                assert!(!app.audio.playing);
                assert!(sink_of(app, "b").is_none());
            },
        );
    }

    #[test]
    fn a_missing_audio_device_is_reported_once_and_cameras_stay_silent() {
        with_app(vec![stored("a", "/a", None)], |app| {
            app.audio.open = || Err("no audio output device".to_string());

            app.set_enlarged(Some("a".to_string()));
            assert!(app.audio.open_failed);
            assert!(sink_of(app, "a").is_none());
            let first = app.notice.as_ref().map(|notice| notice.text.clone());
            assert_eq!(
                first.as_deref(),
                Some("The camera sound is unavailable: no audio output device")
            );

            // Enlarging again does not repeat the error.
            app.notice = None;
            app.set_enlarged(None);
            app.set_enlarged(Some("a".to_string()));
            assert!(app.notice.is_none());

            // Once the device works, the next enlarge brings the sound.
            app.audio.open = || Ok(AudioOutput::detached(48_000));
            app.set_enlarged(None);
            app.set_enlarged(Some("a".to_string()));
            assert!(!app.audio.open_failed);
            assert!(sink_of(app, "a").is_some());
        });
    }

    #[test]
    fn toast_fades_in_stays_solid_then_fades_out_and_ends() {
        let ms = Duration::from_millis;
        // Starts invisible and fades in.
        assert_eq!(toast_opacity(ms(0)), Some(0.0));
        let half = toast_opacity(TOAST_FADE_IN / 2).unwrap();
        assert!((half - 0.5).abs() < 1e-3);
        // Solid in the middle.
        assert_eq!(toast_opacity(TOAST_DURATION / 2), Some(1.0));
        // Fades out during the last `TOAST_FADE_OUT`.
        let fading = toast_opacity(TOAST_DURATION - TOAST_FADE_OUT / 2).unwrap();
        assert!((fading - 0.5).abs() < 1e-3);
        // Gone once the duration is over.
        assert_eq!(toast_opacity(TOAST_DURATION), None);
        assert_eq!(toast_opacity(TOAST_DURATION + ms(1)), None);
    }

    #[test]
    fn toast_opacity_is_always_between_zero_and_one() {
        let mut elapsed = Duration::ZERO;
        while elapsed < TOAST_DURATION {
            let opacity = toast_opacity(elapsed).expect("still on screen");
            assert!((0.0..=1.0).contains(&opacity), "{opacity} at {elapsed:?}");
            elapsed += Duration::from_millis(7);
        }
    }

    #[test]
    fn successes_become_toasts_and_errors_stay_in_the_notice_bar() {
        with_app(vec![], |app| {
            app.show_notice(NoticeKind::Success, "Reconnecting all cameras.");
            assert_eq!(
                app.toast.as_ref().map(|toast| toast.text.as_str()),
                Some("Reconnecting all cameras.")
            );
            assert!(app.notice.is_none());

            app.show_notice(NoticeKind::Error, "The login was not saved.");
            assert_eq!(
                app.notice.as_ref().map(|notice| notice.text.as_str()),
                Some("The login was not saved.")
            );
            // The error does not take the toast away.
            assert!(app.toast.is_some());

            // A new toast replaces the one on screen.
            app.show_notice(NoticeKind::Success, "Camera added.");
            assert_eq!(
                app.toast.as_ref().map(|toast| toast.text.as_str()),
                Some("Camera added.")
            );
        });
    }

    #[test]
    fn refresh_shows_a_toast_not_a_notice() {
        with_app(vec![stored("a", "/main-a", None)], |app| {
            app.run_command(Command::Refresh);
            assert!(app.toast.is_some());
            assert!(app.notice.is_none());
        });
    }

    #[test]
    fn refresh_reopens_every_stream_with_the_same_url() {
        let cameras = vec![
            stored("a", "/main-a", Some("/sub-a")),
            stored("b", "/main-b", None),
        ];
        with_app(cameras, |app| {
            let before: HashMap<String, watch::Receiver<ConnectionStatus>> = app
                .streams
                .iter()
                .map(|(id, stream)| (id.clone(), stream.status.clone()))
                .collect();

            app.refresh_streams();

            for (id, old_status) in &before {
                let stream = &app.streams[id];
                assert!(
                    !stream.status.same_channel(old_status),
                    "{id} was not reopened"
                );
            }
            assert_eq!(open_url(app, "a"), "rtsp://127.0.0.1:1/sub-a");
            assert_eq!(open_url(app, "b"), "rtsp://127.0.0.1:1/main-b");
        });
    }

    #[test]
    fn reorder_is_mirrored_in_the_library() {
        let cameras = vec![
            stored("a", "/a", None),
            stored("b", "/b", None),
            stored("c", "/c", None),
        ];
        with_app(cameras, |app| {
            app.library.path = std::env::temp_dir().join(format!(
                "library_ui_reorder_{}.json",
                store::new_camera_id()
            ));
            app.apply_reorder(0, 2);
            let ids: Vec<&str> = app
                .library
                .data
                .cameras
                .iter()
                .map(|camera| camera.id.as_str())
                .collect();
            assert_eq!(ids, ["c", "b", "a"]);
            let shown: Vec<&str> = app
                .cameras
                .iter()
                .map(|camera| camera.id.as_str())
                .collect();
            assert_eq!(shown, ["c", "b", "a"]);
        });
    }

    #[test]
    fn successful_reorder_updates_order_and_requests_persistence() {
        let cameras = vec![camera("rtsp://a"), camera("rtsp://b"), camera("rtsp://c")];

        let (reordered, changed) = reorder_and_should_persist(&cameras, 0, 2);

        assert!(changed);
        assert_eq!(
            reordered,
            vec![camera("rtsp://c"), camera("rtsp://b"), camera("rtsp://a")]
        );
    }

    #[test]
    fn drop_out_of_bounds_keeps_original_order_without_persistence() {
        let cameras = vec![camera("rtsp://a"), camera("rtsp://b")];

        let (reordered, changed) = reorder_and_should_persist(&cameras, 0, 5);

        assert!(!changed);
        assert_eq!(reordered, cameras);
    }

    #[test]
    fn drop_on_same_position_keeps_original_order_without_persistence() {
        let cameras = vec![camera("rtsp://a"), camera("rtsp://b")];

        let (reordered, changed) = reorder_and_should_persist(&cameras, 1, 1);

        assert!(!changed);
        assert_eq!(reordered, cameras);
    }

    #[test]
    fn empty_camera_list_never_triggers_persistence() {
        let cameras: Vec<Camera> = Vec::new();

        let (reordered, changed) = reorder_and_should_persist(&cameras, 0, 0);

        assert!(!changed);
        assert!(reordered.is_empty());
    }
}
