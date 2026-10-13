//! The menu a right click opens on any item (a track, an album, an artist, a playlist, a mix):
//! what it offers and what each line does are decided in `melodie_desktop::menu`. One menu at a
//! time, at the pointer, inside the window; it fades in, takes the keys while it is open, and
//! gives the focus back when it closes.

use crate::app::Shared;
use crate::catalog::{Catalog, Facet, Page};
use crate::dialogs::{self, Dialog, DialogsHandle};
use crate::motion;
use crate::player::palette;
use crate::tips;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::db::{self, StarKind};
use melodie_core::env::Env;
use melodie_desktop::launch::{Adding, Kind};
use melodie_desktop::menu::{
    self, Action, Chosen, Effect, Focus, Item, MenuKey, Source, Spot, Target,
};
use melodie_desktop::theme::Palette;
use std::sync::Arc;

#[derive(Clone)]
struct Open {
    target: Target,
    rows: Vec<Action>,
    /// The playlists beside "Add to playlist": their ids and names.
    subs: Vec<(String, String)>,
    /// Where the menu's top left corner is in the window.
    at: (f32, f32),
}

pub struct ContextMenu {
    open: Option<Open>,
    /// What the menu shows: the last one, kept while it fades out.
    shown: Option<Open>,
    focus: Focus,
    handle: FocusHandle,
    /// What had the focus before the menu took it.
    before: Option<FocusHandle>,
    sub_scroll: ScrollHandle,
    /// Counts the menus asked for: a slow look-up for an older one opens nothing.
    asked: u64,
    env: Env,
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
}

/// Where the views find the menu: set once, never written again.
pub struct MenuHandle(pub Entity<ContextMenu>);

impl Global for MenuHandle {}

/// Opens the menu of `item` at `at`. `starred` when the place knows it (a queue entry); else it
/// is read from the local copy first.
pub fn open(
    item: Item,
    spot: Spot,
    starred: Option<bool>,
    at: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let menu = cx.global::<MenuHandle>().0.clone();
    menu.update(cx, |m, cx| m.ask(item, spot, starred, at, window, cx));
}

/// Opens the menu of the row picked in the current list (the menu key, Shift+F10), at the
/// pointer.
pub fn open_on_picked(catalog: &Entity<Catalog>, window: &mut Window, cx: &mut App) {
    let found = {
        let c = catalog.read(cx);
        let here = c.here().clone();
        let tracks = c.shown_tracks();
        c.selection.focused(&here).and_then(|ix| {
            let t = tracks.as_ref()?.get(ix)?;
            let spot = match c.pages.get(&here).map(Arc::as_ref) {
                Some(Page::Playlist { playlist, .. }) if c.is_own(playlist) => Spot::Playlist {
                    id: playlist.id.clone(),
                    name: playlist.name.clone(),
                    rank: ix,
                },
                _ => Spot::Library,
            };
            Some((Item::track(t), spot))
        })
    };
    if let Some((item, spot)) = found {
        let at = window.mouse_position();
        open(item, spot, None, at, window, cx);
    }
}

/// A right click on the element opens the menu of `item`.
pub trait Menued: InteractiveElement + Sized {
    fn menu(self, item: Item, spot: Spot) -> Self {
        self.menu_starred(item, spot, None)
    }

    fn menu_starred(self, item: Item, spot: Spot, starred: Option<bool>) -> Self {
        self.on_mouse_down(MouseButton::Right, move |ev, window, cx| {
            cx.stop_propagation();
            open(item.clone(), spot.clone(), starred, ev.position, window, cx)
        })
    }
}

impl<E: InteractiveElement> Menued for E {}

fn star_kind(kind: Kind) -> Option<StarKind> {
    match kind {
        Kind::Track => Some(StarKind::Track),
        Kind::Album => Some(StarKind::Album),
        Kind::Artist => Some(StarKind::Artist),
        Kind::Playlist => None,
    }
}

