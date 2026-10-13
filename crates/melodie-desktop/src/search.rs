//! The search: one field, the results grouped by kind as the user types, and every result of one
//! kind on a page of its own. The local copy is read away from the window's thread, and the
//! answer to an older keystroke is dropped (`find::Latest`).

use crate::albums::{PAD, content_width, cover_box, empty_state};
use crate::app::Shared;
use crate::catalog::{Catalog, Empty};
use crate::context_menu::Menued as _;
use crate::dialogs::{self, Dialog};
use crate::motion;
use crate::player::palette;
use crate::tables::{RowCtx, row_button, row_hover, track_header, track_row};
use crate::widgets::{Hover, IconButton, Revealed as _, title_with_aside};
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::env::Env;
use melodie_core::search::{self, Hit};
use melodie_desktop::browse::Place;
use melodie_desktop::covers::{Covers, Use};
use melodie_desktop::find::{self, Action, Answer, Latest};
use melodie_desktop::launch::{Adding, Kind};
use melodie_desktop::menu::{Item, Spot};
use melodie_desktop::table::{self as track_table, ColSort, Shown, Sorts, Table};
use melodie_desktop::theme::Palette;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

const ROW: f32 = 56.0;

pub struct SearchView {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    env: Env,
    pub input: Entity<InputState>,
    field: Entity<Field>,
    /// The text of the field, as last changed.
    text: String,
    latest: Latest,
    /// The kind of every result asked last, to ask again when another is opened.
    asked_all: Option<Kind>,
    answer: Option<Arc<Answer>>,
    empty: Empty,
    scroll: ScrollHandle,
    all: UniformListScrollHandle,
    /// The tracks found as their tables show them, by place.
    tables: HashMap<Place, Shown>,
    /// The step of the place last seen, to focus the field when the search is opened.
    step: u64,
    _subs: Vec<Subscription>,
}

