//! The home page: a greeting, a large card for something worth playing now (tinted by its cover),
//! then rows that scroll sideways: albums and playlists as squares, artists as circles, mixes as
//! coloured cards. What it shows is decided in `melodie_desktop::home`.

use crate::albums::{GAP, PAD, album_cell, content_width, cover_box, empty_state};
use crate::app::Shared;
use crate::catalog::Catalog;
use crate::context_menu::Menued as _;
use crate::motion;
use crate::pages::plural;
use crate::player::palette;
use crate::widgets::{IconButton, hover};
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::library::{Album, Artist, Playlist};
use melodie_desktop::browse::Place;
use melodie_desktop::covers::{Cover, Covers};
use melodie_desktop::home::{self, Highlight, Kind, Mix, Row, SeeAll, Why};
use melodie_desktop::launch::{Job, Kind as Of};
use melodie_desktop::menu::{Item, Spot};
use melodie_desktop::theme::{Palette, Tint, mix};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

/// The highlight card's height, and its cover's side.
const CARD: f32 = 260.0;
const CARD_COVER: f32 = 212.0;
/// The side of an album or a playlist in a row, of an artist's circle.
pub const SQUARE: f32 = 168.0;
pub const CIRCLE: f32 = 148.0;
/// A mix card.
const MIX_W: f32 = 232.0;
const MIX_H: f32 = 148.0;

pub struct HomeView {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    scroll: ScrollHandle,
    /// Each row a view of its own: a drag rebuilds its row alone, and scrolling the page replays
    /// the rows without building them again. Each keeps where it was scrolled to.
    rows: HashMap<Kind, Entity<RowView>>,
    /// A view of its own: its animation rebuilds it alone, not the rows.
    highlight: Entity<HighlightView>,
}

impl HomeView {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>, cx: &mut Context<Self>) -> Self {
        let highlight = cx.new(|_| HighlightView {
            shared: shared.clone(),
            catalog: catalog.clone(),
            pick: 0,
            turns: 0,
        });
        Self {
            shared,
            catalog,
            scroll: ScrollHandle::new(),
            rows: HashMap::new(),
            highlight,
        }
    }
}

/// The highlight card, browsed with its arrows and dots.
pub struct HighlightView {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    /// The highlight shown, and how many times it changed (its content fades in at each).
    pick: usize,
    turns: u64,
}

impl HighlightView {
    fn turn(&mut self, to: usize, cx: &mut Context<Self>) {
        if to != self.pick {
            self.pick = to;
            self.turns += 1;
            cx.notify();
        }
    }
}

/// The hour of the day where the user is.
fn local_hour() -> u32 {
    // SAFETY: `time` with a null pointer only returns the time; `localtime_r` only writes `tm`.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return 12;
        }
        tm.tm_hour as u32
    }
}

/// A row dragged sideways with the pointer: the last x it was seen at.
#[derive(Clone)]
struct RowDrag {
    row: Kind,
    handle: ScrollHandle,
    last: Rc<Cell<Option<Pixels>>>,
}

/// What follows the pointer during that drag: nothing, the row itself moves.
struct Nothing;

