//! SimpleRtspViewerApp: eframe/egui integration.
//!
//! This module implements the `Viewing_Grid`: it builds the initial app state
//! from the already reconciled camera list (`import` +
//! `layout::restore_order`), drains without blocking the events produced by the
//! `StreamTask`s of the `Stream_Manager`, draws the grid of `Camera_Tile`s and
//! handles drag-and-drop reordering, triggering the async layout persistence
//! when the in-memory order changes.
//!
//! No network I/O, decoding or file write happens directly on the UI thread:
//! communication with the `Stream_Manager` goes through channels
//! (`tokio::sync::mpsc`) and the layout persistence is triggered with
//! `tokio::spawn` (fire-and-forget), as in design.md.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use crate::grid;
use crate::layout;
use crate::model::{Camera, CameraFrame, ConnectionStatus, DisplaySize};
use crate::streaming::{StreamChannels, StreamManagerHandle};
use crate::theme;

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

/// Step, in pixels, by which the display size reported to the `StreamTask`s is
/// rounded up. Without it, dragging the window border would change the size on
/// every frame and each camera would recreate its pixel converter all the time.
const DISPLAY_SIZE_STEP: u32 = 32;

/// Size, in physical pixels, at which a tile of `size_points` (egui points)
/// appears on screen, rounded up to multiples of `DISPLAY_SIZE_STEP`.
fn display_size_for(size_points: egui::Vec2, pixels_per_point: f32) -> DisplaySize {
    let round_up = |points: f32| {
        let pixels = (points * pixels_per_point).ceil().max(1.0) as u32;
        pixels.div_ceil(DISPLAY_SIZE_STEP) * DISPLAY_SIZE_STEP
    };
    DisplaySize {
        width: round_up(size_points.x),
        height: round_up(size_points.y),
    }
}

/// Stream currently open for a camera and the UI's ends of the matching
/// `StreamTask` channels.
struct ActiveStream {
    /// Open URL: the substream in the grid or the main one with the camera
    /// enlarged.
    url: String,
    frames: mpsc::Receiver<CameraFrame>,
    status: watch::Receiver<ConnectionStatus>,
    /// Size at which the camera is being shown, read by the `StreamTask`.
    display_size: watch::Sender<Option<DisplaySize>>,
}

/// What the user did on a grid tile in this frame.
enum BlockAction {
    None,
    /// Dropped tile `from` onto tile `to` (drag and drop).
    Drop {
        from: usize,
        to: usize,
    },
    /// Double-clicked the tile to enlarge it.
    Enlarge,
}

/// State of a drag-and-drop operation in progress on the `Viewing_Grid`
/// (Requirements 4.1, 4.2, 4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DragState {
    /// Index of the `Camera_Tile` being dragged.
    from: usize,
    /// Index of the `Camera_Tile` currently under the cursor (candidate target), if
    /// any.
    hover_to: Option<usize>,
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

/// Largest rectangle with the aspect ratio of `content_size` that fits in
/// `container`, centered in it. Used to draw the video without distorting the
/// image: one dimension fills the whole tile and the other leaves equal space
/// on both sides. Non-positive sizes (no valid frame or empty tile) return
/// `container` itself, avoiding a division by zero.
fn fit_preserving_aspect(container: egui::Rect, content_size: egui::Vec2) -> egui::Rect {
    if content_size.x <= 0.0
        || content_size.y <= 0.0
        || container.width() <= 0.0
        || container.height() <= 0.0
    {
        return container;
    }

    let scale = (container.width() / content_size.x).min(container.height() / content_size.y);
    egui::Rect::from_center_size(container.center(), content_size * scale)
}

/// Windows dark theme; without information from the system, assumes dark.
fn is_dark(system_theme: Option<eframe::Theme>) -> bool {
    system_theme.is_none_or(|theme| theme == eframe::Theme::Dark)
}

