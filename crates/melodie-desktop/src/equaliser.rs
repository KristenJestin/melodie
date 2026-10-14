//! The small equaliser that marks the music playing: three bars that move while it plays and
//! stand still when it is paused. Playing, it is a view of its own that ticks a dozen times a
//! second, so that a frame redraws these bars and not the page; an animation asked at every frame
//! would draw the window at the screen's rate for as long as the mark is on it
//! (`docs/gui-toolkit.md`, "Frame times").

use gpui_kit::base::h_flex;
use gpui_kit::*;
use melodie_desktop::playing::{BARS, Mark, level};
use std::time::{Duration, Instant};

/// The side of the mark.
pub const SIDE: f32 = 14.0;
const BAR: f32 = 3.0;
/// A turn of the animation.
const TURN: Duration = Duration::from_millis(1200);
/// How often the bars move.
const TICK: Duration = Duration::from_millis(80);
/// Where the bars stand when they do not move: apart from each other.
const STILL: f32 = 0.3;

fn bar(color: Hsla, height: f32) -> Div {
    div()
        .w(px(BAR))
        .h(px(SIDE * height))
        .rounded_sm()
        .flex_none()
        .bg(color)
}

fn frame() -> Div {
    h_flex()
        .w(px(SIDE))
        .h(px(SIDE))
        .flex_none()
        .items_end()
        .gap(px((SIDE - BAR * BARS as f32) / (BARS as f32 - 1.0)))
}

/// The bars moving.
struct Moving {
    id: ElementId,
    color: Hsla,
    since: Instant,
    _tick: Task<()>,
}

impl Moving {
    fn new(id: ElementId, color: Hsla, cx: &mut Context<Self>) -> Self {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                // the view is gone when its row is no longer drawn
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            id,
            color,
            since: Instant::now(),
            _tick: tick,
        }
    }
}

impl Render for Moving {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = (self.since.elapsed().as_secs_f32() / TURN.as_secs_f32()).fract();
        frame()
            .debug_selector(|| self.id.to_string())
            .children((0..BARS).map(|i| bar(self.color, level(i, t))))
    }
}

/// The mark of a row, a card or a playlist: `id` names it for as long as it is drawn.
#[derive(IntoElement)]
pub struct Equaliser {
    id: ElementId,
    mark: Mark,
    color: Hsla,
}

impl Equaliser {
    pub fn new(id: impl Into<ElementId>, mark: Mark, color: Hsla) -> Self {
        Self {
            id: id.into(),
            mark,
            color,
        }
    }
}

impl RenderOnce for Equaliser {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, mark, color } = self;
        match mark {
            Mark::Paused => frame()
                .debug_selector(|| id.to_string())
                .children((0..BARS).map(|i| bar(color, level(i, STILL))))
                .into_any_element(),
            Mark::Playing => {
                let moving =
                    window.use_keyed_state(id.clone(), cx, |_, cx| Moving::new(id, color, cx));
                moving.update(cx, |m, cx| {
                    if m.color != color {
                        m.color = color;
                        cx.notify();
                    }
                });
                moving.into_any_element()
            }
        }
    }
}

/// A name on its line: when the music of what it names plays, in the accent and with the
/// equaliser before it. `line` is the line's own look, without the colour of the text.
pub fn name_line(
    line: Div,
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    mark: Option<Mark>,
    accent: Hsla,
) -> Div {
    let id: ElementId = id.into();
    let line = line.debug_selector(|| format!("{id}-line"));
    match mark {
        None => line.truncate().child(name.into()),
        Some(mark) => line
            .text_color(accent)
            .flex()
            .items_start()
            .gap_2()
            // squeezed like the plain line when the card is short, not pushing its neighbours
            .overflow_hidden()
            // on the middle of the first line of a `text_sm`, which the text starts at the top of
            .child(
                div()
                    .mt(px(4.))
                    .child(Equaliser::new(id.clone(), mark, accent)),
            )
            .child(div().min_w_0().truncate().child(name.into())),
    }
}
