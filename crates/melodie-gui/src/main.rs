//! `melodie-gui`: the window. A client of the daemon like `melodie watch`; closing it never stops
//! the music, and it never plays sound itself.

mod app;
mod motion;
mod player;
mod queue;
#[cfg(feature = "frame-stats")]
mod stats;
mod widgets;

use app::Shared;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::env::Env;
use melodie_core::proto::{self, Request};
use melodie_gui::theme::{self, Active, Choice, Palette};
use player::{PlayerView, palette};
use queue::QueuePanel;
use std::process::ExitCode;
use widgets::IconButton;

gpui_kit::assets::icon_assets!(
    Icons,
    [
        AudioLines,
        GripVertical,
        Heart,
        ListMusic,
        ListX,
        Music,
        Palette,
        Pause,
        Play,
        SkipBack,
        SkipForward,
        Volume2,
        X,
    ]
);

const QUEUE_WIDTH: f32 = 380.0;

struct Root {
    shared: Entity<Shared>,
    player: Entity<PlayerView>,
    queue: Entity<QueuePanel>,
    focus: FocusHandle,
    /// The last notice, kept while it fades out.
    notice: SharedString,
    _subs: Vec<Subscription>,
}

impl Root {
    fn new(
        shared: Entity<Shared>,
        player: Entity<PlayerView>,
        queue: Entity<QueuePanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut was_open = false;
        let subs = vec![
            cx.observe_window_appearance(window, |this, window, cx| {
                let dark = app::is_dark(window.appearance());
                this.shared.update(cx, |s, cx| {
                    s.dark = dark;
                    app::apply_component_theme(s.target_palette(), cx);
                    cx.notify()
                });
            }),
            cx.observe(&shared, move |this, shared, cx| {
                let open = shared.read(cx).queue_open;
                if open && !was_open {
                    this.queue.read(cx).reveal_current(cx);
                }
                was_open = open;
            }),
        ];
        Self {
            shared,
            player,
            queue,
            focus,
            notice: SharedString::default(),
            _subs: subs,
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.shared.update(cx, |s, cx| {
                s.queue_open = false;
                s.settings_open = false;
                cx.notify()
            }),
            "space" => self.shared.update(cx, |s, cx| {
                if s.player.current.is_some() {
                    s.send(Request::Toggle, cx)
                }
            }),
            _ => return,
        }
        cx.stop_propagation();
    }
}

