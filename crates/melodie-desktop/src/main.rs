//! `melodie-desktop`: the window. A client of the daemon like `melodie watch`; closing it never stops
//! the music, and it never plays sound itself.

mod albums;
mod app;
mod catalog;
mod dialogs;
mod mini;
mod motion;
mod pages;
mod player;
mod queue;
mod search;
mod sidebar;
mod signin;
#[cfg(feature = "frame-stats")]
mod stats;
mod tables;
mod timing;
mod widgets;

use albums::AlbumGrid;
use app::Shared;
use catalog::Catalog;
use dialogs::{Dialogs, DialogsHandle};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::env::Env;
use melodie_core::proto::{self, Request};
use melodie_desktop::account::{self, Screen};
use melodie_desktop::browse::Place;
use melodie_desktop::keys::{self, Act, Key, Situation};
use melodie_desktop::theme::{self, Active, Choice, Palette};
use mini::MiniPlayer;
use pages::{HomeView, PageView, SelectionBar};
use player::{PlayerView, palette};
use queue::QueuePanel;
use search::SearchView;
use sidebar::Sidebar;
use signin::{Account, SignInView};
use std::process::ExitCode;
use tables::{ArtistList, TrackList};

gpui_kit::assets::icon_assets!(
    Icons,
    [
        ArrowDown,
        ArrowUp,
        AudioLines,
        Check,
        ChevronLeft,
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
        Volume2,
        X,
    ]
);

const QUEUE_WIDTH: f32 = 380.0;

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
    mini: Entity<MiniPlayer>,
    sign_in: Entity<SignInView>,
    dialogs: Entity<Dialogs>,
    picked: Entity<SelectionBar>,
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

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let key = Key::read(k.modifiers.control, &k.key, k.key_char.as_deref());
        let situation = {
            let (s, c, v) = (
                self.shared.read(cx),
                self.catalog.read(cx),
                self.views.search.read(cx),
            );
            Situation {
                here: c.here(),
                field_focused: v.has_focus(window, cx),
                overlay_open: s.queue_open || s.settings_open,
                dialog_open: self.views.dialogs.read(cx).is_open(),
                selection: c.selection.count(c.here()) > 0,
                field_empty: v.is_empty(),
                sign_in: self.account.read(cx).screen(cx) == Screen::SignIn,
                sign_in_field_focused: self.views.sign_in.read(cx).has_focus(window, cx),
            }
        };
        match keys::act(key, situation) {
            Act::OpenSearch => {
                self.catalog
                    .update(cx, |c, cx| c.start_at(Place::Search, cx));
                self.views.search.update(cx, |v, cx| v.focus(window, cx));
            }
            // what is open closes first; then a page goes back
            Act::CloseOverlays => self.shared.update(cx, |s, cx| {
                s.queue_open = false;
                s.settings_open = false;
                cx.notify()
            }),
            Act::CloseDialog => self.views.dialogs.update(cx, |d, cx| d.close(cx)),
            // Enter in the name field is the field's own: it confirms once, through its event
            Act::Confirm if self.views.dialogs.read(cx).field_focused(window, cx) => return,
            Act::Confirm => self.views.dialogs.update(cx, |d, cx| d.confirm(window, cx)),
            Act::ClearSelection => self.catalog.update(cx, |c, cx| c.unselect(cx)),
            Act::ClearField => {
                self.views.search.update(cx, |v, cx| v.clear(window, cx));
            }
            Act::Back => self.catalog.update(cx, |c, cx| c.back(cx)),
            Act::PlayPause => self.shared.update(cx, |s, cx| {
                if s.player.current.is_some() {
                    s.send(Request::Toggle, cx)
                }
            }),
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
        let (step, user, signing) = {
            let a = account.read(cx);
            (a.copy_step, a.user.clone(), a.screen(cx) == Screen::SignIn)
        };
        catalog.update(cx, |c, cx| {
            if c.user != user {
                c.user = user;
                cx.notify();
            }
            if step != seen.0 {
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
        let (open, settings, notice) = {
            let s = self.shared.read(cx);
            (s.queue_open, s.settings_open, s.notice())
        };
        let (signing, notice) = {
            let a = self.account.read(cx);
            let own = account::notice(a.standing).map(SharedString::from);
            (a.screen(cx) == Screen::SignIn, notice.or(own))
        };
        if let Some(n) = &notice {
            self.notice = n.clone();
        }
        let pal = palette(&self.shared, window, cx);
        let q = motion::flag("queue-open", open, motion::PANEL, window, cx);
        let st = motion::flag("settings-open", settings, motion::PANEL, window, cx);
        let nt = motion::flag("notice", notice.is_some(), motion::ENTER, window, cx);
        let si = motion::flag("sign-in", signing, motion::ENTER, window, cx);
        let width = window.viewport_size().width.as_f32();
        let panel = QUEUE_WIDTH.min(width);
        let (here, step) = {
            let c = self.catalog.read(cx);
            (c.here().clone(), c.nav.step)
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
        // the notice stays above the mini player
        let lift = if here == Place::Player {
            0.
        } else {
            mini::HEIGHT
        };
        div()
            .id("root")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(pal.bg)
            .text_color(pal.fg)
            .child(
                h_flex().size_full().child(v.sidebar.clone()).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(
                            // every change of place comes in: a short fade and rise
                            div()
                                .id(("view", step))
                                .flex_1()
                                .min_h_0()
                                .child(content)
                                .with_animation(("view-enter", step), motion::enter(), |el, t| {
                                    el.opacity(t).mt(px((1.0 - t) * 10.0))
                                }),
                        )
                        .when(here != Place::Player, |d| d.child(v.mini.clone())),
                ),
            )
            // the rows picked: what can be done with them, above the mini player
            .child(
                div()
                    .absolute()
                    .bottom(px(lift + 12.))
                    .left(px(sidebar::width(width)))
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(v.picked.clone()),
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
            .child(self.views.dialogs.clone())
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
            let views = Views {
                player: cx.new(|cx| PlayerView::new(s.clone(), clock, window, cx)),
                queue: cx.new(|cx| QueuePanel::new(s.clone(), cx)),
                sidebar: cx.new(|_| Sidebar::new(s.clone(), c.clone())),
                home: cx.new(|_| HomeView::new(s.clone(), c.clone())),
                albums: cx.new(|_| AlbumGrid::new(s.clone(), c.clone())),
                artists: cx.new(|_| ArtistList::new(s.clone(), c.clone())),
                tracks: cx.new(|_| TrackList::new(s.clone(), c.clone())),
                page: cx.new(|_| PageView::new(s.clone(), c.clone())),
                search: cx.new(|cx| SearchView::new(s.clone(), c.clone(), env.clone(), window, cx)),
                mini: cx.new(|_| MiniPlayer::new(s.clone(), c.clone())),
                sign_in: cx.new(|cx| SignInView::new(s.clone(), account.clone(), window, cx)),
                dialogs: cx.new(|cx| Dialogs::new(s.clone(), c.clone(), window, cx)),
                picked: cx.new(|_| SelectionBar::new(s.clone(), c.clone())),
            };
            cx.set_global(DialogsHandle(views.dialogs.clone()));
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
