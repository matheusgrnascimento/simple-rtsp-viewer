//! Windows 11 style components for the sidebar: navigation item, buttons,
//! switch and slider.
//!
//! Every component is painted by hand inside a rectangle it allocates in the
//! layout (`allocate_exact_size`), with its edges on whole physical pixels, so
//! it is crisp and can never draw outside the space it was given: texts are
//! truncated with an ellipsis to fit instead of spilling over the neighbor.

use std::sync::Arc;

use egui::epaint::text::{LayoutJob, TextWrapping};
use egui::{pos2, vec2, Color32, CursorIcon, FontId, Galley, Pos2, Rect, Response, Sense, Ui};

use crate::pixel::{border_rect, hairline_stroke, snap, snap_pos, snap_rect};
use crate::theme::{self, Palette};

/// Height of a navigation item or button, and the touch target of an icon
/// button.
pub const ROW_HEIGHT: f32 = 36.0;
pub const ICON_BUTTON: f32 = 40.0;
pub const CAMERA_ROW_HEIGHT: f32 = 56.0;

/// Corner radius of items and buttons (Windows 11 controls).
pub const CONTROL_ROUNDING: f32 = 4.0;

/// Horizontal padding inside an item.
const PADDING: f32 = 12.0;
/// Width reserved for the icon of an item; the label starts after it.
const ICON_SLOT: f32 = 32.0;

/// Colors and fonts shared by the components.
#[derive(Clone, Copy)]
pub struct Skin<'a> {
    pub palette: &'a Palette,
    /// The Windows icon font is available.
    pub icons: bool,
}

