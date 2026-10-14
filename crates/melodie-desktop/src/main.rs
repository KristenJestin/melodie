//! `melodie-desktop`: the window. A client of the daemon like `melodie watch`; closing it never stops
//! the music, and it never plays sound itself.

mod albums;
mod app;
mod catalog;
mod context_menu;
mod cover_cache;
mod dialogs;
mod equaliser;
mod home_page;
mod motion;
mod pages;
mod playbar;
mod player;
mod queue;
mod quick_view;
mod search;
mod sidebar;
mod signin;
#[cfg(feature = "frame-stats")]
mod stats;
mod tables;
mod timing;
mod tips;
mod volume;
mod wave_cache;
mod widgets;

use albums::AlbumGrid;
use app::Shared;
use catalog::Catalog;
use context_menu::{ContextMenu, MenuHandle};
use dialogs::{Dialogs, DialogsHandle};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::input::{MoveDown, MoveUp};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use home_page::HomeView;
use melodie_core::env::Env;
use melodie_core::proto::{self, Request};
use melodie_desktop::account::{self, Screen};
use melodie_desktop::bar;
use melodie_desktop::browse::Place;
use melodie_desktop::keys::{self, Act, Key, Situation, Thumb};
use melodie_desktop::layout::{self, QUEUE_WIDTH};
use melodie_desktop::theme::{self, Active, Choice, Palette};
use pages::{PageView, SelectionBar};
use playbar::PlayBar;
use player::{Bar, PlayerView, palette};
use queue::QueuePanel;
use quick_view::QuickSearch;
use search::SearchView;
use sidebar::Sidebar;
use signin::{Account, SignInView};
use std::process::ExitCode;
use tables::{ArtistList, TrackList};
use volume::Volume;

gpui_kit::assets::icon_assets!(
    Icons,
    [
        ArrowDown,
        ArrowUp,
        AudioLines,
        Check,
        ChevronLeft,
        ChevronRight,
        CircleUser,
        Copy,
        CopyX,
        Disc3,
        GripVertical,
        Heart,
        House,
        KeyRound,
        ListEnd,
        ListMinus,
        ListMusic,
        ListPlus,
        ListStart,
        ListX,
        LogIn,
        LogOut,
        Maximize2,
        MicVocal,
        Music,
        Pause,
        Pencil,
        Play,
        Plus,
        RefreshCw,
        RotateCcw,
        Search,
        Server,
        Settings,
        Shuffle,
        SkipBack,
        SkipForward,
        Sparkles,
        Tag,
        Trash,
        Volume1,
        Volume2,
        VolumeX,
        X,
    ]
);

/// The views of the window, each an entity of its own (a list in a view of its own keeps its
/// scroll layer; each keeps its scroll position when the user comes back to it).
struct Views {
    player: Entity<PlayerView>,
    queue: Entity<QueuePanel>,
    sidebar: Entity<Sidebar>,
    home: Entity<HomeView>,
    albums: Entity<AlbumGrid>,
    artists: Entity<ArtistList>,
    tracks: Entity<TrackList>,
    page: Entity<PageView>,
    search: Entity<SearchView>,
    quick: Entity<QuickSearch>,
    playbar: Entity<PlayBar>,
    sign_in: Entity<SignInView>,
    dialogs: Entity<Dialogs>,
    picked: Entity<SelectionBar>,
    menu: Entity<ContextMenu>,
}

struct Root {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    account: Entity<Account>,
    views: Views,
    focus: FocusHandle,
    /// The last notice, kept while it fades out.
    notice: SharedString,
    _subs: Vec<Subscription>,
}

