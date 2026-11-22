//! Sidebar: the app's actions, the list of cameras with their status and,
//! while a camera is enlarged, the controls of that camera.
//!
//! Everything that used to float over the video (resolution, sound) lives
//! here, in its own column, so no control can ever cover another one or the
//! picture. The sidebar only draws and reports what the user did as
//! `SidebarAction`s; `ui.rs` applies them.
//!
//! Layout is a plain vertical flow of full-width rows (each component
//! allocates its own rectangle), inside one scroll area, so a short window
//! scrolls instead of overlapping rows.

use egui::{vec2, Align, Color32, Frame, Layout, Margin, ScrollArea, Sense, SidePanel};

use crate::model::ConnectionStatus;
use crate::pixel::{hairline, hairline_stroke, snap_rect};
use crate::probe::Probe;
use crate::theme::{self, Palette};
use crate::widgets::{self, Skin};

/// Total width of the sidebar, expanded and collapsed (icons only).
pub const EXPANDED_WIDTH: f32 = 296.0;
pub const COLLAPSED_WIDTH: f32 = 64.0;

/// Space around the content, in points.
const MARGIN_X: f32 = 12.0;
const MARGIN_Y: f32 = 8.0;
const HEADER_HEIGHT: f32 = 48.0;

/// Gap between consecutive rows.
const ROW_GAP: f32 = 4.0;

/// A camera in the list.
pub struct CameraEntry<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// Host (and port when not the default), shown under the name.
    pub address: String,
    pub status: ConnectionStatus,
}

/// Sound controls of the enlarged camera.
pub struct AudioControls {
    pub muted: bool,
    /// Volume from 0 to 1.
    pub volume: f32,
    /// Volume at which the camera audio plays at its original level (marked
    /// on the slider).
    pub default_volume: f32,
}

/// The enlarged camera's controls.
pub struct Viewing<'a> {
    pub name: &'a str,
    /// Address and which stream is open, under the name.
    pub detail: String,
    /// `Some(on)` when the camera has a substream (so there is a choice);
    /// `on` is whether the main stream is open.
    pub high_resolution: Option<bool>,
    /// `Some` when the stream has audio the app can play.
    pub audio: Option<AudioControls>,
}

/// Everything the sidebar needs to draw.
pub struct SidebarInput<'a> {
    pub palette: &'a Palette,
    /// The Windows icon font is available.
    pub icons: bool,
    /// Mica is on: the panel is transparent so it shows through.
    pub mica: bool,
    pub collapsed: bool,
    /// False while a dialog is open: everything is inert.
    pub enabled: bool,
    pub cameras: &'a [CameraEntry<'a>],
    /// `id` of the enlarged camera.
    pub enlarged: Option<&'a str>,
    pub viewing: Option<Viewing<'a>>,
}

/// What the user did in the sidebar in this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum SidebarAction {
    AddCamera,
    Scan,
    Import,
    Refresh,
    SharedLogin,
    ToggleCollapsed,
    /// Enlarge the camera, or go back to the grid if it already is.
    Select(String),
    Edit(String),
    Remove(String),
    BackToGrid,
    SetHighResolution(bool),
    SetMuted(bool),
    SetVolume(f32),
}

/// Draws the sidebar at the left of the window and returns what the user did.
pub fn show(ctx: &egui::Context, input: &SidebarInput<'_>, probe: &Probe) -> Vec<SidebarAction> {
    let total_width = if input.collapsed {
        COLLAPSED_WIDTH
    } else {
        EXPANDED_WIDTH
    };
    let fill = if input.mica {
        Color32::TRANSPARENT
    } else {
        input.palette.window_background
    };
    let frame = Frame::none()
        .fill(fill)
        .inner_margin(Margin::symmetric(MARGIN_X, MARGIN_Y));

    let mut actions = Vec::new();
    // In egui the width of a side panel includes its frame margins.
    let panel = SidePanel::left("simple_rtsp_viewer_sidebar")
        .exact_width(total_width)
        .resizable(false)
        .show_separator_line(false)
        .frame(frame)
        .show(ctx, |ui| {
            // Rows are packed with a small gap and no horizontal spacing.
            ui.spacing_mut().item_spacing = vec2(0.0, ROW_GAP);
            ui.add_enabled_ui(input.enabled, |ui| {
                let skin = Skin {
                    palette: input.palette,
                    icons: input.icons,
                };
                if input.collapsed {
                    rail(ui, &skin, input, &mut actions, probe);
                } else {
                    expanded(ui, &skin, input, &mut actions, probe);
                }
            });
        });

    // The line between the sidebar and the content: one physical pixel, on
    // the panel's right edge.
    let ppp = ctx.pixels_per_point();
    let panel_rect = panel.response.rect;
    let line = snap_rect(
        egui::Rect::from_min_max(
            egui::pos2(panel_rect.right() - hairline(ppp), panel_rect.top()),
            panel_rect.right_bottom(),
        ),
        ppp,
    );
    ctx.layer_painter(panel.response.layer_id)
        .rect_filled(line, 0.0, input.palette.divider);

    probe!(probe, "container:sidebar", panel_rect);
    actions
}