impl SearchView {
    pub fn new(
        shared: Entity<Shared>,
        catalog: Entity<Catalog>,
        env: Env,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Artists, albums, tracks, playlists")
        });
        let subs = vec![
            cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    if text != this.text {
                        this.catalog.update(cx, |c, _| c.sorts.searched());
                    }
                    this.text = text;
                    this.ask(cx)
                }
            }),
            cx.observe_in(&catalog, window, |this, catalog, window, cx| {
                let (here, step) = {
                    let c = catalog.read(cx);
                    (c.here().clone(), c.nav.step)
                };
                if step == this.step {
                    return;
                }
                this.step = step;
                // the move may have put a list back in its own order
                this.resort(cx);
                match here {
                    Place::Search => this.focus(window, cx),
                    Place::Found(kind) => {
                        this.all.scroll_to_item(0, ScrollStrategy::Top);
                        if this.asked_all != Some(kind) {
                            this.ask(cx)
                        }
                    }
                    _ => {}
                }
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
            text: String::new(),
            latest: Latest::default(),
            asked_all: None,
            answer: None,
            empty: Empty::Loading,
            scroll: ScrollHandle::new(),
            all: UniformListScrollHandle::new(),
            tables: HashMap::new(),
            step: 0,
            _subs: subs,
        }
    }

    /// Puts the cursor in the field, its text selected: typing replaces it.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx)
        });
    }

    pub fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Empties the field; false when it was empty already.
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.text.is_empty() {
            return false;
        }
        // no change event for this one
        self.input.update(cx, |i, cx| i.set_value("", window, cx));
        self.text.clear();
        self.catalog.update(cx, |c, _| c.sorts.searched());
        self.ask(cx);
        true
    }

    /// Reads the local copy for the text of the field, away from the window.
    fn ask(&mut self, cx: &mut Context<Self>) {
        let text = self.text.clone();
        let all = match self.catalog.read(cx).here() {
            Place::Found(kind) => Some(*kind),
            _ => None,
        };
        self.asked_all = all;
        let n = self.latest.ask();
        let env = self.env.clone();
        let started = Instant::now();
        let sorts = self.catalog.read(cx).sorts.clone();
        let task = cx.background_spawn(async move {
            let answer = find::answer(&env, &text, all)?;
            anyhow::Ok(answer.map(|a| {
                let tables = tabulate(&a, &sorts);
                (a, tables)
            }))
        });
        cx.spawn(async move |this, cx| {
            let answer = task.await;
            let _ = this.update(cx, |this, cx| {
                if !this.latest.is_latest(n) {
                    return;
                }
                match answer {
                    Ok(Some((a, tables))) => {
                        this.want_covers(&a, cx);
                        this.answer = Some(Arc::new(a));
                        this.tables = tables;
                        // sorted meanwhile
                        this.resort(cx);
                        this.empty = Empty::Loading;
                    }
                    Ok(None) => this.empty = Empty::NoCopy,
                    Err(_) => this.empty = Empty::Failed,
                }
                if crate::timing::on() {
                    crate::timing::note("search", started.elapsed());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Sorts the tables of the tracks found again where their sort changed.
    fn resort(&mut self, cx: &App) {
        let sorts = &self.catalog.read(cx).sorts;
        for (place, shown) in &mut self.tables {
            let sort = sorts.get(place);
            if shown.sort != sort {
                *shown = Shown::new(shown.tracks.clone(), sort);
            }
        }
    }

    fn sort(&mut self, place: Place, sort: Option<ColSort>, cx: &mut Context<Self>) {
        self.catalog.update(cx, |c, _| c.sorts.set(place, sort));
        self.resort(cx);
        cx.notify();
    }

    /// The header of the tracks found at `place`.
    fn header(
        &self,
        place: Place,
        cols: &[track_table::Col],
        pal: Palette,
        cx: &Context<Self>,
    ) -> Div {
        let sort = self.tables.get(&place).and_then(|s| s.sort);
        let this = cx.entity().downgrade();
        track_header(cols, false, sort, pal, move |s, cx| {
            let _ = this.update(cx, |v, cx| v.sort(place.clone(), s, cx));
        })
    }

    /// The covers of what is shown; of every track only those known already (there may be
    /// thousands).
    fn want_covers(&self, a: &Answer, cx: &mut Context<Self>) {
        let shown = a.groups.iter().flat_map(|g| g.hits.iter());
        let all = a
            .all
            .iter()
            .filter(|(kind, _)| *kind != Kind::Track)
            .flat_map(|(_, hits)| hits.iter());
        let ids: Vec<String> = shown.chain(all).filter_map(|h| h.cover.clone()).collect();
        self.catalog
            .update(cx, |c, cx| c.want_covers(ids, Use::SMALL, true, cx));
    }
}

/// The field, a view of its own: its blinking caret redraws it alone, not the results beside it.
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
            .cleanable(true)
            .h(px(44.))
    }
}

fn kind_title(kind: search::Kind) -> &'static str {
    match kind {
        search::Kind::Playlist => "Playlists",
        search::Kind::Artist => "Artists",
        search::Kind::Album => "Albums",
        search::Kind::Track => "Tracks",
    }
}

fn icon(action: Action) -> IconName {
    match action {
        Action::Play => IconName::Play,
        Action::Shuffle => IconName::Shuffle,
        Action::Next => IconName::ListStart,
        Action::Last => IconName::ListEnd,
        Action::Open => IconName::Disc3,
        Action::AddToPlaylist => IconName::ListPlus,
    }
}

/// What the button of an action says when the pointer rests on it.
fn tip(action: Action) -> &'static str {
    match action {
        Action::Play => "Play",
        Action::Shuffle => "Shuffle",
        Action::Next => "Play next",
        Action::Last => "Add to queue",
        Action::Open => "Open",
        Action::AddToPlaylist => "Add to playlist",
    }
}

/// Does what a result's action says: a job of `melodie`, its page, or the playlist to add to.
pub fn act(
    action: Action,
    hit: &Hit,
    catalog: &Entity<Catalog>,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(job) = find::job(action, hit) {
        catalog.update(cx, |c, cx| c.run(job, cx));
    } else if action == Action::Open {
        if let Some(place) = find::page(hit) {
            catalog.update(cx, |c, cx| c.go(place, cx));
        }
    } else {
        let what = match Kind::from(hit.kind) {
            Kind::Album => Adding::Album(hit.id.clone()),
            _ => Adding::Tracks(vec![hit.id.clone()]),
        };
        let about = hit.name.clone();
        dialogs::open(Dialog::Pick { what, about }, window, cx);
    }
}

/// One result: its picture, its name, what it is; a click plays a track and opens the rest; on
/// hover, its other actions fade in.
fn result_row(
    list: &'static str,
    ix: usize,
    hit: &Hit,
    covers: &Covers,
    pal: Palette,
    catalog: &Entity<Catalog>,
    hover: Hover,
) -> Stateful<Div> {
    let key = |what: &str| ElementId::Name(format!("{list}-{what}-{ix}").into());
    let round = if hit.kind == search::Kind::Artist {
        20.
    } else {
        6.
    };
    let (h, c) = (Arc::new(hit.clone()), catalog.clone());
    hover
        .track(h_flex().id(key("row")))
        .menu(Item::hit(hit), Spot::Library)
        .h(px(ROW))
        .flex_none()
        .w_full()
        .px(px(PAD))
        .on_click(move |_, window, cx| act(find::click(&h), &h, &c, window, cx))
        .child(
            h_flex()
                .size_full()
                .px_2()
                .gap_3()
                .items_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(Hsla { a: 0.07, ..pal.fg }))
                .child(cover_box(
                    hit.cover.as_ref().and_then(|c| covers.get(c, 40.)),
                    40.,
                    round,
                    pal,
                ))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .text_color(pal.fg)
                                .child(hit.name.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(pal.secondary)
                                .child(find::about(hit)),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .revealed(hover.look, true)
                        .children(find::buttons(hit).into_iter().map(|a| {
                            let (h, c) = (Arc::new(hit.clone()), catalog.clone());
                            row_button(
                                key(&format!("{a:?}")),
                                icon(a),
                                tip(a),
                                pal,
                                move |window, cx| act(a, &h, &c, window, cx),
                            )
                        })),
                ),
        )
}

