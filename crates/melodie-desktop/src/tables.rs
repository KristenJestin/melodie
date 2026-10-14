//! The long lists: artists and tracks, sortable by their columns, in virtual lists of fixed-height
//! rows; and the track row the pages reuse. The buttons of a row fade in under the pointer: that
//! is the one animation inside a row, and it runs only when the pointer itself moves to another
//! row; rows scrolled under a still pointer change at once (`docs/gui-toolkit.md`, "Lists").

use crate::albums::{PAD, artist_play, content_width, cover_box, empty_state};
use crate::app::Shared;
use crate::catalog::Catalog;
use crate::context_menu::{self, Menued as _};
use crate::dialogs::{self, Dialog};
use crate::equaliser::Equaliser;
use crate::player::palette;
use crate::tips::Tipped as _;
use crate::widgets::{Hover, Revealed as _, hover, title_with_aside};
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::library::{Artist, ArtistBy, Sort, Track};
use melodie_desktop::browse::Place;
use melodie_desktop::covers::Covers;
use melodie_desktop::figures::{Figures as _, clock_text};
use melodie_desktop::launch::{Adding, Job, Kind};
use melodie_desktop::menu::{Item, Spot};
use melodie_desktop::playing::{Mark, Marks};
use melodie_desktop::selection::{Click, Selection};
use melodie_desktop::table::{self, Col, ColSort, Cols, Shown, Table, Width};
use melodie_desktop::theme::Palette;
use std::sync::Arc;

pub const TRACK_ROW: f32 = 40.0;
const ARTIST_ROW: f32 = 56.0;

/// The width of the buttons a row shows on hover; one more in the user's playlist.
fn actions_width(edit: bool) -> f32 {
    let n = if edit { 5. } else { 4. };
    n * 28. + (n - 1.) * 4.
}

fn cell(w: Option<f32>, flex: f32) -> Div {
    match w {
        Some(w) => div().w(px(w)).flex_none(),
        None => div().flex_grow(flex).flex_basis(px(0.)).min_w_0(),
    }
}

/// The cell of a column of the track table, as wide as its rule says.
fn col_cell(col: Col) -> Div {
    match col.width() {
        Width::Fixed(w) => cell(Some(w), 0.),
        Width::Share(f) => cell(None, f),
    }
}