impl Render for Nothing {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A palette in the colours of `hue`, for a card.
fn card_palette(hue: f32, dark: bool) -> Palette {
    Palette::base(
        Some(Tint {
            hue,
            strength: 0.85,
        }),
        dark,
    )
}

/// A hue of its own for each mix, the same every time.
fn mix_hue(m: &Mix, pal: Palette) -> f32 {
    match m {
        Mix::Mine => pal.accent.h,
        Mix::Rediscover => (pal.accent.h + 0.5).rem_euclid(1.0),
        Mix::Genre { name, .. } => {
            let h = name.bytes().fold(2166136261u32, |h, b| {
                (h ^ u32::from(b)).wrapping_mul(16777619)
            });
            (h % 360) as f32 / 360.0
        }
    }
}

fn mix_text(m: &Mix) -> (IconName, String, String) {
    match m {
        Mix::Mine => (
            IconName::Sparkles,
            "Your mix".into(),
            "Favourites, familiar tracks and discoveries".into(),
        ),
        Mix::Rediscover => (
            IconName::RotateCcw,
            "Rediscover".into(),
            "Albums you played, not lately".into(),
        ),
        Mix::Genre { name, tracks } => (
            IconName::Tag,
            format!("{name} mix"),
            plural(*tracks as i64, "track"),
        ),
    }
}

/// A picture, or while there is none a soft gradient of the palette with `icon` on it.
fn picture(cover: Option<Cover>, side: f32, round: f32, icon: IconName, pal: Palette) -> Div {
    match cover {
        Some(_) => cover_box(cover, side, round, pal),
        None => div()
            .size(px(side))
            .flex_none()
            .rounded(px(round))
            .bg(linear_gradient(
                135.,
                linear_color_stop(mix(pal.surface, pal.accent, 0.22), 0.),
                linear_color_stop(pal.surface, 1.),
            ))
            .flex()
            .items_center()
            .justify_center()
            .child(
                Icon::new(icon)
                    .size(px((side * 0.3).round()))
                    .text_color(Hsla { a: 0.45, ..pal.fg }),
            ),
    }
}

/// A click on a card, unless it ends a drag of its row.
fn on_card(
    f: impl Fn(&mut App) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, _, cx| {
        if !cx.has_active_drag() {
            f(cx)
        }
    }
}

fn artist_cell(
    a: &Artist,
    covers: &Covers,
    pal: Palette,
    catalog: &Entity<Catalog>,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let (catalog, id) = (catalog.clone(), a.id.clone());
    let hover = hover(
        ElementId::Name(format!("home-artist-hover-{}", a.id).into()),
        window,
        cx,
    );
    let look = hover.look;
    hover
        .track(v_flex().id(ElementId::Name(format!("home-artist-{}", a.id).into())))
        .menu(Item::artist(a), Spot::Library)
        .w(px(CIRCLE))
        .flex_none()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .on_click(on_card(move |cx| {
            catalog.update(cx, |c, cx| c.go(Place::Artist(id.clone()), cx))
        }))
        .child(
            picture(
                a.cover.as_ref().and_then(|c| covers.get(c, CIRCLE)),
                CIRCLE,
                CIRCLE / 2.0,
                IconName::MicVocal,
                pal,
            )
            .opacity(look.under()),
        )
        .child(
            div()
                .pt_1()
                .w_full()
                .text_center()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(pal.fg)
                .truncate()
                .child(a.name.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(pal.secondary)
                .child(plural(a.albums, "album")),
        )
}

fn playlist_cell(
    p: &Playlist,
    covers: &Covers,
    pal: Palette,
    catalog: &Entity<Catalog>,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let (catalog, id) = (catalog.clone(), p.id.clone());
    let hover = hover(
        ElementId::Name(format!("home-playlist-hover-{}", p.id).into()),
        window,
        cx,
    );
    let look = hover.look;
    hover
        .track(v_flex().id(ElementId::Name(format!("home-playlist-{}", p.id).into())))
        .menu(Item::playlist(p), Spot::Library)
        .w(px(SQUARE))
        .flex_none()
        .gap_1()
        .cursor_pointer()
        .on_click(on_card(move |cx| {
            catalog.update(cx, |c, cx| c.go(Place::Playlist(id.clone()), cx))
        }))
        .child(
            picture(
                p.cover.as_ref().and_then(|c| covers.get(c, SQUARE)),
                SQUARE,
                10.0,
                IconName::ListMusic,
                pal,
            )
            .opacity(look.under()),
        )
        .child(
            div()
                .pt_1()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(pal.fg)
                .truncate()
                .child(p.name.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(pal.secondary)
                .child(plural(p.songs, "track")),
        )
}

/// A mix: a card in colours of its own.
fn mix_cell(m: &Mix, pal: Palette, catalog: &Entity<Catalog>) -> Stateful<Div> {
    let cp = card_palette(mix_hue(m, pal), pal.dark);
    let (icon, name, about) = mix_text(m);
    let (catalog, job) = (catalog.clone(), m.job());
    let id = match m {
        Mix::Mine => "home-mix-mine".to_string(),
        Mix::Rediscover => "home-mix-rediscover".to_string(),
        Mix::Genre { name, .. } => format!("home-mix-genre-{name}"),
    };
    v_flex()
        .id(ElementId::Name(id.into()))
        .menu(Item::Mix(m.clone()), Spot::Library)
        .w(px(MIX_W))
        .h(px(MIX_H))
        .flex_none()
        .p_4()
        .justify_between()
        .rounded_xl()
        .cursor_pointer()
        .bg(linear_gradient(
            135.,
            linear_color_stop(mix(cp.surface, cp.accent, 0.5), 0.),
            linear_color_stop(cp.surface, 1.),
        ))
        .hover(|s| s.opacity(0.88))
        .on_click(on_card(move |cx| {
            catalog.update(cx, |c, cx| c.run(job.clone(), cx))
        }))
        .child(
            div()
                .size(px(40.))
                .rounded_full()
                .bg(cp.accent)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).size(px(18.)).text_color(cp.on_accent)),
        )
        .child(
            v_flex()
                .min_w_0()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cp.fg)
                        .truncate()
                        .child(name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cp.secondary)
                        .truncate()
                        .child(about),
                ),
        )
}