/// Pill-shaped status notice, centered in the tile: an indicator (spinner or
/// colored dot) and the text.
fn paint_status_pill(
    ui: &egui::Ui,
    painter: &egui::Painter,
    tile: egui::Rect,
    label: &str,
    color: Option<egui::Color32>,
    spinner: bool,
) {
    const MARKER: f32 = 14.0;
    let label = painter.layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(13.0),
        egui::Color32::WHITE,
    );
    let has_marker = spinner || color.is_some();
    let marker_space = if has_marker { MARKER + 8.0 } else { 0.0 };
    let pill = egui::Rect::from_center_size(
        tile.center(),
        egui::vec2(marker_space + label.size().x + 28.0, 30.0),
    );
    painter.rect_filled(pill, 15.0, theme::OVERLAY_CHIP);

    let mut x = pill.left() + 14.0;
    if has_marker {
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
            painter.circle_filled(marker.center(), 4.0, color);
        }
        x += marker_space;
    }
    let y = pill.center().y - label.size().y / 2.0;
    painter.galley(egui::pos2(x, y), label, egui::Color32::WHITE);
}

/// Label with the camera name in the bottom-left corner of the tile, with a
/// green dot when the video is live. Omitted on tiles too narrow for it.
fn paint_name_chip(painter: &egui::Painter, tile: egui::Rect, name: &str, live: bool) {
    let label = painter.layout_no_wrap(
        name.to_owned(),
        egui::FontId::proportional(13.0),
        egui::Color32::WHITE,
    );
    let dot_space = if live { 14.0 } else { 0.0 };
    let size = egui::vec2(dot_space + label.size().x + 16.0, label.size().y + 8.0);
    if size.x > tile.width() - 16.0 || size.y > tile.height() - 16.0 {
        return;
    }
    let chip = egui::Rect::from_min_size(
        egui::pos2(tile.left() + 8.0, tile.bottom() - 8.0 - size.y),
        size,
    );
    painter.rect_filled(chip, 4.0, theme::OVERLAY_CHIP);

    let mut x = chip.left() + 8.0;
    if live {
        painter.circle_filled(egui::pos2(x + 3.0, chip.center().y), 3.0, theme::SUCCESS);
        x += dot_space;
    }
    let y = chip.center().y - label.size().y / 2.0;
    painter.galley(egui::pos2(x, y), label, egui::Color32::WHITE);
}