/// The header of a track table; a click on a column sorts by it (ascending, descending, then the
/// list's own order). `edit`: the rows have the button that removes them.
pub fn track_header(
    cols: &Cols,
    edit: bool,
    sort: Option<ColSort>,
    pal: Palette,
    on_pick: impl Fn(Option<ColSort>, &mut App) + Clone + 'static,
) -> Div {
    h_flex()
        .h(px(32.))
        .flex_none()
        .px(px(PAD + 8.))
        .gap_3()
        .items_center()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .border_b_1()
        .border_color(Hsla { a: 0.08, ..pal.fg })
        .children(cols.iter().map(|&col| {
            let on = sort.filter(|s| s.col == col);
            let pick = on_pick.clone();
            col_cell(col).child(
                h_flex()
                    .id(ElementId::Name(format!("th-{}", col.label()).into()))
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .text_color(if on.is_some() { pal.fg } else { pal.secondary })
                    .hover(|st| st.text_color(pal.fg))
                    // the window's root takes the focus from any press that reaches it: sorting
                    // must not take it from the search field
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(move |_, _, cx| pick(table::clicked(sort, col), cx))
                    .child(col.label())
                    .when_some(on, |d, s| {
                        d.child(
                            Icon::new(if s.descending {
                                IconName::ArrowDown
                            } else {
                                IconName::ArrowUp
                            })
                            .size(px(12.)),
                        )
                    }),
            )
        }))
        .child(cell(Some(actions_width(edit)), 0.))
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
        .on_click(move |ev, window, cx| {
            cx.stop_propagation();
            // GPUI calls it for each click of a series: a double click acts once
            if ev.click_count() <= 1 {
                on_click(window, cx)
            }
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
    /// Its rank in the list's own order (an album's, a playlist's), whatever the sort shows.
    pub rank: usize,
    /// The list is shown sorted: its rows are not dragged.
    pub sorted: bool,
    pub selected: bool,
    /// The row is the music playing now (or paused on): its title in the accent and an equaliser.
    pub mark: Option<Mark>,
    /// The list the row is shown in, which a click picks in.
    pub shown: Shown,
    /// The playlist the row is in, when the user may edit it: remove the row, drag it elsewhere.
    pub playlist: Option<(String, String)>,
    /// Whether the pointer is on the row, for its buttons to fade in (see `row_hover`).
    pub hover: Hover,
}

impl RowCtx {
    /// The row shown at `ix` of `shown`, the list at `here`, picked or not, playing or not.
    pub fn of(
        selection: &Selection,
        here: &Place,
        shown: &Shown,
        ix: usize,
        hover: Hover,
        marks: &Marks,
    ) -> Self {
        let row = shown.row(ix);
        Self {
            rank: row.map_or(ix, |(rank, _)| rank),
            sorted: shown.sort.is_some(),
            selected: selection.has(here, ix),
            mark: row.and_then(|(rank, t)| marks.of(&t.id, rank)),
            shown: shown.clone(),
            playlist: None,
            hover,
        }
    }
}

/// What a left click on a row does, read from the click: `Click::of` decides.
pub fn row_click(
    on: impl Fn(Click, &mut Window, &mut App) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |ev, window, cx| {
        let m = ev.modifiers();
        on(Click::of(m.control, m.shift, ev.click_count()), window, cx)
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

/// One track, shown at `ix`: a click picks it, a double click plays it; on hover, play it next, add
/// it at the end of the queue, add it to a playlist, an instant mix from it, and in the user's
/// playlist, remove it. A row of the user's playlist can be dragged onto another.
pub fn track_row(
    list: &'static str,
    ix: usize,
    t: &Track,
    cols: &Cols,
    pal: Palette,
    catalog: &Entity<Catalog>,
    row: RowCtx,
) -> Stateful<Div> {
    let rank = row.rank;
    let key = |what: &str| ElementId::Name(format!("{list}-{what}-{ix}").into());
    let job = |make: fn(String) -> Job| {
        let id = t.id.clone();
        let catalog = catalog.clone();
        move |_: &mut Window, cx: &mut App| {
            let job = make(id.clone());
            catalog.update(cx, |c, cx| c.run(job, cx))
        }
    };
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
    let (select, shown) = (catalog.clone(), row.shown.clone());
    let (accent, selected, mark) = (pal.accent, row.selected, row.mark);
    let numbered = cols.contains(&Col::Number);
    let look = row.hover.look;
    let (menued, menu_shown, menu_playlist) =
        (catalog.clone(), row.shown.clone(), row.playlist.clone());
    row.hover
        .track(h_flex().id(key("row")))
        .on_mouse_down(MouseButton::Right, move |ev, window, cx| {
            cx.stop_propagation();
            let menu = menued.update(cx, |c, cx| {
                c.menu_on(&menu_shown, ix, menu_playlist.clone(), cx)
            });
            if let Some((item, spot)) = menu {
                context_menu::open(item, spot, None, ev.position, window, cx)
            }
        })
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
                .when(selected, |d| d.bg(Hsla { a: 0.18, ..accent }))
                // a picked row stays picked under the pointer
                .hover(move |s| {
                    s.bg(if selected {
                        Hsla { a: 0.26, ..accent }
                    } else {
                        Hsla { a: 0.07, ..pal.fg }
                    })
                })
                .children(cols.iter().map(|&col| {
                    let c = col_cell(col).text_color(pal.secondary);
                    match col {
                        // the equaliser takes the place of the rank, still when paused
                        Col::Number => match mark {
                            Some(m) => c.child(Equaliser::new(key("equaliser"), m, accent)),
                            None => c.tabular().text_xs().child((rank + 1).to_string()),
                        },
                        Col::Title => match mark {
                            // no rank column in this table: the equaliser goes before the title
                            Some(m) if !numbered => c
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_color(accent)
                                .child(Equaliser::new(key("equaliser"), m, accent))
                                .child(div().min_w_0().truncate().child(t.title.clone())),
                            Some(_) => c.truncate().text_color(accent).child(t.title.clone()),
                            None => c.truncate().text_color(pal.fg).child(t.title.clone()),
                        },
                        Col::Artist => c.child(link(
                            key("artist"),
                            t.artist.clone(),
                            t.artist_id.clone().map(Place::Artist),
                            pal,
                            catalog,
                        )),
                        Col::Album => c.child(link(
                            key("album"),
                            t.album.clone(),
                            t.album_id.clone().map(Place::Album),
                            pal,
                            catalog,
                        )),
                        Col::Year => c.child(if t.year > 0 {
                            t.year.to_string()
                        } else {
                            String::new()
                        }),
                        Col::Plays => c.tabular().child(t.plays.to_string()),
                        Col::Time => c.child(clock_text(t.duration as f64)),
                    }
                }))
                .child(
                    h_flex()
                        .w(px(actions_width(row.playlist.is_some())))
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
                        .when_some(row.playlist.clone(), |d, (pid, name)| {
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
                                        picks: vec![(rank, id.clone())],
                                    };
                                    let said = format!("Removed {title} from {name}");
                                    catalog.update(cx, |c, cx| c.edit(job, Some(said), cx))
                                },
                            ))
                        }),
                ),
        )
        // a sorted view is not the playlist's order: nowhere to drop a row
        .when(row.playlist.is_some() && !row.sorted, |d| {
            let dropped = catalog.clone();
            d.on_drag(
                Dragged {
                    from: rank,
                    title: t.title.clone().into(),
                    pal,
                },
                |d, _, _, cx| cx.new(|_| d.clone()),
            )
            // the line shows where the row will land
            .drag_over::<Dragged>(move |style, d, _, _| {
                if d.from < rank {
                    style.border_b_2().border_color(accent)
                } else if d.from > rank {
                    style.border_t_2().border_color(accent)
                } else {
                    style
                }
            })
            .on_drop(move |d: &Dragged, _, cx| {
                let from = d.from;
                dropped.update(cx, |c, cx| c.move_in_playlist(from, rank, cx))
            })
        })
        .on_click(row_click(move |click, _, cx| {
            select.update(cx, |c, cx| c.left_click(&shown, ix, click, cx))
        }))
}