/// The theme choice: the base theme always, Omarchy only when Omarchy is there.
fn settings_card(shared: &Entity<Shared>, pal: Palette, cx: &App) -> impl IntoElement {
    let s = shared.read(cx);
    let offered = s.theme.detected && s.theme.palette.is_some();
    let gone = s.theme.active() == Active::BaseOmarchyGone;
    let selected = s.theme.selected();
    let mut options: Vec<Choice> = theme::choices(offered);
    // Omarchy stays listed, greyed out, while its files are gone: the choice is kept.
    if gone && !options.contains(&Choice::Omarchy) {
        options.push(Choice::Omarchy);
    }
    v_flex()
        .id("settings-card")
        .w(px(300.))
        .p_3()
        .gap_1()
        .rounded_xl()
        .bg(pal.surface)
        .border_1()
        .border_color(Hsla { a: 0.10, ..pal.fg })
        .shadow_xl()
        // a click inside does not close it
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .px_2()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(pal.secondary)
                .child("THEME"),
        )
        .children(options.into_iter().map(|choice| {
            let (label, about) = match choice {
                Choice::Base => ("Mélodie", "Colours from the cover"),
                Choice::Omarchy => (
                    "Omarchy",
                    if gone {
                        "Theme files gone: using Mélodie's"
                    } else {
                        "The current Omarchy theme, followed live"
                    },
                ),
            };
            let on = choice == selected;
            let shared = shared.clone();
            h_flex()
                .id(match choice {
                    Choice::Base => "theme-base",
                    Choice::Omarchy => "theme-omarchy",
                })
                .px_2()
                .py_2()
                .gap_3()
                .items_center()
                .rounded_lg()
                .cursor_pointer()
                .hover(|d| d.bg(Hsla { a: 0.07, ..pal.fg }))
                .on_click(move |_, _, cx| shared.update(cx, |s, cx| s.choose(choice, cx)))
                .child(
                    div()
                        .size(px(18.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_2()
                        .border_color(if on { pal.accent } else { pal.secondary })
                        .when(on, |d| {
                            d.child(div().size(px(8.)).rounded_full().bg(pal.accent))
                        }),
                )
                .child(
                    v_flex()
                        .child(div().text_sm().text_color(pal.fg).child(label))
                        .child(div().text_xs().text_color(pal.secondary).child(about)),
                )
        }))
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (open, settings, notice) = {
            let s = self.shared.read(cx);
            (s.queue_open, s.settings_open, s.notice())
        };
        if let Some(n) = &notice {
            self.notice = n.clone();
        }
        let pal = palette(&self.shared, window, cx);
        let q = motion::flag("queue-open", open, motion::PANEL, window, cx);
        let st = motion::flag("settings-open", settings, motion::PANEL, window, cx);
        let nt = motion::flag("notice", notice.is_some(), motion::ENTER, window, cx);
        let width = window.viewport_size().width.as_f32();
        let panel = QUEUE_WIDTH.min(width);
        let (scrim, gear, card_scrim) = (
            self.shared.clone(),
            self.shared.clone(),
            self.shared.clone(),
        );
        div()
            .id("root")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(pal.bg)
            .text_color(pal.fg)
            .child(self.player.clone())
            .child(
                div().absolute().top_3().right_3().child(
                    IconButton::new("settings", IconName::Palette, pal)
                        .selected(settings)
                        .on_click(move |_, _, cx| {
                            gear.update(cx, |s, cx| {
                                s.settings_open = !s.settings_open;
                                cx.notify()
                            })
                        }),
                ),
            )
            .when(st > 0.0, |d| {
                d.child(
                    div()
                        .id("settings-scrim")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            card_scrim.update(cx, |s, cx| {
                                s.settings_open = false;
                                cx.notify()
                            })
                        })
                        .child(
                            div()
                                .absolute()
                                .top(px(60. - 8. * (1. - st)))
                                .right_3()
                                .opacity(st)
                                .child(settings_card(&self.shared, pal, cx)),
                        ),
                )
            })
            .when(q > 0.0, |d| {
                d.child(
                    div()
                        .id("queue-scrim")
                        .absolute()
                        .inset_0()
                        .bg(hsla(0., 0., 0., 0.35 * q))
                        .on_click(move |_, _, cx| {
                            scrim.update(cx, |s, cx| {
                                s.queue_open = false;
                                cx.notify()
                            })
                        }),
                )
                .child(
                    div()
                        .id("queue-panel")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px(-panel * (1. - q)))
                        .w(px(panel))
                        // clicks on the panel stay in it
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(self.queue.clone()),
                )
            })
            .when(nt > 0.0, |d| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(12. + 8. * nt))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .opacity(nt)
                        .child(
                            div()
                                .max_w(px(420.))
                                .px_4()
                                .py_2()
                                .rounded_full()
                                .bg(pal.surface)
                                .border_1()
                                .border_color(Hsla { a: 0.12, ..pal.fg })
                                .shadow_lg()
                                .text_sm()
                                .text_color(pal.fg)
                                .child(self.notice.clone()),
                        ),
                )
            })
    }
}

fn main() -> ExitCode {
    let env = match Env::from_process() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("melodie-gui: {e:#}");
            return ExitCode::from(2);
        }
    };
    let socket = match proto::socket_path() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("melodie-gui: {e:#}");
            return ExitCode::from(2);
        }
    };
    gpui_kit::application().with_assets(Icons).run(move |cx| {
        gpui_kit::init(cx);
        #[cfg(feature = "frame-stats")]
        stats::start(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(480.), px(800.)),
                cx,
            ))),
            window_min_size: Some(size(px(360.), px(600.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Mélodie".into()),
                ..Default::default()
            }),
            app_id: Some("melodie".into()),
            // GPUI caps an unfocused window at 30 frames a second by default; a player often
            // sits on another screen, unfocused, and its fades would stutter. It costs nothing
            // at rest: frames are drawn only when something moves.
            inactive_frame_interval: None,
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, move |window, cx| {
            let (shared, clock) = app::start(env.clone(), socket.clone(), window, cx);
            let player = cx.new(|cx| PlayerView::new(shared.clone(), clock, window, cx));
            let queue = cx.new(|cx| QueuePanel::new(shared.clone(), env.clone(), cx));
            cx.new(|cx| Root::new(shared, player, queue, window, cx))
        });
        if let Err(e) = opened {
            eprintln!("melodie-gui: no window: {e:#}");
            cx.quit();
            return;
        }
        // Closing the window ends this program only: the daemon lives in a session of its own.
        cx.on_window_closed(|cx, _| cx.quit()).detach();
    });
    ExitCode::SUCCESS
}
