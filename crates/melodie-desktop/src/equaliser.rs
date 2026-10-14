//! The small equaliser that marks the music playing: three bars that move while it plays and
//! stand still when it is paused. Playing, it is a view of its own: an animation asks for a frame
//! of the view that holds it, and that view is these bars, not the page.

use gpui_kit::base::h_flex;
use gpui_kit::*;
use melodie_desktop::playing::{BARS, Mark, level};
use std::time::Duration;

/// The side of the mark.
pub const SIDE: f32 = 14.0;
const BAR: f32 = 3.0;
/// A turn of the animation.
const TURN: Duration = Duration::from_millis(1200);
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
}

impl Render for Moving {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let color = self.color;
        frame()
            .debug_selector(|| self.id.to_string())
            .children((0..BARS).map(move |i| {
                bar(color, level(i, 0.0)).with_animation(
                    ("equaliser-bar", i),
                    Animation::new(TURN).repeat(),
                    move |b, t| b.h(px(SIDE * level(i, t))),
                )
            }))
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
                let moving = window.use_keyed_state(id.clone(), cx, |_, _| Moving { id, color });
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