/// A section's title, and its "see all" when it has a page.
fn section_header(kind: Kind, pal: Palette, catalog: &Entity<Catalog>) -> Div {
    h_flex()
        .px(px(PAD))
        .pt_6()
        .pb_3()
        .items_center()
        .child(
            div()
                .flex_1()
                .text_lg()
                .font_weight(FontWeight::BOLD)
                .text_color(pal.fg)
                .child(kind.title()),
        )
        .when_some(kind.see_all(), |d, to| {
            let catalog = catalog.clone();
            d.child(
                div()
                    .id(ElementId::Name(format!("home-see-all-{kind:?}").into()))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(pal.secondary)
                    .cursor_pointer()
                    .hover(|s| s.text_color(pal.fg).bg(Hsla { a: 0.08, ..pal.fg }))
                    .on_click(move |_, _, cx| {
                        catalog.update(cx, |c, cx| match to {
                            SeeAll::Albums(sort) => {
                                c.album_sort = sort;
                                c.start_at(Place::Albums, cx)
                            }
                            SeeAll::Artists => c.start_at(Place::Artists, cx),
                        })
                    })
                    .child("See all"),
            )
        })
}

impl Render for HomeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (view, empty) = {
            let c = self.catalog.read(cx);
            (c.home_view.clone(), c.empty)
        };
        let Some(view) = view else {
            return div().size_full().child(empty_state(empty, "", pal));
        };
        let rows: Vec<Entity<RowView>> = view
            .sections
            .iter()
            .map(|sec| {
                self.rows
                    .entry(sec.kind)
                    .or_insert_with(|| {
                        cx.new(|_| RowView {
                            shared: self.shared.clone(),
                            catalog: self.catalog.clone(),
                            kind: sec.kind,
                            handle: ScrollHandle::new(),
                        })
                    })
                    .clone()
            })
            .collect();

        let empty_library = view.sections.is_empty();
        div().size_full().child(
            v_flex()
                .id("home")
                .size_full()
                .overflow_y_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(&self.scroll)
                .pb_6()
                .child(
                    div()
                        .px(px(PAD))
                        .pt_6()
                        .pb_4()
                        .text_2xl()
                        .font_weight(FontWeight::BOLD)
                        .text_color(pal.fg)
                        .child(home::greeting(local_hour())),
                )
                .when(!view.highlights.is_empty(), |d| {
                    d.child(div().px(px(PAD)).child(self.highlight.clone()))
                })
                .when(empty_library, |d| {
                    d.child(
                        div()
                            .px(px(PAD))
                            .text_color(pal.secondary)
                            .child("Nothing in the library yet"),
                    )
                })
                .children(rows),
        )
    }
}

/// One section of the home: its title and its row.
pub struct RowView {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    kind: Kind,
    handle: ScrollHandle,
}

