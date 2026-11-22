//! Pure logic of the Viewing_Grid.

use egui::{pos2, vec2, Rect};

use crate::model::ConnectionStatus;
use crate::pixel::{inset_to_pixels, snap_rect};

/// Visual indicator shown over a Camera_Tile, derived from the connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorKind {
    None, // normal video, no overlay
    Disconnected,
    Reconnecting,
}

/// Proportions of the video the tiles are sized for (16:9, what IP cameras send).
pub const VIDEO_ASPECT: f32 = 16.0 / 9.0;

/// Chooses the columns and rows for `count` tiles in an area of `width` x
/// `height` points (Requirement 2.2), so the 16:9 video of each tile comes out
/// as large as possible: a wide window gets more columns, a tall one more
/// rows, instead of a fixed square-ish grid that leaves big empty bars.
///
/// Ties go to the layout with fewer empty cells, then to fewer columns.
/// `count` = 0 -> (0, 0). A degenerate area falls back to a square-ish grid.
pub fn best_grid(count: usize, width: f32, height: f32, gap: f32) -> (usize, usize) {
    if count == 0 {
        return (0, 0);
    }

    // The video area one tile can show in a given layout.
    let video_area = |columns: usize| -> Option<(f32, usize)> {
        let rows = count.div_ceil(columns);
        let cell_width = (width - gap * (columns - 1) as f32) / columns as f32;
        let cell_height = (height - gap * (rows - 1) as f32) / rows as f32;
        if cell_width <= 0.0 || cell_height <= 0.0 {
            return None;
        }
        let video_width = cell_width.min(cell_height * VIDEO_ASPECT);
        Some((video_width * video_width / VIDEO_ASPECT, rows))
    };

    let mut best: Option<(usize, usize, f32)> = None; // (columns, rows, area)
    for columns in 1..=count {
        let Some((area, rows)) = video_area(columns) else {
            continue;
        };
        let better = match best {
            None => true,
            Some((best_columns, best_rows, best_area)) => {
                // Areas within 0.5% count as equal: float noise must not flip
                // the layout from one frame to the next.
                if area > best_area * 1.005 {
                    true
                } else if area >= best_area * 0.995 {
                    let empty = columns * rows - count;
                    let best_empty = best_columns * best_rows - count;
                    empty < best_empty
                } else {
                    false
                }
            }
        };
        if better {
            best = Some((columns, rows, area));
        }
    }

    match best {
        Some((columns, rows, _)) => (columns, rows),
        None => {
            // No layout fits the area (it is tiny): square-ish.
            let columns = (count as f64).sqrt().ceil() as usize;
            (columns, count.div_ceil(columns))
        }
    }
}

/// Rectangles of `count` tiles filling `area`, in reading order.
///
/// The tiles are the same size, separated by `gap`, with every edge on a whole
/// physical pixel (`pixels_per_point`), and an incomplete last row is centered
/// instead of hugging the left. Tiles never overlap each other nor leave
/// `area`.
pub fn tile_rects(count: usize, area: Rect, gap: f32, pixels_per_point: f32) -> Vec<Rect> {
    // Laid out inside the largest pixel-aligned rectangle within `area`: the
    // rounding of every edge then lands inside it too.
    let area = inset_to_pixels(area, pixels_per_point);
    let (columns, rows) = best_grid(count, area.width(), area.height(), gap);
    if count == 0 {
        return Vec::new();
    }

    let cell = vec2(
        ((area.width() - gap * (columns - 1) as f32) / columns as f32).max(1.0),
        ((area.height() - gap * (rows - 1) as f32) / rows as f32).max(1.0),
    );

    (0..count)
        .map(|index| {
            let row = index / columns;
            let column = index % columns;
            // Tiles in this row: the last one may be incomplete.
            let in_row = (count - row * columns).min(columns);
            let row_width = cell.x * in_row as f32 + gap * (in_row - 1) as f32;
            let left = area.left() + (area.width() - row_width) / 2.0;
            let min = pos2(
                left + column as f32 * (cell.x + gap),
                area.top() + row as f32 * (cell.y + gap),
            );
            snap_rect(Rect::from_min_size(min, cell), pixels_per_point)
        })
        .collect()
}

