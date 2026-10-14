//! The home page and the pages of an album, an artist and a playlist.

use crate::albums::{GAP, MIN_CELL, PAD, album_cell, content_width, cover_box, empty_state};
use crate::app::Shared;
use crate::catalog::{Catalog, Empty, Page};
use crate::context_menu;
use crate::dialogs::{self, Dialog};
use crate::motion;
use crate::player::palette;
use crate::tables::{RowCtx, TRACK_ROW, row_hover, track_header, track_row, track_rows};
use crate::widgets::IconButton;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::library::Album;
use melodie_core::search::human_duration;
use melodie_desktop::browse::{self, Place};
use melodie_desktop::covers::Covers;
use melodie_desktop::figures::Figures as _;
use melodie_desktop::launch::{Adding, Job, Kind};
use melodie_desktop::playing::Playing;
use melodie_desktop::selection::Selection;
use melodie_desktop::table::{ColSort, Cols, Shown, Table};
use melodie_desktop::theme::Palette;
use std::sync::Arc;

fn section_title(text: &'static str, pal: Palette) -> Div {
    div()
        .px(px(PAD))
        .pt_6()
        .pb_3()
        .text_lg()
        .font_weight(FontWeight::BOLD)
        .text_color(pal.fg)
        .child(text)
}

/// How many of the tracks an artist appears on are drawn: the page stays light for one credited
/// on hundreds (Play, Shuffle and the mix still use them all).
const APPEARANCES_SHOWN: usize = 100;

/// The line under the appearances drawn, saying how many are left out.
fn and_more(total: usize) -> Option<String> {
    (total > APPEARANCES_SHOWN).then(|| format!("and {} more", total - APPEARANCES_SHOWN))
}

/// A quiet line under a page's header, for what it lacks.
fn quiet_line(text: impl Into<SharedString>, pal: Palette) -> Div {
    div()
        .px(px(PAD))
        .pt_6()
        .text_sm()
        .text_color(pal.secondary)
        .child(text.into())
}

/// Albums in rows of covers, at most `rows` rows.
fn album_rows(
    albums: &[Album],
    rows: usize,
    covers: &Covers,
    width: f32,
    pal: Palette,
    catalog: &Entity<Catalog>,
    playing: &Playing,
) -> Div {
    let cols = browse::columns(width, MIN_CELL, GAP);
    let side = ((width - GAP * (cols - 1) as f32) / cols as f32).floor();
    v_flex().px(px(PAD)).gap(px(GAP)).children(
        albums
            .chunks(cols)
            .take(rows)
            .map(|row| {
                h_flex().gap(px(GAP)).children(
                    row.iter()
                        .map(|a| album_cell(a, covers, side, pal, catalog, playing.album(&a.id))),
                )
            })
            .collect::<Vec<_>>(),
    )
}

pub struct PageView {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    tracks: UniformListScrollHandle,
    scroll: ScrollHandle,
    /// The place and the sort the lists were last shown with: another one starts from the top.
    shown: Option<(Place, Option<ColSort>)>,
}

impl PageView {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>) -> Self {
        Self {
            shared,
            catalog,
            tracks: UniformListScrollHandle::new(),
            scroll: ScrollHandle::new(),
            shown: None,
        }
    }
}

pub fn plural(n: i64, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// The header of a page: its picture, what it is, its name, a line about it, its actions.
fn header(
    picture: Div,
    kind: &'static str,
    name: String,
    about: AnyElement,
    actions: Div,
    pal: Palette,
) -> Div {
    h_flex()
        .px(px(PAD))
        .pt_2()
        .pb_4()
        .gap_6()
        .items_end()
        .child(picture.shadow_lg())
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(pal.secondary)
                        .child(kind),
                )
                .child(
                    div()
                        .text_3xl()
                        .font_weight(FontWeight::BOLD)
                        .text_color(pal.fg)
                        .truncate()
                        .child(name),
                )
                .child(div().text_sm().text_color(pal.secondary).child(about))
                .child(actions.pt_3()),
        )
}