/// Lays out `text` on one line no wider than `max_width`, ending in an
/// ellipsis when it does not fit.
pub fn single_line(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping {
        max_width: max_width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// Paints `galley` with its left edge at `left`, centered vertically on
/// `center_y`, on whole pixels.
pub fn paint_text_left(ui: &Ui, galley: Arc<Galley>, left: f32, center_y: f32, color: Color32) {
    let ppp = ui.ctx().pixels_per_point();
    let pos = snap_pos(pos2(left, center_y - galley.size().y / 2.0), ppp);
    ui.painter().galley(pos, galley, color);
}

/// Paints an icon glyph centered on `center`, on whole pixels.
pub fn paint_icon(ui: &Ui, glyph: &str, size: f32, center: Pos2, color: Color32) {
    let ppp = ui.ctx().pixels_per_point();
    let galley = ui
        .painter()
        .layout_no_wrap(glyph.to_owned(), theme::icon_font(size), color);
    let pos = snap_pos(center - galley.size() / 2.0, ppp);
    ui.painter().galley(pos, galley, color);
}

/// Allocates a full-width row of `height` and returns its snapped rectangle
/// with the response.
fn allocate_row(ui: &mut Ui, height: f32, sense: Sense) -> (Rect, Response) {
    let ppp = ui.ctx().pixels_per_point();
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
    (snap_rect(rect, ppp), response)
}

/// Pointer cursor, only over enabled controls.
fn pointer(response: Response) -> Response {
    if response.enabled() {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Fill of an item: the selected or hover tint, nothing otherwise.
fn item_fill(skin: &Skin<'_>, response: &Response, selected: bool) -> Option<Color32> {
    if selected || response.is_pointer_button_down_on() {
        Some(skin.palette.item_selected)
    } else if response.hovered() {
        Some(skin.palette.item_hover)
    } else {
        None
    }
}

/// The accent bar at the left of a selected item.
fn paint_selection_bar(ui: &Ui, skin: &Skin<'_>, rect: Rect) {
    let ppp = ui.ctx().pixels_per_point();
    let bar = Rect::from_center_size(pos2(rect.left() + 3.0, rect.center().y), vec2(3.0, 16.0));
    ui.painter()
        .rect_filled(snap_rect(bar, ppp), 1.5, skin.palette.accent);
}

/// Navigation item: icon and label in a full-width row. `selected` shows the
/// accent bar and tint.
pub fn nav_item(
    ui: &mut Ui,
    skin: &Skin<'_>,
    glyph: &str,
    label: &str,
    selected: bool,
) -> Response {
    let (rect, response) = allocate_row(ui, ROW_HEIGHT, Sense::click());
    if ui.is_rect_visible(rect) {
        if let Some(fill) = item_fill(skin, &response, selected) {
            ui.painter().rect_filled(rect, CONTROL_ROUNDING, fill);
        }
        if selected {
            paint_selection_bar(ui, skin, rect);
        }

        let mut label_left = rect.left() + PADDING + 4.0;
        if skin.icons {
            let center = pos2(
                rect.left() + PADDING + ICON_SLOT / 2.0 - 4.0,
                rect.center().y,
            );
            paint_icon(ui, glyph, 16.0, center, skin.palette.text);
            label_left = rect.left() + PADDING + ICON_SLOT;
        }
        let max_width = rect.right() - PADDING - label_left;
        let galley = single_line(
            ui,
            label,
            FontId::proportional(14.0),
            skin.palette.text,
            max_width,
        );
        paint_text_left(ui, galley, label_left, rect.center().y, skin.palette.text);
    }
    pointer(response)
}

/// Square button with only an icon (the collapsed sidebar, and small controls).
/// `fallback` is the text shown when the icon font is missing. `selected`
/// shows the tint; `accent` paints it as the primary action.
pub fn icon_button(
    ui: &mut Ui,
    skin: &Skin<'_>,
    glyph: &str,
    fallback: &str,
    size: f32,
    selected: bool,
    accent: bool,
) -> Response {
    let ppp = ui.ctx().pixels_per_point();
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let rect = snap_rect(rect, ppp);
    if ui.is_rect_visible(rect) {
        let text_color = if accent {
            skin.palette.on_accent
        } else {
            skin.palette.text
        };
        if accent {
            let fill = if response.is_pointer_button_down_on() {
                theme::mix(skin.palette.accent, Color32::BLACK, 0.15)
            } else if response.hovered() {
                theme::mix(skin.palette.accent, Color32::WHITE, 0.1)
            } else {
                skin.palette.accent
            };
            ui.painter().rect_filled(rect, CONTROL_ROUNDING, fill);
        } else if let Some(fill) = item_fill(skin, &response, selected) {
            ui.painter().rect_filled(rect, CONTROL_ROUNDING, fill);
        }
        if selected && !accent {
            paint_selection_bar(ui, skin, rect);
        }

        // An empty glyph (the "HD" toggle) is always drawn as text.
        if skin.icons && !glyph.is_empty() {
            paint_icon(ui, glyph, 16.0, rect.center(), text_color);
        } else {
            let galley = single_line(
                ui,
                fallback,
                theme::semibold(13.0),
                text_color,
                rect.width() - 4.0,
            );
            let pos = snap_pos(rect.center() - galley.size() / 2.0, ppp);
            ui.painter().galley(pos, galley, text_color);
        }
    }
    pointer(response)
}

/// Full-width button with icon and centered label. The primary one is filled
/// with the accent color, the other has the control background and a border.
pub fn button(ui: &mut Ui, skin: &Skin<'_>, glyph: &str, label: &str, primary: bool) -> Response {
    let ppp = ui.ctx().pixels_per_point();
    let (rect, response) = allocate_row(ui, ROW_HEIGHT, Sense::click());
    if ui.is_rect_visible(rect) {
        let palette = skin.palette;
        let (fill, text_color) = if primary {
            let fill = if response.is_pointer_button_down_on() {
                theme::mix(palette.accent, Color32::BLACK, 0.15)
            } else if response.hovered() {
                theme::mix(palette.accent, Color32::WHITE, 0.1)
            } else {
                palette.accent
            };
            (fill, palette.on_accent)
        } else {
            let fill = if response.is_pointer_button_down_on() {
                palette.control_fill_pressed
            } else if response.hovered() {
                palette.control_fill_hover
            } else {
                palette.control_fill
            };
            (fill, palette.text)
        };
        ui.painter().rect_filled(rect, CONTROL_ROUNDING, fill);
        if !primary {
            ui.painter().rect_stroke(
                border_rect(rect, ppp),
                CONTROL_ROUNDING,
                hairline_stroke(ppp, palette.control_stroke),
            );
        }

        // Icon and label as one centered group.
        let icon_width = if skin.icons { 16.0 + 8.0 } else { 0.0 };
        let galley = single_line(
            ui,
            label,
            theme::semibold(14.0),
            text_color,
            rect.width() - 2.0 * PADDING - icon_width,
        );
        let group = icon_width + galley.size().x;
        let left = rect.center().x - group / 2.0;
        if skin.icons {
            paint_icon(
                ui,
                glyph,
                16.0,
                pos2(left + 8.0, rect.center().y),
                text_color,
            );
        }
        paint_text_left(ui, galley, left + icon_width, rect.center().y, text_color);
    }
    pointer(response)
}

/// A row with a label at the left and a Windows 11 switch at the right.
/// Returns the response; `value` is flipped when it is clicked.
pub fn switch_row(ui: &mut Ui, skin: &Skin<'_>, label: &str, value: &mut bool) -> Response {
    let ppp = ui.ctx().pixels_per_point();
    let (rect, mut response) = allocate_row(ui, ROW_HEIGHT, Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }

    if ui.is_rect_visible(rect) {
        let palette = skin.palette;
        // The switch: 40 x 20, at the right edge.
        let track = snap_rect(
            Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), vec2(40.0, 20.0)),
            ppp,
        );
        let label_width = (track.left() - 12.0 - rect.left()).max(1.0);
        let galley = single_line(
            ui,
            label,
            FontId::proportional(14.0),
            palette.text,
            label_width,
        );
        paint_text_left(ui, galley, rect.left(), rect.center().y, palette.text);

        let progress = ui.ctx().animate_bool_with_time(response.id, *value, 0.12);
        let radius = track.height() / 2.0;
        if progress > 0.0 {
            // Filled with the accent as it turns on.
            ui.painter()
                .rect_filled(track, radius, palette.accent.gamma_multiply(progress));
        }
        ui.painter().rect_stroke(
            border_rect(track, ppp),
            radius,
            hairline_stroke(
                ppp,
                theme::mix(palette.switch_off_stroke, palette.accent, progress),
            ),
        );

        // The knob slides between the two ends, a little larger on hover.
        let knob_radius = if response.hovered() { 7.0 } else { 6.0 };
        let travel = track.width() - 2.0 * (radius - 1.0) - 2.0;
        let knob_x = track.left() + radius + 1.0 + travel * progress - 1.0;
        let knob_color = theme::mix(palette.switch_off_knob, palette.on_accent, progress);
        ui.painter().circle_filled(
            snap_pos(pos2(knob_x, track.center().y), ppp),
            knob_radius,
            knob_color,
        );
    }
    pointer(response)
}

/// Horizontal slider from 0 to 1 filling `width`, with an optional tick at
/// `marker`. Returns the response; `changed()` is set when the value moves.
pub fn slider(
    ui: &mut Ui,
    skin: &Skin<'_>,
    value: &mut f32,
    width: f32,
    marker: Option<f32>,
) -> Response {
    let ppp = ui.ctx().pixels_per_point();
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 32.0), Sense::click_and_drag());
    let rect = snap_rect(rect, ppp);

    // The knob center travels between these x positions.
    let knob_radius = 10.0;
    let left = rect.left() + knob_radius;
    let right = rect.right() - knob_radius;

    if response.enabled() {
        if let Some(pointer_pos) = response.interact_pointer_pos() {
            if right > left {
                let new_value = ((pointer_pos.x - left) / (right - left)).clamp(0.0, 1.0);
                if (new_value - *value).abs() > f32::EPSILON {
                    *value = new_value;
                    response.mark_changed();
                }
            }
        }
    }

    if ui.is_rect_visible(rect) {
        let palette = skin.palette;
        let value_clamped = value.clamp(0.0, 1.0);
        let knob_x = snap(left + (right - left) * value_clamped, ppp);
        let rail = snap_rect(
            Rect::from_min_max(
                pos2(left, rect.center().y - 2.0),
                pos2(right, rect.center().y + 2.0),
            ),
            ppp,
        );
        // Rail, then the filled part up to the knob.
        ui.painter()
            .rect_filled(rail, 2.0, palette.switch_off_stroke.gamma_multiply(0.45));
        let filled = Rect::from_min_max(rail.min, pos2(knob_x, rail.max.y));
        ui.painter().rect_filled(filled, 2.0, palette.accent);

        if let Some(marker) = marker {
            let x = snap(left + (right - left) * marker.clamp(0.0, 1.0), ppp);
            let tick = Rect::from_min_max(
                pos2(x - 0.5, rail.max.y + 3.0),
                pos2(x + 0.5, rail.max.y + 8.0),
            );
            ui.painter()
                .rect_filled(snap_rect(tick, ppp), 0.0, palette.text_secondary);
        }

        // Knob: a control-colored disc with a border and the accent dot.
        let center = pos2(knob_x, rect.center().y);
        let inner = if response.dragged() {
            4.0
        } else if response.hovered() {
            6.0
        } else {
            5.0
        };
        ui.painter()
            .circle_filled(center, knob_radius, palette.control_fill);
        ui.painter().circle_stroke(
            center,
            knob_radius - 0.5 * crate::pixel::hairline(ppp),
            hairline_stroke(ppp, palette.control_stroke),
        );
        ui.painter().circle_filled(center, inner, palette.accent);
    }
    pointer(response)
}

