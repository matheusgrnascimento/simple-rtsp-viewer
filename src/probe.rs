//! Test-only record of where each component was drawn.
//!
//! The layout tests render the interface without a window, at many window
//! sizes, and check that no two components overlap and that everything stays
//! inside its area. Components report their rectangles with the `probe!`
//! macro. Outside the tests the macro expands to nothing, so the real app
//! pays nothing (not even for building the names).

/// Rectangles recorded during one frame, with a name each.
#[cfg(test)]
#[derive(Default)]
pub struct Probe {
    rects: std::cell::RefCell<Vec<(String, egui::Rect)>>,
}

#[cfg(test)]
impl Probe {
    pub fn record(&self, name: impl Into<String>, rect: egui::Rect) {
        self.rects.borrow_mut().push((name.into(), rect));
    }

    /// Takes everything recorded so far.
    pub fn take(&self) -> Vec<(String, egui::Rect)> {
        std::mem::take(&mut *self.rects.borrow_mut())
    }
}

#[cfg(not(test))]
#[derive(Default)]
pub struct Probe(());

/// Records `rect` under `name` in `probe`, in tests only. Outside the tests it
/// only borrows its arguments (so they do not count as unused), and the name,
/// which can be a `format!`, is never evaluated.
macro_rules! probe {
    ($probe:expr, $name:expr, $rect:expr) => {{
        #[cfg(test)]
        {
            $probe.record($name, $rect);
        }
        #[cfg(not(test))]
        {
            let _ = (&$probe, &$rect);
        }
    }};
}
