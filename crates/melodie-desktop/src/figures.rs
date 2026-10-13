//! Numbers on screen: times and counts that change in place use tabular digits (every digit as
//! wide as the others, so `1:19` becoming `1:20` moves nothing), and a count beside a title
//! shares the title's baseline.

use gpui_kit::{Div, FontFeatures, ParentElement as _, SharedString, Styled, div};
use melodie_core::track::clock;
use std::sync::Arc;

/// The font features of a number that changes in place: tabular figures (`tnum`).
pub fn tabular_features() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

pub trait Figures: Styled + Sized {
    /// Tabular digits for this element and its children.
    fn tabular(self) -> Self {
        self.font_features(tabular_features())
    }
}

impl<T: Styled> Figures for T {}

/// A time as `m:ss`, in tabular digits.
pub fn clock_text(secs: f64) -> Div {
    count_text(clock(secs))
}

/// A count (or any text of figures), in tabular digits.
pub fn count_text(text: impl Into<SharedString>) -> Div {
    div().tabular().child(text.into())
}

/// A line of text: its font size and the height of its line box, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Line {
    pub size: f32,
    pub height: f32,
}

/// How far a small text, aligned on the bottom of its box with a large one, has to be raised
/// for the two to share a baseline. GPUI lays a text out without telling the flex layout where
/// its baseline is, so `items_baseline` falls back to the bottom edge and the small text sits
/// lower. `ascent` and `descent` are the font's, as fractions of the font size; the line box
/// puts half of its spare room above the text and half below.
pub fn baseline_lift(big: Line, small: Line, ascent: f32, descent: f32) -> f32 {
    let under = |l: Line| l.height / 2.0 - (ascent - descent) * l.size / 2.0;
    under(big) - under(small)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_figures_are_tabular() {
        let f = tabular_features();
        assert_eq!(f.tag_value_list(), &[("tnum".to_string(), 1)]);
    }

    #[test]
    fn a_time_text_uses_tabular_digits() {
        let mut t = clock_text(79.0);
        assert_eq!(t.style().text.font_features, Some(tabular_features()));
    }

    #[test]
    fn a_count_text_uses_tabular_digits() {
        let mut c = count_text("6901");
        assert_eq!(c.style().text.font_features, Some(tabular_features()));
    }

    #[test]
    fn any_element_can_be_made_tabular() {
        let mut d = div().tabular();
        assert_eq!(d.style().text.font_features, Some(tabular_features()));
    }

    /// Distance from the bottom of a line box to its text's baseline.
    fn under_baseline(l: Line, ascent: f32, descent: f32) -> f32 {
        let top = (l.height - (ascent + descent) * l.size) / 2.0 + ascent * l.size;
        l.height - top
    }

    const TITLE: Line = Line {
        size: 24.0,
        height: 32.0,
    };
    const COUNT: Line = Line {
        size: 14.0,
        height: 20.0,
    };

    #[test]
    fn raised_by_the_lift_the_count_shares_the_title_baseline() {
        let (a, d) = (0.93, 0.24);
        let lift = baseline_lift(TITLE, COUNT, a, d);
        let big = under_baseline(TITLE, a, d);
        let small = under_baseline(COUNT, a, d) + lift;
        assert!((big - small).abs() < 1e-4, "{big} against {small}");
        assert!(lift > 0.0, "the small text is the one that sits lower");
    }

    #[test]
    fn two_equal_lines_need_no_lift() {
        assert_eq!(baseline_lift(TITLE, TITLE, 0.93, 0.24), 0.0);
    }

    #[test]
    fn a_taller_line_box_alone_lifts_by_half_the_difference() {
        // a font with no ascent or descent difference: only the line boxes differ
        let lift = baseline_lift(
            Line {
                size: 20.0,
                height: 40.0,
            },
            Line {
                size: 20.0,
                height: 20.0,
            },
            0.5,
            0.5,
        );
        assert_eq!(lift, 10.0);
    }

    /// Every source of the crate but `skip`, with its name.
    fn sources(skip: &str) -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        std::fs::read_dir(dir)
            .unwrap()
            .map(|f| f.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "rs") && !p.ends_with(skip))
            .map(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                (name, std::fs::read_to_string(&p).unwrap())
            })
            .collect()
    }

    #[test]
    fn no_time_is_drawn_in_proportional_digits() {
        let mut seen = 0;
        for (name, src) in sources("figures.rs") {
            seen += src.matches("clock_text(").count();
            for (n, line) in src.lines().enumerate() {
                let code = line.split("//").next().unwrap();
                // a call to `clock` itself, not `clock_text` nor a name that ends in `clock`
                let bare = code.match_indices("clock(").any(|(at, _)| {
                    !code[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_')
                });
                assert!(
                    !bare,
                    "{name}:{}: a time drawn by `clock(…)` shifts as its digits change; use \
                     `figures::clock_text`",
                    n + 1
                );
            }
        }
        assert!(seen >= 4, "the check reads the sources: {seen}");
    }
}
