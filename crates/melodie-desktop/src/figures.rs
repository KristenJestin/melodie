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
    use crate::scan::{Source, sources};

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

    /// The lines of `src` where a time is drawn by `clock(…)`, not `clock_text`.
    fn bare_clocks(src: &Source) -> Vec<usize> {
        src.calls("clock")
            .into_iter()
            .map(|(at, _)| src.line(at))
            .collect()
    }

    /// The lines of `src` where a count that changes in place (`format!("{n} …")`, a row's
    /// `(ix + 1).to_string()`) is drawn in proportional digits.
    fn proportional_counts(src: &Source) -> Vec<usize> {
        let code = src.code();
        // `format!("{n} …")`: a format that starts with a named number and a word
        let a_count = |args: &str| {
            args.contains("+ 1).to_string()")
                || args.match_indices("format!(\"{").any(|(at, f)| {
                    let rest = &args[at + f.len()..];
                    let name = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'));
                    name.is_some_and(|n| n > 0 && rest[n..].starts_with("} "))
                })
        };
        // the element the child goes in, from where it is made to the call
        let element = |at: usize| {
            let start = ["div()", "cell(", "h_flex()", "v_flex()"]
                .iter()
                .filter_map(|e| code[..at].rfind(e))
                .max()
                .unwrap_or(0);
            &code[start..at]
        };
        src.calls("child")
            .into_iter()
            .filter(|(at, args)| {
                a_count(args)
                    && !args.contains("count_text(")
                    && !args.contains(".tabular()")
                    && !element(*at).contains(".tabular()")
            })
            .map(|(at, _)| src.line(at))
            .collect()
    }

    #[test]
    fn a_proportional_count_is_seen_however_it_is_written() {
        let caught = |code: &str| proportional_counts(&Source::new("a.rs", code));
        assert_eq!(
            caught("div()\n    .px_2()\n    .child(format!(\"{n} selected\")),\n"),
            [3]
        );
        assert_eq!(
            caught(
                "div().child(match count {\n    1 => \"1 track\".to_string(),\n    n => format!(\"{n} tracks\"),\n})"
            ),
            [1]
        );
        assert_eq!(caught("div().map(|d| d.child((ix + 1).to_string()))"), [1]);
        assert!(
            caught("cell(None, 0.)\n    .tabular()\n    .child((ix + 1).to_string())").is_empty()
        );
        assert!(caught("div().child(count_text(format!(\"{n} tracks\")))").is_empty());
        assert!(caught("div().child(format!(\"{} by {}\", a, b))").is_empty());
        assert!(caught("d.child(div().id(format!(\"{list}-all\")).child(\"See all\"))").is_empty());
    }

    #[test]
    fn no_time_is_drawn_in_proportional_digits() {
        let mut seen = 0;
        for src in sources("figures.rs") {
            seen += src.code().matches("clock_text(").count();
            if let Some(n) = bare_clocks(&src).first() {
                panic!(
                    "{}:{n}: a time drawn by `clock(…)` shifts as its digits change; use \
                     `figures::clock_text`",
                    src.name
                );
            }
        }
        assert!(seen >= 4, "the check reads the sources: {seen}");
    }

    #[test]
    fn no_count_is_drawn_in_proportional_digits() {
        let mut seen = 0;
        for src in sources("figures.rs") {
            seen += src.code().matches(".tabular()").count();
            if let Some(n) = proportional_counts(&src).first() {
                panic!(
                    "{}:{n}: a count drawn in proportional digits shifts as it changes; use \
                     `figures::count_text` or `.tabular()`",
                    src.name
                );
            }
        }
        assert!(seen >= 2, "the check reads the sources: {seen}");
    }
}