/// `SimpleRtspViewerApp`: integration of the `Viewing_Grid` with
/// `eframe`/`egui`.
pub struct SimpleRtspViewerApp {
    /// Current in-memory camera order (reflected in the grid).
    cameras: Vec<Camera>,
    /// Stream open for each camera, keyed by the camera's main `rtsp_url`
    /// (which identifies it even when the open one is the substream). The
    /// status comes in a `watch`, which always holds the latest value, so no
    /// status change is lost even with the frame channel full.
    streams: HashMap<String, ActiveStream>,
    /// Last GPU texture loaded for each camera, keyed by `rtsp_url`.
    textures: HashMap<String, egui::TextureHandle>,
    /// Last `ConnectionStatus` read for each camera, keyed by `rtsp_url`.
    statuses: HashMap<String, ConnectionStatus>,
    /// State of the drag-and-drop operation in progress, if any.
    drag_state: Option<DragState>,
    /// Enlarged camera (filling the whole window), by its main `rtsp_url`;
    /// `None` shows the grid.
    enlarged: Option<String>,
    /// When the current camera was enlarged, to show the hint on how to go back
    /// for a few seconds.
    enlarged_since: Option<Instant>,
    /// Handle to the `Stream_Manager`, used to open and close streams.
    stream_manager_handle: StreamManagerHandle,
    /// Path of the Layout_Config_File, used to persist the grid order.
    layout_path: PathBuf,
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
    /// Builds the initial app state from `cameras` (already reconciled by
    /// `layout::restore_order` from the `Config_Importer` result), starting the
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
    pub fn new(
        cameras: Vec<Camera>,
        stream_manager_handle: StreamManagerHandle,
        layout_path: PathBuf,
    ) -> Self {
        let statuses = cameras
            .iter()
            .map(|camera| (camera.rtsp_url.clone(), ConnectionStatus::Disconnected))
            .collect();

        let mut app = Self {
            streams: HashMap::with_capacity(cameras.len()),
            cameras,
            textures: HashMap::new(),
            statuses,
            drag_state: None,
            enlarged: None,
            enlarged_since: None,
            stream_manager_handle,
            layout_path,
            look: Look::new(true),
        };
        app.sync_streams();
        app
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
    /// the main one for the enlarged camera (or one without a substream), the
    /// substream for the others. Only switches what changed.
    fn sync_streams(&mut self) {
        let changes: Vec<(String, String)> = self
            .cameras
            .iter()
            .filter_map(|camera| {
                let key = &camera.rtsp_url;
                let wanted = camera.stream_url(self.enlarged.as_ref() == Some(key));
                let current = self.streams.get(key).map(|stream| stream.url.as_str());
                (current != Some(wanted)).then(|| (key.clone(), wanted.to_string()))
            })
            .collect();

        for (key, url) in changes {
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

        if let Some(previous) = self.streams.remove(key) {
            self.stream_manager_handle.remove_stream(previous.url);
        }

        let (frame_tx, frame_rx) = mpsc::channel(FRAME_CHANNEL_CAPACITY);
        let (status_tx, status_rx) = watch::channel(current_status);
        let (display_tx, display_rx) = watch::channel(current_display);
        self.stream_manager_handle.add_stream(
            url.clone(),
            StreamChannels {
                frames: frame_tx,
                status: status_tx,
                display_size: display_rx,
            },
        );
        self.streams.insert(
            key.to_string(),
            ActiveStream {
                url,
                frames: frame_rx,
                status: status_rx,
                display_size: display_tx,
            },
        );
    }

    /// Enlarges camera `key` (or goes back to the grid, with `None`) and
    /// switches the open streams accordingly.
    fn set_enlarged(&mut self, key: Option<String>) {
        if self.enlarged != key {
            self.enlarged_since = key.as_ref().map(|_| Instant::now());
            self.enlarged = key;
            self.sync_streams();
        }
    }

    /// Index, in `cameras`, of the enlarged camera.
    fn enlarged_index(&self) -> Option<usize> {
        let key = self.enlarged.as_ref()?;
        self.cameras
            .iter()
            .position(|camera| &camera.rtsp_url == key)
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
            let key = &camera.rtsp_url;
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

    /// Draws the `Viewing_Grid`: a "no cameras loaded" message when the list is
    /// empty (Requirement 2.3), or the grid of `Camera_Tile`s sized by
    /// `grid::grid_dimensions` (Requirements 2.1, 2.2), handling drag-and-drop
    /// (Requirements 4.1, 4.2, 4.3) and applying the reorder + persistence when
    /// a valid drop happens.
    fn draw_grid(&mut self, ctx: &egui::Context) {
        // A drag-and-drop operation only exists while a pointer button is held
        // down; when the button is released (or cancelled), egui already clears
        // its own dnd payload, so here we clear the auxiliary visual state kept
        // in `drag_state`.
        if !ctx.input(|i| i.pointer.any_down()) {
            self.drag_state = None;
        }

        // With Mica, the panel is transparent so the effect shows around and
        // between the tiles.
        let panel_fill = if self.look.mica {
            egui::Color32::TRANSPARENT
        } else {
            self.look.palette.window_background
        };
        let panel = egui::Frame::none()
            .fill(panel_fill)
            .inner_margin(egui::Margin::same(GAP));

        egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
            if self.cameras.is_empty() {
                self.paint_empty_state(ui);
                return;
            }

            if let Some(index) = self.enlarged_index() {
                self.enlarged_block(ui, index);
                return;
            }

            let (columns, rows) = grid::grid_dimensions(self.cameras.len());
            let available = ui.available_size();
            // The gaps between the tiles come out of the available size, so the
            // grid fits the panel exactly.
            let cell_size = egui::vec2(
                ((available.x - GAP * (columns - 1) as f32) / columns as f32).max(1.0),
                ((available.y - GAP * (rows - 1) as f32) / rows as f32).max(1.0),
            );

            let mut reorder: Option<(usize, usize)> = None;
            let mut enlarge: Option<usize> = None;

            egui::Grid::new("camera_grid")
                .min_col_width(cell_size.x)
                .min_row_height(cell_size.y)
                .spacing(egui::vec2(GAP, GAP))
                .show(ui, |ui| {
                    for row in 0..rows {
                        for col in 0..columns {
                            let index = row * columns + col;
                            if index >= self.cameras.len() {
                                ui.label("");
                                continue;
                            }
                            match self.camera_block(ui, index, cell_size) {
                                BlockAction::None => {}
                                BlockAction::Drop { from, to } => reorder = Some((from, to)),
                                BlockAction::Enlarge => enlarge = Some(index),
                            }
                        }
                        ui.end_row();
                    }
                });

            if let Some((from, to)) = reorder {
                self.apply_reorder(from, to);
            } else if let Some(index) = enlarge {
                let key = self.cameras[index].rtsp_url.clone();
                self.set_enlarged(Some(key));
            }
        });
    }

    /// Draws the enlarged camera filling the whole panel. Double-click or Esc
    /// returns to the grid.
    fn enlarged_block(&mut self, ui: &mut egui::Ui, index: usize) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
        self.paint_camera(ui, index, rect);
        self.paint_enlarged_hint(ui, rect);

        if response.double_clicked() || ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.set_enlarged(None);
        }
    }

    /// Hint on how to go back to the grid, in the top-right corner of the
    /// enlarged camera, during the first seconds after enlarging.
    fn paint_enlarged_hint(&self, ui: &egui::Ui, rect: egui::Rect) {
        let Some(since) = self.enlarged_since else {
            return;
        };
        let elapsed = since.elapsed();
        if elapsed >= ENLARGED_HINT_DURATION {
            return;
        }
        let remaining = ENLARGED_HINT_DURATION - elapsed;
        let opacity = (remaining.as_secs_f32() / ENLARGED_HINT_FADE.as_secs_f32()).min(1.0);
        let text_color = egui::Color32::WHITE.gamma_multiply(opacity);

        let painter = ui.painter().with_clip_rect(rect);
        let label = painter.layout_no_wrap(
            "Double-click or Esc to go back to the grid".to_owned(),
            egui::FontId::proportional(13.0),
            text_color,
        );
        let icon = self.look.icons.then(|| {
            painter.layout_no_wrap(
                theme::ICON_BACK_TO_WINDOW.to_owned(),
                theme::icon_font(14.0),
                text_color,
            )
        });
        let icon_space = icon.as_ref().map_or(0.0, |icon| icon.size().x + 8.0);

        let size = egui::vec2(icon_space + label.size().x + 24.0, 32.0);
        let chip = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 12.0 - size.x, rect.top() + 12.0),
            size,
        );
        painter.rect_filled(chip, 6.0, theme::OVERLAY_CHIP.gamma_multiply(opacity));

        let mut x = chip.left() + 12.0;
        if let Some(icon) = icon {
            let y = chip.center().y - icon.size().y / 2.0;
            painter.galley(egui::pos2(x, y), icon, text_color);
            x += icon_space;
        }
        let y = chip.center().y - label.size().y / 2.0;
        painter.galley(egui::pos2(x, y), label, text_color);

        // Keeps the fade-out animation running even without new frames.
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }

    /// Screen without cameras: icon, title and how to open a camera file.
    fn paint_empty_state(&self, ui: &egui::Ui) {
        let palette = &self.look.palette;
        let rect = ui.available_rect_before_wrap();
        let painter = ui.painter();
        let center_x = rect.center().x;
        let mut y = rect.center().y - 48.0;

        if self.look.icons {
            painter.text(
                egui::pos2(center_x, y),
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
            "No cameras loaded",
            theme::semibold(20.0),
            palette.text,
        );
        y += 32.0;
        painter.text(
            egui::pos2(center_x, y),
            egui::Align2::CENTER_CENTER,
            "Open the app passing a camera file:",
            egui::FontId::proportional(14.0),
            palette.text_secondary,
        );
        y += 24.0;
        painter.text(
            egui::pos2(center_x, y),
            egui::Align2::CENTER_CENTER,
            "simple-rtsp-viewer.exe cameras.json",
            egui::FontId::monospace(13.0),
            palette.text_secondary,
        );
    }

    /// Draws camera `index` in `rect` and tells its `StreamTask` the size at
    /// which it is shown.
    fn paint_camera(&self, ui: &egui::Ui, index: usize, rect: egui::Rect) {
        let camera = &self.cameras[index];
        let key = &camera.rtsp_url;
        let status = self
            .statuses
            .get(key)
            .copied()
            .unwrap_or(ConnectionStatus::Disconnected);

        self.paint_tile(
            ui,
            rect,
            self.textures.get(key),
            status,
            camera.name.as_deref(),
        );
        self.report_display_size(key, rect.size(), ui.ctx().pixels_per_point());
    }

    /// Draws a single `Camera_Tile` (index `index`) with size `size`, treating
    /// it as both a drag-and-drop source and target (Requirements 2.1, 4.1,
    /// 4.2, 4.3). Reports a valid drop (target different from the source)
    /// finished over this tile, or a double-click to enlarge it.
    fn camera_block(&mut self, ui: &mut egui::Ui, index: usize, size: egui::Vec2) -> BlockAction {
        let item_id = egui::Id::new("simple_rtsp_viewer_camera_block").with(index);

        let response = ui
            .dnd_drag_source(item_id, index, |ui| {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                self.paint_camera(ui, index, rect);
            })
            .response;

        // `dnd_drag_source` only detects dragging; adding click sensing to the
        // same widget allows double-clicking to enlarge. With both, egui tells
        // a click from a drag by the pointer movement.
        let response = response.interact(egui::Sense::click());
        if response.double_clicked() {
            return BlockAction::Enlarge;
        }

        // Border a little more visible when hovered, as on Windows 11 cards.
        if response.hovered() && !response.dragged() {
            ui.painter().rect_stroke(
                response.rect,
                TILE_ROUNDING,
                egui::Stroke::new(1.0_f32, self.look.palette.tile_border_hover),
            );
        }

        // While this tile is being dragged, keeps `drag_state.from` pointing to
        // it, preserving the `hover_to` already detected (if it still belongs
        // to this same drag).
        if response.dragged() {
            let hover_to = self
                .drag_state
                .filter(|state| state.from == index)
                .and_then(|state| state.hover_to);
            self.drag_state = Some(DragState {
                from: index,
                hover_to,
            });
        }

        let mut action = BlockAction::None;

        if let Some(hovered_from) = response.dnd_hover_payload::<usize>() {
            let from = *hovered_from;
            if from != index {
                if let Some(state) = self.drag_state.as_mut() {
                    if state.from == from {
                        state.hover_to = Some(index);
                    }
                }
                // Visual indication of the target position during the drag
                // (Requirement 4.3), in the Windows accent color.
                ui.painter().rect_stroke(
                    response.rect,
                    TILE_ROUNDING,
                    egui::Stroke::new(2.0_f32, self.look.palette.accent),
                );
            }
        }

        if let Some(dragged_from) = response.dnd_release_payload::<usize>() {
            let from = *dragged_from;
            if from != index {
                action = BlockAction::Drop { from, to: index };
            }
            self.drag_state = None;
        }

        action
    }

    /// Paints a `Camera_Tile` as a Windows 11 card: dark background with
    /// rounded corners, the latest video fitted keeping its aspect ratio, the
    /// camera name in a label in the bottom-left corner and the connection
    /// status notice (Requirements 2.4, 2.5, 2.6).
    fn paint_tile(
        &self,
        ui: &egui::Ui,
        rect: egui::Rect,
        texture: Option<&egui::TextureHandle>,
        status: ConnectionStatus,
        name: Option<&str>,
    ) {
        let palette = &self.look.palette;
        let painter = ui.painter().with_clip_rect(rect.expand(1.0));
        painter.rect_filled(rect, TILE_ROUNDING, palette.tile_background);

        if let Some(texture) = texture {
            let mut video = egui::epaint::RectShape::filled(
                fit_preserving_aspect(rect, texture.size_vec2()),
                TILE_ROUNDING,
                egui::Color32::WHITE,
            );
            video.fill_texture_id = texture.id();
            video.uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.add(video);
        }

        // Status notices: distinct from each other and from normal video, which
        // gets no notice (Requirements 2.5, 2.6). Over a frozen video, the
        // video is darkened so the notice stands out.
        let has_video = texture.is_some();
        let dim = |painter: &egui::Painter| {
            painter.rect_filled(rect, TILE_ROUNDING, egui::Color32::from_black_alpha(140));
        };
        match grid::status_indicator(status) {
            grid::IndicatorKind::None => {
                if !has_video {
                    // Connected, but the first frame has not arrived yet.
                    paint_status_pill(ui, &painter, rect, "Connecting…", None, true);
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
                );
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
                );
            }
            grid::IndicatorKind::Disconnected => {
                // Initial state: the first connection is still in progress.
                paint_status_pill(ui, &painter, rect, "Connecting…", None, true);
            }
        }

        if let Some(name) = name {
            let live = has_video && status == ConnectionStatus::Connected;
            paint_name_chip(&painter, rect, name, live);
        }

        painter.rect_stroke(
            rect,
            TILE_ROUNDING,
            egui::Stroke::new(1.0_f32, palette.tile_border),
        );
    }

    /// Applies the `from`/`to` position swap to `self.cameras` and, if the
    /// order actually changed, triggers the async layout persistence
    /// (Requirements 4.4, 5.1). If `to` is invalid or equal to `from`,
    /// `grid::reorder` already returns the original list unchanged and no write
    /// is triggered.
    fn apply_reorder(&mut self, from: usize, to: usize) {
        let (reordered, changed) = reorder_and_should_persist(&self.cameras, from, to);
        self.cameras = reordered;

        if changed {
            self.persist_layout();
        }
    }

    /// Triggers the async (fire-and-forget) write of the `Layout_Config_File`
    /// reflecting the current order of `self.cameras`, without blocking the UI thread
    /// (Requirement 5.1). The disk write (`layout::save_json_atomically`, synchronous)
    /// runs on a thread of tokio's blocking pool via `spawn_blocking`; failures are
    /// only logged, without interrupting the app (Requirement 5.7).
    fn persist_layout(&self) {
        let layout_path = self.layout_path.clone();
        let layout_file = layout::build_layout_file(&self.cameras);

        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                layout::save_json_atomically(&layout_path, &layout_file)
            })
            .await;

            match result {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::error!(error = %err, "failed to save the layout file");
                }
                Err(join_err) => {
                    tracing::error!(error = %join_err, "failed to run the background layout write");
                }
            }
        });
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
    /// of each camera, redraws the grid (handling drag-and-drop) and requests
    /// an immediate repaint, to keep the video display responsive even without
    /// user interaction (Requirements 2.4, 2.7, 3.5).
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.sync_theme(ctx, frame);
        let received_update = self.drain_camera_events(ctx);
        self.draw_grid(ctx);

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
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn camera(url: &str) -> Camera {
        Camera {
            id: url.to_string(),
            rtsp_url: url.to_string(),
            name: None,
            substream_url: None,
        }
    }

    #[test]
    fn display_size_is_in_physical_pixels_rounded_up_to_the_step() {
        // 300x170 points on a screen with 1.5 scale = 450x255 pixels ->
        // 480x256.
        assert_eq!(
            display_size_for(egui::vec2(300.0, 170.0), 1.5),
            DisplaySize {
                width: 480,
                height: 256
            }
        );
        // Exact multiples of the step do not change.
        assert_eq!(
            display_size_for(egui::vec2(640.0, 352.0), 1.0),
            DisplaySize {
                width: 640,
                height: 352
            }
        );
        // An empty tile (minimized window) still asks for at least one step.
        assert_eq!(
            display_size_for(egui::vec2(0.0, 0.0), 1.0),
            DisplaySize {
                width: DISPLAY_SIZE_STEP,
                height: DISPLAY_SIZE_STEP
            }
        );
    }

    #[test]
    fn enlarging_switches_only_that_camera_to_the_main_stream() {
        // Addresses without a server: the StreamTasks just keep trying to
        // connect.
        let with_substream = Camera {
            id: "a".to_string(),
            rtsp_url: "rtsp://127.0.0.1:1/main-a".to_string(),
            name: None,
            substream_url: Some("rtsp://127.0.0.1:1/sub-a".to_string()),
        };
        let without_substream = camera("rtsp://127.0.0.1:1/main-b");
        let key_a = with_substream.rtsp_url.clone();
        let key_b = without_substream.rtsp_url.clone();

        let runtime = tokio::runtime::Runtime::new().expect("failed to create the test runtime");
        let guard = runtime.enter();
        let handle = StreamManagerHandle::new();
        let mut app = SimpleRtspViewerApp::new(
            vec![with_substream, without_substream],
            handle.clone(),
            std::env::temp_dir().join("layout_ui_test_unused.json"),
        );
        let open_url = |app: &SimpleRtspViewerApp, key: &str| app.streams[key].url.clone();

        // In the grid: substream when there is one, main stream when not.
        assert_eq!(open_url(&app, &key_a), "rtsp://127.0.0.1:1/sub-a");
        assert_eq!(open_url(&app, &key_b), key_b);

        // Enlarged: only camera A switches to the main stream.
        app.set_enlarged(Some(key_a.clone()));
        assert_eq!(app.enlarged_index(), Some(0));
        assert_eq!(open_url(&app, &key_a), key_a);
        assert_eq!(open_url(&app, &key_b), key_b);

        // Back to the grid: A goes back to the substream.
        app.set_enlarged(None);
        assert_eq!(app.enlarged_index(), None);
        assert_eq!(open_url(&app, &key_a), "rtsp://127.0.0.1:1/sub-a");

        runtime.block_on(handle.shutdown());
        drop(guard);
        runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    }

    fn assert_rect_close(actual: egui::Rect, expected: egui::Rect) {
        let close = (actual.min - expected.min).length() < 1e-3
            && (actual.max - expected.max).length() < 1e-3;
        assert!(close, "expected {expected:?}, got {actual:?}");
    }

    #[test]
    fn wide_video_in_square_block_gets_bars_above_and_below() {
        // 16:9 in a 400x400 tile: fills the whole width, height 225, centered.
        let fitted =
            fit_preserving_aspect(rect(0.0, 0.0, 400.0, 400.0), egui::vec2(1920.0, 1080.0));
        assert_rect_close(fitted, rect(0.0, 87.5, 400.0, 225.0));
    }

    #[test]
    fn tall_video_in_wide_block_gets_bars_on_the_sides() {
        // 9:16 in an offset 800x450 tile: fills the whole height, width 450 *
        // 9/16 = 253.125, centered at x = 10 + 400.
        let fitted =
            fit_preserving_aspect(rect(10.0, 20.0, 800.0, 450.0), egui::vec2(1080.0, 1920.0));
        let width = 450.0 * 1080.0 / 1920.0;
        assert_rect_close(fitted, rect(410.0 - width / 2.0, 20.0, width, 450.0));
    }

    #[test]
    fn invalid_sizes_return_the_container() {
        let container = rect(0.0, 0.0, 300.0, 200.0);
        assert_eq!(
            fit_preserving_aspect(container, egui::vec2(0.0, 1080.0)),
            container
        );
        assert_eq!(
            fit_preserving_aspect(container, egui::vec2(1920.0, 0.0)),
            container
        );
        let empty = rect(5.0, 5.0, 0.0, 100.0);
        assert_eq!(
            fit_preserving_aspect(empty, egui::vec2(1920.0, 1080.0)),
            empty
        );
    }

    proptest! {
        // The fitted video keeps the original aspect ratio, fits in the tile,
        // is centered and fills the whole tile in at least one dimension.
        // Realistic size ranges (aspect ratio up to 80:1) keep the f32 rounding
        // error well below the tolerances.
        #[test]
        fn property_fit_preserving_aspect(
            x in -500.0f32..500.0,
            y in -500.0f32..500.0,
            container_w in 50.0f32..4000.0,
            container_h in 50.0f32..4000.0,
            content_w in 100.0f32..8000.0,
            content_h in 100.0f32..8000.0,
        ) {
            let container = rect(x, y, container_w, container_h);
            let fitted = fit_preserving_aspect(container, egui::vec2(content_w, content_h));
            let tolerance = 1e-2 * container_w.max(container_h);

            let content_aspect = content_w / content_h;
            let fitted_aspect = fitted.width() / fitted.height();
            prop_assert!((fitted_aspect - content_aspect).abs() <= 5e-3 * content_aspect);

            prop_assert!(fitted.width() <= container_w + tolerance);
            prop_assert!(fitted.height() <= container_h + tolerance);
            prop_assert!((fitted.center() - container.center()).length() <= tolerance);
            prop_assert!(
                (fitted.width() - container_w).abs() <= tolerance
                    || (fitted.height() - container_h).abs() <= tolerance
            );
        }
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
