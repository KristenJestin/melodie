//! The long lists: artists and tracks, sortable by their columns, in virtual lists of fixed-height
//! rows; and the track row the pages reuse. Nothing is animated inside a row, so that GPUI Fast
//! keeps a list on its scroll layer (`docs/gui-toolkit.md`, "Lists").

use crate::albums::{PAD, content_width, cover_box, empty_state};
use crate::app::Shared;
use crate::catalog::Catalog;
use crate::player::palette;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::library::{Artist, ArtistBy, Sort, Track, TrackBy};
use melodie_core::track::clock;
use melodie_desktop::browse::Place;
use melodie_desktop::launch::{Job, Kind};
use melodie_desktop::theme::Palette;
use std::sync::Arc;

pub const TRACK_ROW: f32 = 40.0;
const ARTIST_ROW: f32 = 56.0;
/// Under this content width, the secondary columns go.
const NARROW: f32 = 640.0;

/// Which columns a list of tracks shows.
#[derive(Clone, Copy)]
pub struct Cols {
    /// The position in the album or playlist instead of nothing.
    pub number: bool,
    pub artist: bool,
    pub album: bool,
    pub year: bool,
    pub plays: bool,
}

impl Cols {
    /// The secondary columns go when the content is narrow.
    pub fn fit(self, width: f32) -> Self {
        if width >= NARROW {
            return self;
        }
        Self {
            album: false,
            year: false,
            plays: false,
            ..self
        }
    }
}

fn cell(w: Option<f32>, flex: f32) -> Div {
    match w {
        Some(w) => div().w(px(w)).flex_none(),
        None => div().flex_grow(flex).flex_basis(px(0.)).min_w_0(),
    }
}

/// The header of a track list; a click on a sortable column sorts by it.
pub fn track_header(
    cols: Cols,
    sort: Option<Sort<TrackBy>>,
    pal: Palette,
    on_pick: impl Fn(Sort<TrackBy>, &mut App) + Clone + 'static,
) -> Div {
    let col = |id: &'static str,
               label: &'static str,
               by: Option<(TrackBy, bool)>,
               (w, flex): (Option<f32>, f32),
               base: Div| {
        let on = sort.zip(by).is_some_and(|(s, (b, _))| s.by == b);
        let pick = on_pick.clone();
        base.child(
            cell(w, flex).child(
                h_flex()
                    .id(id)
                    .gap_1()
                    .items_center()
                    .text_color(if on { pal.fg } else { pal.secondary })
                    .when_some(sort.zip(by), |d, (s, (b, natural))| {
                        d.cursor_pointer()
                            .hover(|st| st.text_color(pal.fg))
                            .on_click(move |_, _, cx| pick(s.clicked(b, natural), cx))
                    })
                    .child(label)
                    .when(on, |d| {
                        d.child(
                            Icon::new(if sort.is_some_and(|s| s.descending) {
                                IconName::ArrowDown
                            } else {
                                IconName::ArrowUp
                            })
                            .size(px(12.)),
                        )
                    }),
            ),
        )
    };
    let sortable = sort.is_some();
    let by = |b: TrackBy, natural: bool| sortable.then_some((b, natural));
    h_flex()
        .h(px(32.))
        .px(px(PAD + 8.))
        .gap_3()
        .items_center()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .border_b_1()
        .border_color(Hsla { a: 0.08, ..pal.fg })
        .when(cols.number, |d| d.child(cell(Some(28.), 0.).child("#")))
        .map(|d| {
            col(
                "th-title",
                "TITLE",
                by(TrackBy::Title, false),
                (None, 3.),
                d,
            )
        })
        .when(cols.artist, |d| {
            col(
                "th-artist",
                "ARTIST",
                by(TrackBy::Artist, false),
                (None, 2.),
                d,
            )
        })
        .when(cols.album, |d| {
            col(
                "th-album",
                "ALBUM",
                by(TrackBy::Album, false),
                (None, 2.),
                d,
            )
        })
        .when(cols.year, |d| {
            col(
                "th-year",
                "YEAR",
                by(TrackBy::Year, true),
                (Some(48.), 0.),
                d,
            )
        })
        .when(cols.plays, |d| {
            col(
                "th-plays",
                "PLAYS",
                by(TrackBy::Plays, true),
                (Some(48.), 0.),
                d,
            )
        })
        .map(|d| {
            col(
                "th-time",
                "TIME",
                by(TrackBy::Duration, true),
                (Some(48.), 0.),
                d,
            )
        })
        .child(cell(Some(64.), 0.))
}

/// A small button inside a row: shown on the row's hover, never animated.
fn row_button(
    id: ElementId,
    icon: IconName,
    pal: Palette,
    on_click: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|s| s.bg(Hsla { a: 0.12, ..pal.fg }))
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            on_click(cx)
        })
        .child(Icon::new(icon).size(px(16.)).text_color(pal.secondary))
}

