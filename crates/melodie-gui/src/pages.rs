//! The home page and the pages of an album, an artist and a playlist.

use crate::albums::{GAP, MIN_CELL, PAD, album_cell, content_width, cover_box, empty_state};
use crate::app::Shared;
use crate::catalog::{Catalog, Empty, Page};
use crate::motion;
use crate::player::palette;
use crate::tables::{Cols, track_header, track_row};
use crate::widgets::IconButton;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::library::{Album, Track};
use melodie_core::search::human_duration;
use melodie_gui::browse::{self, Place};
use melodie_gui::launch::{Job, Kind};
use melodie_gui::theme::Palette;
use std::collections::HashMap;
use std::path::PathBuf;
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

/// Albums in rows of covers, at most `rows` rows.
fn album_rows(
    albums: &[Album],
    rows: usize,
    covers: &HashMap<String, PathBuf>,
    width: f32,
    pal: Palette,
    catalog: &Entity<Catalog>,
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
                        .map(|a| album_cell(a, covers, side, pal, catalog)),
                )
            })
            .collect::<Vec<_>>(),
    )
}

pub struct HomeView {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    scroll: ScrollHandle,
}

impl HomeView {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>) -> Self {
        Self {
            shared,
            catalog,
            scroll: ScrollHandle::new(),
        }
    }
}

/// A mix to start: a large card.
fn mix_card(
    id: &'static str,
    icon: IconName,
    name: &'static str,
    about: &'static str,
    job: Job,
    pal: Palette,
    catalog: &Entity<Catalog>,
) -> Stateful<Div> {
    let catalog = catalog.clone();
    h_flex()
        .id(id)
        .flex_1()
        .min_w(px(220.))
        .h(px(88.))
        .px_5()
        .gap_4()
        .items_center()
        .rounded_xl()
        .cursor_pointer()
        .bg(Hsla {
            a: 0.14,
            ..pal.accent
        })
        .hover(|s| {
            s.bg(Hsla {
                a: 0.22,
                ..pal.accent
            })
        })
        .on_click(move |_, _, cx| catalog.update(cx, |c, cx| c.run(job.clone(), cx)))
        .child(
            div()
                .size(px(44.))
                .flex_none()
                .rounded_full()
                .bg(pal.accent)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).size(px(20.)).text_color(pal.on_accent)),
        )
        .child(
            v_flex()
                .min_w_0()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(pal.fg)
                        .child(name),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(pal.secondary)
                        .truncate()
                        .child(about),
                ),
        )
}

impl Render for HomeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (home, covers, empty) = {
            let c = self.catalog.read(cx);
            (c.home.clone(), c.covers.clone(), c.empty)
        };
        let width = content_width(window);
        let catalog = &self.catalog;
        let Some(home) = home else {
            return div().size_full().child(empty_state(empty, "", pal));
        };
        div().size_full().child(
            v_flex()
                .id("home")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .pb_6()
                .child(section_title("Recently added", pal))
                .map(|d| {
                    if home.recent.is_empty() {
                        d.child(
                            div()
                                .px(px(PAD))
                                .text_color(pal.secondary)
                                .child("No albums yet"),
                        )
                    } else {
                        d.child(album_rows(&home.recent, 2, &covers, width, pal, catalog))
                    }
                })
                .child(section_title("Mixes", pal))
                .child(
                    h_flex()
                        .px(px(PAD))
                        .gap_4()
                        .flex_wrap()
                        .child(mix_card(
                            "mix-me",
                            IconName::Sparkles,
                            "Your mix",
                            "Favourites, familiar tracks and discoveries",
                            Job::PlayMix,
                            pal,
                            catalog,
                        ))
                        .child(mix_card(
                            "mix-rediscover",
                            IconName::RotateCcw,
                            "Rediscover",
                            "Albums you played, not lately",
                            Job::Rediscover,
                            pal,
                            catalog,
                        )),
                )
                .when(!home.most_played.is_empty(), |d| {
                    d.child(section_title("Most played", pal)).child(album_rows(
                        &home.most_played,
                        2,
                        &covers,
                        width,
                        pal,
                        catalog,
                    ))
                }),
        )
    }
}

