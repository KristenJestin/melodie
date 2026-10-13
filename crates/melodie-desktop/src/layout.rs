//! Where the queue goes: a column beside the main area when both fit, over the main area on a
//! narrow window. Kept away from GPUI so that it is tested.

/// The queue's width, docked or over the main area.
pub const QUEUE_WIDTH: f32 = 380.0;

/// The narrowest the main area gets beside the queue: the player's cover stays large and a track
/// list keeps its title, artist, album and duration readable. With the full sidebar (220) the
/// queue docks from 1160 pixels, so the window's default width (1180) docks it.
pub const MAIN_MIN: f32 = 560.0;

/// How the queue stands in a window `viewport` pixels wide, beside a sidebar `sidebar` wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queue {
    Hidden,
    /// A column of its own: the main area narrows, nothing is covered.
    Docked,
    /// Over the main area, as the window is too narrow for both.
    Over,
}

impl Queue {
    pub fn of(viewport: f32, sidebar: f32, shown: bool) -> Self {
        if !shown {
            Self::Hidden
        } else if viewport >= sidebar + MAIN_MIN + QUEUE_WIDTH {
            Self::Docked
        } else {
            Self::Over
        }
    }

    /// The width the queue takes from the main area.
    pub fn column(self) -> f32 {
        if self == Self::Docked {
            QUEUE_WIDTH
        } else {
            0.0
        }
    }

    /// Escape closes it, and a click beside it: it is laid over the rest.
    pub fn covers(self) -> bool {
        self == Self::Over
    }
}

/// The width left to the main area once the sidebar and the docked queue have theirs.
pub fn main_width(viewport: f32, sidebar: f32, shown: bool) -> f32 {
    viewport - sidebar - Queue::of(viewport, sidebar, shown).column()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIDEBAR: f32 = 220.0;

    #[test]
    fn shown_on_a_wide_window_the_queue_is_a_column_and_the_main_area_narrows() {
        let q = Queue::of(1400.0, SIDEBAR, true);
        assert_eq!(q, Queue::Docked);
        assert_eq!(q.column(), QUEUE_WIDTH);
        assert!(!q.covers(), "nothing is covered");
        assert_eq!(
            main_width(1400.0, SIDEBAR, true),
            1400.0 - SIDEBAR - QUEUE_WIDTH
        );
    }

    #[test]
    fn hidden_the_main_area_takes_the_whole_width_back() {
        for viewport in [400.0, 1000.0, 1400.0] {
            let q = Queue::of(viewport, SIDEBAR, false);
            assert_eq!(q, Queue::Hidden);
            assert_eq!(q.column(), 0.0);
            assert!(!q.covers());
            assert_eq!(main_width(viewport, SIDEBAR, false), viewport - SIDEBAR);
        }
    }

    #[test]
    fn on_a_window_too_narrow_for_both_the_queue_covers_the_main_area() {
        let edge = SIDEBAR + MAIN_MIN + QUEUE_WIDTH;
        assert_eq!(Queue::of(edge, SIDEBAR, true), Queue::Docked);
        let q = Queue::of(edge - 1.0, SIDEBAR, true);
        assert_eq!(q, Queue::Over);
        assert!(q.covers());
        assert_eq!(q.column(), 0.0, "the main area keeps its width under it");
        assert_eq!(main_width(edge - 1.0, SIDEBAR, true), edge - 1.0 - SIDEBAR);
        // the compact sidebar of a narrow window never leaves room for both
        assert_eq!(Queue::of(700.0, 64.0, true), Queue::Over);
    }

    #[test]
    fn the_window_s_default_width_docks_the_queue() {
        assert_eq!(Queue::of(1180.0, SIDEBAR, true), Queue::Docked);
    }
}
