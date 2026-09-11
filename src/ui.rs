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

use tokio::sync::mpsc;

use crate::grid;
use crate::layout;
use crate::model::{Camera, CameraFrame, ConnectionStatus, StreamEvent};
use crate::streaming::StreamManagerHandle;

/// Capacity of each camera's event channel (`StreamEvent`). Small on purpose:
/// only the latest frame/status matters to the UI, which drains the channel
/// completely on every render frame. The matching `StreamTask` uses `try_send`;
/// when the channel is full (UI momentarily slower than decoding), the newly
/// decoded frame is dropped, limiting memory use to a few frames per camera
/// (Requirement 7.2, see design.md).
const FRAME_CHANNEL_CAPACITY: usize = 2;

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

/// `SimpleRtspViewerApp`: integration of the `Viewing_Grid` with
/// `eframe`/`egui`.
pub struct SimpleRtspViewerApp {
    /// Current in-memory camera order (reflected in the grid).
    cameras: Vec<Camera>,
    /// Receiving channel of `StreamEvent`s for each camera, keyed by `rtsp_url`.
    frame_receivers: HashMap<String, mpsc::Receiver<StreamEvent>>,
    /// Last GPU texture loaded for each camera, keyed by `rtsp_url`.
    textures: HashMap<String, egui::TextureHandle>,
    /// Last known `ConnectionStatus` of each camera, keyed by `rtsp_url`.
    statuses: HashMap<String, ConnectionStatus>,
    /// State of the drag-and-drop operation in progress, if any.
    drag_state: Option<DragState>,
    /// Handle to the `Stream_Manager`, used to add/remove cameras. Kept in the
    /// app state for future wiring of interactive camera management operations
    /// (add/remove/reimport), not exposed by the UI yet at this stage.
    #[allow(dead_code)]
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
    /// A dedicated `StreamEvent` channel is created for each camera (keyed by
    /// `rtsp_url`); the `Receiver` is kept in `frame_receivers` and the
    /// `Sender` is handed to the `Stream_Manager` through `add_camera`. The
    /// initial `statuses` are all `Disconnected`, until the matching
    /// `StreamTask` emits its first `StreamEvent::Status`.
    ///
    /// Since `StreamManagerHandle::add_camera` is async, this function sends it
    /// through `tokio::spawn`: calling `new` requires a tokio runtime already
    /// running on the current thread (e.g. inside `#[tokio::main]` or a
    /// `Runtime::block_on`, before handing control to `eframe::run_native`).
    pub fn new(
        cameras: Vec<Camera>,
        stream_manager_handle: StreamManagerHandle,
        layout_path: PathBuf,
    ) -> Self {
        let mut frame_receivers = HashMap::with_capacity(cameras.len());
        let mut statuses = HashMap::with_capacity(cameras.len());

        for camera in &cameras {
            let (sender, receiver) = mpsc::channel(FRAME_CHANNEL_CAPACITY);
            frame_receivers.insert(camera.rtsp_url.clone(), receiver);
            statuses.insert(camera.rtsp_url.clone(), ConnectionStatus::Disconnected);

            let handle = stream_manager_handle.clone();
            let camera_to_add = camera.clone();
            tokio::spawn(async move {
                handle.add_camera(camera_to_add, sender).await;
            });
        }

        Self {
            cameras,
            frame_receivers,
            textures: HashMap::new(),
            statuses,
            drag_state: None,
            stream_manager_handle,
            layout_path,
        }
    }

    /// Drains without blocking (`try_recv`) the pending `StreamEvent`s of each
    /// camera, updating the associated GPU texture from `CameraFrame`s and the
    /// `ConnectionStatus` from `StreamEvent::Status` (Requirements 2.4, 2.7,
    /// 3.5). No network I/O or decoding happens here, only reads from channels
    /// already filled by the background `StreamTask`s. Returns `true` if at
    /// least one event was drained in this cycle, so the caller can adjust the
    /// repaint rate.
    fn drain_camera_events(&mut self, ctx: &egui::Context) -> bool {
        let mut received_any = false;
        for camera in &self.cameras {
            let url = &camera.rtsp_url;
            if let Some(receiver) = self.frame_receivers.get_mut(url) {
                while let Ok(event) = receiver.try_recv() {
                    received_any = true;
                    match event {
                        StreamEvent::Frame(frame) => {
                            Self::update_texture(&mut self.textures, ctx, url, &frame);
                        }
                        StreamEvent::Status(status) => {
                            self.statuses.insert(url.clone(), status);
                        }
                    }
                }
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

            let (columns, rows) = grid::grid_dimensions(self.cameras.len());
            let available = ui.available_size();
            let cell_size = egui::vec2(
                (available.x / columns as f32).max(1.0),
                (available.y / rows as f32).max(1.0),
            );

            let mut reorder: Option<(usize, usize)> = None;

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
                            if let Some(pair) = self.camera_block(ui, index, cell_size) {
                                reorder = Some(pair);
                            }
                        }
                        ui.end_row();
                    }
                });

            if let Some((from, to)) = reorder {
                self.apply_reorder(from, to);
            }
        });
    }

    /// Draws a single `Camera_Tile` (index `index`) with size `size`, treating
    /// it as both a drag-and-drop source and target (Requirements 2.1, 4.1,
    /// 4.2, 4.3). Returns `Some((from, to))` when a valid drop (target
    /// different from the source) finished over this tile.
    fn camera_block(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        size: egui::Vec2,
    ) -> Option<(usize, usize)> {
        let url = self.cameras[index].rtsp_url.clone();
        let name = self.cameras[index].name.clone();
        let status = self
            .statuses
            .get(&url)
            .copied()
            .unwrap_or(ConnectionStatus::Disconnected);
        let indicator = grid::status_indicator(status);
        let texture = self.textures.get(&url).cloned();

        let item_id = egui::Id::new("simple_rtsp_viewer_camera_block").with(index);

        let response = ui
            .dnd_drag_source(item_id, index, |ui| {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                Self::paint_block_contents(ui, rect, texture.as_ref(), indicator, name.as_deref());
            })
            .response;

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

        let mut dropped = None;

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
                dropped = Some((from, index));
            }
            self.drag_state = None;
        }

        dropped
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
                painter.image(
                    tex.id(),
                    rect,
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

    fn camera(url: &str) -> Camera {
        Camera {
            rtsp_url: url.to_string(),
            name: None,
            substream_url: None,
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