impl Root {
    fn new(
        shared: Entity<Shared>,
        catalog: Entity<Catalog>,
        account: Entity<Account>,
        views: Views,
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
            // the field stays focused when it is no longer drawn: the root takes the focus back,
            // or Space and / would go on being ignored
            cx.observe_in(&catalog, window, |this, catalog, window, cx| {
                let left = !keys::in_search(catalog.read(cx).here());
                if left && this.views.search.read(cx).has_focus(window, cx) {
                    window.focus(&this.focus, cx);
                }
            }),
            sign_in_follow(&account, &catalog, window, cx),
            // a dialog closed (or one without a field opened): its field gives the focus back
            cx.observe_in(&views.dialogs, window, |this, dialogs, window, cx| {
                let d = dialogs.read(cx);
                let wants_field = d.open.as_ref().is_some_and(|d| d.has_field());
                if !wants_field && d.field_focused(window, cx) {
                    window.focus(&this.focus, cx);
                }
            }),
            // the palette closed without anything focused before it: the root takes the focus
            cx.observe_in(&views.quick, window, |this, quick, window, cx| {
                let q = quick.read(cx);
                if !q.open && q.has_focus(window, cx) {
                    window.focus(&this.focus, cx);
                }
            }),
            cx.observe(&shared, move |this, shared, cx| {
                let open = shared.read(cx).queue_open;
                if open && !was_open {
                    this.views.queue.read(cx).reveal_current(cx);
                }
                was_open = open;
            }),
        ];
        Self {
            shared,
            catalog,
            account,
            views,
            focus,
            notice: SharedString::default(),
            _subs: subs,
        }
    }

    fn queue_place(&self, window: &Window, cx: &App) -> layout::Queue {
        let width = window.viewport_size().width.as_f32();
        layout::Queue::of(
            width,
            sidebar::width(width),
            self.shared.read(cx).queue_open,
        )
    }

    /// What the window knows now, for `keys` to judge by.
    fn situation<R>(
        &self,
        held: bool,
        window: &Window,
        cx: &App,
        judge: impl FnOnce(Situation) -> R,
    ) -> R {
        let (s, c, v) = (
            self.shared.read(cx),
            self.catalog.read(cx),
            self.views.search.read(cx),
        );
        judge(Situation {
            here: c.here(),
            palette_open: self.views.quick.read(cx).open,
            menu_open: self.views.menu.read(cx).is_open(),
            field_focused: v.has_focus(window, cx),
            overlay_open: self.queue_place(window, cx).covers() || s.settings_open,
            dialog_open: self.views.dialogs.read(cx).is_open(),
            selection: c.selection.count(c.here()) > 0,
            several: c.selection.several(c.here()),
            held,
            can_back: c.nav.can_go_back(),
            field_empty: v.is_empty(),
            palette_empty: self.views.quick.read(cx).is_empty(),
            sign_in: self.account.read(cx).screen(cx) == Screen::SignIn,
            sign_in_field_focused: self.views.sign_in.read(cx).has_focus(window, cx),
        })
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        context_menu::forget_pending(cx);
        let m = k.modifiers;
        let key = if Key::opens_menu(m.shift, &k.key) {
            Key::Menu
        } else {
            Key::read(m.control, m.shift, &k.key, k.key_char.as_deref())
        };
        let act = self.situation(ev.is_held, window, cx, |s| keys::act(key, s));
        self.run(act, window, cx);
    }

    /// The arrows, which the palette's field takes as actions before any key listener.
    fn on_arrow(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let act = self.situation(false, window, cx, |s| keys::act(key, s));
        self.run(act, window, cx);
    }

    fn on_thumb(&mut self, button: Thumb, window: &mut Window, cx: &mut Context<Self>) {
        let act = self.situation(false, window, cx, |s| keys::pointer_act(button, s));
        self.run(act, window, cx);
    }

    /// Does what a key or a mouse button asked for; what is not ours goes on.
    fn run(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        // a tooltip would outlive the button of a page that goes
        tips::hide(cx);
        let covers = self.queue_place(window, cx).covers();
        match act {
            Act::OpenSearch => {
                self.views.search.update(cx, |v, cx| v.open(window, cx));
            }
            // what is open closes first; then a page goes back
            Act::CloseOverlays => self.shared.update(cx, |s, cx| {
                s.settings_open = false;
                if covers {
                    s.show_queue(false, cx);
                }
                cx.notify()
            }),
            Act::ToggleQueue => self.shared.update(cx, |s, cx| s.toggle_queue(cx)),
            Act::OpenMenu => context_menu::open_on_picked(&self.catalog, window, cx),
            Act::PlayPicked => {
                let c = self.catalog.read(cx);
                if let Some(job) = c.selection.play_picked(c.here()) {
                    let here = c.here().clone();
                    self.shared.update(cx, |s, cx| s.play_from(&here, job, cx));
                }
            }
            Act::CloseDialog => self.views.dialogs.update(cx, |d, cx| d.close(cx)),
            // Enter in the name field is the field's own: it confirms once, through its event
            Act::Confirm if self.views.dialogs.read(cx).field_focused(window, cx) => return,
            Act::Confirm => self.views.dialogs.update(cx, |d, cx| d.confirm(window, cx)),
            Act::ClearSelection => self.catalog.update(cx, |c, cx| c.unselect(cx)),
            Act::ClearField => {
                self.views.search.update(cx, |v, cx| v.clear(window, cx));
            }
            Act::ClearPalette => self.views.quick.update(cx, |q, cx| q.clear(window, cx)),
            Act::Back => self.catalog.update(cx, |c, cx| c.back(cx)),
            Act::Forward => self.catalog.update(cx, |c, cx| c.forward(cx)),
            Act::PlayPause => self.shared.update(cx, |s, cx| {
                if s.player.current.is_some() {
                    s.send(Request::Toggle, cx)
                }
            }),
            Act::CloseMenu => self.views.menu.update(cx, |m, cx| m.close(window, cx)),
            // the menu first, so that the palette keeps the focus the menu gives back
            Act::MenuToPalette => {
                self.views.menu.update(cx, |m, cx| m.close(window, cx));
                self.views.quick.update(cx, |q, cx| q.toggle(window, cx));
            }
            Act::TogglePalette => self.views.quick.update(cx, |q, cx| q.toggle(window, cx)),
            Act::PaletteUp => self.views.quick.update(cx, |q, cx| q.step(false, cx)),
            Act::PaletteDown => self.views.quick.update(cx, |q, cx| q.step(true, cx)),
            Act::PaletteRun(how) => self.views.quick.update(cx, |q, cx| q.run(how, window, cx)),
            Act::Pass => return,
        }
        cx.stop_propagation();
    }
}

