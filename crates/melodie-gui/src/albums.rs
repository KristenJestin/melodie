//! The album grid: every album as a cover, sortable; rows of covers in a virtual list, so that
//! only what is on screen is built. Also the cell the home and artist pages reuse.

use crate::app::Shared;
use crate::catalog::{Catalog, Empty};
use crate::player::palette;
use crate::sidebar;
use crate::widgets::IconButton;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::library::{Album, AlbumBy, Sort};
use melodie_gui::browse::{self, Place};
use melodie_gui::launch::{Job, Kind};
use melodie_gui::theme::Palette;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The narrowest a cover gets before a column goes.
pub const MIN_CELL: f32 = 160.0;
pub const GAP: f32 = 20.0;
/// Room under a cover for its two lines of text.
const TEXT: f32 = 48.0;
/// The padding on each side of a view's content.
pub const PAD: f32 = 24.0;

/// The width of a view's content, between the sidebar and the window's edge.
pub fn content_width(window: &Window) -> f32 {
    let w = window.viewport_size().width.as_f32();
    w - sidebar::width(w) - 2.0 * PAD
}

/// A cover, or its place while there is none: the image comes from a file GPUI reads and decodes
/// in the background, over the placeholder.
pub fn cover_box(path: Option<&PathBuf>, side: f32, round: f32, pal: Palette) -> Div {
    div()
        .size(px(side))
        .flex_none()
        .rounded(px(round))
        .overflow_hidden()
        .bg(Hsla { a: 0.07, ..pal.fg })
        .flex()
        .items_center()
        .justify_center()
        .map(|d| match path {
            // GPUI does not clip children to rounded corners: the image is rounded itself
            Some(p) => d.child(
                img(p.clone())
                    .size_full()
                    .rounded(px(round))
                    .object_fit(ObjectFit::Cover),
            ),
            None => d.child(
                Icon::new(IconName::Disc3)
                    .size(px((side * 0.3).round()))
                    .text_color(Hsla { a: 0.3, ..pal.fg }),
            ),
        })
}

/// One album: its cover (a play button over it on hover), its name and artist. A click opens it.
pub fn album_cell(
    album: &Album,
    covers: &HashMap<String, PathBuf>,
    side: f32,
    pal: Palette,
    catalog: &Entity<Catalog>,
) -> Stateful<Div> {
    let (open, play) = (catalog.clone(), catalog.clone());
    let id = album.id.clone();
    let play_id = album.id.clone();
    v_flex()
        .id(ElementId::Name(format!("album-{}", album.id).into()))
        .group("album-cell")
        .w(px(side))
        .h(px(side + TEXT))
        .flex_none()
        .gap_1()
        .cursor_pointer()
        .on_click(move |_, _, cx| open.update(cx, |c, cx| c.go(Place::Album(id.clone()), cx)))
        .child(
            div()
                .relative()
                .child(
                    cover_box(
                        album.cover.as_ref().and_then(|c| covers.get(c)),
                        side,
                        10.0,
                        pal,
                    )
                    .group_hover("album-cell", |s| s.opacity(0.85)),
                )
                .child(
                    div()
                        .absolute()
                        .right_2()
                        .bottom_2()
                        .invisible()
                        .group_hover("album-cell", |s| s.visible())
                        .child(
                            IconButton::new(
                                ElementId::Name(format!("album-play-{}", album.id).into()),
                                IconName::Play,
                                pal,
                            )
                            .primary()
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                play.update(cx, |c, cx| {
                                    c.run(
                                        Job::Play {
                                            kind: Kind::Album,
                                            id: play_id.clone(),
                                            shuffle: false,
                                        },
                                        cx,
                                    )
                                })
                            }),
                        ),
                ),
        )
        .child(
            div()
                .pt_1()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(pal.fg)
                .truncate()
                .child(album.name.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(pal.secondary)
                .truncate()
                .child(album.artist.clone()),
        )
}

/// A centred line saying why a view is empty.
pub fn empty_state(why: Empty, nothing: &'static str, pal: Palette) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(pal.secondary)
        .child(match why {
            Empty::Loading => "",
            Empty::NoCopy => "No local copy of the library yet: run melodie sync",
            Empty::Failed => "The local copy could not be read",
        })
        .when(why == Empty::Loading, |d| d.child(nothing))
}