/// A link to an artist or an album inside a row.
fn link(
    id: ElementId,
    text: String,
    to: Option<Place>,
    pal: Palette,
    catalog: &Entity<Catalog>,
) -> Stateful<Div> {
    let catalog = catalog.clone();
    div()
        .id(id)
        .truncate()
        .text_color(pal.secondary)
        .when_some(to, |d, to| {
            d.cursor_pointer()
                .hover(|s| s.text_color(pal.fg).underline())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    catalog.update(cx, |c, cx| c.go(to.clone(), cx))
                })
        })
        .child(text)
}

/// One track: a click plays it; on hover, play it next or add it at the end of the queue.
pub fn track_row(
    list: &'static str,
    ix: usize,
    t: &Track,
    cols: Cols,
    pal: Palette,
    catalog: &Entity<Catalog>,
) -> Stateful<Div> {
    let key = |what: &str| ElementId::Name(format!("{list}-{what}-{ix}").into());
    let job = |next: Option<bool>| {
        let id = t.id.clone();
        let catalog = catalog.clone();
        move |cx: &mut App| {
            let id = id.clone();
            let job = match next {
                None => Job::Play {
                    kind: Kind::Track,
                    id,
                    shuffle: false,
                },
                Some(next) => Job::Queue {
                    kind: Kind::Track,
                    id,
                    next,
                },
            };
            catalog.update(cx, |c, cx| c.run(job, cx))
        }
    };
    let play = job(None);
    h_flex()
        .id(key("row"))
        .group(list)
        .h(px(TRACK_ROW))
        .flex_none()
        .w_full()
        .px(px(PAD))
        .child(
            h_flex()
                .size_full()
                .px_2()
                .gap_3()
                .items_center()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .hover(|s| s.bg(Hsla { a: 0.07, ..pal.fg }))
                .when(cols.number, |d| {
                    d.child(
                        cell(Some(28.), 0.)
                            .text_xs()
                            .text_color(pal.secondary)
                            .child((ix + 1).to_string()),
                    )
                })
                .child(
                    cell(None, 3.)
                        .truncate()
                        .text_color(pal.fg)
                        .child(t.title.clone()),
                )
                .when(cols.artist, |d| {
                    d.child(cell(None, 2.).child(link(
                        key("artist"),
                        t.artist.clone(),
                        t.artist_id.clone().map(Place::Artist),
                        pal,
                        catalog,
                    )))
                })
                .when(cols.album, |d| {
                    d.child(cell(None, 2.).child(link(
                        key("album"),
                        t.album.clone(),
                        t.album_id.clone().map(Place::Album),
                        pal,
                        catalog,
                    )))
                })
                .when(cols.year, |d| {
                    d.child(
                        cell(Some(48.), 0.)
                            .text_color(pal.secondary)
                            .child(if t.year > 0 {
                                t.year.to_string()
                            } else {
                                String::new()
                            }),
                    )
                })
                .when(cols.plays, |d| {
                    d.child(
                        cell(Some(48.), 0.)
                            .text_color(pal.secondary)
                            .child(t.plays.to_string()),
                    )
                })
                .child(
                    cell(Some(48.), 0.)
                        .text_color(pal.secondary)
                        .child(clock(t.duration as f64)),
                )
                .child(
                    h_flex()
                        .w(px(64.))
                        .flex_none()
                        .gap_1()
                        .invisible()
                        .group_hover(list, |s| s.visible())
                        .child(row_button(
                            key("next"),
                            IconName::ListStart,
                            pal,
                            job(Some(true)),
                        ))
                        .child(row_button(
                            key("last"),
                            IconName::ListEnd,
                            pal,
                            job(Some(false)),
                        )),
                ),
        )
        .on_click(move |_, _, cx| play(cx))
}

pub struct TrackList {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    scroll: UniformListScrollHandle,
    shown_sort: Option<Sort<TrackBy>>,
}

impl TrackList {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>) -> Self {
        Self {
            shared,
            catalog,
            scroll: UniformListScrollHandle::new(),
            shown_sort: None,
        }
    }
}

fn title(text: &'static str, count: Option<usize>, pal: Palette) -> Div {
    h_flex()
        .px(px(PAD))
        .pt_5()
        .pb_3()
        .gap_3()
        .items_baseline()
        .child(
            div()
                .text_2xl()
                .font_weight(FontWeight::BOLD)
                .text_color(pal.fg)
                .child(text),
        )
        .when_some(count, |d, n| {
            d.child(
                div()
                    .text_sm()
                    .text_color(pal.secondary)
                    .child(n.to_string()),
            )
        })
}