/// What a page's header offers to change, besides playing.
enum Extra {
    None,
    /// An album: add it to a playlist.
    Album {
        name: String,
    },
    /// A playlist of the user's: rename, find its duplicates, delete.
    Playlist {
        name: String,
        tracks: usize,
    },
}

/// Play, shuffle, play next, add at the end, an instant mix; then what `extra` says. Without
/// anything to play (`playable` false), the buttons that play stay but cannot be pressed.
fn actions(
    kind: Kind,
    id: &str,
    (extra, playable): (Extra, bool),
    pal: Palette,
    catalog: &Entity<Catalog>,
) -> Div {
    let job = |make: fn(Kind, String) -> Job| {
        let (catalog, id) = (catalog.clone(), id.to_string());
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            catalog.update(cx, |c, cx| c.run(make(kind, id.clone()), cx))
        }
    };
    let open = |make: Box<dyn Fn() -> Dialog>| {
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| dialogs::open(make(), window, cx)
    };
    let id = id.to_string();
    h_flex()
        .gap_2()
        .flex_wrap()
        .child(
            IconButton::new("page-play", IconName::Play, pal)
                .label("Play")
                .primary()
                .diameter(38.)
                .disabled(!playable)
                .on_click(job(|kind, id| Job::Play {
                    kind,
                    id,
                    shuffle: false,
                })),
        )
        .child(
            IconButton::new("page-shuffle", IconName::Shuffle, pal)
                .label("Shuffle")
                .diameter(38.)
                .disabled(!playable)
                .on_click(job(|kind, id| Job::Play {
                    kind,
                    id,
                    shuffle: true,
                })),
        )
        .when(kind != Kind::Artist, |d| {
            d.child(
                IconButton::new("page-next", IconName::ListStart, pal)
                    .label("Play next")
                    .diameter(38.)
                    .on_click(job(|kind, id| Job::Queue {
                        kind,
                        id,
                        next: true,
                    })),
            )
            .child(
                IconButton::new("page-last", IconName::ListEnd, pal)
                    .label("Add to queue")
                    .diameter(38.)
                    .on_click(job(|kind, id| Job::Queue {
                        kind,
                        id,
                        next: false,
                    })),
            )
        })
        // `melodie mix` makes instant mixes from a track, an album or an artist
        .when(kind != Kind::Playlist, |d| {
            d.child(
                IconButton::new("page-mix", IconName::Sparkles, pal)
                    .label("Instant mix")
                    .diameter(38.)
                    .disabled(!playable)
                    .on_click(job(|kind, id| Job::Mix { kind, id })),
            )
        })
        .map(|d| match extra {
            Extra::None => d,
            Extra::Album { name } => d.child(
                IconButton::new("page-add", IconName::ListPlus, pal)
                    .label("Add to playlist")
                    .diameter(38.)
                    .on_click(open(Box::new(move || Dialog::Pick {
                        what: Adding::Album(id.clone()),
                        about: name.clone(),
                    }))),
            ),
            Extra::Playlist { name, tracks } => {
                let (id2, id3, name2, name3) = (id.clone(), id.clone(), name.clone(), name.clone());
                d.child(
                    IconButton::new("page-rename", IconName::Pencil, pal)
                        .label("Rename")
                        .diameter(38.)
                        .on_click(open(Box::new(move || Dialog::Rename {
                            id: id.clone(),
                            name: name.clone(),
                        }))),
                )
                .child(
                    IconButton::new("page-duplicates", IconName::CopyX, pal)
                        .label("Duplicates")
                        .diameter(38.)
                        .on_click(open(Box::new(move || Dialog::Duplicates {
                            id: id2.clone(),
                            name: name2.clone(),
                            found: None,
                        }))),
                )
                .child(
                    IconButton::new("page-delete", IconName::Trash, pal)
                        .label("Delete")
                        .diameter(38.)
                        .on_click(open(Box::new(move || Dialog::Delete {
                            id: id3.clone(),
                            name: name3.clone(),
                            tracks,
                        }))),
                )
            }
        })
}