/// Small caption row: a title at the left and an optional count at the right.
pub fn section_header(ui: &mut Ui, skin: &Skin<'_>, title: &str, count: Option<usize>) {
    let (rect, _) = allocate_row(ui, 28.0, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let palette = skin.palette;
    let count_text = count.map(|count| count.to_string());
    let mut right = rect.right() - 4.0;
    if let Some(text) = &count_text {
        let galley = ui.painter().layout_no_wrap(
            text.clone(),
            FontId::proportional(12.0),
            palette.text_secondary,
        );
        right -= galley.size().x;
        paint_text_left(ui, galley, right, rect.center().y, palette.text_secondary);
        right -= 8.0;
    }
    let galley = single_line(
        ui,
        title,
        theme::semibold(12.0),
        palette.text_secondary,
        (right - rect.left() - 4.0).max(1.0),
    );
    paint_text_left(
        ui,
        galley,
        rect.left() + 4.0,
        rect.center().y,
        palette.text_secondary,
    );
}

/// A hairline between sections, with space around it.
pub fn divider(ui: &mut Ui, skin: &Skin<'_>) {
    let ppp = ui.ctx().pixels_per_point();
    ui.add_space(8.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    let y = snap(rect.center().y, ppp);
    ui.painter().rect_filled(
        snap_rect(
            Rect::from_min_max(
                pos2(rect.left(), y - 0.5 * crate::pixel::hairline(ppp)),
                pos2(rect.right(), y + 0.5 * crate::pixel::hairline(ppp)),
            ),
            ppp,
        ),
        0.0,
        skin.palette.divider,
    );
    ui.add_space(8.0);
}

/// Connection status color of a camera.
pub fn status_color(status: crate::model::ConnectionStatus) -> Color32 {
    match status {
        crate::model::ConnectionStatus::Connected => theme::SUCCESS,
        crate::model::ConnectionStatus::Reconnecting => theme::CAUTION,
        crate::model::ConnectionStatus::Disconnected => theme::CRITICAL,
    }
}

/// Short word for a connection status (tooltips).
pub fn status_word(status: crate::model::ConnectionStatus) -> &'static str {
    match status {
        crate::model::ConnectionStatus::Connected => "Connected",
        crate::model::ConnectionStatus::Reconnecting => "Reconnecting",
        crate::model::ConnectionStatus::Disconnected => "Disconnected",
    }
}

/// One camera in the sidebar list: status dot, name and address.
pub fn camera_row(
    ui: &mut Ui,
    skin: &Skin<'_>,
    status: crate::model::ConnectionStatus,
    name: &str,
    address: &str,
    selected: bool,
) -> Response {
    let ppp = ui.ctx().pixels_per_point();
    let (rect, response) = allocate_row(ui, CAMERA_ROW_HEIGHT, Sense::click());
    if ui.is_rect_visible(rect) {
        let palette = skin.palette;
        if let Some(fill) = item_fill(skin, &response, selected) {
            ui.painter().rect_filled(rect, CONTROL_ROUNDING, fill);
        }
        if selected {
            paint_selection_bar(ui, skin, rect);
        }

        let dot = snap_pos(pos2(rect.left() + 24.0, rect.center().y), ppp);
        ui.painter().circle_filled(dot, 5.0, status_color(status));

        let text_left = rect.left() + 44.0;
        let max_width = rect.right() - PADDING - text_left;
        let name_galley = single_line(
            ui,
            name,
            FontId::proportional(14.0),
            palette.text,
            max_width,
        );
        let address_galley = single_line(
            ui,
            address,
            FontId::proportional(12.0),
            palette.text_secondary,
            max_width,
        );
        let block = name_galley.size().y + 2.0 + address_galley.size().y;
        let top = rect.center().y - block / 2.0;
        let name_center = top + name_galley.size().y / 2.0;
        let address_center = top + name_galley.size().y + 2.0 + address_galley.size().y / 2.0;
        paint_text_left(ui, name_galley, text_left, name_center, palette.text);
        paint_text_left(
            ui,
            address_galley,
            text_left,
            address_center,
            palette.text_secondary,
        );
    }
    pointer(response).on_hover_text(format!("{name}\n{address} · {}", status_word(status)))
}

/// Rail button for a camera: its number with a status dot in the corner.
pub fn camera_chip(
    ui: &mut Ui,
    skin: &Skin<'_>,
    status: crate::model::ConnectionStatus,
    number: usize,
    selected: bool,
) -> Response {
    let ppp = ui.ctx().pixels_per_point();
    let (rect, response) = ui.allocate_exact_size(vec2(ICON_BUTTON, ICON_BUTTON), Sense::click());
    let rect = snap_rect(rect, ppp);
    if ui.is_rect_visible(rect) {
        let palette = skin.palette;
        if let Some(fill) = item_fill(skin, &response, selected) {
            ui.painter().rect_filled(rect, CONTROL_ROUNDING, fill);
        }
        if selected {
            paint_selection_bar(ui, skin, rect);
        }
        let galley = single_line(
            ui,
            &number.to_string(),
            theme::semibold(13.0),
            palette.text,
            rect.width() - 8.0,
        );
        let pos = snap_pos(rect.center() - galley.size() / 2.0, ppp);
        ui.painter().galley(pos, galley, palette.text);
        ui.painter().circle_filled(
            snap_pos(pos2(rect.right() - 8.0, rect.top() + 8.0), ppp),
            3.5,
            status_color(status),
        );
    }
    pointer(response)
}

/// A caption-sized, wrapping text block (hints and empty states).
pub fn paragraph(ui: &mut Ui, skin: &Skin<'_>, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .size(12.0)
                .color(skin.palette.text_secondary),
        )
        .wrap(),
    );
}
