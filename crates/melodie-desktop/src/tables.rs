//! The long lists: artists and tracks, sortable by their columns, in virtual lists of fixed-height
//! rows; and the track row the pages reuse. The buttons of a row fade in under the pointer: that
//! is the one animation inside a row, and it runs only while the pointer moves from a row to
//! the next (`docs/gui-toolkit.md`, "Lists").

use crate::albums::{PAD, content_width, cover_box, empty_state};
use crate::app::Shared;
use crate::catalog::Catalog;
use crate::dialogs::{self, Dialog};
use crate::player::palette;
use crate::tips::Tipped as _;
use crate::widgets::{Hover, Revealed as _, hover, title_with_aside};
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::library::{Artist, ArtistBy, Sort, Track, TrackBy};
use melodie_desktop::browse::Place;
use melodie_desktop::covers::Covers;
use melodie_desktop::figures::{Figures as _, clock_text};
use melodie_desktop::launch::{Adding, Job, Kind};
use melodie_desktop::selection::{How, Selection};
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
    /// The rows can be removed (a playlist of the user's): one more button.
    pub edit: bool,
}

/// The width of the buttons a row shows on hover.
fn actions_width(cols: Cols) -> f32 {
    let n = if cols.edit { 5. } else { 4. };
    n * 28. + (n - 1.) * 4.
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
        .child(cell(Some(actions_width(cols)), 0.))
}

/// A small button inside a row; the row shows and hides its buttons together (`Revealed`).
pub fn row_button(
    id: ElementId,
    icon: IconName,
    tip: &'static str,
    pal: Palette,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
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
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx)
        })
        .child(Icon::new(icon).size(px(16.)).text_color(pal.secondary))
        .tip(tip, pal)
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

/// Where a track row is and what it may do besides playing.
#[derive(Clone)]
pub struct RowCtx {
    /// Rows of this list can be picked (Ctrl+click, Shift+click).
    pub selectable: bool,
    pub selected: bool,
    /// The playlist the row is in, when the user may edit it: remove the row, drag it elsewhere.
    pub playlist: Option<(String, String)>,
    /// Whether the pointer is on the row, for its buttons to fade in (see `row_hover`).
    pub hover: Hover,
}

impl RowCtx {
    /// A row that only plays.
    pub fn plain(hover: Hover) -> Self {
        Self {
            selectable: false,
            selected: false,
            playlist: None,
            hover,
        }
    }

    /// The row `ix` of the list at `here`, picked or not.
    pub fn of(selection: &Selection, here: &Place, ix: usize, hover: Hover) -> Self {
        Self {
            selectable: true,
            selected: selection.has(here, ix),
            playlist: None,
            hover,
        }
    }
}

/// The hover of the row `ix` of the list `list`: the pointer on the row, the buttons coming in.
pub fn row_hover(list: &'static str, ix: usize, window: &mut Window, cx: &mut App) -> Hover {
    hover(
        ElementId::Name(format!("{list}-hover-{ix}").into()),
        window,
        cx,
    )
}

/// A row of a playlist being dragged to another rank.
#[derive(Clone)]
pub struct Dragged {
    from: usize,
    title: SharedString,
    pal: Palette,
}

impl Render for Dragged {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        h_flex()
            .w(px(300.))
            .h(px(TRACK_ROW - 4.))
            .px_3()
            .gap_3()
            .items_center()
            .rounded_lg()
            .bg(pal.surface)
            .border_1()
            .border_color(pal.accent)
            .shadow_lg()
            .opacity(0.94)
            .child(
                Icon::new(IconName::GripVertical)
                    .size(px(16.))
                    .text_color(pal.secondary),
            )
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .text_color(pal.fg)
                    .child(self.title.clone()),
            )
    }
}

