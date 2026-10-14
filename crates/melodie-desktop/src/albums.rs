//! The album grid: every album as a cover, sortable; rows of covers in a virtual list, so that
//! only what is on screen is built. Also the cell the home and artist pages reuse.

use crate::app::Shared;
use crate::catalog::{Catalog, Empty};
use crate::context_menu::Menued as _;
use crate::motion;
use crate::player::palette;
use crate::sidebar;
use crate::widgets::{IconButton, Revealed as _, hover};
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::library::{Album, AlbumBy, Sort};
use melodie_desktop::browse::{self, Place};
use melodie_desktop::card::play_job;
use melodie_desktop::covers::{Cover, Covers, Use};
use melodie_desktop::launch::{Job, Kind};
use melodie_desktop::layout;
use melodie_desktop::menu::{Item, Spot};
use melodie_desktop::reveal;
use melodie_desktop::theme::Palette;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// The narrowest a cover gets before a column goes.
pub const MIN_CELL: f32 = 160.0;
pub const GAP: f32 = 20.0;
/// Room under a cover for its two lines of text.
const TEXT: f32 = 48.0;
/// The padding on each side of a view's content.
pub const PAD: f32 = 24.0;

/// The width of a view's content, between the sidebar and the window's edge or the queue. The
/// queue's settled place, not its animated one: the content is laid out once when it opens or
/// closes, not on every frame of the slide.
pub fn content_width(window: &Window, shared: &Entity<Shared>, cx: &App) -> f32 {
    let w = window.viewport_size().width.as_f32();
    layout::main_width(w, sidebar::width(w), shared.read(cx).queue_open) - 2.0 * PAD
}

/// A cover, or its place while there is none: the image comes from a file GPUI reads and decodes
/// in the background, over the placeholder. A cover that has just replaced a smaller one fades
/// in over it.
pub fn cover_box(cover: Option<Cover>, side: f32, round: f32, pal: Palette) -> Div {
    div()
        .size(px(side))
        .flex_none()
        .rounded(px(round))
        .overflow_hidden()
        .bg(Hsla { a: 0.07, ..pal.fg })
        .flex()
        .items_center()
        .justify_center()
        .map(|d| match cover {
            // GPUI does not clip children to rounded corners: the image is rounded itself
            Some(Cover { path, under: None }) => d.child(picture_file(path, round)),
            Some(Cover {
                path,
                under: Some(under),
            }) => {
                let id = ElementId::Name(format!("cover-fade-{}", path.display()).into());
                d.relative()
                    .child(picture_file(under, round).absolute().top_0().left_0())
                    .child(
                        div()
                            .size_full()
                            .absolute()
                            .top_0()
                            .left_0()
                            .child(picture_file(path, round))
                            .with_animation(id, motion::crossfade(), |el, t| el.opacity(t)),
                    )
            }
            None => d.child(
                Icon::new(IconName::Disc3)
                    .size(px((side * 0.3).round()))
                    .text_color(Hsla { a: 0.3, ..pal.fg }),
            ),
        })
}

fn picture_file(path: PathBuf, round: f32) -> Img {
    img(path)
        .size_full()
        .rounded(px(round))
        .object_fit(ObjectFit::Cover)
}

/// One album: its cover (a play button fades in over it on hover), its name and artist. A click
/// opens it.
pub fn album_cell(
    album: &Album,
    covers: &Covers,
    side: f32,
    pal: Palette,
    catalog: &Entity<Catalog>,
) -> AlbumCell {
    AlbumCell {
        id: album.id.clone(),
        name: album.name.clone(),
        artist: album.artist.clone(),
        item: Item::album(album),
        cover: album.cover.as_ref().and_then(|c| covers.get(c, side)),
        side,
        pal,
        catalog: catalog.clone(),
    }
}

/// A component and not a function of `Stateful<Div>`: it follows the pointer, which takes the
/// window, and its callers have none to hand over.
#[derive(IntoElement)]
pub struct AlbumCell {
    id: String,
    name: String,
    artist: String,
    item: Item,
    cover: Option<Cover>,
    side: f32,
    pal: Palette,
    catalog: Entity<Catalog>,
}

