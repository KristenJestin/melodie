//! The search palette: Ctrl+K opens it over the current place, the field focused; the results
//! come grouped as the user types, the arrows move through them and Enter runs one. Before
//! anything is typed it lists what was opened or played lately. The search is the search
//! page's (`find::answer`), read away from the window's thread.

use crate::albums::cover_box;
use crate::app::{Shared, Started};
use crate::catalog::Catalog;
use crate::motion;
use crate::player::palette;
use crate::search;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::db;
use melodie_core::env::Env;
use melodie_core::search::{self as core, Genre};
use melodie_desktop::covers::{Covers, Use};
use melodie_desktop::find::{self, Latest};
use melodie_desktop::keys::{self, PaletteClick};
use melodie_desktop::launch::Job;
use melodie_desktop::quick::{self, Do, Group, How, Item, Recent, Section, Seen};
use melodie_desktop::theme::Palette;
use std::sync::Arc;
use std::time::Instant;

const WIDTH: f32 = 640.;
const ROW: f32 = 48.;
/// The results scroll past this height.
const LIST: f32 = 420.;

/// What the palette shows under its field.
#[derive(Clone)]
enum Shown {
    Rows(Arc<[Group]>),
    /// Nothing found for this text.
    Nothing(String),
    NoCopy,
    Failed,
}

pub struct QuickSearch {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    env: Env,
    pub input: Entity<InputState>,
    field: Entity<Field>,
    pub open: bool,
    text: String,
    latest: Latest,
    shown: Shown,
    /// The row chosen, counted across the groups.
    chosen: usize,
    /// The genres of the copy, read with the recent list at each opening (a new copy may have
    /// come); none until that answer is in.
    genres: Option<Arc<[Genre]>>,
    /// Numbers the openings: what an earlier one is still reading is not wanted.
    openings: Latest,
    recent: Recent,
    scroll: ScrollHandle,
    /// What had the focus before it opened: it gets it back.
    before: Option<FocusHandle>,
    _subs: Vec<Subscription>,
}

