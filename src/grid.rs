//! Pure logic of the Viewing_Grid.

use crate::model::ConnectionStatus;

/// Visual indicator shown over a Camera_Tile, derived from the connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorKind {
    None, // normal video, no overlay
    Disconnected,
    Reconnecting,
}

/// Computes the integer square root rounded up (ceil(sqrt(n))), for n > 0.
fn ceil_sqrt(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    // Initial floating-point estimate, corrected to avoid precision errors.
    let mut c = (n as f64).sqrt() as usize;
    if c == 0 {
        c = 1;
    }
    while c * c < n {
        c += 1;
    }
    while c > 1 && (c - 1) * (c - 1) >= n {
        c -= 1;
    }
    c
}

/// Computes the grid columns/rows for N cameras (Requirement 2.2).
///
/// N = 0 -> (0, 0). N > 0 -> columns = ceil(sqrt(N)), rows = ceil(N / columns).
pub fn grid_dimensions(n: usize) -> (usize, usize) {
    if n == 0 {
        return (0, 0);
    }
    let columns = ceil_sqrt(n);
    let rows = n.div_ceil(columns);
    (columns, rows)
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

    proptest! {
        // Feature: simple-rtsp-viewer, Property 2: Grid dimension calculation
        #[test]
        fn property_grid_dimensions(n in 0usize..100_000) {
            let (columns, rows) = grid_dimensions(n);
            if n == 0 {
                prop_assert_eq!(columns, 0);
                prop_assert_eq!(rows, 0);
            } else {
                // columns = ceil(sqrt(n))
                prop_assert!(columns * columns >= n);
                prop_assert!((columns - 1) * (columns - 1) < n);
                // rows = ceil(n / columns); the grid fits all cameras
                prop_assert!(columns * rows >= n);
                // no row is entirely superfluous
                prop_assert!(columns * (rows - 1) < n);
            }
        }

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

    #[test]
    fn grid_dimensions_edge_cases() {
        assert_eq!(grid_dimensions(0), (0, 0));
        assert_eq!(grid_dimensions(1), (1, 1));
        assert_eq!(grid_dimensions(2), (2, 1));
        assert_eq!(grid_dimensions(3), (2, 2));
        assert_eq!(grid_dimensions(4), (2, 2));
        assert_eq!(grid_dimensions(64), (8, 8));
    }
}