fn icon(action: Action) -> IconName {
    match action {
        Action::Play => IconName::Play,
        Action::Shuffle => IconName::Shuffle,
        Action::Next => IconName::ListStart,
        Action::Last => IconName::ListEnd,
        Action::Mix => IconName::Sparkles,
        Action::AddToPlaylist => IconName::ListPlus,
        Action::Like | Action::Unlike => IconName::Heart,
        Action::GoToAlbum => IconName::Disc3,
        Action::GoToArtist => IconName::MicVocal,
        Action::RemoveFromPlaylist => IconName::ListMinus,
        Action::RemoveFromQueue => IconName::ListX,
        Action::Rename => IconName::Pencil,
        Action::Delete => IconName::Trash,
    }
}

impl ContextMenu {
    pub fn new(
        env: Env,
        shared: Entity<Shared>,
        catalog: Entity<Catalog>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            open: None,
            shown: None,
            focus: Focus::default(),
            handle: cx.focus_handle(),
            before: None,
            sub_scroll: ScrollHandle::new(),
            asked: 0,
            env,
            shared,
            catalog,
        }
    }

    fn ask(
        &mut self,
        mut item: Item,
        spot: Spot,
        starred: Option<bool>,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tips::hide(cx);
        self.asked += 1;
        let asked = self.asked;
        let here = {
            let c = self.catalog.read(cx);
            if let Item::Playlist { id, own, .. } = &mut item {
                *own = c.playlists.iter().any(|p| p.id == *id && c.is_own(p));
            }
            c.here().clone()
        };
        let mut target = Target {
            item,
            spot,
            here,
            starred,
        };
        let at = (at.x.as_f32(), at.y.as_f32());
        let lookup = match target.item.likeable() {
            Some((kind, id)) if target.starred.is_none() => {
                star_kind(kind).map(|k| (k, id.to_string()))
            }
            _ => None,
        };
        let Some((kind, id)) = lookup else {
            self.show(target, at, window, cx);
            return;
        };
        // a few milliseconds on the local copy, away from the window
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            let conn = db::open_existing(&env).ok().flatten()?;
            db::is_starred(&conn, kind, &id)
        });
        cx.spawn_in(window, async move |this, cx| {
            target.starred = task.await;
            let _ = this.update_in(cx, |m, window, cx| {
                if m.asked == asked {
                    m.show(target, at, window, cx)
                }
            });
        })
        .detach();
    }

    fn show(
        &mut self,
        target: Target,
        at: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = menu::actions(&target);
        if rows.is_empty() {
            return;
        }
        let subs = if rows.contains(&Action::AddToPlaylist) {
            menu::playlists_for(&target, &self.catalog.read(cx).own_playlists())
        } else {
            Vec::new()
        };
        let viewport = window.viewport_size();
        let size = (menu::WIDTH, menu::height(&rows));
        let at = menu::place(
            at,
            size,
            (viewport.width.as_f32(), viewport.height.as_f32()),
        );
        if !self.handle.is_focused(window) {
            self.before = window.focused(cx);
        }
        window.focus(&self.handle, cx);
        self.focus = Focus::default();
        self.open = Some(Open {
            target,
            rows,
            subs,
            at,
        });
        self.shown = self.open.clone();
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.take().is_none() {
            return;
        }
        if self.handle.is_focused(window)
            && let Some(before) = self.before.take()
        {
            window.focus(&before, cx);
        }
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // the menu keeps the keys while it is open: nothing plays or goes back behind it
        cx.stop_propagation();
        let Some(open) = &self.open else { return };
        let Some(key) = MenuKey::read(&ev.keystroke.key) else {
            return;
        };
        let rows = open.rows.clone();
        let chosen = self.focus.key(key, &rows, open.subs.len() + 1);
        if let Some(sub) = self.focus.sub.filter(|_| self.focus.sub_open) {
            self.sub_scroll.scroll_to_item(sub);
        }
        match chosen {
            Some(c) => self.choose(c, window, cx),
            None => cx.notify(),
        }
    }

    fn choose(&mut self, chosen: Chosen, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.open.clone() else {
            return;
        };
        let effect = match chosen {
            Chosen::Close => None,
            Chosen::Row(ix) => {
                let Some(&action) = open.rows.get(ix) else {
                    return;
                };
                if action.has_sub() {
                    self.focus.row = Some(ix);
                    self.focus.sub_open = true;
                    cx.notify();
                    return;
                }
                menu::effect(action, &open.target)
            }
            // 0 is "New playlist…"
            Chosen::Sub(ix) => {
                let to = match ix {
                    0 => None,
                    n => match open.subs.get(n - 1) {
                        Some(p) => Some(p.clone()),
                        None => return,
                    },
                };
                menu::add_to(&open.target, to)
            }
        };
        self.close(window, cx);
        if let Some(effect) = effect {
            self.apply(effect, window, cx);
        }
    }

    fn apply(&mut self, effect: Effect, window: &mut Window, cx: &mut Context<Self>) {
        let (shared, catalog) = (&self.shared, &self.catalog);
        match effect {
            Effect::Run(job) => shared.update(cx, |s, cx| s.run(job, cx)),
            Effect::Row { ix, entry, req } => {
                shared.update(cx, |s, cx| s.send_for_row(ix, entry, req, cx))
            }
            Effect::Go(place) => catalog.update(cx, |c, cx| c.go(place, cx)),
            Effect::GoOfTrack { id, artist } => {
                let facet = if artist { Facet::Artist } else { Facet::Album };
                catalog.update(cx, |c, cx| c.open_of_track(id, facet, cx))
            }
            Effect::Edit { job, said } => catalog.update(cx, |c, cx| c.edit(job, Some(said), cx)),
            Effect::Rename { id, name } => dialogs::open(Dialog::Rename { id, name }, window, cx),
            Effect::Delete { id, name, tracks } => {
                dialogs::open(Dialog::Delete { id, name, tracks }, window, cx)
            }
            Effect::Add {
                to,
                what: Source::Ready(what),
            } => add(to, what, window, cx),
            // the tracks of an artist or a playlist, as the local copy holds them
            Effect::Add { to, what } => {
                let env = self.env.clone();
                let task = cx.background_spawn(async move {
                    let conn =
                        db::open_existing(&env)?.ok_or_else(|| anyhow::anyhow!("no local copy"))?;
                    what.read(&conn)
                });
                cx.spawn_in(window, async move |this, cx| {
                    let read = task.await;
                    let _ = this.update_in(cx, |m, window, cx| match read {
                        Ok(what) => add(to, what, window, cx),
                        Err(_) => m
                            .shared
                            .update(cx, |s, cx| s.say("Nothing to add from the library", cx)),
                    });
                })
                .detach();
            }
        }
    }
}

