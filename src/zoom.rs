//! Zoom of the enlarged camera: scroll zooms in and out around the pointer.
//!
//! The zoom is a window over the picture in normalized texture coordinates
//! (`0..1`). It is never persisted: the app resets it whenever the enlarged
//! camera changes.

use egui::{pos2, vec2, Pos2, Rect, Vec2};

/// Largest zoom factor.
pub const MAX_SCALE: f32 = 16.0;
/// Zoom factor change per scrolled point (exponential, so every step feels the
/// same at any zoom level).
const SCROLL_SENSITIVITY: f32 = 0.002;

/// The part of the picture that is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zoom {
    /// 1.0 shows the whole picture.
    scale: f32,
    /// Top-left corner of the visible window, in normalized texture coordinates.
    origin: Vec2,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            scale: 1.0,
            origin: Vec2::ZERO,
        }
    }
}

impl Zoom {
    /// The visible window, in normalized texture coordinates (the `uv` of the
    /// picture).
    pub fn view(&self) -> Rect {
        let size = 1.0 / self.scale;
        Rect::from_min_size(self.origin.to_pos2(), vec2(size, size))
    }

    /// Nothing is zoomed.
    #[cfg(test)]
    pub fn is_reset(&self) -> bool {
        self.scale <= 1.0
    }

    /// Applies a scroll of `scroll_y` points (positive zooms in) keeping the
    /// picture point under `pointer` where it is. `video` is where the whole
    /// picture is drawn.
    pub fn scroll(&mut self, scroll_y: f32, pointer: Pos2, video: Rect) {
        if scroll_y == 0.0 || video.width() <= 0.0 || video.height() <= 0.0 {
            return;
        }
        let scale = (self.scale * (scroll_y * SCROLL_SENSITIVITY).exp()).clamp(1.0, MAX_SCALE);
        if scale == self.scale {
            return;
        }
        // Where the pointer is inside the drawn picture (0..1), and the
        // texture coordinate under it before the change.
        let inside = vec2(
            ((pointer.x - video.min.x) / video.width()).clamp(0.0, 1.0),
            ((pointer.y - video.min.y) / video.height()).clamp(0.0, 1.0),
        );
        let under_pointer = self.origin + inside / self.scale;
        let size = 1.0 / scale;
        self.scale = scale;
        let max_origin = 1.0 - size;
        let origin = under_pointer - inside * size;
        self.origin = vec2(
            origin.x.clamp(0.0, max_origin),
            origin.y.clamp(0.0, max_origin),
        );
    }
}

/// The whole picture.
pub fn full_view() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video() -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), vec2(400.0, 200.0))
    }

    #[test]
    fn starts_showing_the_whole_picture() {
        let zoom = Zoom::default();
        assert!(zoom.is_reset());
        assert_eq!(zoom.view(), full_view());
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer_in_place() {
        let mut zoom = Zoom::default();
        let pointer = pos2(200.0, 100.0);
        // Pointer at (0.25, 0.25) of the picture.
        zoom.scroll(300.0, pointer, video());
        assert!(!zoom.is_reset());
        let view = zoom.view();
        let texture_point = view.min + vec2(0.25, 0.25) * view.width();
        assert!((texture_point.x - 0.25).abs() < 1e-5);
        assert!((texture_point.y - 0.25).abs() < 1e-5);
        // And again, from the zoomed state.
        let other = pos2(400.0, 200.0);
        let before = zoom.view();
        let inside = vec2(0.75, 0.75);
        let point_before = before.min + inside * before.width();
        zoom.scroll(200.0, other, video());
        let after = zoom.view();
        let point_after = after.min + inside * after.width();
        assert!((point_before.x - point_after.x).abs() < 1e-5);
        assert!((point_before.y - point_after.y).abs() < 1e-5);
    }

    #[test]
    fn scale_is_limited_and_the_view_stays_inside_the_picture() {
        let mut zoom = Zoom::default();
        for _ in 0..100 {
            zoom.scroll(500.0, pos2(499.0, 249.0), video());
        }
        assert!((zoom.view().width() - 1.0 / MAX_SCALE).abs() < 1e-6);
        let view = zoom.view();
        assert!(view.max.x <= 1.0 + 1e-6 && view.max.y <= 1.0 + 1e-6);
        for _ in 0..200 {
            zoom.scroll(-500.0, pos2(100.0, 50.0), video());
        }
        assert!(zoom.is_reset());
        assert_eq!(zoom.view(), full_view());
    }

    #[test]
    fn a_pointer_outside_the_picture_zooms_around_its_edge() {
        let mut zoom = Zoom::default();
        zoom.scroll(300.0, pos2(0.0, 0.0), video());
        assert_eq!(zoom.view().min, pos2(0.0, 0.0));
    }

    #[test]
    fn zooming_out_at_the_minimum_does_nothing() {
        let mut zoom = Zoom::default();
        zoom.scroll(-300.0, pos2(200.0, 100.0), video());
        assert_eq!(zoom, Zoom::default());
    }
}