/// The track table under a page's header, in a virtual list. Its rows can be picked; in a
/// playlist of the user's (`playlist`: its id and name) they can be removed and dragged.
#[allow(clippy::too_many_arguments)]
fn track_list(
    id: &'static str,
    shown: Shown,
    cols: Cols,
    pal: Palette,
    scroll: &UniformListScrollHandle,
    catalog: &Entity<Catalog>,
    (here, selection, playing): (Place, Selection, Playing),
    playlist: Option<(String, String)>,
) -> Div {
    let sort = {
        let (catalog, here) = (catalog.clone(), here.clone());
        move |s, cx: &mut App| catalog.update(cx, |c, cx| c.sort_tracks(here.clone(), s, cx))
    };
    let marks = playing.marks(&here, &shown.tracks);
    v_flex()
        .flex_1()
        .min_h_0()
        .child(track_header(
            &cols,
            playlist.is_some(),
            shown.sort,
            pal,
            sort,
        ))
        .child(
            track_rows(
                id,
                shown,
                Arc::new(cols),
                pal,
                catalog,
                (here, selection, marks),
                playlist,
            )
            .track_scroll(scroll)
            .flex_1(),
        )
}

impl Render for PageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (here, page, shown, covers, can_back, empty, selection, own) = {
            let c = self.catalog.read(cx);
            let here = c.here().clone();
            let page = c.pages.get(&here).cloned();
            let own = match page.as_deref() {
                Some(Page::Playlist { playlist, .. }) => c.is_own(playlist),
                _ => false,
            };
            (
                here.clone(),
                page,
                c.views.get(&here).cloned(),
                Covers::new(c.covers.clone(), window.scale_factor()),
                c.nav.can_go_back(),
                c.empty,
                c.selection.clone(),
                own,
            )
        };
        let now = (here.clone(), shown.as_ref().and_then(|s| s.sort));
        if self.shown.as_ref() != Some(&now) {
            self.tracks.scroll_to_item(0, ScrollStrategy::Top);
            self.shown = Some(now);
        }
        context_menu::list_is(&here, &self.tracks, TRACK_ROW, cx);
        let width = content_width(window, &self.shared, cx);
        let cols = |t: Table| t.cols(width);
        let shown = || {
            shown
                .clone()
                .unwrap_or_else(|| Shown::new(Arc::from([]), None))
        };
        let catalog = &self.catalog;
        let playing = self.shared.read(cx).playing.clone();
        let back = {
            let catalog = catalog.clone();
            h_flex()
                .px(px(PAD - 8.))
                .pt_3()
                .h(px(52.))
                .when(can_back, |d| {
                    d.child(
                        IconButton::new("page-back", IconName::ChevronLeft, pal)
                            .tip("Back")
                            .on_click(move |_, _, cx| catalog.update(cx, |c, cx| c.back(cx))),
                    )
                })
        };
        let cover = |id: &Option<String>, round: f32| {
            cover_box(
                id.as_ref().and_then(|c| covers.get(c, 180.)),
                180.,
                round,
                pal,
            )
        };
        let body: AnyElement = match page.as_deref() {
            None => empty_state(empty, "", pal).into_any_element(),
            Some(Page::Gone) => {
                empty_state(Empty::Loading, "Not in the library any more", pal).into_any_element()
            }
            Some(Page::Album { album, tracks }) => {
                let about = h_flex()
                    .gap_1()
                    .child(
                        div()
                            .id("page-artist")
                            .text_color(pal.fg)
                            .when_some(album.artist_id.clone(), |d, aid| {
                                let catalog = catalog.clone();
                                d.cursor_pointer().hover(|s| s.underline()).on_click(
                                    move |_, _, cx| {
                                        catalog.update(cx, |c, cx| {
                                            c.go(Place::Artist(aid.clone()), cx)
                                        })
                                    },
                                )
                            })
                            .child(album.artist.clone()),
                    )
                    .child(
                        [
                            (album.year > 0).then(|| album.year.to_string()),
                            Some(plural(tracks.len() as i64, "track")),
                            Some(human_duration(album.duration)),
                        ]
                        .into_iter()
                        .flatten()
                        .map(|s| format!(" · {s}"))
                        .collect::<String>(),
                    );
                v_flex()
                    .size_full()
                    .relative()
                    .child(header(
                        cover(&album.cover, 10.),
                        "ALBUM",
                        album.name.clone(),
                        about.into_any_element(),
                        actions(
                            Kind::Album,
                            &album.id,
                            (
                                Extra::Album {
                                    name: album.name.clone(),
                                },
                                true,
                            ),
                            pal,
                            catalog,
                        ),
                        pal,
                    ))
                    .child(track_list(
                        "album-tracks",
                        shown(),
                        cols(Table::album(&album.artist, tracks)),
                        pal,
                        &self.tracks,
                        catalog,
                        (here.clone(), selection.clone(), playing.clone()),
                        None,
                    ))
                    .into_any_element()
            }
            Some(Page::Playlist { playlist, tracks }) => {
                let about = [
                    playlist.owner.clone(),
                    Some(plural(tracks.len() as i64, "track")),
                    Some(human_duration(tracks.iter().map(|t| t.duration).sum())),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                v_flex()
                    .size_full()
                    .child(header(
                        cover(&playlist.cover, 10.),
                        "PLAYLIST",
                        playlist.name.clone(),
                        about.into_any_element(),
                        actions(
                            Kind::Playlist,
                            &playlist.id,
                            (
                                if own {
                                    Extra::Playlist {
                                        name: playlist.name.clone(),
                                        tracks: tracks.len(),
                                    }
                                } else {
                                    Extra::None
                                },
                                true,
                            ),
                            pal,
                            catalog,
                        ),
                        pal,
                    ))
                    .child(track_list(
                        "playlist-tracks",
                        shown(),
                        cols(Table::Playlist),
                        pal,
                        &self.tracks,
                        catalog,
                        (here.clone(), selection.clone(), playing.clone()),
                        own.then(|| (playlist.id.clone(), playlist.name.clone())),
                    ))
                    .into_any_element()
            }
            Some(
                artist_page @ Page::Artist {
                    artist,
                    albums,
                    top,
                    appears,
                },
            ) => v_flex()
                .id("artist-page")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .pb_6()
                .child(header(
                    cover(&artist.cover, 90.),
                    "ARTIST",
                    artist.name.clone(),
                    [
                        Some(plural(artist.albums, "album")),
                        Some(plural(artist.tracks, "track")),
                        (!appears.is_empty()).then(|| plural(appears.len() as i64, "appearance")),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ")
                    .into_any_element(),
                    actions(
                        Kind::Artist,
                        &artist.id,
                        (Extra::None, artist_page.playable()),
                        pal,
                        catalog,
                    ),
                    pal,
                ))
                .when(!top.is_empty(), |d| {
                    let (shown, cols) = (shown(), cols(Table::TopTracks));
                    let marks = playing.marks(&here, &shown.tracks);
                    let sort = {
                        let (catalog, here) = (catalog.clone(), here.clone());
                        move |s, cx: &mut App| {
                            catalog.update(cx, |c, cx| c.sort_tracks(here.clone(), s, cx))
                        }
                    };
                    d.child(section_title("Top tracks", pal))
                        .child(track_header(&cols, false, shown.sort, pal, sort))
                        .children((0..shown.len()).filter_map(|ix| {
                            let (_, t) = shown.row(ix)?;
                            let hover = row_hover("top-track", ix, window, cx);
                            let row = RowCtx::of(&selection, &here, &shown, ix, hover, &marks);
                            Some(track_row("top-track", ix, t, &cols, pal, catalog, row))
                        }))
                })
                .when(!albums.is_empty(), |d| {
                    d.child(section_title("Albums", pal)).child(album_rows(
                        albums,
                        usize::MAX,
                        &covers,
                        width,
                        pal,
                        catalog,
                        &playing,
                    ))
                })
                // their own list, picked apart from the top tracks; in the album order, unsorted
                .when(!appears.is_empty(), |d| {
                    let list = Place::Appearances(artist.id.clone());
                    let (shown, cols) = (Shown::new(appears.clone(), None), cols(Table::Tracks));
                    let marks = playing.marks(&list, &shown.tracks);
                    d.child(section_title("Appears on", pal))
                        .children((0..shown.len().min(APPEARANCES_SHOWN)).filter_map(|ix| {
                            let (_, t) = shown.row(ix)?;
                            let hover = row_hover("appears-track", ix, window, cx);
                            let row = RowCtx::of(&selection, &list, &shown, ix, hover, &marks);
                            Some(
                                track_row("appears-track", ix, t, &cols, pal, catalog, row)
                                    .debug_selector(|| format!("appears-track-{ix}")),
                            )
                        }))
                        .when_some(and_more(shown.len()), |d, more| {
                            d.child(
                                quiet_line(more, pal)
                                    .pt_3()
                                    .debug_selector(|| "appears-more".into()),
                            )
                        })
                })
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .child(back)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// What is done with the rows picked in a list: a bar that rises at the bottom while some are.
pub struct SelectionBar {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    /// What the bar said last, kept while it fades out.
    count: usize,
}

impl SelectionBar {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>) -> Self {
        Self {
            shared,
            catalog,
            count: 0,
        }
    }
}

impl Render for SelectionBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (count, several, playlist) = {
            let c = self.catalog.read(cx);
            let here = c.picking();
            let playlist = match c.pages.get(&here).map(Arc::as_ref) {
                Some(Page::Playlist { playlist, .. }) if c.is_own(playlist) => {
                    Some((playlist.id.clone(), playlist.name.clone()))
                }
                _ => None,
            };
            (
                c.selection.count(&here),
                c.selection.several(&here),
                playlist,
            )
        };
        // one row picked is what a plain click leaves: no bar for it
        let shows = several;
        let t = motion::flag("selection-bar", shows, motion::ENTER, window, cx);
        if shows {
            self.count = count;
        }
        if t == 0.0 {
            return div().into_any_element();
        }
        let pal = palette(&self.shared, window, cx);
        let n = self.count;
        // what the picked rows hold, read when a button is pressed
        let picked = {
            let catalog = self.catalog.clone();
            move |cx: &App| {
                let c = catalog.read(cx);
                // the list the rows were picked in: the search keeps its tables apart
                let Some(shown) = c.selection.shown(&c.picking()) else {
                    return (vec![], vec![]);
                };
                let picks = shown.picks(&c.selection, &c.picking());
                let titles = picks
                    .iter()
                    .map(|(rank, _)| shown.tracks[*rank].title.clone())
                    .collect::<Vec<_>>();
                (picks, titles)
            }
        };
        let add = {
            let picked = picked.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                let (picks, _) = picked(cx);
                let about = plural(picks.len() as i64, "track");
                let what = Adding::Tracks(picks.into_iter().map(|(_, id)| id).collect());
                dialogs::open(Dialog::Pick { what, about }, window, cx)
            }
        };
        let clear = self.catalog.clone();
        h_flex()
            .id("selection-bar")
            // a click on the bar is not a click on the row under it
            .occlude()
            .mb(px(8. * (1. - t)))
            .opacity(t)
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .rounded_full()
            .bg(pal.surface)
            .border_1()
            .border_color(Hsla { a: 0.12, ..pal.fg })
            .shadow_lg()
            .child(
                div()
                    .px_2()
                    .tabular()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(pal.fg)
                    .child(format!("{n} selected")),
            )
            .child(
                IconButton::new("picked-add", IconName::ListPlus, pal)
                    .label("Add to playlist")
                    .diameter(34.)
                    .on_click(add),
            )
            .when_some(playlist, |d, (id, name)| {
                d.child(
                    IconButton::new("picked-remove", IconName::ListMinus, pal)
                        .label("Remove")
                        .diameter(34.)
                        .on_click(move |_, window, cx| {
                            let (picks, titles) = picked(cx);
                            let dialog = Dialog::Remove {
                                id: id.clone(),
                                name: name.clone(),
                                picks,
                                titles,
                            };
                            dialogs::open(dialog, window, cx)
                        }),
                )
            })
            .child(
                IconButton::new("picked-clear", IconName::X, pal)
                    .tip("Unpick")
                    .diameter(34.)
                    .on_click(move |_, _, cx| clear.update(cx, |c, cx| c.unselect(cx))),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // not `super::*`: it would bring the `test` macro of GPUI along
    use super::{APPEARANCES_SHOWN, PageView, and_more};
    use crate::catalog::Catalog;
    use gpui_kit::{AppContext as _, TestAppContext, VisualTestContext};
    use melodie_core::db;
    use melodie_core::env::Env;
    use melodie_desktop::browse::Place;

    /// `ar9` featured on `featured` tracks of `ar1`; `ar8` credited on nothing.
    fn copy(env: &Env, featured: usize) {
        let conn = db::create(&db::db_path(env)).unwrap();
        conn.execute_batch(
            "INSERT INTO artists(id, name, albumCount) VALUES ('ar1', 'Ada Marlow', 1),
               ('ar9', 'Lior Castell', 0), ('ar8', 'Nova Reyes', 0);
             INSERT INTO albums(id, name, artistId) VALUES ('al1', 'Lanterns', 'ar1');",
        )
        .unwrap();
        for n in 0..featured {
            conn.execute(
                "INSERT INTO tracks(id, title, artistId, albumId, track)
                 VALUES (?1, ?1, 'ar1', 'al1', ?2)",
                (format!("t{n:03}"), n as i64),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO track_artists(track, artist) VALUES (?1, 'ar9')",
                [format!("t{n:03}")],
            )
            .unwrap();
        }
    }

    /// The page of the artist `id`, drawn without a screen: which of the elements the tests look
    /// for are there.
    fn drawn(featured: usize, id: &str, found: &[&'static str]) -> Vec<bool> {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        copy(&env, featured);
        let mut app = TestAppContext::single();
        let (shared, catalog) = app.update(|cx| {
            let shared = crate::app::for_tests(env.clone(), cx);
            let catalog = cx.new(|cx| Catalog::new(env.clone(), shared.clone(), cx));
            (shared, catalog)
        });
        catalog.update(&mut app, |c, cx| c.start_at(Place::Artist(id.into()), cx));
        app.run_until_parked();
        let (_, cx): (_, &mut VisualTestContext) =
            app.add_window_view(|_, _| PageView::new(shared.clone(), catalog.clone()));
        cx.run_until_parked();
        found
            .iter()
            .map(|selector| cx.debug_bounds(selector).is_some())
            .collect()
    }

    #[test]
    fn at_most_a_hundred_appearances_are_drawn_and_the_rest_is_counted() {
        assert_eq!(APPEARANCES_SHOWN, 100);
        let rows = ["appears-track-0", "appears-track-99", "appears-track-100"];
        let found = [rows.as_slice(), &["appears-more"]].concat();
        assert_eq!(
            drawn(APPEARANCES_SHOWN + 50, "ar9", &found),
            [true, true, false, true]
        );
        // a list within the cap says nothing more
        assert_eq!(
            drawn(APPEARANCES_SHOWN, "ar9", &found),
            [true, true, false, false]
        );
        assert_eq!(
            and_more(APPEARANCES_SHOWN + 50).as_deref(),
            Some("and 50 more")
        );
        assert_eq!(and_more(APPEARANCES_SHOWN), None);
    }
}