/// Adds to the playlist chosen, through the dialogs (they ask when some of it is there already),
/// or names a new one.
fn add(to: Option<(String, String)>, what: Adding, window: &mut Window, cx: &mut App) {
    match to {
        Some((id, name)) => {
            let dialogs = cx.global::<DialogsHandle>().0.clone();
            dialogs.update(cx, |d, cx| d.chose(id, name, what, window, cx));
        }
        None => dialogs::open(Dialog::Create { with: Some(what) }, window, cx),
    }
}

/// A line of the menu or of the list beside it.
fn line(
    id: ElementId,
    icon: IconName,
    label: SharedString,
    lit: bool,
    color: Hsla,
    pal: Palette,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        .h(px(menu::ROW))
        .flex_none()
        .mx_1()
        .px_2()
        .gap_3()
        .items_center()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .text_color(color)
        .when(lit, |d| d.bg(Hsla { a: 0.09, ..pal.fg }))
        .child(Icon::new(icon).size(px(16.)).text_color(pal.secondary))
        .child(div().flex_1().min_w_0().truncate().child(label))
}

impl Render for ContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = motion::flag("menu-open", self.open.is_some(), motion::PANEL, window, cx);
        let s = motion::flag(
            "menu-sub",
            self.open.is_some() && self.focus.sub_open,
            motion::HOVER,
            window,
            cx,
        );
        // 0 is also where an opening starts: only a closed menu is forgotten
        if t == 0.0 && self.open.is_none() {
            self.shown = None;
            return div().into_any_element();
        }
        let Some(open) = self.shown.clone() else {
            return div().into_any_element();
        };
        let live = self.open.is_some();
        let pal = palette(&self.shared, window, cx);
        let viewport = window.viewport_size();
        let window_size = (viewport.width.as_f32(), viewport.height.as_f32());
        let (x, y) = open.at;
        let focus = self.focus;
        let card = |id: &'static str, x: f32, y: f32, w: f32| {
            v_flex()
                .id(id)
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(w))
                .py(px(menu::PAD))
                .rounded_lg()
                .bg(pal.surface)
                .border_1()
                .border_color(Hsla { a: 0.12, ..pal.fg })
                .shadow_xl()
                // a click inside is the menu's
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        };

        let mut lines: Vec<AnyElement> = Vec::new();
        for (ix, &action) in open.rows.iter().enumerate() {
            if menu::separated(&open.rows, ix) {
                lines.push(
                    div()
                        .h(px(menu::SEPARATOR))
                        .flex_none()
                        .flex()
                        .items_center()
                        .px_3()
                        .child(div().h(px(1.)).w_full().bg(Hsla { a: 0.10, ..pal.fg }))
                        .into_any_element(),
                );
            }
            let rows = open.rows.clone();
            let label = action.label(&open.target.item);
            lines.push(
                line(
                    ("menu-row", ix).into(),
                    icon(action),
                    label.into(),
                    focus.row == Some(ix),
                    pal.fg,
                    pal,
                )
                .when(action.has_sub(), |d| {
                    d.child(
                        Icon::new(IconName::ChevronRight)
                            .size(px(14.))
                            .text_color(pal.secondary),
                    )
                })
                .when(live, |d| {
                    d.on_hover(cx.listener(move |m, on: &bool, _, cx| {
                        if *on {
                            m.focus.hover_row(ix, &rows);
                            cx.notify();
                        }
                    }))
                    .on_click(
                        cx.listener(move |m, _, window, cx| m.choose(Chosen::Row(ix), window, cx)),
                    )
                })
                .into_any_element(),
            );
        }
        let main = card("menu", x, y + 6. * (1. - t), menu::WIDTH)
            .track_focus(&self.handle)
            .on_key_down(cx.listener(Self::on_key))
            .opacity(t)
            .children(lines);

        // the playlists, beside "Add to playlist"
        let sub = open
            .rows
            .iter()
            .position(|a| a.has_sub())
            .filter(|_| s > 0.0)
            .map(|ix| {
                let n = open.subs.len() + 1;
                let size = (menu::SUB_WIDTH, menu::sub_height(n));
                let (sx, sy) = menu::place_sub(
                    (x, y, menu::WIDTH),
                    menu::row_top(&open.rows, ix),
                    size,
                    window_size,
                );
                let entries = std::iter::once((IconName::Plus, "New playlist…".to_string())).chain(
                    open.subs
                        .iter()
                        .map(|(_, name)| (IconName::ListMusic, name.clone())),
                );
                card("menu-sub", sx, sy, menu::SUB_WIDTH)
                    .max_h(px(size.1))
                    .overflow_y_scroll()
                    .track_scroll(&self.sub_scroll)
                    .opacity(s.min(t))
                    .children(entries.enumerate().map(|(i, (icon, name))| {
                        line(
                            ("menu-sub-row", i).into(),
                            icon,
                            name.into(),
                            focus.sub_open && focus.sub == Some(i),
                            if i == 0 { pal.accent } else { pal.fg },
                            pal,
                        )
                        .when(live, |d| {
                            d.on_hover(cx.listener(move |m, on: &bool, _, cx| {
                                if *on {
                                    m.focus.hover_sub(i);
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(
                                move |m, _, window, cx| m.choose(Chosen::Sub(i), window, cx),
                            ))
                        })
                    }))
            });

        div()
            .absolute()
            .inset_0()
            // open, a click anywhere else closes it, and goes no further
            .when(live, |d| {
                let close =
                    |m: &mut Self,
                     _: &MouseDownEvent,
                     window: &mut Window,
                     cx: &mut Context<Self>| { m.close(window, cx) };
                d.child(
                    div()
                        .id("menu-layer")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, cx.listener(close))
                        .on_mouse_down(MouseButton::Right, cx.listener(close))
                        .on_mouse_down(MouseButton::Middle, cx.listener(close)),
                )
            })
            .child(main)
            .children(sub)
            .into_any_element()
    }
}