impl Render for RowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (view, covers) = {
            let c = self.catalog.read(cx);
            (
                c.home_view.clone(),
                Covers::new(c.covers.clone(), window.scale_factor()),
            )
        };
        let width = content_width(window, &self.shared, cx);
        let Some(view) = view else {
            return div();
        };
        let catalog = &self.catalog;
        let me = cx.entity_id();

        let Some(s) = view.sections.iter().find(|s| s.kind == self.kind) else {
            return div();
        };
        let handle = self.handle.clone();
        let drag = RowDrag {
            row: s.kind,
            handle: handle.clone(),
            last: Rc::default(),
        };
        let kind = s.kind;
        let items: Vec<AnyElement> = match &s.row {
            Row::Albums(v) => v
                .iter()
                .map(|a| album_cell(a, &covers, SQUARE, pal, catalog).into_any_element())
                .collect(),
            Row::Artists(v) => v
                .iter()
                .map(|a| artist_cell(a, &covers, pal, catalog, window, cx).into_any_element())
                .collect(),
            Row::Mixes(v) => v
                .iter()
                .map(|m| mix_cell(m, pal, catalog).into_any_element())
                .collect(),
            Row::Playlists(v) => v
                .iter()
                .map(|p| playlist_cell(p, &covers, pal, catalog, window, cx).into_any_element())
                .collect(),
        };
        v_flex().child(section_header(kind, pal, catalog)).child(
            // the settled width, as the pages: nothing moves inside while the queue slides
            h_flex()
                .id(ElementId::Name(format!("home-row-{kind:?}").into()))
                .w(px(width + 2.0 * PAD))
                .flex_none()
                .px(px(PAD))
                .gap(px(GAP))
                .items_start()
                .overflow_x_scroll()
                // the wheel scrolls the page; shift and the touchpad's sideways
                // scroll move the row
                .restrict_scroll_to_axis()
                .track_scroll(&handle)
                .on_drag(drag, |d: &RowDrag, _, _, cx| {
                    d.last.set(None);
                    cx.new(|_| Nothing)
                })
                .on_drag_move::<RowDrag>(move |ev, _, cx| {
                    let d = ev.drag(cx);
                    if d.row != kind {
                        return;
                    }
                    let x = ev.event.position.x;
                    if let Some(last) = d.last.replace(Some(x)) {
                        let o = d.handle.offset();
                        let max = d.handle.max_offset().x;
                        let to = (o.x + (x - last)).clamp(-max, px(0.));
                        d.handle.set_offset(point(to, o.y));
                        cx.notify(me);
                    }
                })
                .children(items),
        )
    }
}

impl Render for HighlightView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (view, covers, tints) = {
            let c = self.catalog.read(cx);
            (
                c.home_view.clone(),
                Covers::new(c.covers.clone(), window.scale_factor()),
                c.tints.clone(),
            )
        };
        let width = content_width(window, &self.shared, cx);
        let Some(view) = view else {
            return div();
        };
        let catalog = &self.catalog;

        // the highlight: its colours move to the next one's as it turns
        let pick = self.pick.min(view.highlights.len().saturating_sub(1));
        let highlight = view.highlights.get(pick).cloned();
        let target = match &highlight {
            Some(Highlight::Album { album, .. }) => Palette::base(
                album
                    .cover
                    .as_ref()
                    .and_then(|c| tints.get(c).copied().flatten()),
                pal.dark,
            ),
            Some(Highlight::Mix) | None => card_palette(pal.accent.h, pal.dark),
        };
        let cp = motion::to("home-card", target, motion::COVER, window, cx);
        let count = view.highlights.len();
        let card = highlight.map(|h| {
            let turn = |to: usize| {
                cx.listener(move |this: &mut Self, _: &ClickEvent, _, cx| this.turn(to, cx))
            };
            let (prev, next) = (turn((pick + count - 1) % count), turn((pick + 1) % count));
            let dots: Vec<_> = (0..count)
                .map(|i| {
                    div()
                        .id(("home-dot", i))
                        .size(px(if i == pick { 10. } else { 8. }))
                        .rounded_full()
                        .cursor_pointer()
                        .bg(Hsla {
                            a: if i == pick { 0.9 } else { 0.35 },
                            ..cp.fg
                        })
                        .on_click(turn(i))
                })
                .collect();
            highlight_card(h, &covers, cp, catalog)
                .w(px(width))
                .relative()
                .when(count > 1, |d| {
                    d.child(
                        h_flex()
                            .absolute()
                            .right_4()
                            .bottom_4()
                            .gap_2()
                            .items_center()
                            .child(
                                IconButton::new("home-prev", IconName::ChevronLeft, cp)
                                    .diameter(32.)
                                    .tip("Previous")
                                    .on_click(prev),
                            )
                            .children(dots)
                            .child(
                                IconButton::new("home-next", IconName::ChevronRight, cp)
                                    .diameter(32.)
                                    .tip("Next")
                                    .on_click(next),
                            ),
                    )
                })
        });

        div().when_some(card, |d, card| {
            d.child(card.with_animation(
                ("home-card-enter", self.turns),
                motion::enter(),
                |el, t| el.opacity(0.4 + 0.6 * t),
            ))
        })
    }
}

