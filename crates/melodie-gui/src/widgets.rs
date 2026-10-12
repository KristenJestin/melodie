//! A round icon button whose hover and press are animated (gpui-kit's buttons change at once).

use crate::motion;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_gui::theme::{Palette, mix};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// No background until hovered.
    Ghost,
    /// Filled with the accent: the play button.
    Primary,
}

type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    diameter: f32,
    pal: Palette,
    look: Look,
    selected: bool,
    disabled: bool,
    on_click: Option<OnClick>,
}

/// Where the pointer is, kept per button by its id.
#[derive(Default, Clone, Copy)]
struct Pointer {
    hovered: bool,
    pressed: bool,
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, icon: IconName, pal: Palette) -> Self {
        Self {
            id: id.into(),
            icon,
            diameter: 40.0,
            pal,
            look: Look::Ghost,
            selected: false,
            disabled: false,
            on_click: None,
        }
    }

    pub fn diameter(mut self, d: f32) -> Self {
        self.diameter = d;
        self
    }

    pub fn primary(mut self) -> Self {
        self.look = Look::Primary;
        self
    }

    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    pub fn disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pointer = window.use_keyed_state(self.id.clone(), cx, |_, _| Pointer::default());
        let p = *pointer.read(cx);
        let live = !self.disabled;
        let h = motion::flag(
            (self.id.clone(), "hover"),
            p.hovered && live,
            motion::HOVER,
            window,
            cx,
        );
        let d = motion::flag(
            (self.id.clone(), "press"),
            p.pressed && live,
            motion::PRESS,
            window,
            cx,
        );
        let s = motion::flag(
            (self.id.clone(), "selected"),
            self.selected,
            motion::HOVER,
            window,
            cx,
        );
        let pal = self.pal;
        let (bg, fg) = match self.look {
            Look::Ghost => (
                Hsla {
                    a: 0.09 * h + 0.08 * d + 0.12 * s,
                    ..pal.fg
                },
                mix(pal.fg, pal.accent, s),
            ),
            Look::Primary => (
                mix(mix(pal.accent, pal.fg, 0.14 * h), pal.bg, 0.18 * d),
                pal.on_accent,
            ),
        };
        let icon_size = (self.diameter * 0.45).round();
        let (hover, press, release, out) =
            (pointer.clone(), pointer.clone(), pointer.clone(), pointer);
        div()
            .id(self.id)
            .size(px(self.diameter))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(bg)
            .when(self.disabled, |d| d.opacity(0.4))
            .when(live, |d| d.cursor_pointer())
            .on_hover(move |on, _, cx| {
                hover.update(cx, |p, cx| {
                    p.hovered = *on;
                    p.pressed &= *on;
                    cx.notify()
                })
            })
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                press.update(cx, |p, cx| {
                    p.pressed = true;
                    cx.notify()
                })
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                release.update(cx, |p, cx| {
                    p.pressed = false;
                    cx.notify()
                })
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                out.update(cx, |p, cx| {
                    p.pressed = false;
                    cx.notify()
                })
            })
            .when_some(self.on_click.filter(|_| live), |d, f| d.on_click(f))
            .child(
                Icon::new(self.icon)
                    .size(px(icon_size))
                    .text_color(fg)
                    // the press sinks the icon a little: felt, not seen
                    .mt(px(d)),
            )
    }
}
