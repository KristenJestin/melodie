//! The small equaliser that marks the music playing: three bars that move while it plays and
//! stand still when it is paused. Playing, it is a view of its own that ticks a dozen times a
//! second. Each tick also draws again the view that drew the mark (a table, a page: the window
//! observes the keyed state and dirties its ancestors), about 1.3 ms a tick on the library's
//! 7 200 tracks, 22 ms of drawing a second; an animation asked at every frame would draw the
//! window at the screen's rate for as long as the mark is on it, about 340 ms a second
//! (`docs/gui-toolkit.md`, "Frame times"). Every mark ticks on one clock, in the same frame.

use gpui_kit::base::h_flex;
use gpui_kit::*;
use melodie_desktop::playing::{BARS, Mark, level};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The side of the mark.
pub const SIDE: f32 = 14.0;
const BAR: f32 = 3.0;
/// A turn of the animation.
const TURN: Duration = Duration::from_millis(2400);
/// How often the bars move.
const TICK: Duration = Duration::from_millis(80);
/// Where the bars stand when they do not move: apart from each other.
const STILL: f32 = 0.3;

/// How much of a badge its bars take.
const BADGE_BARS: f32 = 0.6;

/// The clock of every mark, so that they all tick in the same frame and move in phase.
fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// How long a mark, `since` into the clock, sleeps until the next tick.
fn wait(since: Duration) -> Duration {
    let tick = TICK.as_nanos();
    Duration::from_nanos((tick - since.as_nanos() % tick) as u64)
}

/// A bar of the mark of side `side`: the bars keep their proportions when it is smaller.
fn bar(side: f32, color: Hsla, height: f32) -> Div {
    div()
        .w(px(BAR * side / SIDE))
        .h(px(side * height))
        .rounded_sm()
        .flex_none()
        .bg(color)
}

fn frame(side: f32) -> Div {
    let bar = BAR * side / SIDE;
    h_flex()
        .w(px(side))
        .h(px(side))
        .flex_none()
        .items_end()
        .gap(px((side - bar * BARS as f32) / (BARS as f32 - 1.0)))
}

/// The bars moving.
struct Moving {
    id: ElementId,
    side: f32,
    color: Hsla,
    _tick: Task<()>,
}

impl Moving {
    fn new(id: ElementId, side: f32, color: Hsla, cx: &mut Context<Self>) -> Self {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(wait(epoch().elapsed()))
                    .await;
                // the view is gone when its row is no longer drawn
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            id,
            side,
            color,
            _tick: tick,
        }
    }
}

impl Render for Moving {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = (epoch().elapsed().as_secs_f32() / TURN.as_secs_f32()).fract();
        frame(self.side)
            .debug_selector(|| self.id.to_string())
            .children((0..BARS).map(|i| bar(self.side, self.color, level(i, t))))
    }
}

/// The mark of a row, a card or a playlist: `id` names it for as long as it is drawn.
#[derive(IntoElement)]
pub struct Equaliser {
    id: ElementId,
    mark: Mark,
    color: Hsla,
    side: f32,
}

impl Equaliser {
    pub fn new(id: impl Into<ElementId>, mark: Mark, color: Hsla) -> Self {
        Self {
            id: id.into(),
            mark,
            color,
            side: SIDE,
        }
    }

    /// Smaller (or larger) than the mark of a row.
    pub fn side(mut self, side: f32) -> Self {
        self.side = side;
        self
    }
}

impl RenderOnce for Equaliser {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            mark,
            color,
            side,
        } = self;
        match mark {
            Mark::Paused => frame(side)
                .debug_selector(|| id.to_string())
                .children((0..BARS).map(|i| bar(side, color, level(i, STILL))))
                .into_any_element(),
            Mark::Playing => {
                let moving = window
                    .use_keyed_state(id.clone(), cx, |_, cx| Moving::new(id, side, color, cx));
                moving.update(cx, |m, cx| {
                    if (m.color, m.side) != (color, side) {
                        (m.color, m.side) = (color, side);
                        cx.notify();
                    }
                });
                moving.into_any_element()
            }
        }
    }
}

/// The mark on the corner of a cover: a small dark square holding the equaliser, laid over the
/// bottom-right corner of the nearest `relative` parent. `id` names the equaliser; the badge is
/// `<id>-badge`.
pub fn badge(id: impl Into<ElementId>, mark: Mark, side: f32, accent: Hsla) -> Div {
    let id: ElementId = id.into();
    div()
        .debug_selector(|| format!("{id}-badge"))
        .absolute()
        .right(px(-2.))
        .bottom(px(-2.))
        .size(px(side))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(side / 4.))
        .bg(hsla(0., 0., 0., 0.78))
        .child(Equaliser::new(id, mark, accent).side(side * BADGE_BARS))
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

#[cfg(test)]
mod tests {
    use super::{Equaliser, TICK, TURN, wait};
    use gpui_kit::{
        Context, IntoElement, ParentElement as _, Render, TestAppContext, VisualTestContext,
        Window, div, hsla,
    };
    use melodie_desktop::playing::{BARS, Mark, level};
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    #[test]
    fn every_mark_ticks_on_the_same_clock() {
        for since in [0, 30, 79, 80, 1234] {
            let w = wait(Duration::from_millis(since));
            assert!(!w.is_zero() && w <= TICK, "{since} ms: {w:?}");
            let at = Duration::from_millis(since) + w;
            assert_eq!(at.as_millis() % TICK.as_millis(), 0, "{since} ms: {w:?}");
        }
    }

    #[test]
    fn no_bar_jumps_half_its_room_in_a_tick() {
        let step = TICK.as_secs_f32() / TURN.as_secs_f32();
        for bar in 0..BARS {
            for i in 0..1000 {
                let t = i as f32 / 1000.0;
                let jump = (level(bar, t + step) - level(bar, t)).abs();
                // a bar that crosses most of its room between two ticks looks jerky, not quick
                assert!(jump < 0.8 / 2.0, "bar {bar} at {t}: {jump}");
            }
        }
    }

    /// A view showing one mark, counting its renders.
    struct Host {
        mark: Mark,
        renders: Rc<Cell<u32>>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            div().child(Equaliser::new("eq", self.mark, hsla(0., 0., 0., 1.)))
        }
    }

    #[test]
    fn the_bars_stop_ticking_once_paused() {
        let mut app = TestAppContext::single();
        let renders = Rc::new(Cell::new(0));
        let (view, cx) = app.add_window_view(|_, _| Host {
            mark: Mark::Playing,
            renders: renders.clone(),
        });
        cx.run_until_parked();
        let ticks = |cx: &mut VisualTestContext| {
            let before = renders.get();
            for _ in 0..10 {
                cx.executor().advance_clock(TICK);
                cx.run_until_parked();
            }
            renders.get() - before
        };
        // each tick draws the view that holds the mark again, not only the bars
        assert!(ticks(cx) >= 9, "playing: it ticks");
        view.update(cx, |h, cx| {
            h.mark = Mark::Paused;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(ticks(cx), 0, "paused: the timer stopped");
    }
}