impl Render for TrackList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (tracks, sort, empty): (Option<Arc<[Track]>>, _, _) = {
            let c = self.catalog.read(cx);
            (c.tracks.clone(), c.track_sort, c.empty)
        };
        if tracks.is_some() && self.shown_sort != Some(sort) {
            if self.shown_sort.is_some() {
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            self.shown_sort = Some(sort);
        }
        let cols = Cols {
            number: false,
            artist: true,
            album: true,
            year: true,
            plays: true,
        }
        .fit(content_width(window));
        let catalog = self.catalog.clone();
        let header = track_header(cols, Some(sort), pal, move |s, cx| {
            catalog.update(cx, |c, cx| c.sort_tracks(s, cx))
        });
        let body = match &tracks {
            None => empty_state(empty, "", pal).into_any_element(),
            Some(list) => {
                let list = list.clone();
                let catalog = self.catalog.clone();
                uniform_list("track-rows", list.len(), move |range, _, _| {
                    range
                        .map(|ix| track_row("track-row", ix, &list[ix], cols, pal, &catalog))
                        .collect()
                })
                .track_scroll(&self.scroll)
                .flex_1()
                .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .child(title("Tracks", tracks.as_ref().map(|t| t.len()), pal))
            .child(header)
            .child(body)
    }
}

pub struct ArtistList {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    scroll: UniformListScrollHandle,
    shown_sort: Option<Sort<ArtistBy>>,
}

impl ArtistList {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>) -> Self {
        Self {
            shared,
            catalog,
            scroll: UniformListScrollHandle::new(),
            shown_sort: None,
        }
    }
}

fn artist_header(sort: Sort<ArtistBy>, pal: Palette, catalog: Entity<Catalog>) -> Div {
    let col = |id: &'static str,
               label: &'static str,
               by: ArtistBy,
               natural: bool,
               flex: f32,
               base: Div| {
        let on = sort.by == by;
        let catalog = catalog.clone();
        base.child(
            cell(None, flex).child(
                h_flex()
                    .id(id)
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .text_color(if on { pal.fg } else { pal.secondary })
                    .hover(|s| s.text_color(pal.fg))
                    .on_click(move |_, _, cx| {
                        catalog.update(cx, |c, cx| c.sort_artists(sort.clicked(by, natural), cx))
                    })
                    .child(label)
                    .when(on, |d| {
                        d.child(
                            Icon::new(if sort.descending {
                                IconName::ArrowDown
                            } else {
                                IconName::ArrowUp
                            })
                            .size(px(12.)),
                        )
                    }),
            ),
        )
    };
    h_flex()
        .h(px(32.))
        .px(px(PAD + 8.))
        .gap_3()
        .items_center()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .border_b_1()
        .border_color(Hsla { a: 0.08, ..pal.fg })
        .child(cell(Some(40.), 0.))
        .map(|d| col("ah-name", "NAME", ArtistBy::Name, false, 4., d))
        .map(|d| col("ah-albums", "ALBUMS", ArtistBy::Albums, true, 1., d))
        .map(|d| col("ah-tracks", "TRACKS", ArtistBy::Tracks, true, 1., d))
}

fn artist_row(
    ix: usize,
    a: &Artist,
    covers: &std::collections::HashMap<String, std::path::PathBuf>,
    pal: Palette,
    catalog: &Entity<Catalog>,
) -> Stateful<Div> {
    let (catalog, id) = (catalog.clone(), a.id.clone());
    h_flex()
        .id(ElementId::Name(format!("artist-row-{ix}").into()))
        .h(px(ARTIST_ROW))
        .w_full()
        .px(px(PAD))
        .child(
            h_flex()
                .size_full()
                .px_2()
                .gap_3()
                .items_center()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .hover(|s| s.bg(Hsla { a: 0.07, ..pal.fg }))
                .child(cover_box(
                    a.cover.as_ref().and_then(|c| covers.get(c)),
                    40.,
                    20.,
                    pal,
                ))
                .child(
                    cell(None, 4.)
                        .truncate()
                        .text_color(pal.fg)
                        .child(a.name.clone()),
                )
                .child(
                    cell(None, 1.)
                        .text_color(pal.secondary)
                        .child(a.albums.to_string()),
                )
                .child(
                    cell(None, 1.)
                        .text_color(pal.secondary)
                        .child(a.tracks.to_string()),
                ),
        )
        .on_click(move |_, _, cx| catalog.update(cx, |c, cx| c.go(Place::Artist(id.clone()), cx)))
}

impl Render for ArtistList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (artists, covers, sort, empty) = {
            let c = self.catalog.read(cx);
            (c.artists.clone(), c.covers.clone(), c.artist_sort, c.empty)
        };
        if artists.is_some() && self.shown_sort != Some(sort) {
            if self.shown_sort.is_some() {
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            self.shown_sort = Some(sort);
        }
        let body = match &artists {
            None => empty_state(empty, "", pal).into_any_element(),
            Some(list) => {
                let list = list.clone();
                let catalog = self.catalog.clone();
                uniform_list("artist-rows", list.len(), move |range, _, _| {
                    range
                        .map(|ix| artist_row(ix, &list[ix], &covers, pal, &catalog))
                        .collect()
                })
                .track_scroll(&self.scroll)
                .flex_1()
                .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .child(title("Artists", artists.as_ref().map(|a| a.len()), pal))
            .child(artist_header(sort, pal, self.catalog.clone()))
            .child(body)
    }
}