/// The theme choice: the base theme always, Omarchy only when Omarchy is there.
fn settings_card(
    shared: &Entity<Shared>,
    account: &Entity<Account>,
    pal: Palette,
    cx: &App,
) -> impl IntoElement {
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
        .child(signin::settings_section(account, pal, cx))
}

/// The library follows the account: its user, and each new copy. The focus comes back to the
/// root when the sign-in screen goes, or Space and / would stay with a field no longer drawn.
fn sign_in_follow(
    account: &Entity<Account>,
    catalog: &Entity<Catalog>,
    window: &mut Window,
    cx: &mut Context<Root>,
) -> Subscription {
    let mut seen = (0, false);
    let catalog = catalog.clone();
    cx.observe_in(account, window, move |this, account, window, cx| {
        let (step, user, signing, filling) = {
            let a = account.read(cx);
            let signing = a.screen(cx) == Screen::SignIn;
            (a.copy_step, a.user.clone(), signing, a.filling())
        };
        catalog.update(cx, |c, cx| {
            if c.user != user {
                c.user = user;
                cx.notify();
            }
            if step != seen.0 || c.copying != filling {
                c.copying = filling;
                c.reload(cx);
            }
        });
        if seen.1 && !signing && this.views.sign_in.read(cx).has_focus(window, cx) {
            window.focus(&this.focus, cx);
        }
        seen = (step, signing);
    })
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The window moved to a screen of another scale factor: the covers are asked again at
        // the pixels it needs. Not while drawing: after this frame.
        let (scale, covers) = (window.scale_factor(), self.shared.read(cx).covers.clone());
        if (covers.read(cx).scale() - scale).abs() >= 0.01 {
            cx.defer(move |cx| covers.update(cx, |c, cx| c.set_scale(scale, cx)));
        }
        let (settings, notice) = {
            let s = self.shared.read(cx);
            (s.settings_open, s.notice())
        };
        let (signing, notice) = {
            let a = self.account.read(cx);
            let own = account::notice(a.standing)
                .map(SharedString::from)
                .or_else(|| a.copying.map(|c| account::copy_notice(c).into()));
            (a.screen(cx) == Screen::SignIn, notice.or(own))
        };
        if let Some(n) = &notice {
            self.notice = n.clone();
        }
        let pal = palette(&self.shared, window, cx);
        let place = self.queue_place(window, cx);
        // Docked, the column's width moves and the main area follows; on a narrow window the
        // queue slides over the main area instead.
        let col = motion::to("queue-column", place.column(), motion::PANEL, window, cx);
        let q = motion::flag("queue-over", place.covers(), motion::PANEL, window, cx);
        let st = motion::flag("settings-open", settings, motion::PANEL, window, cx);
        let nt = motion::flag("notice", notice.is_some(), motion::ENTER, window, cx);
        let si = motion::flag("sign-in", signing, motion::ENTER, window, cx);
        let width = window.viewport_size().width.as_f32();
        let panel = QUEUE_WIDTH.min(width);
        let (here, step) = {
            let c = self.catalog.read(cx);
            (c.here().clone(), c.nav.view)
        };
        let v = &self.views;
        let content: AnyView = match here {
            Place::Player => v.player.clone().into(),
            Place::Home => v.home.clone().into(),
            Place::Albums => v.albums.clone().into(),
            Place::Artists => v.artists.clone().into(),
            Place::Tracks => v.tracks.clone().into(),
            Place::Album(_) | Place::Artist(_) | Place::Playlist(_) => v.page.clone().into(),
            Place::Search | Place::Found(_) => v.search.clone().into(),
        };
        let (scrim, card_scrim) = (self.shared.clone(), self.shared.clone());
        // the notice stays above the player bar
        let lift = if here == Place::Player {
            0.
        } else {
            bar::HEIGHT
        };
        div()
            .id("root")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .capture_action(
                cx.listener(|this, _: &MoveUp, window, cx| this.on_arrow(Key::Up, window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &MoveDown, window, cx| this.on_arrow(Key::Down, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, window, cx| this.on_thumb(Thumb::Back, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _, window, cx| this.on_thumb(Thumb::Forward, window, cx)),
            )
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(pal.bg)
            .text_color(pal.fg)
            .child(
                h_flex()
                    .size_full()
                    .child(v.sidebar.clone())
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            // laid out for the queue's settled place: what is wider than the
                            // area while the column slides away stays under it, clipped
                            .overflow_hidden()
                            .child(
                                // every change of place comes in: a short fade and rise
                                div()
                                    .id(("view", step))
                                    .flex_1()
                                    .min_h_0()
                                    .child(content)
                                    .with_animation(
                                        ("view-enter", step),
                                        motion::enter(),
                                        |el, t| el.opacity(t).mt(px((1.0 - t) * 10.0)),
                                    ),
                            )
                            .when(here != Place::Player, |d| d.child(v.playbar.clone())),
                    )
                    // the panel moves with the column's edge, so it slides in as the column opens
                    .when(col > 0.0, |d| {
                        d.child(
                            div()
                                .id("queue-column")
                                .flex_none()
                                .h_full()
                                .w(px(col))
                                .relative()
                                .overflow_hidden()
                                // a view is drawn once: while it slides over a narrow window, the
                                // column closing beside it stays empty
                                .when(q == 0.0, |d| {
                                    d.child(
                                        div()
                                            .absolute()
                                            .top_0()
                                            .bottom_0()
                                            .left_0()
                                            .w(px(QUEUE_WIDTH))
                                            .child(self.views.queue.clone()),
                                    )
                                }),
                        )
                    }),
            )
            // the rows picked: what can be done with them, above the player bar
            .child(
                div()
                    .absolute()
                    .bottom(px(lift + 12.))
                    .left(px(sidebar::width(width)))
                    .right(px(col))
                    .flex()
                    .justify_center()
                    .child(v.picked.clone()),
            )
            .when(st > 0.0, |d| {
                // any button closes it, and none reaches what is behind (a menu, a row)
                let hide = |shared: &Entity<Shared>| {
                    let shared = shared.clone();
                    move |_: &MouseDownEvent, _: &mut Window, cx: &mut App| {
                        shared.update(cx, |s, cx| {
                            s.settings_open = false;
                            cx.notify()
                        })
                    }
                };
                d.child(
                    div()
                        .id("settings-scrim")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, hide(&card_scrim))
                        .on_mouse_down(MouseButton::Right, hide(&card_scrim))
                        .on_mouse_down(MouseButton::Middle, hide(&card_scrim))
                        .child(
                            div()
                                .absolute()
                                .bottom(px(60. - 8. * (1. - st)))
                                .left_2()
                                .opacity(st)
                                .child(settings_card(&self.shared, &self.account, pal, cx)),
                        ),
                )
            })
            .when(q > 0.0, |d| {
                d.child(
                    div()
                        .id("queue-scrim")
                        .absolute()
                        .inset_0()
                        // a click beside the queue only hides it
                        .occlude()
                        .bg(hsla(0., 0., 0., 0.35 * q))
                        .on_click(move |_, _, cx| {
                            scrim.update(cx, |s, cx| s.show_queue(false, cx))
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
                        .child(self.views.queue.clone()),
                )
            })
            .when(nt > 0.0, |d| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(lift + 12. + 8. * nt))
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
            .child(self.views.quick.clone())
            .child(self.views.dialogs.clone())
            .child(self.views.menu.clone())
            // over everything, the notices included; clicks stay on it
            .when(si > 0.0, |d| {
                d.child(
                    div()
                        .id("sign-in-screen")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .bg(Hsla { a: si, ..pal.bg })
                        .child(
                            div()
                                .size_full()
                                .opacity(si)
                                .mt(px((1.0 - si) * 14.0))
                                .child(self.views.sign_in.clone()),
                        ),
                )
            })
            // the tooltips, above all
            .when_some(tips::overlay(cx), |d, tips| d.child(tips))
    }
}

fn main() -> ExitCode {
    let env = match Env::from_process() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("melodie-desktop: {e:#}");
            return ExitCode::from(2);
        }
    };
    let socket = match proto::socket_path() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("melodie-desktop: {e:#}");
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
                size(px(1180.), px(780.)),
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
            let catalog = cx.new(|cx| Catalog::new(env.clone(), shared.clone(), cx));
            let account = cx.new(|cx| Account::new(env.clone(), shared.clone(), cx));
            let (s, c) = (&shared, &catalog);
            let volume = cx.new(|cx| Volume::new(s.clone(), window, cx));
            // the player and the bar each have a position bar of their own: the layouts differ
            let position = |inline: bool, window: &mut Window, cx: &mut App| {
                cx.new(|cx| Bar::new(s.clone(), clock.clone(), inline, window, cx))
            };
            let (stacked, inline) = (position(false, window, cx), position(true, window, cx));
            let views = Views {
                player: cx.new(|_| PlayerView::new(s.clone(), stacked, volume.clone())),
                queue: cx.new(|cx| QueuePanel::new(s.clone(), cx)),
                sidebar: cx.new(|_| Sidebar::new(s.clone(), c.clone())),
                home: cx.new(|cx| HomeView::new(s.clone(), c.clone(), cx)),
                albums: cx.new(|_| AlbumGrid::new(s.clone(), c.clone())),
                artists: cx.new(|_| ArtistList::new(s.clone(), c.clone())),
                tracks: cx.new(|_| TrackList::new(s.clone(), c.clone())),
                page: cx.new(|_| PageView::new(s.clone(), c.clone())),
                search: cx.new(|cx| SearchView::new(s.clone(), c.clone(), env.clone(), window, cx)),
                quick: cx.new(|cx| QuickSearch::new(s.clone(), c.clone(), env.clone(), window, cx)),
                playbar: cx.new(|_| PlayBar::new(s.clone(), c.clone(), volume.clone(), inline)),
                sign_in: cx.new(|cx| SignInView::new(s.clone(), account.clone(), window, cx)),
                dialogs: cx.new(|cx| Dialogs::new(s.clone(), c.clone(), window, cx)),
                picked: cx.new(|_| SelectionBar::new(s.clone(), c.clone())),
                menu: cx.new(|cx| ContextMenu::new(env.clone(), s.clone(), c.clone(), cx)),
            };
            cx.set_global(DialogsHandle(views.dialogs.clone()));
            cx.set_global(MenuHandle(views.menu.clone()));
            tips::install(cx);
            cx.new(|cx| Root::new(shared, catalog, account, views, window, cx))
        });
        if let Err(e) = opened {
            eprintln!("melodie-desktop: no window: {e:#}");
            cx.quit();
            return;
        }
        // Closing the window ends this program only: the daemon lives in a session of its own.
        cx.on_window_closed(|cx, _| cx.quit()).detach();
    });
    ExitCode::SUCCESS
}