pub struct TrackList {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    scroll: UniformListScrollHandle,
    shown_sort: Option<Option<ColSort>>,
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
        let (shown, empty) = {
            let c = self.catalog.read(cx);
            (c.views.get(&Place::Tracks).cloned(), c.empty)
        };
        if let Some(shown) = &shown
            && self.shown_sort != Some(shown.sort)
        {
            if self.shown_sort.is_some() {
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            self.shown_sort = Some(shown.sort);
        }
        context_menu::list_is(&Place::Tracks, &self.scroll, TRACK_ROW, cx);
        let cols = Arc::new(Table::Tracks.cols(content_width(window, &self.shared, cx)));
        let selection = self.catalog.read(cx).selection.clone();
        let catalog = self.catalog.clone();
        let sort = self.catalog.read(cx).sorts.get(&Place::Tracks);
        let header = track_header(&cols, false, sort, pal, move |s, cx| {
            catalog.update(cx, |c, cx| c.sort_tracks(Place::Tracks, s, cx))
        });
        let body = match &shown {
            None => empty_state(empty, "", pal).into_any_element(),
            Some(shown) => {
                let marks = self
                    .shared
                    .read(cx)
                    .playing
                    .marks(&Place::Tracks, &shown.tracks);
                track_rows(
                    "track-row",
                    shown.clone(),
                    cols,
                    pal,
                    &self.catalog,
                    (Place::Tracks, selection, marks),
                    None,
                )
                .track_scroll(&self.scroll)
                .flex_1()
                .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .child(title("Tracks", shown.as_ref().map(Shown::len), pal, window))
            .child(header)
            .child(body)
    }
}

/// The rows of a track table in a virtual list, `marks` saying which is playing. They can be picked; in a playlist of the user's
/// (`playlist`: its id and name) they can be removed, and dragged while the view is not sorted.
pub fn track_rows(
    id: &'static str,
    shown: Shown,
    cols: Arc<Cols>,
    pal: Palette,
    catalog: &Entity<Catalog>,
    (here, selection, marks): (Place, Selection, Marks),
    playlist: Option<(String, String)>,
) -> UniformList {
    let catalog = catalog.clone();
    uniform_list(id, shown.len(), move |range, window, cx| {
        range
            .filter_map(|ix| {
                let (_, t) = shown.row(ix)?;
                let hover = row_hover(id, ix, window, cx);
                let row = RowCtx {
                    playlist: playlist.clone(),
                    ..RowCtx::of(&selection, &here, &shown, ix, hover, &marks)
                };
                Some(track_row(id, ix, t, &cols, pal, &catalog, row))
            })
            .collect()
    })
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
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let (catalog, id) = (catalog.clone(), a.id.clone());
    let item = Item::artist(a);
    let hover = row_hover("artist-row", ix, window, cx);
    let look = hover.look;
    hover
        .track(h_flex().id(ElementId::Name(format!("artist-row-{ix}").into())))
        .menu(item.clone(), Spot::Library)
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
                .child(
                    div()
                        .relative()
                        .flex_none()
                        .child(
                            cover_box(
                                a.cover.as_ref().and_then(|c| covers.get(c, 40.)),
                                40.,
                                20.,
                                pal,
                            )
                            .opacity(look.under()),
                        )
                        .children(
                            artist_play(
                                &format!("artist-row-play-{ix}"),
                                &item,
                                28.,
                                look,
                                pal,
                                &catalog,
                            )
                            .map(|b| div().absolute().top(px(6.)).left(px(6.)).child(b)),
                        ),
                )
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
                uniform_list("artist-rows", list.len(), move |range, window, cx| {
                    range
                        .map(|ix| artist_row(ix, &list[ix], &covers, pal, &catalog, window, cx))
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

#[cfg(test)]
mod tests {
    // not `super::*`: it would bring the `test` macro of GPUI along
    use super::{RowCtx, TRACK_ROW, row_button, row_click, row_hover, track_header, track_row};
    use crate::albums::PAD;
    use crate::catalog::Catalog;
    use crate::context_menu::{ContextMenu, MenuHandle};
    use gpui_kit::assets::IconName;
    use gpui_kit::{
        AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
        Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement as _, Pixels, Point,
        Render, StatefulInteractiveElement as _, Styled as _, TestAppContext, VisualTestContext,
        Window, div, point, px,
    };
    use melodie_core::env::Env;
    use melodie_core::library::Track;
    use melodie_core::proto::QueueEntry;
    use melodie_desktop::menu::Item;
    use melodie_desktop::model::{PlayState, Player};
    use melodie_desktop::playing::Playing;
    use melodie_desktop::selection::{Click, How};
    use melodie_desktop::table::Shown;
    use melodie_desktop::table::{Cols, Table};
    use melodie_desktop::theme::Palette;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::Arc;

    const FIELD: f32 = 44.;

    /// The search page's shape: the window's root, which takes the focus when it is clicked, a
    /// field above, and the header of the tracks under it.
    struct Page {
        root: FocusHandle,
        field: FocusHandle,
        cols: Cols,
        sorted: Rc<Cell<u32>>,
    }

    impl Render for Page {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let sorted = self.sorted.clone();
            div()
                .id("root")
                .track_focus(&self.root)
                .size_full()
                .child(
                    div()
                        .id("field")
                        .track_focus(&self.field)
                        .h(px(FIELD))
                        .w_full(),
                )
                .child(track_header(
                    &self.cols,
                    false,
                    None,
                    Palette::base(None, true),
                    move |_, _| sorted.set(sorted.get() + 1),
                ))
        }
    }

    #[test]
    fn sorting_the_tracks_keeps_the_fields_focus() {
        let mut app = TestAppContext::single();
        let sorted = Rc::new(Cell::new(0));
        let (page, cx): (Entity<Page>, _) = app.add_window_view(|_, cx| Page {
            root: cx.focus_handle(),
            field: cx.focus_handle(),
            cols: Table::Search.cols(1200.),
            sorted: sorted.clone(),
        });
        let field = page.read_with(cx, |p, _| p.field.clone());
        cx.update(|window, cx| window.focus(&field, cx));
        cx.run_until_parked();
        assert!(
            cx.update(|window, _| field.is_focused(window)),
            "the field starts with the focus"
        );

        // on the first column's title, in the header under the field
        cx.simulate_click(point(px(PAD + 8. + 3.), px(FIELD + 16.)), Modifiers::none());
        assert_eq!(sorted.get(), 1, "the click reached the header");
        assert!(
            cx.update(|window, _| field.is_focused(window)),
            "the field lost the focus to the click"
        );
    }

    type Clicks = Rc<RefCell<Vec<Click>>>;

    /// A track row's shape: the row, and one of its buttons at its left end.
    struct Row {
        clicks: Clicks,
        button: Rc<Cell<u32>>,
    }

    impl Render for Row {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let (clicks, button) = (self.clicks.clone(), self.button.clone());
            div().size_full().child(
                div()
                    .id("row")
                    .flex()
                    .h(px(40.))
                    .w_full()
                    .on_click(row_click(move |c, _, _| clicks.borrow_mut().push(c)))
                    .child(row_button(
                        "next".into(),
                        IconName::ListStart,
                        "Play next",
                        Palette::base(None, true),
                        move |_, _| button.set(button.get() + 1),
                    )),
            )
        }
    }

    fn row_page(app: &mut TestAppContext) -> (Clicks, Rc<Cell<u32>>, &mut VisualTestContext) {
        let clicks = Rc::new(RefCell::new(Vec::new()));
        let button = Rc::new(Cell::new(0));
        let (_, cx): (Entity<Row>, _) = app.add_window_view(|_, _| Row {
            clicks: clicks.clone(),
            button: button.clone(),
        });
        (clicks, button, cx)
    }

    /// The second click of a double click: the platform counts it.
    fn second_click(cx: &mut VisualTestContext, position: Point<Pixels>) {
        cx.simulate_click(position, Modifiers::none());
        cx.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 2,
        });
    }

    #[test]
    fn a_click_on_a_row_picks_it_a_double_click_plays_it() {
        let mut app = TestAppContext::single();
        let (clicks, button, cx) = row_page(&mut app);
        let on_row = point(px(300.), px(20.));

        cx.simulate_click(on_row, Modifiers::none());
        assert_eq!(
            *clicks.borrow(),
            [Click::Pick(How::Only)],
            "a click picks, never plays"
        );

        clicks.borrow_mut().clear();
        second_click(cx, on_row);
        assert_eq!(
            *clicks.borrow(),
            [Click::Pick(How::Only), Click::Play],
            "a double click picks, then plays"
        );

        clicks.borrow_mut().clear();
        cx.simulate_click(on_row, Modifiers::control());
        cx.simulate_click(on_row, Modifiers::shift());
        assert_eq!(
            *clicks.borrow(),
            [Click::Pick(How::Toggle), Click::Pick(How::Range)]
        );

        // a button of the row does what it does, once for a double click, and the row hears
        // nothing
        clicks.borrow_mut().clear();
        second_click(cx, point(px(14.), px(20.)));
        assert_eq!(button.get(), 1);
        assert!(clicks.borrow().is_empty(), "{:?}", clicks.borrow());
    }

    /// Three tracks in the real track rows, with the window's state: the jobs run a script that
    /// writes down what it was asked, never the machine's `melodie`.
    struct Tracks {
        catalog: Entity<Catalog>,
        shown: Shown,
        playing: Playing,
    }

    impl Render for Tracks {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let (here, selection) = {
                let c = self.catalog.read(cx);
                (c.here().clone(), c.selection.clone())
            };
            let cols = Table::Album.cols(1200.);
            let pal = Palette::base(None, true);
            let marks = self.playing.marks(&here, &self.shown.tracks);
            div()
                .size_full()
                .children((0..self.shown.len()).filter_map(|ix| {
                    let (_, t) = self.shown.row(ix)?;
                    let hover = row_hover("t", ix, window, cx);
                    let row = RowCtx::of(&selection, &here, &self.shown, ix, hover, &marks);
                    Some(track_row("t", ix, t, &cols, pal, &self.catalog, row))
                }))
        }
    }