/// Applies a position swap from drag and drop (Requirement 4.1/4.2). If `to` is invalid (outside
/// [0, len)) or equal to `from`, returns the original list unchanged.
pub fn reorder<T: Clone>(items: &[T], from: usize, to: usize) -> Vec<T> {
    let mut result = items.to_vec();
    if to >= items.len() || to == from || from >= items.len() {
        return result;
    }
    result.swap(from, to);
    result
}

/// Deterministic mapping from connection status to the visual indicator shown
/// (Requirement 2.5/2.6).
pub fn status_indicator(status: ConnectionStatus) -> IndicatorKind {
    match status {
        ConnectionStatus::Connected => IndicatorKind::None,
        ConnectionStatus::Disconnected => IndicatorKind::Disconnected,
        ConnectionStatus::Reconnecting => IndicatorKind::Reconnecting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Video area one tile shows in a `columns` x `rows` layout (for comparing).
    fn video_area(count: usize, columns: usize, width: f32, height: f32, gap: f32) -> f32 {
        let rows = count.div_ceil(columns);
        let cell_width = (width - gap * (columns - 1) as f32) / columns as f32;
        let cell_height = (height - gap * (rows - 1) as f32) / rows as f32;
        let video_width = cell_width.min(cell_height * VIDEO_ASPECT).max(0.0);
        video_width * video_width / VIDEO_ASPECT
    }

    #[test]
    fn best_grid_edge_cases() {
        assert_eq!(best_grid(0, 1000.0, 600.0, 8.0), (0, 0));
        assert_eq!(best_grid(1, 1000.0, 600.0, 8.0), (1, 1));
        // Four 16:9 tiles in a 16:9 area: 2 x 2.
        assert_eq!(best_grid(4, 1600.0, 900.0, 8.0), (2, 2));
    }

    #[test]
    fn best_grid_follows_the_shape_of_the_area() {
        // Two cameras: side by side in a wide area, stacked in a tall one.
        assert_eq!(best_grid(2, 1200.0, 500.0, 8.0), (2, 1));
        assert_eq!(best_grid(2, 500.0, 1200.0, 8.0), (1, 2));
        // Five cameras in a 16:9-ish area: 3 + 2.
        assert_eq!(best_grid(5, 1000.0, 560.0, 8.0), (3, 2));
        // A single row of many cameras only in a very wide area.
        assert_eq!(best_grid(4, 4000.0, 300.0, 8.0), (4, 1));
    }

    #[test]
    fn degenerate_areas_fall_back_to_a_square_grid() {
        let (columns, rows) = best_grid(9, 4.0, 4.0, 8.0);
        assert_eq!((columns, rows), (3, 3));
    }

    proptest! {
        // The chosen layout holds every camera and shows video at least as
        // large as the old fixed ceil(sqrt(n)) grid did.
        #[test]
        fn property_best_grid(
            count in 1usize..=64,
            width in 200.0f32..4000.0,
            height in 150.0f32..2400.0,
        ) {
            let gap = 8.0;
            let (columns, rows) = best_grid(count, width, height, gap);
            prop_assert!(columns >= 1 && rows >= 1);
            prop_assert!(columns * rows >= count);
            // No row is entirely superfluous.
            prop_assert!(columns * (rows - 1) < count);

            let square = (count as f64).sqrt().ceil() as usize;
            let chosen = video_area(count, columns, width, height, gap);
            let baseline = video_area(count, square, width, height, gap);
            prop_assert!(chosen >= baseline * 0.994, "{chosen} < {baseline}");
        }

        // Tiles are inside the area, never overlap, keep the gap, and have
        // every edge on a physical pixel.
        #[test]
        fn property_tile_rects(
            count in 0usize..=64,
            width in 320.0f32..3000.0,
            height in 200.0f32..2000.0,
            left in 0.0f32..400.0,
            top in 0.0f32..400.0,
            ppp_index in 0usize..5,
        ) {
            let ppp = [1.0f32, 1.25, 1.5, 2.0, 2.5][ppp_index];
            let gap = 8.0;
            let area = Rect::from_min_size(pos2(left, top), vec2(width, height));
            let rects = tile_rects(count, area, gap, ppp);
            prop_assert_eq!(rects.len(), count);

            for (index, rect) in rects.iter().enumerate() {
                prop_assert!(area.expand(1e-3).contains_rect(*rect), "{rect:?} outside {area:?}");
                prop_assert!(rect.width() >= 1.0 / ppp - 1e-3);
                prop_assert!(rect.width() > 0.0 && rect.height() > 0.0);
                for value in [rect.min.x, rect.min.y, rect.max.x, rect.max.y] {
                    let pixels = value * ppp;
                    prop_assert!((pixels - pixels.round()).abs() < 1e-2, "edge {value} at {ppp}");
                }
                for other in &rects[index + 1..] {
                    prop_assert!(
                        !rect.shrink(1e-3).intersects(*other),
                        "{rect:?} overlaps {other:?}"
                    );
                    // At least (gap - 1 pixel) between any two tiles.
                    let dx = (other.min.x - rect.max.x).max(rect.min.x - other.max.x);
                    let dy = (other.min.y - rect.max.y).max(rect.min.y - other.max.y);
                    prop_assert!(dx.max(dy) >= gap - 1.5 / ppp, "gap {dx} / {dy}");
                }
            }
        }
    }

    #[test]
    fn incomplete_last_row_is_centered() {
        // 5 tiles, 3 columns: the second row has 2 tiles, centered.
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 560.0));
        let rects = tile_rects(5, area, 8.0, 1.0);
        assert_eq!(best_grid(5, 1000.0, 560.0, 8.0), (3, 2));

        let first_row_left = rects[0].min.x;
        let first_row_right = rects[2].max.x;
        let second_row_left = rects[3].min.x;
        let second_row_right = rects[4].max.x;
        // Same space on both sides of the incomplete row (to the pixel).
        let left_margin = second_row_left - first_row_left;
        let right_margin = first_row_right - second_row_right;
        assert!(
            (left_margin - right_margin).abs() <= 1.0,
            "{left_margin} vs {right_margin}"
        );
        assert!(left_margin > 100.0);
    }

    #[test]
    fn a_single_tile_fills_the_area() {
        let area = Rect::from_min_size(pos2(10.0, 20.0), vec2(801.0, 450.0));
        let rects = tile_rects(1, area, 8.0, 1.0);
        assert_eq!(rects, vec![area]);
    }

    proptest! {
        // Feature: simple-rtsp-viewer, Property 4: Drag-and-drop reordering
        #[test]
        fn property_reorder(len in 1usize..50, from_raw in 0usize..50, to in 0usize..60) {
            let items: Vec<usize> = (0..len).collect();
            let from = from_raw % len;
            let result = reorder(&items, from, to);

            prop_assert_eq!(result.len(), items.len());
            if to >= len || to == from {
                prop_assert_eq!(&result, &items);
            } else {
                let mut expected = items.clone();
                expected.swap(from, to);
                prop_assert_eq!(&result, &expected);
            }
        }
    }

    // Feature: simple-rtsp-viewer, Property 3: Status to visual indicator mapping
    #[test]
    fn property_status_indicator_distinct_and_specific() {
        use crate::model::ConnectionStatus::*;
        assert_eq!(status_indicator(Disconnected), IndicatorKind::Disconnected);
        assert_eq!(status_indicator(Reconnecting), IndicatorKind::Reconnecting);
        assert_eq!(status_indicator(Connected), IndicatorKind::None);

        let all = [Connected, Disconnected, Reconnecting];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i != j {
                    assert_ne!(status_indicator(*a), status_indicator(*b));
                }
            }
        }
    }
}
