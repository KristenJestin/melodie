//! What shows only under the pointer (the play button on a cover, the actions of a row) fades
//! in and out and moves a little, instead of popping. Kept away from GPUI so that it is tested.

use std::time::Duration;

/// How far, in pixels, a revealed thing travels while it appears.
pub const TRAVEL: f32 = 6.0;

/// Coming in: as quick as a hover.
pub const IN: Duration = Duration::from_millis(160);
/// Going out: quicker, so a pointer moving on leaves nothing behind.
pub const OUT: Duration = Duration::from_millis(110);

/// How a revealed thing looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub opacity: f32,
    /// Pixels still to travel to its place: `TRAVEL` hidden, 0 shown.
    pub offset: f32,
}

impl Look {
    /// Nothing to draw, nothing to hit: the thing is left out of the hit test.
    pub fn hidden(&self) -> bool {
        self.opacity <= 0.0
    }
}

/// The look of a revealed thing `t` of the way in (0 hidden, 1 shown; out of range clamps).
pub fn at(t: f32) -> Look {
    let t = t.clamp(0.0, 1.0);
    Look {
        opacity: t,
        offset: TRAVEL * (1.0 - t),
    }
}

/// How long the move to `shown` takes. A change the pointer did not cause (the layout moved
/// under a still pointer: a list scrolled with the wheel) is instant: an animation in a row would
/// keep the whole list off its scroll layer for as long as it runs.
pub fn duration(shown: bool, moved: bool) -> Duration {
    match (moved, shown) {
        (false, _) => Duration::ZERO,
        (true, true) => IN,
        (true, false) => OUT,
    }
}

/// Tells a hover change caused by the pointer moving from one caused by the layout moving under
/// it: GPUI reports both alike. One move can change several hovers (leaving a row enters the
/// next), so it counts as moving until `settle` says that move is over.
#[derive(Debug, Default)]
pub struct Moves<P> {
    last: Option<P>,
    moving: bool,
}

impl<P: PartialEq + Copy> Moves<P> {
    /// A hover changes with the pointer at `at`: whether the pointer moved to get there.
    pub fn seen(&mut self, at: P) -> bool {
        if self.last != Some(at) {
            self.last = Some(at);
            self.moving = true;
        }
        self.moving
    }

    /// The changes of the last move are all told: another one at the same place is the layout's.
    pub fn settle(&mut self) {
        self.moving = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Source, sources};

    #[test]
    fn hidden_it_is_transparent_and_a_step_away() {
        let l = at(0.0);
        assert_eq!(l.opacity, 0.0);
        assert_eq!(l.offset, TRAVEL);
        assert!(l.hidden());
    }

    #[test]
    fn shown_it_is_opaque_and_in_its_place() {
        let l = at(1.0);
        assert_eq!(l.opacity, 1.0);
        assert_eq!(l.offset, 0.0);
        assert!(!l.hidden());
    }

    #[test]
    fn it_fades_and_moves_steadily_between_the_two() {
        let mut last = at(0.0);
        for i in 1..=20 {
            let l = at(i as f32 / 20.0);
            assert!(l.opacity > last.opacity, "{l:?} after {last:?}");
            assert!(l.offset < last.offset, "{l:?} after {last:?}");
            last = l;
        }
    }

    #[test]
    fn it_moves_a_little_not_a_lot() {
        assert!((2.0..=10.0).contains(&TRAVEL));
    }

    #[test]
    fn out_of_range_progress_is_clamped() {
        assert_eq!(at(-0.5), at(0.0));
        assert_eq!(at(1.5), at(1.0));
    }

    #[test]
    fn coming_in_takes_longer_than_going_out_and_both_are_short() {
        assert!(duration(true, true) > duration(false, true));
        assert_eq!(duration(true, true), IN);
        assert_eq!(duration(false, true), OUT);
        assert!(IN <= Duration::from_millis(200), "a hover is felt at once");
        assert!(OUT >= Duration::from_millis(80), "or it is a pop again");
    }

    #[test]
    fn a_change_under_a_still_pointer_is_instant() {
        assert_eq!(duration(true, false), Duration::ZERO);
        assert_eq!(duration(false, false), Duration::ZERO);
    }

    #[test]
    fn the_first_change_seen_is_a_move() {
        let mut m = Moves::default();
        assert!(m.seen((10.0, 20.0)));
    }

    #[test]
    fn every_change_of_one_move_is_a_move() {
        // leaving a row and entering the next one, at the same place, in the same move
        let mut m = Moves::default();
        assert!(m.seen((10.0, 20.0)));
        assert!(m.seen((10.0, 20.0)));
    }

    #[test]
    fn a_list_scrolled_under_a_still_pointer_is_not_a_move() {
        let mut m = Moves::default();
        m.seen((10.0, 20.0));
        m.settle();
        // rows pass under the pointer, which stays where it was
        assert!(!m.seen((10.0, 20.0)));
        assert!(!m.seen((10.0, 20.0)));
        m.settle();
        assert!(!m.seen((10.0, 20.0)));
    }

    #[test]
    fn a_pointer_that_moves_again_is_a_move_again() {
        let mut m = Moves::default();
        m.seen((10.0, 20.0));
        m.settle();
        assert!(!m.seen((10.0, 20.0)));
        assert!(m.seen((10.0, 52.0)));
        assert!(m.seen((10.0, 52.0)));
    }

    /// The lines of `src` where a thing is shown at once under the pointer.
    fn popping(src: &Source) -> Vec<usize> {
        src.calls("group_hover")
            .into_iter()
            .filter(|(_, args)| args.contains(".visible()"))
            .map(|(at, _)| src.line(at))
            .collect()
    }

    #[test]
    fn a_pop_is_seen_however_it_is_written() {
        let one_line = Source::new("a.rs", "d.group_hover(\"g\", |s| s.visible())");
        assert_eq!(popping(&one_line), [1]);
        let split = Source::new(
            "a.rs",
            "d.invisible()\n    .group_hover(\"row\", |style| {\n        style.visible()\n    })\n",
        );
        assert_eq!(popping(&split), [2]);
        let dimmed = Source::new("a.rs", "d.group_hover(\"g\", |s| s.opacity(0.85))");
        assert!(popping(&dimmed).is_empty());
    }

    #[test]
    fn nothing_pops_under_the_pointer() {
        let mut seen = 0;
        for src in sources("reveal.rs") {
            seen += src.code().matches("reveal::").count();
            if let Some(n) = popping(&src).first() {
                panic!(
                    "{}:{n}: shown at once under the pointer; fade it with `reveal`",
                    src.name
                );
            }
        }
        assert!(seen >= 3, "the check reads the sources: {seen}");
    }
}