/// One track: a click plays it, Ctrl+click or Shift+click picks it; on hover, play it next, add
/// it at the end of the queue, add it to a playlist, an instant mix from it, and in the user's
/// playlist, remove it. A row of the user's playlist can be dragged onto another.
pub fn track_row(
    list: &'static str,
    ix: usize,
    t: &Track,
    cols: Cols,
    pal: Palette,
    catalog: &Entity<Catalog>,
    row: RowCtx,
) -> Stateful<Div> {
    let key = |what: &str| ElementId::Name(format!("{list}-{what}-{ix}").into());
    let job = |make: fn(String) -> Job| {
        let id = t.id.clone();
        let catalog = catalog.clone();
        move |_: &mut Window, cx: &mut App| {
            let job = make(id.clone());
            catalog.update(cx, |c, cx| c.run(job, cx))
        }
    };
    let play = job(|id| Job::Play {
        kind: Kind::Track,
        id,
        shuffle: false,
    });
    let pick = {
        let (id, title) = (t.id.clone(), t.title.clone());
        move |window: &mut Window, cx: &mut App| {
            let dialog = Dialog::Pick {
                what: Adding::Tracks(vec![id.clone()]),
                about: title.clone(),
            };
            dialogs::open(dialog, window, cx)
        }
    };
    let (select, selectable) = (catalog.clone(), row.selectable);
    let accent = pal.accent;
    let look = row.hover.look;
    row.hover
        .track(h_flex().id(key("row")))
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
                .when(row.selected, |d| {
                    d.bg(Hsla {
                        a: 0.18,
                        ..pal.accent
                    })
                })
                .hover(|s| s.bg(Hsla { a: 0.07, ..pal.fg }))
                .when(cols.number, |d| {
                    d.child(
                        cell(Some(28.), 0.)
                            .tabular()
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
                            .tabular()
                            .text_color(pal.secondary)
                            .child(t.plays.to_string()),
                    )
                })
                .child(
                    cell(Some(48.), 0.)
                        .text_color(pal.secondary)
                        .child(clock_text(t.duration as f64)),
                )
                .child(
                    h_flex()
                        .w(px(actions_width(cols)))
                        .flex_none()
                        .gap_1()
                        .revealed(look, true)
                        .child(row_button(
                            key("next"),
                            IconName::ListStart,
                            "Play next",
                            pal,
                            job(|id| Job::Queue {
                                kind: Kind::Track,
                                id,
                                next: true,
                            }),
                        ))
                        .child(row_button(
                            key("last"),
                            IconName::ListEnd,
                            "Add to queue",
                            pal,
                            job(|id| Job::Queue {
                                kind: Kind::Track,
                                id,
                                next: false,
                            }),
                        ))
                        .child(row_button(
                            key("add"),
                            IconName::ListPlus,
                            "Add to playlist",
                            pal,
                            pick,
                        ))
                        .child(row_button(
                            key("mix"),
                            IconName::Sparkles,
                            "Instant mix",
                            pal,
                            job(|id| Job::Mix {
                                kind: Kind::Track,
                                id,
                            }),
                        ))
                        .when_some(
                            row.playlist.clone().filter(|_| cols.edit),
                            |d, (pid, name)| {
                                let (catalog, id, title) =
                                    (catalog.clone(), t.id.clone(), t.title.clone());
                                d.child(row_button(
                                    key("remove"),
                                    IconName::ListMinus,
                                    "Remove from playlist",
                                    pal,
                                    move |_, cx| {
                                        let job = Job::RemoveFromPlaylist {
                                            id: pid.clone(),
                                            picks: vec![(ix, id.clone())],
                                        };
                                        let said = format!("Removed {title} from {name}");
                                        catalog.update(cx, |c, cx| c.edit(job, Some(said), cx))
                                    },
                                ))
                            },
                        ),
                ),
        )
        .when(row.playlist.is_some(), |d| {
            let dropped = catalog.clone();
            d.on_drag(
                Dragged {
                    from: ix,
                    title: t.title.clone().into(),
                    pal,
                },
                |d, _, _, cx| cx.new(|_| d.clone()),
            )
            // the line shows where the row will land
            .drag_over::<Dragged>(move |style, d, _, _| {
                if d.from < ix {
                    style.border_b_2().border_color(accent)
                } else if d.from > ix {
                    style.border_t_2().border_color(accent)
                } else {
                    style
                }
            })
            .on_drop(move |d: &Dragged, _, cx| {
                let from = d.from;
                dropped.update(cx, |c, cx| c.move_in_playlist(from, ix, cx))
            })
        })
        .on_click(move |ev, window, cx| {
            let m = ev.modifiers();
            if selectable && (m.control || m.shift) {
                let how = if m.shift { How::Range } else { How::Toggle };
                select.update(cx, |c, cx| c.select(ix, how, cx))
            } else {
                play(window, cx)
            }
        })
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

fn title(text: &'static str, count: Option<usize>, pal: Palette, window: &Window) -> Div {
    title_with_aside(text, count.map(|n| n.to_string().into()), pal, window)
        .px(px(PAD))
        .pt_5()
        .pb_3()
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
            edit: false,
        }
        .fit(content_width(window, &self.shared, cx));
        let selection = self.catalog.read(cx).selection.clone();
        let catalog = self.catalog.clone();
        let header = track_header(cols, Some(sort), pal, move |s, cx| {
            catalog.update(cx, |c, cx| c.sort_tracks(s, cx))
        });
        let body = match &tracks {
            None => empty_state(empty, "", pal).into_any_element(),
            Some(list) => {
                let list = list.clone();
                let catalog = self.catalog.clone();
                uniform_list("track-rows", list.len(), move |range, window, cx| {
                    range
                        .map(|ix| {
                            let hover = row_hover("track-row", ix, window, cx);
                            let row = RowCtx::of(&selection, &Place::Tracks, ix, hover);
                            track_row("track-row", ix, &list[ix], cols, pal, &catalog, row)
                        })
                        .collect()
                })
                .track_scroll(&self.scroll)
                .flex_1()
                .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .child(title(
                "Tracks",
                tracks.as_ref().map(|t| t.len()),
                pal,
                window,
            ))
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
    covers: &Covers,
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
                    a.cover.as_ref().and_then(|c| covers.get(c, 40.)),
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
            (
                c.artists.clone(),
                Covers::new(c.covers.clone(), window.scale_factor()),
                c.artist_sort,
                c.empty,
            )
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
            .child(title(
                "Artists",
                artists.as_ref().map(|a| a.len()),
                pal,
                window,
            ))
            .child(artist_header(sort, pal, self.catalog.clone()))
            .child(body)
    }
}