/// The sort choices above a list, as chips; a click on the current one flips its direction.
pub fn sort_chips<C: Copy + PartialEq + 'static>(
    id: &'static str,
    current: Sort<C>,
    choices: &[(C, &'static str, bool)],
    pal: Palette,
    on_pick: impl Fn(Sort<C>, &mut App) + Clone + 'static,
) -> Div {
    h_flex()
        .gap_1()
        .children(choices.iter().map(|&(by, label, natural_desc)| {
            let on = current.by == by;
            let pick = on_pick.clone();
            h_flex()
                .id(ElementId::Name(format!("{id}-{label}").into()))
                .h(px(30.))
                .px_3()
                .gap_1()
                .items_center()
                .rounded_full()
                .cursor_pointer()
                .text_sm()
                .text_color(if on { pal.fg } else { pal.secondary })
                .bg(Hsla {
                    a: if on { 0.12 } else { 0.0 },
                    ..pal.fg
                })
                .hover(|s| s.bg(Hsla { a: 0.08, ..pal.fg }))
                .on_click(move |_, _, cx| pick(current.clicked(by, natural_desc), cx))
                .child(label)
                .when(on, |d| {
                    d.child(
                        Icon::new(if current.descending {
                            IconName::ArrowDown
                        } else {
                            IconName::ArrowUp
                        })
                        .size(px(14.)),
                    )
                })
        }))
}

pub struct AlbumGrid {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    scroll: UniformListScrollHandle,
    /// The sort the list was last shown in: another one starts from the top.
    shown_sort: Option<Sort<AlbumBy>>,
}

impl AlbumGrid {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>) -> Self {
        Self {
            shared,
            catalog,
            scroll: UniformListScrollHandle::new(),
            shown_sort: None,
        }
    }
}

impl Render for AlbumGrid {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (albums, covers, sort, empty): (Option<Arc<[Album]>>, _, _, _) = {
            let c = self.catalog.read(cx);
            (c.albums.clone(), c.covers.clone(), c.album_sort, c.empty)
        };
        if albums.is_some() && self.shown_sort != Some(sort) {
            if self.shown_sort.is_some() {
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            self.shown_sort = Some(sort);
        }
        let width = content_width(window);
        let cols = browse::columns(width, MIN_CELL, GAP);
        let side = ((width - GAP * (cols - 1) as f32) / cols as f32).floor();
        let catalog = self.catalog.clone();
        let header = h_flex()
            .px(px(PAD))
            .pt_5()
            .pb_3()
            .gap_4()
            .items_center()
            .flex_wrap()
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(pal.fg)
                    .child("Albums"),
            )
            .child(div().flex_1())
            .child(sort_chips(
                "album-sort",
                sort,
                &[
                    (AlbumBy::Added, "Recently added", true),
                    (AlbumBy::Name, "Name", false),
                    (AlbumBy::Artist, "Artist", false),
                    (AlbumBy::Year, "Year", true),
                ],
                pal,
                move |s, cx| catalog.update(cx, |c, cx| c.sort_albums(s, cx)),
            ));
        let body = match albums {
            None => empty_state(empty, "", pal).into_any_element(),
            Some(list) if list.is_empty() => {
                empty_state(Empty::Loading, "No albums", pal).into_any_element()
            }
            Some(list) => {
                let rows = list.len().div_ceil(cols);
                let catalog = self.catalog.clone();
                uniform_list("album-rows", rows, move |range, _, _| {
                    range
                        .map(|row| {
                            let start = row * cols;
                            let end = (start + cols).min(list.len());
                            h_flex()
                                .w_full()
                                .px(px(PAD))
                                .pb(px(GAP))
                                .gap(px(GAP))
                                .children(
                                    list[start..end]
                                        .iter()
                                        .map(|a| album_cell(a, &covers, side, pal, &catalog)),
                                )
                        })
                        .collect()
                })
                .track_scroll(&self.scroll)
                .flex_1()
                .into_any_element()
            }
        };
        v_flex().size_full().child(header).child(body)
    }
}
