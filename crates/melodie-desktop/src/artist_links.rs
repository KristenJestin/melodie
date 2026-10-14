//! The artists of a track on one line, each a link of its own to their page, separated by a plain
//! ", ". The line stays one line: when it is too long, the name that crosses its end is cut with
//! an ellipsis and those after it are hidden, as a single name is cut.

use gpui_kit::base::h_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::library::Credit;

/// How much sooner each name gives up its room than the one before it, when the line is too long.
/// Flexbox takes the missing room from every item, in proportion to its shrink factor times its
/// width: with factors this far apart, an earlier name loses far less than a float can tell (and
/// GPUI cuts a text only past a whole pixel) until the later ones are down to nothing.
const STEP: f32 = 1e10;
/// Past the fourth name, the names share what is missing (an `f32` ends near 1e38).
const STEPS: i32 = 3;

/// The artists of a track on one line. `link` draws the name of the artist at `ix`: it must cut
/// itself (`truncate`) and may shrink to nothing (`min_w_0`).
pub fn line(credits: &[Credit], mut link: impl FnMut(usize, &Credit) -> AnyElement) -> Div {
    h_flex()
        .min_w_0()
        .overflow_hidden()
        .children(credits.iter().enumerate().map(|(ix, c)| {
            let mut name = div();
            name.style().flex_shrink = Some(STEP.powi((ix as i32).min(STEPS)));
            name.flex()
                .min_w_0()
                .overflow_hidden()
                .when(ix > 0, |d| {
                    d.child(div().flex_none().whitespace_nowrap().child(", "))
                })
                .child(link(ix, c))
        }))
}

#[cfg(test)]
mod tests {
    use super::line;
    use gpui_kit::{
        Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
        Styled as _, TestAppContext, VisualTestContext, Window, div, px,
    };
    use melodie_core::library::Credit;

    struct Line {
        width: f32,
    }

    fn credits() -> Vec<Credit> {
        ["Nova Reyes", "Lior Castell", "Ada Marlow"]
            .map(|name| Credit {
                id: Some(name.into()),
                name: name.into(),
            })
            .to_vec()
    }

    impl Render for Line {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(self.width)).child(line(&credits(), |ix, c| {
                div()
                    .min_w_0()
                    .truncate()
                    .debug_selector(move || format!("name-{ix}"))
                    .child(c.name.clone())
                    .into_any_element()
            }))
        }
    }

    /// The width of each name drawn on a line `width` wide.
    fn widths(width: f32) -> Vec<Pixels> {
        let mut app = TestAppContext::single();
        let (_, cx): (Entity<Line>, &mut VisualTestContext) =
            app.add_window_view(|_, _| Line { width });
        cx.run_until_parked();
        ["name-0", "name-1", "name-2"]
            .iter()
            .map(|s| cx.debug_bounds(s).map_or(px(0.), |b| b.size.width))
            .collect()
    }

    #[test]
    fn a_line_too_long_cuts_the_name_that_crosses_its_end_and_hides_the_rest() {
        let whole = widths(2000.);
        assert!(whole.iter().all(|w| *w > px(0.)), "{whole:?}");
        // room for the first name and a part of the second
        let short = whole[0] + whole[1] / 2.;
        let cut = widths(short.as_f32());
        assert_eq!(cut[0], whole[0], "the first name stays whole: {cut:?}");
        assert!(
            cut[1] > px(0.) && cut[1] < whole[1],
            "the second is cut: {cut:?}"
        );
        assert_eq!(cut[2], px(0.), "the third is hidden: {cut:?}");
        // room for less than the first name: it is cut, the others hidden
        let tiny = widths((whole[0] / 2.).as_f32());
        assert!(tiny[0] > px(0.) && tiny[0] < whole[0], "{tiny:?}");
        assert_eq!((tiny[1], tiny[2]), (px(0.), px(0.)), "{tiny:?}");
    }
}