pub struct PageView {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    tracks: UniformListScrollHandle,
    scroll: ScrollHandle,
    /// The place the lists were last shown for: another one starts from the top.
    shown: Option<Place>,
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

fn plural(n: i64, one: &str) -> String {
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

/// Play, shuffle, play next, add at the end; and, for an album, add to a playlist.
fn actions(kind: Kind, id: &str, picker: bool, pal: Palette, catalog: &Entity<Catalog>) -> Div {
    let job = |make: fn(Kind, String) -> Job| {
        let (catalog, id) = (catalog.clone(), id.to_string());
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            catalog.update(cx, |c, cx| c.run(make(kind, id.clone()), cx))
        }
    };
    let pick = catalog.clone();
    h_flex()
        .gap_2()
        .flex_wrap()
        .child(
            IconButton::new("page-play", IconName::Play, pal)
                .label("Play")
                .primary()
                .diameter(38.)
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
                .on_click(job(|kind, id| Job::Play {
                    kind,
                    id,
                    shuffle: true,
                })),
        )
        .when(kind == Kind::Artist, |d| {
            d.child(
                IconButton::new("page-mix", IconName::Sparkles, pal)
                    .label("Instant mix")
                    .diameter(38.)
                    .on_click(job(|kind, id| Job::Mix { kind, id })),
            )
        })
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
        .when(kind == Kind::Album, |d| {
            d.child(
                IconButton::new("page-add", IconName::ListPlus, pal)
                    .label("Add to playlist")
                    .diameter(38.)
                    .selected(picker)
                    .on_click(move |_, _, cx| {
                        pick.update(cx, |c, cx| {
                            c.picker_open = !c.picker_open;
                            cx.notify()
                        })
                    }),
            )
        })
}

/// The user's playlists, to add the album to.
fn picker(album: &str, t: f32, pal: Palette, catalog: &Entity<Catalog>, cx: &App) -> Div {
    let lists = catalog.read(cx).own_playlists();
    div()
        .absolute()
        .top(px(8. - 8. * (1. - t)))
        .left(px(PAD))
        .opacity(t)
        .child(
            v_flex()
                .id("playlist-picker")
                .w(px(280.))
                .max_h(px(320.))
                .overflow_y_scroll()
                .p_2()
                .gap_1()
                .rounded_xl()
                .bg(pal.surface)
                .border_1()
                .border_color(Hsla { a: 0.10, ..pal.fg })
                .shadow_xl()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(pal.secondary)
                        .child("ADD TO PLAYLIST"),
                )
                .when(lists.is_empty(), |d| {
                    d.child(
                        div()
                            .px_2()
                            .py_2()
                            .text_sm()
                            .text_color(pal.secondary)
                            .child("You have no playlist yet"),
                    )
                })
                .children(lists.into_iter().map(|p| {
                    let (catalog, album, pid) = (catalog.clone(), album.to_string(), p.id.clone());
                    h_flex()
                        .id(ElementId::Name(format!("pick-{}", p.id).into()))
                        .px_2()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .rounded_lg()
                        .cursor_pointer()
                        .text_sm()
                        .text_color(pal.fg)
                        .hover(|s| s.bg(Hsla { a: 0.08, ..pal.fg }))
                        .on_click(move |_, _, cx| {
                            catalog.update(cx, |c, cx| {
                                c.picker_open = false;
                                c.run(
                                    Job::AddToPlaylist {
                                        playlist: pid.clone(),
                                        kind: Kind::Album,
                                        id: album.clone(),
                                    },
                                    cx,
                                );
                                cx.notify()
                            })
                        })
                        .child(
                            Icon::new(IconName::ListMusic)
                                .size(px(16.))
                                .text_color(pal.secondary),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(p.name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(pal.secondary)
                                .child(p.songs.to_string()),
                        )
                })),
        )
}

/// A list of tracks under a page's header, in a virtual list.
fn track_list(
    id: &'static str,
    tracks: Arc<[Track]>,
    cols: Cols,
    pal: Palette,
    scroll: &UniformListScrollHandle,
    catalog: &Entity<Catalog>,
) -> Div {
    let catalog = catalog.clone();
    v_flex()
        .flex_1()
        .min_h_0()
        .child(track_header(cols, None, pal, |_, _| {}))
        .child(
            uniform_list(id, tracks.len(), move |range, _, _| {
                range
                    .map(|ix| track_row(id, ix, &tracks[ix], cols, pal, &catalog))
                    .collect()
            })
            .track_scroll(scroll)
            .flex_1(),
        )
}

impl Render for PageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (here, page, covers, can_back, picker_open, empty) = {
            let c = self.catalog.read(cx);
            let here = c.here().clone();
            (
                here.clone(),
                c.pages.get(&here).cloned(),
                c.covers.clone(),
                c.nav.can_go_back(),
                c.picker_open,
                c.empty,
            )
        };
        if self.shown.as_ref() != Some(&here) {
            self.tracks.scroll_to_item(0, ScrollStrategy::Top);
            self.shown = Some(here.clone());
        }
        let width = content_width(window);
        let cols = |c: Cols| c.fit(width);
        let catalog = &self.catalog;
        let pk = motion::flag("picker", picker_open, motion::PANEL, window, cx);
        let back = {
            let catalog = catalog.clone();
            h_flex()
                .px(px(PAD - 8.))
                .pt_3()
                .h(px(52.))
                .when(can_back, |d| {
                    d.child(
                        IconButton::new("page-back", IconName::ChevronLeft, pal)
                            .on_click(move |_, _, cx| catalog.update(cx, |c, cx| c.back(cx))),
                    )
                })
        };
        let cover = |id: &Option<String>, round: f32| {
            cover_box(id.as_ref().and_then(|c| covers.get(c)), 180., round, pal)
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
                        actions(Kind::Album, &album.id, picker_open, pal, catalog),
                        pal,
                    ))
                    .child(track_list(
                        "album-tracks",
                        tracks.clone(),
                        cols(Cols {
                            number: true,
                            artist: tracks.iter().any(|t| t.artist != album.artist),
                            album: false,
                            year: false,
                            plays: true,
                        }),
                        pal,
                        &self.tracks,
                        catalog,
                    ))
                    .when(pk > 0.0, |d| {
                        let close = catalog.clone();
                        d.child(
                            div()
                                .id("picker-scrim")
                                .absolute()
                                .inset_0()
                                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                    close.update(cx, |c, cx| {
                                        c.picker_open = false;
                                        cx.notify()
                                    })
                                })
                                .child(
                                    div()
                                        .absolute()
                                        .top(px(240.))
                                        .left_0()
                                        .child(picker(&album.id, pk, pal, catalog, cx)),
                                ),
                        )
                    })
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
                        actions(Kind::Playlist, &playlist.id, false, pal, catalog),
                        pal,
                    ))
                    .child(track_list(
                        "playlist-tracks",
                        tracks.clone(),
                        cols(Cols {
                            number: true,
                            artist: true,
                            album: true,
                            year: false,
                            plays: false,
                        }),
                        pal,
                        &self.tracks,
                        catalog,
                    ))
                    .into_any_element()
            }
            Some(Page::Artist {
                artist,
                albums,
                top,
            }) => v_flex()
                .id("artist-page")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .pb_6()
                .child(header(
                    cover(&artist.cover, 90.),
                    "ARTIST",
                    artist.name.clone(),
                    format!(
                        "{} · {}",
                        plural(artist.albums, "album"),
                        plural(artist.tracks, "track")
                    )
                    .into_any_element(),
                    actions(Kind::Artist, &artist.id, false, pal, catalog),
                    pal,
                ))
                .when(!top.is_empty(), |d| {
                    d.child(section_title("Top tracks", pal))
                        .children(top.iter().enumerate().map(|(ix, t)| {
                            track_row(
                                "top-track",
                                ix,
                                t,
                                cols(Cols {
                                    number: true,
                                    artist: false,
                                    album: true,
                                    year: false,
                                    plays: true,
                                }),
                                pal,
                                catalog,
                            )
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
                    ))
                })
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .child(back)
            .child(div().flex_1().min_h_0().child(body))
    }
}
