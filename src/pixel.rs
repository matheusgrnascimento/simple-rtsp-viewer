//! Pixel-exact geometry.
//!
//! egui lays out in points, and a point is a whole number of physical pixels
//! only at 100% and 200% scaling. At 125% or 150% a 1-point line or a box
//! edge at x = 10.4 lands between two pixels and gets smeared over both. Text,
//! borders and video look crisp when their edges sit on whole physical pixels,
//! so everything drawn goes through these helpers.

use egui::{pos2, Color32, Pos2, Rect, Stroke, Vec2};

/// Rounds `value` (in points) to the nearest physical pixel.
pub fn snap(value: f32, pixels_per_point: f32) -> f32 {
    (value * pixels_per_point).round() / pixels_per_point
}

/// Rounds a position to the physical pixel grid.
pub fn snap_pos(pos: Pos2, pixels_per_point: f32) -> Pos2 {
    Pos2::new(snap(pos.x, pixels_per_point), snap(pos.y, pixels_per_point))
}

/// Rounds every edge of `rect` to the physical pixel grid.
pub fn snap_rect(rect: Rect, pixels_per_point: f32) -> Rect {
    Rect::from_min_max(
        snap_pos(rect.min, pixels_per_point),
        snap_pos(rect.max, pixels_per_point),
    )
}

/// The largest rectangle with every edge on a whole physical pixel that fits
/// inside `rect` (the opposite of `snap_rect`, which rounds to the nearest and
/// can stick out by half a pixel). Layouts that must stay inside an area, like
/// the grid, are computed inside this one.
pub fn inset_to_pixels(rect: Rect, pixels_per_point: f32) -> Rect {
    let ceil = |value: f32| (value * pixels_per_point).ceil() / pixels_per_point;
    let floor = |value: f32| (value * pixels_per_point).floor() / pixels_per_point;
    let min = pos2(ceil(rect.min.x), ceil(rect.min.y));
    let max = pos2(floor(rect.max.x), floor(rect.max.y));
    // A rectangle thinner than a pixel collapses to a line, never inverts.
    Rect::from_min_max(min, pos2(max.x.max(min.x), max.y.max(min.y)))
}

/// One physical pixel, in points.
pub fn hairline(pixels_per_point: f32) -> f32 {
    1.0 / pixels_per_point
}

/// A border exactly one physical pixel wide.
pub fn hairline_stroke(pixels_per_point: f32, color: Color32) -> Stroke {
    Stroke::new(hairline(pixels_per_point), color)
}

/// The rectangle to give `rect_stroke` so a `hairline_stroke` around `rect`
/// is crisp: egui centers a stroke on the path, so the path has to run through
/// the middle of the outermost pixel row/column, half a pixel inside the edge.
pub fn border_rect(rect: Rect, pixels_per_point: f32) -> Rect {
    snap_rect(rect, pixels_per_point).shrink(0.5 * hairline(pixels_per_point))
}

/// Size of `rect` in physical pixels.
pub fn physical_size(size: Vec2, pixels_per_point: f32) -> (u32, u32) {
    (
        (size.x * pixels_per_point).floor().max(0.0) as u32,
        (size.y * pixels_per_point).floor().max(0.0) as u32,
    )
}

/// Largest rectangle with the proportions of `content_size` that fits in
/// `container`, centered in it. Used to draw video without distorting it: one
/// of the dimensions fills the container and the other is left over equally on
/// both sides. Non-positive sizes (no valid frame or an empty container)
/// return `container` itself, avoiding a division by zero.
pub fn fit_preserving_aspect(container: Rect, content_size: Vec2) -> Rect {
    if content_size.x <= 0.0
        || content_size.y <= 0.0
        || container.width() <= 0.0
        || container.height() <= 0.0
    {
        return container;
    }

    let scale = (container.width() / content_size.x).min(container.height() / content_size.y);
    Rect::from_center_size(container.center(), content_size * scale)
}

/// How far from 1:1 a video may be scaled and still be drawn 1:1 (with the
/// few left-over pixels as bars): 3%.
const ONE_TO_ONE_TOLERANCE: f32 = 0.03;