    #[test]
    fn the_track_rows_pick_on_a_click_play_on_a_double_click_and_open_the_selections_menu() {
        let dir = tempfile::tempdir().unwrap();
        let program = crate::app::fake_melodie(dir.path());
        let ran = || crate::app::ran(&program);
        let env = Env::with_home(dir.path().join("home"));

        let mut app = TestAppContext::single();
        let (view, cx): (Entity<Tracks>, _) = app.add_window_view(|_, cx| {
            let shared = crate::app::bare(env.clone(), program.clone(), cx);
            let catalog = cx.new(|cx| Catalog::new(env.clone(), shared.clone(), cx));
            let menu = cx.new(|cx| ContextMenu::new(env.clone(), shared, catalog.clone(), cx));
            cx.set_global(MenuHandle(menu));
            let tracks: Arc<[Track]> = (0..3)
                .map(|i| Track {
                    id: format!("t{i}"),
                    title: format!("Title {i}"),
                    ..Track::default()
                })
                .collect();
            Tracks {
                catalog,
                shown: Shown::new(tracks, None),
                playing: Playing::default(),
            }
        });
        cx.run_until_parked();
        let catalog = view.read_with(cx, |v, _| v.catalog.clone());
        let picked =
            |cx: &mut VisualTestContext| catalog.read_with(cx, |c, _| c.selection.ranks(c.here()));
        // on the number of each row
        let on = |ix: usize| point(px(PAD + 8. + 3.), px(TRACK_ROW * (ix as f32 + 0.5)));

        cx.simulate_click(on(1), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(picked(cx), [1], "a click picks the row");
        assert_eq!(ran(), "", "and plays nothing");

        second_click(cx, on(1));
        cx.run_until_parked();
        assert_eq!(
            ran(),
            "play track --from 2 -\nt0\nt1\nt2\n",
            "a double click plays the list from this row, in one call"
        );

        // Ctrl+click another row, then a right click on a picked one: the menu of both
        cx.simulate_click(on(2), Modifiers::control());
        cx.simulate_event(MouseDownEvent {
            position: on(2),
            modifiers: Modifiers::none(),
            button: MouseButton::Right,
            click_count: 1,
            first_mouse: false,
        });
        cx.run_until_parked();
        assert_eq!(picked(cx), [1, 2], "the selection is kept");
        let menu = cx.update(|_, cx| cx.global::<MenuHandle>().0.clone());
        let item = menu.read_with(cx, |m, _| m.target().map(|t| t.item.clone()));
        assert_eq!(
            item,
            Some(Item::Tracks {
                picks: vec![(1, "t1".into()), (2, "t2".into())],
                titles: vec!["Title 1".into(), "Title 2".into()],
            })
        );
    }

    /// A player holding the tracks `t0` to `t2`, at `current`.
    fn player_at(current: Option<usize>, state: PlayState) -> Player {
        Player {
            following: true,
            state,
            tracks: (0..3u64)
                .map(|i| QueueEntry {
                    entry: i + 1,
                    id: format!("t{i}"),
                    ..QueueEntry::default()
                })
                .collect(),
            current,
            ..Player::default()
        }
    }

    #[test]
    fn the_equaliser_is_in_the_playing_row_and_follows_the_player() {
        let dir = tempfile::tempdir().unwrap();
        let program = crate::app::fake_melodie(dir.path());
        let env = Env::with_home(dir.path().join("home"));
        let mut app = TestAppContext::single();
        let (view, cx): (Entity<Tracks>, _) = app.add_window_view(|_, cx| {
            let shared = crate::app::bare(env.clone(), program.clone(), cx);
            let catalog = cx.new(|cx| Catalog::new(env.clone(), shared, cx));
            let tracks: Arc<[Track]> = (0..3)
                .map(|i| Track {
                    id: format!("t{i}"),
                    title: format!("Title {i}"),
                    ..Track::default()
                })
                .collect();
            Tracks {
                catalog,
                shown: Shown::new(tracks, None),
                playing: Playing::default(),
            }
        });
        let marked = |cx: &mut VisualTestContext| {
            (0..3)
                .filter(|i| match i {
                    0 => cx.debug_bounds("t-equaliser-0").is_some(),
                    1 => cx.debug_bounds("t-equaliser-1").is_some(),
                    _ => cx.debug_bounds("t-equaliser-2").is_some(),
                })
                .collect::<Vec<_>>()
        };
        let follow = |cx: &mut VisualTestContext, p: Player| {
            view.update(cx, |v, cx| {
                v.playing.follow(&p);
                cx.notify();
            });
            cx.run_until_parked();
        };
        cx.run_until_parked();
        assert_eq!(marked(cx), Vec::<i32>::new(), "nothing plays");
        follow(cx, player_at(Some(1), PlayState::Playing));
        assert_eq!(marked(cx), [1], "the row of the current track");
        follow(cx, player_at(Some(1), PlayState::Paused));
        assert_eq!(marked(cx), [1], "still there when paused");
        follow(cx, player_at(Some(2), PlayState::Playing));
        assert_eq!(marked(cx), [2], "and on the next track at once");
        follow(cx, Player::default());
        assert_eq!(marked(cx), Vec::<i32>::new(), "gone with the player");
    }
}
