//! The artists of a track on one line, each a link of its own to their page, separated by a plain
//! ", ". The line stays one line: when it is too long, the name that crosses its end is cut with
//! an ellipsis and those after it are hidden, as a single name is cut. A name left less room than
//! its ", …" is not drawn at all: the line ends on the whole name before it, never on a clipped
//! ellipsis.

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
            let name = name
                .flex()
                .min_w_0()
                .overflow_hidden()
                .when(ix > 0, |d| {
                    d.child(div().flex_none().whitespace_nowrap().child(", "))
                })
                .child(link(ix, c));
            Whole {
                name: name.into_any_element(),
                least: SharedString::new_static(if ix > 0 { ", …" } else { "…" }),
            }
        }))
}

/// A name of the line, drawn only when its room holds `least`: with less, its ellipsis would be
/// clipped. Its room comes from its own layout, so this costs no layout pass, only the shaping of
/// `least` (cached by the text system) when the line is drawn.
struct Whole {
    name: AnyElement,
    least: SharedString,
}

impl IntoElement for Whole {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Whole {
    type RequestLayoutState = ();
    /// Whether the name is drawn.
    type PrepaintState = bool;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.name.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let style = window.text_style();
        let size = style.font_size.to_pixels(window.rem_size());
        let run = style.to_run(self.least.len());
        let least = window
            .text_system()
            .shape_line(self.least.clone(), size, &[run], None)
            .width;
        let drawn = bounds.size.width >= least;
        if drawn {
            self.name.prepaint(window, cx);
        }
        drawn
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        drawn: &mut bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        if *drawn {
            self.name.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::line;
    use gpui_kit::{
        Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
        SharedString, Styled as _, TestAppContext, VisualTestContext, Window, div, px,
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

    /// The width of `text` drawn in the window's text style.
    fn measure(text: &'static str) -> Pixels {
        let mut app = TestAppContext::single();
        let (_, cx): (Entity<Line>, &mut VisualTestContext) =
            app.add_window_view(|_, _| Line { width: 0. });
        cx.update(|window, _| {
            let style = window.text_style();
            let size = style.font_size.to_pixels(window.rem_size());
            let run = style.to_run(text.len());
            window
                .text_system()
                .shape_line(SharedString::new_static(text), size, &[run], None)
                .width
        })
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

    #[test]
    fn a_name_with_less_room_than_its_ellipsis_is_not_drawn() {
        let whole = widths(2000.);
        let comma = measure(", ");
        let ellipsis = measure("…");
        // a few pixels for the second name, less than its ellipsis
        let few = widths((whole[0] + comma + ellipsis / 2.).as_f32());
        assert_eq!(few[0], whole[0], "the first name stays whole: {few:?}");
        assert_eq!(few[1], px(0.), "no clipped ellipsis: {few:?}");
        assert_eq!(few[2], px(0.), "{few:?}");
        // room for the whole ellipsis: it is drawn
        let enough = widths((whole[0] + comma + ellipsis + px(1.)).as_f32());
        assert!(enough[1] >= ellipsis, "a whole ellipsis: {enough:?}");
    }
}