/// The large card at the top: the cover, why it is here, its name, play.
fn highlight_card(
    h: Highlight,
    covers: &Covers,
    cp: Palette,
    catalog: &Entity<Catalog>,
) -> Stateful<Div> {
    let (why, title, about, cover, play, open): (_, String, String, Div, Job, Option<Place>) =
        match &h {
            Highlight::Album { album, why } => {
                let Album {
                    id, name, artist, ..
                } = album;
                (
                    match why {
                        Why::New => "NEW IN YOUR LIBRARY",
                        Why::LastPlayed => "PLAYED LATELY",
                        Why::MostPlayed => "YOUR MOST PLAYED",
                    },
                    name.clone(),
                    artist.clone(),
                    picture(
                        album.cover.as_ref().and_then(|c| covers.get(c, CARD_COVER)),
                        CARD_COVER,
                        12.0,
                        IconName::Disc3,
                        cp,
                    ),
                    Job::Play {
                        kind: Of::Album,
                        id: id.clone(),
                        shuffle: false,
                    },
                    Some(Place::Album(id.clone())),
                )
            }
            Highlight::Mix => {
                let (icon, name, about) = mix_text(&Mix::Mine);
                (
                    "YOUR MIX",
                    name,
                    about,
                    picture(None, CARD_COVER, 12.0, icon, cp),
                    Job::PlayMix,
                    None,
                )
            }
        };
    let (player, opener) = (catalog.clone(), catalog.clone());
    let item = match &h {
        Highlight::Album { album, .. } => Item::album(album),
        Highlight::Mix => Item::Mix(Mix::Mine),
    };
    h_flex()
        .id("home-card")
        .menu(item, Spot::Library)
        .h(px(CARD))
        .flex_none()
        .p(px((CARD - CARD_COVER) / 2.0))
        .gap_6()
        .items_center()
        .rounded_2xl()
        .overflow_hidden()
        .bg(linear_gradient(
            90.,
            linear_color_stop(mix(cp.surface, cp.accent, 0.35), 0.),
            linear_color_stop(cp.bg, 1.),
        ))
        .child(cover.shadow_lg())
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cp.secondary)
                        .child(why),
                )
                .child(
                    div()
                        .text_3xl()
                        .font_weight(FontWeight::BOLD)
                        .text_color(cp.fg)
                        .truncate()
                        .child(title),
                )
                .child(div().text_color(cp.secondary).truncate().child(about))
                .child(
                    h_flex()
                        .pt_4()
                        .gap_2()
                        .child(
                            IconButton::new("home-card-play", IconName::Play, cp)
                                .label("Play")
                                .primary()
                                .diameter(40.)
                                .on_click(move |_, _, cx| {
                                    player.update(cx, |c, cx| c.run(play.clone(), cx))
                                }),
                        )
                        .when_some(open, |d, place| {
                            d.child(
                                IconButton::new("home-card-open", IconName::Disc3, cp)
                                    .label("Open")
                                    .diameter(40.)
                                    .on_click(move |_, _, cx| {
                                        opener.update(cx, |c, cx| c.go(place.clone(), cx))
                                    }),
                            )
                        }),
                ),
        )
}