// ---------------------------------------------------------------------------
// Expanded
// ---------------------------------------------------------------------------

fn expanded(
    ui: &mut egui::Ui,
    skin: &Skin<'_>,
    input: &SidebarInput<'_>,
    actions: &mut Vec<SidebarAction>,
    probe: &Probe,
) {
    // Header, pinned above the scrolling part.
    let toggle = ui
        .allocate_ui_with_layout(
            vec2(ui.available_width(), HEADER_HEIGHT),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if skin.icons {
                    let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                    widgets::paint_icon(
                        ui,
                        theme::ICON_VIDEO,
                        20.0,
                        rect.center(),
                        skin.palette.accent,
                    );
                    probe!(probe, "sidebar:logo", rect);
                }
                let title_width = (ui.available_width() - widgets::ICON_BUTTON - 8.0).max(1.0);
                let (title_rect, _) =
                    ui.allocate_exact_size(vec2(title_width, 24.0), Sense::hover());
                let galley = widgets::single_line(
                    ui,
                    "Simple RTSP Viewer",
                    theme::semibold(16.0),
                    skin.palette.text,
                    title_width,
                );
                widgets::paint_text_left(
                    ui,
                    galley,
                    title_rect.left(),
                    title_rect.center().y,
                    skin.palette.text,
                );
                probe!(probe, "sidebar:title", title_rect);

                let button = widgets::icon_button(
                    ui,
                    skin,
                    theme::ICON_MENU,
                    "<",
                    widgets::ICON_BUTTON,
                    false,
                    false,
                )
                .on_hover_text("Collapse the sidebar");
                probe!(probe, "sidebar:toggle", button.rect);
                button.clicked()
            },
        )
        .inner;
    if toggle {
        actions.push(SidebarAction::ToggleCollapsed);
    }

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, ROW_GAP);
            // The scroll bar takes its own space instead of floating over the
            // rows.
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();

            if let Some(viewing) = &input.viewing {
                viewing_card(ui, skin, viewing, actions, probe);
                ui.add_space(8.0);
            }

            let add = widgets::button(ui, skin, theme::ICON_ADD, "Add camera", true);
            probe!(probe, "sidebar:add", add.rect);
            if add.clicked() {
                actions.push(SidebarAction::AddCamera);
            }
            ui.add_space(4.0);

            let items = [
                (theme::ICON_SEARCH, "Scan network", SidebarAction::Scan),
                (theme::ICON_IMPORT, "Import", SidebarAction::Import),
                (theme::ICON_REFRESH, "Refresh", SidebarAction::Refresh),
                (theme::ICON_KEY, "Default login", SidebarAction::SharedLogin),
            ];
            for (glyph, label, action) in items {
                let item = widgets::nav_item(ui, skin, glyph, label, false);
                probe!(probe, format!("sidebar:{label}"), item.rect);
                if item.clicked() {
                    actions.push(action);
                }
            }

            widgets::divider(ui, skin);
            widgets::section_header(ui, skin, "Cameras", Some(input.cameras.len()));

            if input.cameras.is_empty() {
                widgets::paragraph(
                    ui,
                    skin,
                    "No cameras yet. Add one by its address, or scan the network to find them.",
                );
            }
            for camera in input.cameras {
                let selected = input.enlarged == Some(camera.id);
                let row = widgets::camera_row(
                    ui,
                    skin,
                    camera.status,
                    camera.name,
                    &camera.address,
                    selected,
                );
                probe!(probe, format!("sidebar:camera:{}", camera.id), row.rect);
                camera_menu(&row, camera.id, actions);
                if row.clicked() {
                    actions.push(SidebarAction::Select(camera.id.to_owned()));
                }
            }
        });
}