impl QuickSearch {
    pub fn new(
        shared: Entity<Shared>,
        catalog: Entity<Catalog>,
        env: Env,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search tracks, albums, artists, playlists, genres")
        });
        let mut step = None;
        let subs = vec![
            cx.subscribe(&input, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.text = input.read(cx).value().to_string();
                    this.ask(None, cx)
                }
            }),
            // every page opened is remembered, wherever it was opened from
            cx.observe(&catalog, move |this, catalog, cx| {
                let c = catalog.read(cx);
                if step == Some(c.nav.step) {
                    return;
                }
                step = Some(c.nav.step);
                if let Some(seen) = Seen::of_place(c.here()) {
                    this.recent.note(seen);
                }
            }),
            // and what the user starts playing: not what the daemon goes on to play by itself
            cx.subscribe(&shared, |this, _, Started(seen): &Started, _| {
                this.recent.note(seen.clone())
            }),
        ];
        let field = cx.new(|_| Field {
            input: input.clone(),
            shared: shared.clone(),
        });
        Self {
            shared,
            catalog,
            env,
            input,
            field,
            open: false,
            text: String::new(),
            latest: Latest::default(),
            shown: Shown::Rows(Arc::from([])),
            chosen: 0,
            genres: None,
            openings: Latest::default(),
            recent: Recent::default(),
            scroll: ScrollHandle::new(),
            before: None,
            _subs: subs,
        }
    }

    pub fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(window, cx)
        } else {
            self.show(window, cx)
        }
    }

    /// Opens it empty, on what was seen lately. What the last opening left is gone: Enter has
    /// nothing to run until this opening's own answer is in.
    fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        self.before = window.focused(cx);
        self.genres = None;
        self.shown = Shown::Rows(Arc::from([]));
        self.chosen = 0;
        self.text.clear();
        // no change event for this one
        self.input.update(cx, |i, cx| {
            i.set_value("", window, cx);
            i.focus(window, cx)
        });
        let opening = self.openings.ask();
        self.ask(Some(opening), cx);
        cx.notify();
    }

    /// Closes it; the focus goes back where it was.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        // an answer still on its way is not wanted any more
        self.latest.ask();
        if let Some(before) = self.before.take() {
            window.focus(&before, cx);
        }
        cx.notify();
    }

    /// Whether what is shown answers the text of the field, in a palette that is open.
    fn ready(&self) -> bool {
        self.open && self.latest.is_ready()
    }

    /// Reads the local copy for the text of the field, or what was seen lately when it is
    /// empty, away from the window. An opening (`Some`: its number) reads the genres too, once.
    fn ask(&mut self, opening: Option<u64>, cx: &mut Context<Self>) {
        let text = self.text.clone();
        let n = self.latest.ask();
        let env = self.env.clone();
        let seen = self.recent.list().to_vec();
        let known = self.genres.clone().unwrap_or_else(|| Arc::from([]));
        let started = Instant::now();
        let task = cx.background_spawn(async move {
            let genres = opening.map(|_| load_genres(&env));
            (genres, read(&env, &text, &seen, &known))
        });
        cx.spawn(async move |this, cx| {
            let (genres, shown) = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(opening) = opening {
                    // an earlier opening's genres never replace this one's, whatever was typed
                    // since
                    if !this.openings.is_latest(opening) {
                        return;
                    }
                    this.genres = genres;
                    if !this.latest.is_latest(n) {
                        // typed before the genres were in: ask again with them
                        if this.open {
                            this.ask(None, cx);
                        }
                        return;
                    }
                }
                if !this.latest.answered(n) {
                    return;
                }
                if let Shown::Rows(groups) = &shown {
                    let ids = groups
                        .iter()
                        .flat_map(|g| &g.items)
                        .filter_map(|i| match i {
                            Item::Found(h) => h.cover.clone(),
                            Item::Genre(_) => None,
                        });
                    let ids: Vec<String> = ids.collect();
                    this.catalog
                        .update(cx, |c, cx| c.want_covers(ids, Use::SMALL, true, cx));
                }
                this.shown = shown;
                this.chosen = 0;
                this.scroll.scroll_to_item(0);
                if crate::timing::on() {
                    crate::timing::note("palette", started.elapsed());
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn rows(&self) -> &[Group] {
        match &self.shown {
            Shown::Rows(groups) => groups,
            _ => &[],
        }
    }

    /// Moves the choice one row up or down.
    pub fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let len = quick::count(self.rows());
        self.chosen = quick::step(self.chosen, len, down);
        self.scroll
            .scroll_to_item(child_of(self.rows(), self.chosen));
        cx.notify();
    }

    /// Runs what `how` does to the row chosen, after closing. Nothing while it fades out, nor
    /// while the rows shown are not yet the answer to the text.
    pub fn run(&mut self, how: How, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ready() {
            return;
        }
        let Some(item) = quick::nth(self.rows(), self.chosen).cloned() else {
            return;
        };
        let Some(what) = quick::what(&item, how) else {
            return;
        };
        self.close(window, cx);
        match (what, item) {
            (Do::Hit(action), Item::Found(hit)) => {
                search::act(action, &hit, &self.catalog, window, cx)
            }
            (Do::Mix, Item::Genre(g)) => self
                .catalog
                .update(cx, |c, cx| c.run(Job::MixGenre { name: g.name }, cx)),
            _ => {}
        }
    }

    /// A click on a row: its main action.
    fn pick(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.ready() {
            self.chosen = ix;
            self.run(How::Main, window, cx)
        }
    }

    fn choose(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.open && self.chosen != ix {
            self.chosen = ix;
            cx.notify();
        }
    }
}

/// Every genre of the copy with its track count; none when there is no copy, nor when it cannot
/// be read (the search says so). Blocking.
fn load_genres(env: &Env) -> Arc<[Genre]> {
    let read = db::open_existing(env).and_then(|c| match c {
        Some(c) => Ok(core::genres(&c)?.into()),
        None => Ok(Arc::from([])),
    });
    read.unwrap_or_else(|_| Arc::from([]))
}

/// What the palette shows for `text`, the genres being those read at the opening. Blocking.
fn read(env: &Env, text: &str, seen: &[Seen], genres: &[Genre]) -> Shown {
    if text.trim().is_empty() {
        return match quick::recent(env, seen) {
            Ok(Some(items)) if items.is_empty() => Shown::Rows(Arc::from([])),
            Ok(Some(items)) => Shown::Rows(Arc::from([Group {
                section: Section::Recent,
                items,
            }])),
            Ok(None) => Shown::NoCopy,
            Err(_) => Shown::Failed,
        };
    }
    match find::answer(env, text, None) {
        Ok(Some(a)) => {
            let matched = core::matching_genres(genres, text, quick::GENRES);
            let groups = quick::groups(&a.groups, matched);
            if groups.is_empty() {
                Shown::Nothing(text.to_string())
            } else {
                Shown::Rows(groups.into())
            }
        }
        Ok(None) => Shown::NoCopy,
        Err(_) => Shown::Failed,
    }
}

/// The child of the list a row is: each group has its title first.
fn child_of(groups: &[Group], ix: usize) -> usize {
    let mut before = 0;
    let mut rows = 0;
    for g in groups {
        before += 1;
        if ix < rows + g.items.len() {
            return before + ix - rows;
        }
        rows += g.items.len();
        before += g.items.len();
    }
    before
}

/// The field, a view of its own: its blinking caret redraws it alone.
struct Field {
    input: Entity<InputState>,
    shared: Entity<Shared>,
}

impl Render for Field {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        Input::new(&self.input)
            .prefix(
                Icon::new(IconName::Search)
                    .size(px(18.))
                    .text_color(pal.secondary),
            )
            .appearance(false)
            .h(px(48.))
    }
}

/// The line under a row's name; in the recent list, what it is first.
fn about(item: &Item, section: Section) -> String {
    match item {
        Item::Genre(g) if g.tracks == 1 => "Genre · 1 track".into(),
        Item::Genre(g) => format!("Genre · {} tracks", g.tracks),
        Item::Found(h) if section == Section::Recent => {
            let what = Section::Kind(h.kind).title().trim_end_matches('s');
            format!("{what} · {}", find::about(h))
        }
        Item::Found(h) => find::about(h),
    }
}

fn row(
    this: &Entity<QuickSearch>,
    ix: usize,
    item: &Item,
    section: Section,
    chosen: bool,
    covers: &Covers,
    pal: Palette,
) -> AnyElement {
    let (name, picture) = match item {
        Item::Found(h) => {
            let round = if h.kind == core::Kind::Artist {
                18.
            } else {
                5.
            };
            let path = h.cover.as_ref().and_then(|c| covers.get(c, 36.));
            (h.name.clone(), cover_box(path, 36., round, pal))
        }
        Item::Genre(g) => (
            g.name.clone(),
            div()
                .size(px(36.))
                .flex_none()
                .rounded(px(5.))
                .bg(Hsla { a: 0.07, ..pal.fg })
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::Tag)
                        .size(px(16.))
                        .text_color(pal.secondary),
                ),
        ),
    };
    let (hover, click) = (this.clone(), this.clone());
    h_flex()
        .id(("quick-row", ix))
        .h(px(ROW))
        .flex_none()
        .w_full()
        .px_2()
        .gap_3()
        .items_center()
        .rounded_md()
        .cursor_pointer()
        .when(chosen, |d| d.bg(Hsla { a: 0.10, ..pal.fg }))
        // the pointer moving chooses, not a row scrolled under a pointer at rest
        .on_mouse_move(move |_, _, cx| hover.update(cx, |q, cx| q.choose(ix, cx)))
        .on_click(move |_, window, cx| click.update(cx, |q, cx| q.pick(ix, window, cx)))
        .child(picture)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().truncate().text_sm().text_color(pal.fg).child(name))
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(pal.secondary)
                        .child(about(item, section)),
                ),
        )
        .into_any_element()
}

