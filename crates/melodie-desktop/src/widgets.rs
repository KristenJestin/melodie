//! A round icon button, or a pill with a label, whose hover and press are animated (gpui-kit's
//! buttons change at once).

use crate::motion;
use crate::tips::{self, Tip};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_desktop::theme::{Palette, mix};

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
    label: Option<SharedString>,
    diameter: f32,
    pal: Palette,
    look: Look,
    selected: bool,
    disabled: bool,
    tip: Option<SharedString>,
    /// Another icon the first one changes into, and how far it has (0 to 1).
    morph: Option<(IconName, f32)>,
    on_click: Option<OnClick>,
}

/// Where the pointer is, kept per element by its id.
#[derive(Default, Clone, Copy)]
pub struct Pointer {
    pub hovered: bool,
    pub pressed: bool,
}

/// The pointer state of the element `id`, and its hover and press animated from 0 to 1 (both
/// stay 0 while it is not `live`).
pub fn pointer(
    id: &ElementId,
    live: bool,
    window: &mut Window,
    cx: &mut App,
) -> (Entity<Pointer>, f32, f32) {
    let pointer = window.use_keyed_state(id.clone(), cx, |_, _| Pointer::default());
    let p = *pointer.read(cx);
    let h = motion::flag(
        (id.clone(), "hover"),
        p.hovered && live,
        motion::HOVER,
        window,
        cx,
    );
    let d = motion::flag(
        (id.clone(), "press"),
        p.pressed && live,
        motion::PRESS,
        window,
        cx,
    );
    (pointer, h, d)
}

/// Keeps `pointer` up to date from the element's own events.
pub fn follow_pointer(el: Stateful<Div>, pointer: Entity<Pointer>) -> Stateful<Div> {
    follow_pointer_tip(el, pointer, None)
}

/// The same, and the tooltip `tip` while the pointer rests on the element (an element has one
/// hover listener, so the tooltip rides on it).
pub fn follow_pointer_tip(
    el: Stateful<Div>,
    pointer: Entity<Pointer>,
    tip: Option<Tip>,
) -> Stateful<Div> {
    let (hover, press, release, out) = (pointer.clone(), pointer.clone(), pointer.clone(), pointer);
    let el = match &tip {
        Some(tip) => tip.track(el),
        None => el,
    };
    el.on_hover(move |on, window, cx| {
        if let Some(tip) = &tip {
            tip.hovered(*on, window, cx);
        }
        hover.update(cx, |p, cx| {
            p.hovered = *on;
            p.pressed &= *on;
            cx.notify()
        })
    })
    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
        tips::hide(cx);
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
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, icon: IconName, pal: Palette) -> Self {
        Self {
            id: id.into(),
            icon,
            label: None,
            diameter: 40.0,
            pal,
            look: Look::Ghost,
            selected: false,
            disabled: false,
            tip: None,
            morph: None,
            on_click: None,
        }
    }

    /// The icon changes into `into` as `t` goes from 0 to 1: play and pause cross-fade.
    pub fn morph(mut self, into: IconName, t: f32) -> Self {
        self.morph = Some((into, t));
        self
    }

    /// What the button does, said when the pointer rests on it. Every button without a text
    /// has one (a test checks).
    pub fn tip(mut self, text: impl Into<SharedString>) -> Self {
        self.tip = Some(text.into());
        self
    }

    pub fn diameter(mut self, d: f32) -> Self {
        self.diameter = d;
        self
    }

    /// A pill: the icon, then this text.
    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.label = Some(text.into());
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
        let live = !self.disabled;
        let (pointer, h, d) = pointer(&self.id, live, window, cx);
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
        let label = self.label;
        let tip = self.tip.map(|text| Tip::new(text, pal));
        follow_pointer_tip(div().id(self.id), pointer, tip)
            .h(px(self.diameter))
            .when(label.is_none(), |d| d.w(px(self.diameter)))
            .when(label.is_some(), |d| d.pl_3().pr_4().gap_2())
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(bg)
            .when(self.disabled, |d| d.opacity(0.4))
            .when(live, |d| d.cursor_pointer())
            .when_some(self.on_click.filter(|_| live), |d, f| d.on_click(f))
            .map(|b| match self.morph {
                Some((into, t)) if t > 0.0 => b.child(
                    div()
                        .relative()
                        .size(px(icon_size))
                        .mt(px(d))
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .opacity(1.0 - t)
                                .child(Icon::new(self.icon).size(px(icon_size)).text_color(fg)),
                        )
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .opacity(t)
                                .child(Icon::new(into).size(px(icon_size)).text_color(fg)),
                        ),
                ),
                _ => b.child(
                    Icon::new(self.icon)
                        .size(px(icon_size))
                        .text_color(fg)
                        // the press sinks the icon a little: felt, not seen
                        .mt(px(d)),
                ),
            })
            .when_some(label, |b, text| {
                b.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(fg)
                        .mt(px(d))
                        .child(text),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    /// The line (from 1) of every `IconButton::new(…)` in `src` whose chain of calls says neither
    /// `.tip(…)` nor `.label(…)`: a button of an icon alone, mute when the pointer rests on it.
    fn mute_buttons(src: &str) -> Vec<usize> {
        let bytes = src.as_bytes();
        let mut mute = Vec::new();
        for (at, _) in src.match_indices("IconButton::new(") {
            let line_start = src[..at].rfind('\n').map_or(0, |n| n + 1);
            if src[line_start..at].trim_start().starts_with("//") {
                continue;
            }
            // follow `( … ).call( … ).call( … )` to the end of the chain
            let mut i = at + "IconButton::new".len();
            loop {
                let mut depth = 0;
                while i < bytes.len() {
                    match bytes[i] {
                        b'"' => {
                            i += 1;
                            while bytes[i] != b'"' {
                                i += if bytes[i] == b'\\' { 2 } else { 1 };
                            }
                        }
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                i += 1;
                let rest = src[i..].trim_start();
                let Some(call) = rest.strip_prefix('.') else {
                    break;
                };
                let name = call
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(call.len());
                i = src.len() - call.len() + name;
                if !src[i..].starts_with('(') {
                    break;
                }
            }
            let chain = &src[at..i];
            if !chain.contains(".tip(") && !chain.contains(".label(") {
                mute.push(src[..at].matches('\n').count() + 1);
            }
        }
        mute
    }

    #[test]
    fn the_check_finds_a_button_without_a_tip_or_a_label() {
        let src = r#"
            IconButton::new("a", IconName::Play, pal).tip("Play (now)").on_click(|_, _, _| {});
            // IconButton::new("commented", IconName::X, pal)
            IconButton::new("b", IconName::X, pal)
                .diameter(34.)
                .on_click(move |_, _, cx| clear.update(cx, |c, cx| c.unselect(cx))),
            IconButton::new("c", IconName::Plus, pal).label("New").primary()
            IconButton::new("d", IconName::Plus, pal).disabled(true)
        "#;
        assert_eq!(mute_buttons(src), [4, 8]);
    }

    #[test]
    fn every_icon_button_says_what_it_does() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut seen = 0;
        for file in std::fs::read_dir(dir).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            seen += src.matches("IconButton::new(").count();
            // the definition of the button is not a use of it
            if path.ends_with("widgets.rs") {
                continue;
            }
            assert_eq!(
                mute_buttons(&src),
                Vec::<usize>::new(),
                "{}: an IconButton without .tip(…) or .label(…) (lines)",
                path.display()
            );
        }
        assert!(seen > 20, "the check looks at the sources: {seen}");
    }
}