/// Context menu of a camera in the list.
fn camera_menu(row: &egui::Response, id: &str, actions: &mut Vec<SidebarAction>) {
    row.context_menu(|ui| {
        ui.set_min_width(160.0);
        if ui.button("Edit…").clicked() {
            actions.push(SidebarAction::Edit(id.to_owned()));
            ui.close_menu();
        }
        ui.separator();
        if ui.button("Remove…").clicked() {
            actions.push(SidebarAction::Remove(id.to_owned()));
            ui.close_menu();
        }
    });
}

/// The enlarged camera's card: name, resolution switch, sound and a way back
/// to the grid.
fn viewing_card(
    ui: &mut egui::Ui,
    skin: &Skin<'_>,
    viewing: &Viewing<'_>,
    actions: &mut Vec<SidebarAction>,
    probe: &Probe,
) {
    let palette = skin.palette;
    let ppp = ui.ctx().pixels_per_point();
    let card = Frame::none()
        .fill(palette.card_background)
        .stroke(hairline_stroke(ppp, palette.divider))
        .rounding(8.0)
        .inner_margin(Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(0.0, ROW_GAP);
            let width = ui.available_width();

            let (rect, _) = ui.allocate_exact_size(vec2(width, 22.0), Sense::hover());
            let galley =
                widgets::single_line(ui, viewing.name, theme::semibold(15.0), palette.text, width);
            widgets::paint_text_left(ui, galley, rect.left(), rect.center().y, palette.text);
            probe!(probe, "sidebar:viewing:name", rect);

            let (rect, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
            let galley = widgets::single_line(
                ui,
                &viewing.detail,
                egui::FontId::proportional(12.0),
                palette.text_secondary,
                width,
            );
            widgets::paint_text_left(
                ui,
                galley,
                rect.left(),
                rect.center().y,
                palette.text_secondary,
            );
            probe!(probe, "sidebar:viewing:detail", rect);
            ui.add_space(6.0);

            if let Some(high) = viewing.high_resolution {
                let mut value = high;
                let row = widgets::switch_row(ui, skin, "Higher resolution", &mut value);
                probe!(probe, "sidebar:viewing:resolution", row.rect);
                if row.changed() {
                    actions.push(SidebarAction::SetHighResolution(value));
                }
            }

            if let Some(audio) = &viewing.audio {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let (glyph, fallback, tip) = if audio.muted {
                        (theme::ICON_MUTE, "Unmute", "Turn the sound on")
                    } else {
                        (theme::ICON_VOLUME, "Mute", "Mute")
                    };
                    let mute = widgets::icon_button(
                        ui,
                        skin,
                        glyph,
                        fallback,
                        widgets::ICON_BUTTON,
                        false,
                        false,
                    )
                    .on_hover_text(tip);
                    probe!(probe, "sidebar:viewing:mute", mute.rect);
                    if mute.clicked() {
                        actions.push(SidebarAction::SetMuted(!audio.muted));
                    }

                    // Greyed out while muted: the button turns the sound on.
                    ui.add_enabled_ui(!audio.muted, |ui| {
                        let mut volume = audio.volume;
                        let width = ui.available_width();
                        let slider = widgets::slider(
                            ui,
                            skin,
                            &mut volume,
                            width,
                            Some(audio.default_volume),
                        )
                        .on_hover_text(
                            "Volume. The tick is the camera's original level; \
                             above it boosts quiet microphones.",
                        );
                        probe!(probe, "sidebar:viewing:volume", slider.rect);
                        if slider.changed() {
                            actions.push(SidebarAction::SetVolume(volume));
                        }
                    });
                });
            }

            ui.add_space(4.0);
            let back = widgets::button(ui, skin, theme::ICON_BACK_TO_WINDOW, "Back to grid", false);
            probe!(probe, "sidebar:viewing:back", back.rect);
            if back.clicked() {
                actions.push(SidebarAction::BackToGrid);
            }
        });
    probe!(probe, "container:sidebar:viewing-card", card.response.rect);
    let _ = (&card, hairline(ppp));
}

// ---------------------------------------------------------------------------
// Collapsed (icons only)
// ---------------------------------------------------------------------------

