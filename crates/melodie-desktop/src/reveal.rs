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

/// How long the move to `shown` takes.
pub fn duration(shown: bool) -> Duration {
    if shown { IN } else { OUT }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(duration(true) > duration(false));
        assert_eq!(duration(true), IN);
        assert_eq!(duration(false), OUT);
        assert!(IN <= Duration::from_millis(200), "a hover is felt at once");
        assert!(OUT >= Duration::from_millis(80), "or it is a pop again");
    }

    #[test]
    fn nothing_pops_under_the_pointer() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut seen = 0;
        for file in std::fs::read_dir(dir).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") || path.ends_with("reveal.rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            seen += src.matches("reveal::").count();
            for (n, line) in src.lines().enumerate() {
                assert!(
                    !(line.contains("group_hover(") && line.contains("s.visible()")),
                    "{}:{}: shown at once under the pointer; fade it with `reveal`",
                    path.display(),
                    n + 1
                );
            }
        }
        assert!(seen >= 3, "the check reads the sources: {seen}");
    }
}
