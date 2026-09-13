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

use tokio::sync::{mpsc, watch};

use crate::grid;
use crate::layout;
use crate::model::{Camera, CameraFrame, ConnectionStatus, DisplaySize};
use crate::streaming::{StreamChannels, StreamManagerHandle};

/// Capacity of each camera's frame channel (`CameraFrame`). Small on purpose:
/// only the latest frame matters to the UI, which drains the channel completely
/// on every render frame. The matching `StreamTask` uses `try_send`; when the
/// channel is full (UI momentarily slower than decoding), the newly decoded
/// frame is dropped, limiting memory use to a few frames per camera
/// (Requirement 7.2, see design.md).
const FRAME_CHANNEL_CAPACITY: usize = 2;

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
    /// Handle to the `Stream_Manager`, used to open and close streams.
    stream_manager_handle: StreamManagerHandle,
    /// Path of the Layout_Config_File, used to persist the grid order.
    layout_path: PathBuf,
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
            stream_manager_handle,
            layout_path,
        };
        app.sync_streams();
        app
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

            while let Ok(frame) = stream.frames.try_recv() {
                received_any = true;
                Self::update_texture(&mut self.textures, ctx, key, &frame);
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

    /// Converts a `CameraFrame` (RGBA8) into an `egui::ColorImage` and updates
    /// (or creates) the matching `egui::TextureHandle`, reusing the existing
    /// texture through `TextureHandle::set` when possible.
    fn update_texture(
        textures: &mut HashMap<String, egui::TextureHandle>,
        ctx: &egui::Context,
        url: &str,
        frame: &CameraFrame,
    ) {
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            &frame.rgba,
        );

        if let Some(texture) = textures.get_mut(url) {
            texture.set(color_image, egui::TextureOptions::default());
        } else {
            let texture = ctx.load_texture(
                url.to_string(),
                color_image,
                egui::TextureOptions::default(),
            );
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

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.cameras.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label("No cameras loaded.");
                });
                return;
            }

            if let Some(index) = self.enlarged_index() {
                self.enlarged_block(ui, index);
                return;
            }

            let (columns, rows) = grid::grid_dimensions(self.cameras.len());
            let available = ui.available_size();
            let cell_size = egui::vec2(
                (available.x / columns as f32).max(1.0),
                (available.y / rows as f32).max(1.0),
            );

            let mut reorder: Option<(usize, usize)> = None;
            let mut enlarge: Option<usize> = None;

            egui::Grid::new("camera_grid")
                .min_col_width(cell_size.x)
                .min_row_height(cell_size.y)
                .spacing(egui::vec2(2.0, 2.0))
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

        if response.double_clicked() || ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.set_enlarged(None);
        }
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

        Self::paint_block_contents(
            ui,
            rect,
            self.textures.get(key),
            grid::status_indicator(status),
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
        if response.interact(egui::Sense::click()).double_clicked() {
            return BlockAction::Enlarge;
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
                // Visual indication of the target position during the drag (Requirement
                // 4.3).
                ui.painter().rect_stroke(
                    response.rect,
                    4.0,
                    egui::Stroke::new(3.0, egui::Color32::YELLOW),
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

    /// Paints the visual content of a `Camera_Tile`: the latest video texture
    /// (or a placeholder, if no frame has been decoded yet), the camera name
    /// (if any) and the connection status overlay matching `indicator`
    /// (Requirements 2.4, 2.5, 2.6).
    fn paint_block_contents(
        ui: &egui::Ui,
        rect: egui::Rect,
        texture: Option<&egui::TextureHandle>,
        indicator: grid::IndicatorKind,
        name: Option<&str>,
    ) {
        let painter = ui.painter();

        match texture {
            Some(tex) => {
                // The video is fitted keeping its original aspect ratio; the
                // rest of the tile gets black bars (on the sides or
                // above/below).
                painter.rect_filled(rect, 0.0, egui::Color32::BLACK);
                painter.image(
                    tex.id(),
                    fit_preserving_aspect(rect, tex.size_vec2()),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            None => {
                // No frame decoded yet: neutral placeholder instead of video.
                painter.rect_filled(rect, 0.0, egui::Color32::from_gray(40));
            }
        }

        if let Some(name) = name {
            painter.text(
                rect.left_top() + egui::vec2(4.0, 4.0),
                egui::Align2::LEFT_TOP,
                name,
                egui::FontId::default(),
                egui::Color32::WHITE,
            );
        }

        // "disconnected"/"reconnecting" overlays: visually distinct from each
        // other (different color and text) and from normal video rendering,
        // which gets no overlay when `indicator` is `IndicatorKind::None`
        // (Requirements 2.5, 2.6).
        let overlay = match indicator {
            grid::IndicatorKind::None => None,
            grid::IndicatorKind::Disconnected => Some((
                "Disconnected",
                egui::Color32::from_rgba_unmultiplied(120, 20, 20, 200),
            )),
            grid::IndicatorKind::Reconnecting => Some((
                "Reconnecting",
                egui::Color32::from_rgba_unmultiplied(130, 95, 0, 200),
            )),
        };

        if let Some((label, color)) = overlay {
            painter.rect_filled(rect, 0.0, color);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(16.0),
                egui::Color32::WHITE,
            );
        }
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
    /// (Requirement 5.1). The disk write (`layout::save_layout_file`, synchronous) runs
    /// on a thread of tokio's blocking pool via `spawn_blocking`; failures are only
    /// logged, without interrupting the app (Requirement 5.7).
    fn persist_layout(&self) {
        let layout_path = self.layout_path.clone();
        let layout_file = layout::build_layout_file(&self.cameras);

        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                layout::save_layout_file(&layout_path, &layout_file)
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
    /// Main UI loop, called on every `eframe` frame. Drains the pending events
    /// of each camera, redraws the grid (handling drag-and-drop) and requests
    /// an immediate repaint, to keep the video display responsive even without
    /// user interaction (Requirements 2.4, 2.7, 3.5).
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