fn rail(
    ui: &mut egui::Ui,
    skin: &Skin<'_>,
    input: &SidebarInput<'_>,
    actions: &mut Vec<SidebarAction>,
    probe: &Probe,
) {
    let size = widgets::ICON_BUTTON;

    // Header: just the button that expands the sidebar.
    let (header, _) = ui.allocate_exact_size(vec2(size, HEADER_HEIGHT), Sense::hover());
    let button_rect = egui::Rect::from_center_size(header.center(), vec2(size, size));
    let expand = ui
        .allocate_ui_at_rect(button_rect, |ui| {
            widgets::icon_button(ui, skin, theme::ICON_MENU, ">", size, false, false)
                .on_hover_text("Expand the sidebar")
        })
        .inner;
    probe!(probe, "sidebar:toggle", expand.rect);
    if expand.clicked() {
        actions.push(SidebarAction::ToggleCollapsed);
    }

    // The rail never shows a scroll bar (it would not fit next to the
    // buttons); the mouse wheel still scrolls it.
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, ROW_GAP);

            let add = widgets::icon_button(ui, skin, theme::ICON_ADD, "+", size, false, true)
                .on_hover_text("Add camera");
            probe!(probe, "sidebar:add", add.rect);
            if add.clicked() {
                actions.push(SidebarAction::AddCamera);
            }
            ui.add_space(4.0);

            let buttons = [
                (theme::ICON_SEARCH, "S", "Scan network", SidebarAction::Scan),
                (theme::ICON_IMPORT, "I", "Import", SidebarAction::Import),
                (theme::ICON_REFRESH, "R", "Refresh", SidebarAction::Refresh),
                (
                    theme::ICON_KEY,
                    "L",
                    "Default login",
                    SidebarAction::SharedLogin,
                ),
            ];
            for (glyph, fallback, tip, action) in buttons {
                let button = widgets::icon_button(ui, skin, glyph, fallback, size, false, false)
                    .on_hover_text(tip);
                probe!(probe, format!("sidebar:{tip}"), button.rect);
                if button.clicked() {
                    actions.push(action);
                }
            }

            widgets::divider(ui, skin);

            for (index, camera) in input.cameras.iter().enumerate() {
                let selected = input.enlarged == Some(camera.id);
                let chip = widgets::camera_chip(ui, skin, camera.status, index + 1, selected)
                    .on_hover_text(format!(
                        "{}\n{} · {}",
                        camera.name,
                        camera.address,
                        widgets::status_word(camera.status)
                    ));
                probe!(probe, format!("sidebar:camera:{}", camera.id), chip.rect);
                camera_menu(&chip, camera.id, actions);
                if chip.clicked() {
                    actions.push(SidebarAction::Select(camera.id.to_owned()));
                }
            }

            if let Some(viewing) = &input.viewing {
                widgets::divider(ui, skin);

                let back = widgets::icon_button(
                    ui,
                    skin,
                    theme::ICON_BACK_TO_WINDOW,
                    "<",
                    size,
                    false,
                    false,
                )
                .on_hover_text("Back to grid");
                probe!(probe, "sidebar:viewing:back", back.rect);
                if back.clicked() {
                    actions.push(SidebarAction::BackToGrid);
                }

                if let Some(high) = viewing.high_resolution {
                    let toggle = widgets::icon_button(ui, skin, "", "HD", size, high, false)
                        .on_hover_text(if high {
                            "Back to the substream"
                        } else {
                            "Higher resolution (main stream)"
                        });
                    probe!(probe, "sidebar:viewing:resolution", toggle.rect);
                    if toggle.clicked() {
                        actions.push(SidebarAction::SetHighResolution(!high));
                    }
                }

                if let Some(audio) = &viewing.audio {
                    let (glyph, fallback, tip) = if audio.muted {
                        (theme::ICON_MUTE, "M", "Turn the sound on")
                    } else {
                        (theme::ICON_VOLUME, "V", "Mute")
                    };
                    let mute = widgets::icon_button(ui, skin, glyph, fallback, size, false, false)
                        .on_hover_text(tip);
                    probe!(probe, "sidebar:viewing:mute", mute.rect);
                    if mute.clicked() {
                        actions.push(SidebarAction::SetMuted(!audio.muted));
                    }
                }
            }
        });
}