/// The title of every result of one kind, with how many were found for `text`.
fn found_title(kind: Kind, n: usize, text: &str, pal: Palette, window: &Window) -> Div {
    title_with_aside(
        kind_title(kind.into()),
        Some(format!("{} for “{}”", n, text).into()),
        pal,
        window,
    )
    .px(px(PAD))
    .pb_2()
}

/// The tracks of an answer as their tables show them, by place. Blocking: run it away from the
/// window (there may be thousands).
fn tabulate(a: &Answer, sorts: &Sorts) -> HashMap<Place, Shown> {
    let grouped = a.groups.iter().find(|g| g.kind == search::Kind::Track);
    let all = a.all.as_ref().filter(|(k, _)| *k == Kind::Track);
    let lists = [
        (Place::Search, grouped.map(|g| &g.hits[..])),
        (Place::Found(Kind::Track), all.map(|(_, hits)| &hits[..])),
    ];
    lists
        .into_iter()
        .filter_map(|(place, hits)| {
            let tracks = hits?.iter().map(track_table::track_of).collect();
            let shown = Shown::new(tracks, sorts.get(&place));
            Some((place, shown))
        })
        .collect()
}

fn hint(id: &'static str, text: String, pal: Palette) -> AnyElement {
    div()
        .id(id)
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(pal.secondary)
        .child(text)
        .with_animation(id, motion::enter(), |el, t| el.opacity(t))
        .into_any_element()
}

