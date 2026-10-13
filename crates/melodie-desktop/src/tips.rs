//! The tooltips of the icon-only buttons: short, after a short delay, fading in and rising a
//! little, and placed beside the button, never on it. GPUI's own `.tooltip()` is not used: it
//! sits on the pointer, below and right of it, so it would cover the button being pressed.
//! gpui-kit's managed tooltips need gpui-component's `Root`, which the window does not use; the
//! overlay below is gpui-base's, the one those are built on.

use crate::motion;
use gpui_kit::base::animation::{EffectTransition, ease_in_out_cubic, ease_out_cubic};
use gpui_kit::base::{
    ElementExt, TooltipDefaults, TooltipOverlay, TooltipRequest, TooltipTransition,
};
use gpui_kit::*;
use melodie_desktop::theme::Palette;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// How long the pointer rests on a button before it speaks. Moving to another button within
/// the grace period of the overlay (300 ms) switches at once.
const DELAY: Duration = Duration::from_millis(400);

struct Tips(Entity<TooltipOverlay>);

impl Global for Tips {}

/// Makes the overlay of this window; the root view draws it (`overlay`).
pub fn install(cx: &mut App) {
    TooltipDefaults::new().with_show_delay(DELAY).install(cx);
    let overlay = cx.new(|_| TooltipOverlay::new().render_with(present));
    cx.set_global(Tips(overlay));
}

/// The overlay to draw above everything else.
pub fn overlay(cx: &App) -> Option<Entity<TooltipOverlay>> {
    cx.try_global::<Tips>().map(|t| t.0.clone())
}

/// A tooltip leaves at once: a press, a key, a page that goes.
pub fn hide(cx: &mut App) {
    if let Some(overlay) = overlay(cx) {
        overlay.update(cx, |o, cx| o.hide(cx));
    }
}

/// How it comes in: a short fade and rise; from one button to the next on the same row, it
/// slides across instead.
fn present(
    view: AnyView,
    transition: TooltipTransition,
    _: &mut Window,
    _: &mut App,
) -> AnyElement {
    let popup = div().child(view);
    match transition {
        TooltipTransition::Enter { epoch } => EffectTransition::new(motion::HOVER)
            .ease(ease_out_cubic)
            .slide_y(px(4.), px(0.))
            .fade(0.0, 1.0)
            .apply(
                popup,
                ElementId::NamedInteger("tip-enter".into(), epoch as u64),
            )
            .into_any_element(),
        TooltipTransition::Switch {
            epoch,
            previous,
            current,
        } => {
            if (current.origin.y - previous.origin.y).abs() >= px(10.) {
                return popup.into_any_element();
            }
            let dx = current.center().x - previous.center().x;
            EffectTransition::new(motion::PANEL)
                .ease(ease_in_out_cubic)
                .slide_x(-dx, px(0.))
                .apply(
                    popup,
                    ElementId::NamedInteger("tip-slide".into(), epoch as u64),
                )
                .into_any_element()
        }
    }
}

/// What the tooltip shows.
struct Words {
    text: SharedString,
    pal: Palette,
}

impl Render for Words {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        // the outer space keeps it off the button it speaks of
        div().m_2().child(
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(pal.surface)
                .border_1()
                .border_color(Hsla { a: 0.14, ..pal.fg })
                .shadow_md()
                .text_xs()
                .text_color(pal.fg)
                .child(self.text.clone()),
        )
    }
}

/// A button's tooltip: where the button is, and what to say when the pointer rests on it.
#[derive(Clone)]
pub struct Tip {
    text: SharedString,
    pal: Palette,
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl Tip {
    pub fn new(text: impl Into<SharedString>, pal: Palette) -> Self {
        Self {
            text: text.into(),
            pal,
            bounds: Rc::default(),
        }
    }

    /// Learns where `el` is drawn, for the tooltip to sit beside it.
    pub fn track<E: ParentElement + ElementExt>(&self, el: E) -> E {
        let bounds = self.bounds.clone();
        el.on_prepaint(move |b, _, _| bounds.set(b))
    }

    /// The pointer enters or leaves the button.
    pub fn hovered(&self, on: bool, window: &mut Window, cx: &mut App) {
        let Some(overlay) = overlay(cx) else { return };
        if !on {
            overlay.update(cx, |o, cx| o.request_hide(window, cx));
            return;
        }
        let words = cx.new(|_| Words {
            text: self.text.clone(),
            pal: self.pal,
        });
        let request = TooltipRequest::new(self.bounds.get(), move |_, _| words.clone().into());
        overlay.update(cx, |o, cx| o.request_show(request, window, cx));
    }
}

/// `.tip("…")` on a button that is not an `IconButton`.
pub trait Tipped {
    fn tip(self, text: impl Into<SharedString>, pal: Palette) -> Self;
}

impl Tipped for Stateful<Div> {
    fn tip(self, text: impl Into<SharedString>, pal: Palette) -> Self {
        let tip = Tip::new(text, pal);
        let hover = tip.clone();
        tip.track(self)
            .on_hover(move |on, window, cx| hover.hovered(*on, window, cx))
            .on_mouse_down(MouseButton::Left, |_, _, cx| hide(cx))
    }
}