fn hint(text: impl Into<SharedString>, pal: Palette) -> Div {
    div()
        .w_full()
        .py_6()
        .flex()
        .justify_center()
        .text_sm()
        .text_color(pal.secondary)
        .child(text.into())
}

/// A key and what it does, at the bottom.
fn key_hint(key: &'static str, does: &'static str, pal: Palette) -> Div {
    h_flex()
        .gap_1p5()
        .items_center()
        .child(
            div()
                .px_1p5()
                .rounded_sm()
                .border_1()
                .border_color(Hsla { a: 0.18, ..pal.fg })
                .text_color(pal.fg)
                .child(key),
        )
        .child(does)
}

impl Render for QuickSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // exactly 0 on the first frame of an opening: still drawn then
        let t = motion::flag("quick-open", self.open, motion::PANEL, window, cx);
        if t == 0.0 && !self.open {
            return div().into_any_element();
        }
        let pal = palette(&self.shared, window, cx);
        let covers = Covers::new(self.catalog.read(cx).covers.clone(), window.scale_factor());
        let this = cx.entity();
        let groups: Arc<[Group]> = match &self.shown {
            Shown::Rows(g) => g.clone(),
            _ => Arc::from([]),
        };
        let mut children: Vec<AnyElement> = Vec::new();
        let mut ix = 0;
        for g in groups.iter() {
            children.push(
                div()
                    .px_2()
                    .pt_3()
                    .pb_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(pal.secondary)
                    .child(g.section.title().to_uppercase())
                    .into_any_element(),
            );
            for item in &g.items {
                let chosen = ix == self.chosen;
                children.push(row(&this, ix, item, g.section, chosen, &covers, pal));
                ix += 1;
            }
        }
        let body: AnyElement = match &self.shown {
            Shown::Rows(_) if ix > 0 => v_flex()
                .id("quick-rows")
                .max_h(px(LIST))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .px_2()
                .pb_2()
                .children(children)
                .into_any_element(),
            Shown::Rows(_) if self.text.trim().is_empty() && self.latest.is_ready() => {
                hint("Type to search the library", pal).into_any_element()
            }
            Shown::Rows(_) => div().into_any_element(),
            Shown::Nothing(text) => {
                hint(format!("Nothing found for “{text}”"), pal).into_any_element()
            }
            Shown::NoCopy => hint("No local copy of the library yet", pal).into_any_element(),
            Shown::Failed => hint("The library could not be read", pal).into_any_element(),
        };
        let keys = quick::hints(quick::nth(&groups, self.chosen));
        let (close, back) = (this.clone(), this.clone());
        let width = WIDTH.min(window.viewport_size().width.as_f32() - 32.);
        div()
            .id("quick-scrim")
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0., 0., 0., 0.4 * t))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                close.update(cx, |q, cx| q.close(window, cx))
            })
            // a right click beside the card closes it, as a left click does, and opens no menu
            // on what was behind
            .on_mouse_down(MouseButton::Right, {
                let close = this.clone();
                move |_, window, cx| {
                    if keys::palette_right_click(false) == PaletteClick::Close {
                        close.update(cx, |q, cx| q.close(window, cx))
                    }
                }
            })
            // the layer keeps the pointer from the root, whose listener would close it
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                move |_, window, cx| back.update(cx, |q, cx| q.close(window, cx)),
            )
            .flex()
            .justify_center()
            .items_start()
            .child(
                v_flex()
                    .id("quick-card")
                    .mt(px(72. + 12. * (1. - t)))
                    .w(px(width))
                    .h_auto()
                    .max_h(px(LIST + 140.))
                    .opacity(t)
                    .rounded_xl()
                    .bg(pal.surface)
                    .border_1()
                    .border_color(Hsla { a: 0.12, ..pal.fg })
                    .shadow_xl()
                    .overflow_hidden()
                    // a click inside does not close it
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    // a right click inside does nothing: the rows have no menu
                    .on_mouse_down(MouseButton::Right, |_, _, cx| {
                        if keys::palette_right_click(true) == PaletteClick::Keep {
                            cx.stop_propagation()
                        }
                    })
                    .child(div().px_3().child(self.field.clone()))
                    .child(div().h(px(1.)).bg(Hsla { a: 0.08, ..pal.fg }))
                    .child(body)
                    .child(
                        h_flex()
                            .px_4()
                            .py_2()
                            .gap_4()
                            .border_t_1()
                            .border_color(Hsla { a: 0.08, ..pal.fg })
                            .text_xs()
                            .text_color(pal.secondary)
                            .children(keys.into_iter().map(|(k, d)| key_hint(k, d, pal))),
                    ),
            )
            .into_any_element()
    }
}