impl Render for SearchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (here, covers, can_back) = {
            let c = self.catalog.read(cx);
            (
                c.here().clone(),
                Covers::new(c.covers.clone(), window.scale_factor()),
                c.nav.can_go_back(),
            )
        };
        let text = self.text.clone();
        let catalog = &self.catalog;
        let field = h_flex()
            .px(px(PAD))
            .pt_4()
            .pb_3()
            .gap_2()
            .items_center()
            .when(can_back, |d| {
                let catalog = catalog.clone();
                d.child(
                    IconButton::new("search-back", IconName::ChevronLeft, pal)
                        .tip("Back")
                        .on_click(move |_, _, cx| catalog.update(cx, |c, cx| c.back(cx))),
                )
            })
            .child(div().flex_1().min_w_0().child(self.field.clone()));
        let answer = self.answer.clone().filter(|_| !text.trim().is_empty());
        let cols = Table::Search.cols(content_width(window, &self.shared, cx));
        let tracks = self.tables.get(&Place::Search);
        let body: AnyElement = match (&here, answer) {
            _ if text.trim().is_empty() => hint(
                "search-hint",
                "Search artists, albums, tracks and playlists (Ctrl+F or / from anywhere)".into(),
                pal,
            ),
            (_, None) => empty_state(self.empty, "", pal).into_any_element(),
            (Place::Found(Kind::Track), Some(a)) => match self.tables.get(&here).cloned() {
                None => div().into_any_element(),
                Some(shown) => {
                    let c = catalog.clone();
                    let sorted = shown.sort.is_some();
                    let header = self.header(here.clone(), &cols, pal, cx);
                    let rows = cols.clone();
                    v_flex()
                        .size_full()
                        .child(found_title(Kind::Track, shown.len(), &a.text, pal, window))
                        .child(header)
                        .child(
                            uniform_list("search-all", shown.len(), move |range, window, cx| {
                                range
                                    .filter_map(|ix| {
                                        let (rank, t) = shown.row(ix)?;
                                        let hover = row_hover("search-all", ix, window, cx);
                                        let row = RowCtx::plain(rank, sorted, hover);
                                        Some(track_row("search-all", ix, t, &rows, pal, &c, row))
                                    })
                                    .collect()
                            })
                            .track_scroll(&self.all)
                            .flex_1(),
                        )
                        .into_any_element()
                }
            },
            (Place::Found(kind), Some(a)) => match a.all.as_ref().filter(|(k, _)| k == kind) {
                None => div().into_any_element(),
                Some((kind, hits)) => {
                    let hits = Arc::clone(hits);
                    let c = catalog.clone();
                    v_flex()
                        .size_full()
                        .child(found_title(*kind, hits.len(), &a.text, pal, window))
                        .child(
                            uniform_list("search-all", hits.len(), move |range, window, cx| {
                                range
                                    .map(|ix| {
                                        let hover = row_hover("search-all", ix, window, cx);
                                        result_row(
                                            "search-all",
                                            ix,
                                            &hits[ix],
                                            &covers,
                                            pal,
                                            &c,
                                            hover,
                                        )
                                    })
                                    .collect()
                            })
                            .track_scroll(&self.all)
                            .flex_1(),
                        )
                        .into_any_element()
                }
            },
            (_, Some(a)) if a.groups.is_empty() => hint(
                "search-none",
                format!("Nothing found for “{}”", a.text),
                pal,
            ),
            (_, Some(a)) => v_flex()
                .id("search-groups")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .pb_6()
                .children(a.groups.iter().map(|g| {
                    let list = match g.kind {
                        search::Kind::Playlist => "found-playlist",
                        search::Kind::Artist => "found-artist",
                        search::Kind::Album => "found-album",
                        search::Kind::Track => "found-track",
                    };
                    let (kind, c) = (Kind::from(g.kind), catalog.clone());
                    v_flex()
                        .flex_none()
                        .child(
                            h_flex()
                                .px(px(PAD))
                                .pt_4()
                                .pb_2()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(pal.fg)
                                        .child(kind_title(g.kind)),
                                )
                                .when(g.more, |d| {
                                    d.child(
                                        div()
                                            .id(ElementId::Name(format!("{list}-all").into()))
                                            .px_3()
                                            .py_1()
                                            .rounded_full()
                                            .cursor_pointer()
                                            .text_sm()
                                            .text_color(pal.secondary)
                                            .hover(|s| {
                                                s.text_color(pal.fg).bg(Hsla { a: 0.07, ..pal.fg })
                                            })
                                            .on_click(move |_, _, cx| {
                                                c.update(cx, |c, cx| c.go(Place::Found(kind), cx))
                                            })
                                            .child("See all"),
                                    )
                                }),
                        )
                        .map(|d| match tracks.filter(|_| g.kind == search::Kind::Track) {
                            // the tracks are a track table, as everywhere
                            Some(shown) => {
                                let sorted = shown.sort.is_some();
                                d.child(self.header(Place::Search, &cols, pal, cx))
                                    .children((0..shown.len()).filter_map(|ix| {
                                        let (rank, t) = shown.row(ix)?;
                                        let hover = row_hover(list, ix, window, cx);
                                        let row = RowCtx::plain(rank, sorted, hover);
                                        Some(track_row(list, ix, t, &cols, pal, catalog, row))
                                    }))
                            }
                            None => d.children(g.hits.iter().enumerate().map(|(ix, h)| {
                                let hover = row_hover(list, ix, window, cx);
                                result_row(list, ix, h, &covers, pal, catalog, hover)
                            })),
                        })
                }))
                .with_animation("search-groups", motion::enter(), |el, t| el.opacity(t))
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .relative()
            .child(field)
            .child(div().flex_1().min_h_0().child(body))
    }
}