impl RenderOnce for AlbumCell {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            name,
            artist,
            item,
            cover,
            side,
            pal,
            catalog,
        } = self;
        let (open, play) = (catalog.clone(), catalog);
        let (open_id, play_id) = (id.clone(), id.clone());
        let hover = hover(
            ElementId::Name(format!("album-hover-{id}").into()),
            window,
            cx,
        );
        let look = hover.look;
        hover
            .track(v_flex().id(ElementId::Name(format!("album-{id}").into())))
            .menu(item, Spot::Library)
            .w(px(side))
            .h(px(side + TEXT))
            .flex_none()
            .gap_1()
            .cursor_pointer()
            .on_click(move |_, _, cx| {
                // the end of a drag of the home's rows is not a click
                if !cx.has_active_drag() {
                    open.update(cx, |c, cx| c.go(Place::Album(open_id.clone()), cx))
                }
            })
            .child(
                div()
                    .relative()
                    // the cover dims as the button comes
                    .child(cover_box(cover, side, 10.0, pal).opacity(look.under()))
                    .child(
                        div().absolute().right_2().bottom_2().child(
                            div().revealed(look, false).child(
                                IconButton::new(
                                    ElementId::Name(format!("album-play-{id}").into()),
                                    IconName::Play,
                                    pal,
                                )
                                .primary()
                                .tip("Play")
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    if cx.has_active_drag() {
                                        return;
                                    }
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
                    ),
            )
            .child(
                div()
                    .pt_1()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(pal.fg)
                    .truncate()
                    .child(name),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(pal.secondary)
                    .truncate()
                    .child(artist),
            )
    }
}

/// The round play button that fades in over the cover of an artist (`id` names it; place it with
/// `absolute`, it takes no room). It asks for what the "Play" of the right-click menu asks for.
pub fn artist_play(
    id: &str,
    item: &Item,
    diameter: f32,
    look: reveal::Look,
    pal: Palette,
    catalog: &Entity<Catalog>,
) -> Option<Div> {
    let job = play_job(item)?;
    let catalog = catalog.clone();
    Some(
        div().revealed(look, false).child(
            IconButton::new(ElementId::Name(id.to_string().into()), IconName::Play, pal)
                .primary()
                .diameter(diameter)
                .tip("Play")
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    if cx.has_active_drag() {
                        return;
                    }
                    catalog.update(cx, |c, cx| c.run(job.clone(), cx))
                }),
        ),
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
            Empty::NoCopy => "No local copy of the library yet",
            Empty::Copying => "Copying the library from Navidrome…",
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

/// The list, rows and columns shown, and the side of the covers: what an ask was made for.
type Shown = (usize, usize, usize, usize, u32);

pub struct AlbumGrid {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    scroll: UniformListScrollHandle,
    /// The sort the list was last shown in: another one starts from the top.
    shown_sort: Option<Sort<AlbumBy>>,
    /// What the covers of the rows shown were last asked for, so that a frame that shows the same
    /// rows asks nothing.
    asked: Rc<Cell<Option<Shown>>>,
}

impl AlbumGrid {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>) -> Self {
        Self {
            shared,
            catalog,
            scroll: UniformListScrollHandle::new(),
            shown_sort: None,
            asked: Rc::default(),
        }
    }
}

impl Render for AlbumGrid {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (albums, covers, sort, empty): (Option<Arc<[Album]>>, _, _, _) = {
            let c = self.catalog.read(cx);
            (
                c.albums.clone(),
                Covers::new(c.covers.clone(), window.scale_factor()),
                c.album_sort,
                c.empty,
            )
        };
        if albums.is_some() && self.shown_sort != Some(sort) {
            if self.shown_sort.is_some() {
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            self.shown_sort = Some(sort);
        }
        let width = content_width(window, &self.shared, cx);
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
                    (AlbumBy::Plays, "Most played", true),
                    (AlbumBy::Played, "Recently played", true),
                ],
                pal,
                move |s, cx| catalog.update(cx, |c, cx| c.sort_albums(s, cx)),
            ));
        let body = match albums {
            None => empty_state(empty, "", pal).into_any_element(),
            Some(list) if list.is_empty() => {
                // `Copying` while the first copy has not reached the albums yet
                let why = if empty == Empty::Copying {
                    empty
                } else {
                    Empty::Loading
                };
                empty_state(why, "No albums", pal).into_any_element()
            }
            Some(list) => {
                let rows = list.len().div_ceil(cols);
                let catalog = self.catalog.clone();
                let asked = self.asked.clone();
                uniform_list("album-rows", rows, move |range, _, cx| {
                    // the library came at thumbnail size; the rows shown want their own, before
                    // anything that waits. Asked later: nothing here reads a file.
                    let key = (
                        Arc::as_ptr(&list) as *const Album as usize,
                        range.start,
                        range.end,
                        cols,
                        side as u32,
                    );
                    if asked.replace(Some(key)) != Some(key) {
                        let ids = browse::row_covers(&list, range.clone(), cols);
                        let catalog = catalog.clone();
                        cx.defer(move |cx| {
                            catalog.update(cx, |c, cx| c.want_covers(ids, Use::of(side), true, cx))
                        });
                    }
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