/// Where to draw a video texture of `texture_pixels` (physical pixels) inside
/// `tile`.
///
/// The decoder already scales every frame to the size the tile shows it at, so
/// the texture is usually within a few pixels of the tile. Then it is drawn at
/// its native size, centered on whole pixels, which makes every texel land on
/// exactly one screen pixel: no GPU resampling at all, so no blur. When the
/// video is much smaller than the tile (or still larger, as during a window
/// resize) it is fitted instead, keeping its proportions.
pub fn video_rect(tile: Rect, texture_pixels: Vec2, pixels_per_point: f32) -> Rect {
    let native = texture_pixels / pixels_per_point;
    let fitted = fit_preserving_aspect(tile, native);
    if native.x <= 0.0 || native.y <= 0.0 {
        return fitted;
    }

    let scale = fitted.width() / native.x;
    if (scale - 1.0).abs() <= ONE_TO_ONE_TOLERANCE {
        let min = snap_pos(tile.center() - native / 2.0, pixels_per_point);
        Rect::from_min_size(min, native)
    } else {
        snap_rect(fitted, pixels_per_point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    fn assert_rect_close(actual: Rect, expected: Rect) {
        let close = (actual.min - expected.min).length() < 1e-3
            && (actual.max - expected.max).length() < 1e-3;
        assert!(close, "expected {expected:?}, got {actual:?}");
    }

    /// `value` is a whole number of physical pixels.
    fn on_pixel_grid(value: f32, ppp: f32) -> bool {
        let pixels = value * ppp;
        (pixels - pixels.round()).abs() < 1e-3
    }

    #[test]
    fn snapping_lands_on_whole_pixels_at_every_common_scale() {
        for ppp in [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0] {
            for value in [0.0, 0.3, 10.4, 99.99, 1234.567] {
                let snapped = snap(value, ppp);
                assert!(on_pixel_grid(snapped, ppp), "{value} at {ppp}: {snapped}");
                // Never moves by more than half a pixel.
                assert!((snapped - value).abs() <= 0.5 / ppp + 1e-4);
            }
        }
    }

    #[test]
    fn inset_stays_inside_the_rectangle_and_on_whole_pixels() {
        for ppp in [1.0, 1.25, 1.5, 2.0, 2.5] {
            let area = rect(10.3, 7.9, 600.37, 310.61);
            let inset = inset_to_pixels(area, ppp);
            assert!(area.contains_rect(inset), "{inset:?} not inside {area:?}");
            for value in [inset.min.x, inset.min.y, inset.max.x, inset.max.y] {
                assert!(on_pixel_grid(value, ppp), "{value} at {ppp}");
            }
            // Loses less than one pixel on each side.
            assert!(inset.min.x - area.min.x < 1.0 / ppp + 1e-4);
            assert!(area.max.y - inset.max.y < 1.0 / ppp + 1e-4);
        }
        // A rectangle already on the grid is unchanged.
        let exact = rect(10.0, 20.0, 300.0, 200.0);
        assert_eq!(inset_to_pixels(exact, 1.0), exact);
        // Thinner than a pixel: collapses, never inverts.
        let thin = inset_to_pixels(rect(0.2, 0.2, 0.3, 0.3), 1.0);
        assert!(thin.width() >= 0.0 && thin.height() >= 0.0);
    }

    #[test]
    fn hairline_is_one_physical_pixel_and_the_border_path_is_inside_the_edge() {
        assert_eq!(hairline(1.0), 1.0);
        assert_eq!(hairline(2.0), 0.5);
        let ppp = 1.5;
        let tile = rect(10.4, 20.7, 100.0, 50.0);
        let snapped = snap_rect(tile, ppp);
        let path = border_rect(tile, ppp);
        // The path is half a pixel inside each edge of the snapped rect...
        assert!((path.min.x - (snapped.min.x + 0.5 / ppp)).abs() < 1e-4);
        assert!((path.max.y - (snapped.max.y - 0.5 / ppp)).abs() < 1e-4);
        // ...so the stroke, centered on it, covers exactly one pixel row.
        let stroke = hairline_stroke(ppp, Color32::WHITE);
        assert!((stroke.width - 1.0 / ppp).abs() < 1e-6);
        assert!(on_pixel_grid(path.min.x - stroke.width / 2.0, ppp));
    }

    #[test]
    fn physical_size_floors_to_whole_pixels() {
        assert_eq!(physical_size(vec2(300.0, 170.0), 1.5), (450, 255));
        assert_eq!(physical_size(vec2(10.9, 0.4), 1.0), (10, 0));
        assert_eq!(physical_size(vec2(-5.0, 3.0), 2.0), (0, 6));
    }

    #[test]
    fn wide_video_in_square_block_gets_bars_above_and_below() {
        // 16:9 in a 400x400 block: fills the width, height 225, centered.
        let fitted = fit_preserving_aspect(rect(0.0, 0.0, 400.0, 400.0), vec2(1920.0, 1080.0));
        assert_rect_close(fitted, rect(0.0, 87.5, 400.0, 225.0));
    }

    #[test]
    fn tall_video_in_wide_block_gets_bars_on_the_sides() {
        // 9:16 in an offset 800x450 block: fills the height, width
        // 450 * 9/16 = 253.125, centered on x = 10 + 400.
        let fitted = fit_preserving_aspect(rect(10.0, 20.0, 800.0, 450.0), vec2(1080.0, 1920.0));
        let width = 450.0 * 1080.0 / 1920.0;
        assert_rect_close(fitted, rect(410.0 - width / 2.0, 20.0, width, 450.0));
    }

    #[test]
    fn invalid_sizes_return_the_container() {
        let container = rect(0.0, 0.0, 300.0, 200.0);
        assert_eq!(
            fit_preserving_aspect(container, vec2(0.0, 1080.0)),
            container
        );
        assert_eq!(
            fit_preserving_aspect(container, vec2(1920.0, 0.0)),
            container
        );
        let empty = rect(5.0, 5.0, 0.0, 100.0);
        assert_eq!(fit_preserving_aspect(empty, vec2(1920.0, 1080.0)), empty);
    }

    #[test]
    fn video_close_to_the_tile_size_is_drawn_one_to_one_on_whole_pixels() {
        for ppp in [1.0, 1.25, 1.5, 2.0] {
            // A tile of 640x360 pixels at this scale, at an awkward position.
            let tile = snap_rect(
                Rect::from_min_size(pos2(13.3, 7.7), vec2(640.0, 360.0) / ppp),
                ppp,
            );
            // The decoder produced 636x358 (width aligned to 4).
            let texture = vec2(636.0, 358.0);
            let drawn = video_rect(tile, texture, ppp);

            // Exactly the texture size in pixels: one texel per screen pixel.
            assert!((drawn.width() * ppp - 636.0).abs() < 1e-3, "{drawn:?}");
            assert!((drawn.height() * ppp - 358.0).abs() < 1e-3, "{drawn:?}");
            assert!(on_pixel_grid(drawn.min.x, ppp) && on_pixel_grid(drawn.min.y, ppp));
            // Centered in the tile (to the pixel).
            assert!((drawn.center() - tile.center()).length() <= 1.0 / ppp);
        }
    }

    #[test]
    fn video_far_from_the_tile_size_is_fitted_keeping_its_proportions() {
        // A 640x360 video in a 1280x1000 tile: upscaled 2x, with bars.
        let tile = rect(0.0, 0.0, 1280.0, 1000.0);
        let drawn = video_rect(tile, vec2(640.0, 360.0), 1.0);
        assert_rect_close(drawn, rect(0.0, 140.0, 1280.0, 720.0));

        // Larger than the tile (a resize in progress): scaled down to fit.
        let drawn = video_rect(rect(0.0, 0.0, 800.0, 450.0), vec2(1600.0, 900.0), 1.0);
        assert_rect_close(drawn, rect(0.0, 0.0, 800.0, 450.0));
    }

    #[test]
    fn empty_texture_does_not_divide_by_zero() {
        let tile = rect(0.0, 0.0, 100.0, 100.0);
        assert_eq!(video_rect(tile, vec2(0.0, 0.0), 1.0), tile);
    }
}
