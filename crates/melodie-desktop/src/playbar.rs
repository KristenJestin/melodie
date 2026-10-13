//! The player bar at the bottom of the window while the user browses: the track on the left, the
//! controls and the position in the middle, the volume and the window's buttons on the right.
//! What fits is decided by `bar::parts`, from the width of the main area.

use crate::app::{Shared, Shown};
use crate::catalog::{Catalog, Facet};
use crate::context_menu::Menued as _;
use crate::motion;
use crate::player::{Bar, palette};
use crate::sidebar;
use crate::volume::{self, Volume};
use crate::widgets::{IconButton, follow_pointer, pointer};
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::component::slider::Slider;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::proto::Request;
use melodie_desktop::bar;
use melodie_desktop::browse::Place;
use melodie_desktop::launch::{Job, Kind};
use melodie_desktop::layout;
use melodie_desktop::menu::{Item, Spot};
use melodie_desktop::model::PlayState;
use melodie_desktop::theme::{Palette, mix};

/// What the bar shows now: from the main area's width once the queue has its place, as the
/// pages do, so that nothing flickers while the queue's column slides.
pub fn parts(shared: &Entity<Shared>, window: &Window, cx: &App) -> bar::Parts {
    let w = window.viewport_size().width.as_f32();
    bar::parts(layout::main_width(
        w,
        sidebar::width(w),
        shared.read(cx).queue_open,
    ))
}

pub struct PlayBar {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    volume: Entity<Volume>,
    position: Entity<Bar>,
}

impl PlayBar {
    pub fn new(
        shared: Entity<Shared>,
        catalog: Entity<Catalog>,
        volume: Entity<Volume>,
        position: Entity<Bar>,
    ) -> Self {
        Self {
            shared,
            catalog,
            volume,
            position,
        }
    }
}

/// A cover, or its place while there is none.
fn cover(shown: Option<&Shown>, size: f32, pal: Palette) -> AnyElement {
    let tile = div()
        .size(px(size))
        .flex_none()
        .rounded_md()
        .overflow_hidden()
        .bg(Hsla { a: 0.08, ..pal.fg })
        .flex()
        .items_center()
        .justify_center();
    match shown.and_then(|s| s.image.clone()) {
        Some(i) => tile
            .child(img(i).size_full().object_fit(ObjectFit::Cover))
            .into_any_element(),
        None => tile
            .child(
                Icon::new(IconName::Music)
                    .size(px(size * 0.4))
                    .text_color(pal.secondary),
            )
            .into_any_element(),
    }
}

/// A line of text that opens a page: lit and underlined under the pointer.
fn link(
    id: &'static str,
    text: String,
    base: Hsla,
    pal: Palette,
    open: impl Fn(&mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let id = ElementId::from(id);
    let (ptr, h, _) = pointer(&id, true, window, cx);
    let hovered = ptr.read(cx).hovered;
    follow_pointer(div().id(id), ptr)
        .max_w_full()
        .truncate()
        .cursor_pointer()
        .text_color(mix(base, pal.fg, h))
        .when(hovered, |d| d.underline())
        .on_click(move |_, _, cx| open(cx))
        .child(text)
}

impl Render for PlayBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let parts = parts(&self.shared, window, cx);
        let (track, playing, following, queue_open, step, volume_now) = {
            let s = self.shared.read(cx);
            (
                s.player.track().cloned(),
                s.player.state == PlayState::Playing,
                s.player.following,
                s.queue_open,
                s.art.step,
                s.player.volume,
            )
        };
        // The cover cross-fades through the steps, and the title comes in, as in the player.
        let at = motion::to("bar-cover-step", step as f32, motion::COVER, window, cx);
        let (lo, hi) = (at.floor() as u64, at.ceil() as u64);
        let (under, over) = {
            let art = &self.shared.read(cx).art;
            (art.at(lo).cloned(), art.at(hi).cloned())
        };
        let play = motion::flag("bar-playing", playing, motion::HOVER, window, cx);

        let small = parts.small;
        let has_track = track.is_some();
        let size = if small { 40. } else { 44. };
        let side_d = if small { 32. } else { 36. };
        let gap = if small { 4. } else { 8. };
        let pad = if small { 8. } else { 12. };

        let (starred, id) = match &track {
            Some(t) => (t.starred, Some(t.id.clone())),
            None => (false, None),
        };

        let menu = track.as_ref().map(|t| (Item::entry(t), t.starred));
        let left = {
            let cover = div()
                .relative()
                .size(px(size))
                .flex_none()
                .child(cover(under.as_ref(), size, pal))
                .when(hi != lo, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .opacity(at - at.floor())
                            .child(self::cover(over.as_ref(), size, pal)),
                    )
                });
            let text = parts.text.then(|| {
                let (title, artist) = match &track {
                    Some(t) => (t.title.clone(), t.artist.clone()),
                    None => ("Nothing playing".to_string(), String::new()),
                };
                let (to_album, to_artist) = (id.clone(), id.clone());
                let (catalog_a, catalog_b) = (self.catalog.clone(), self.catalog.clone());
                let open = |track: Option<String>, catalog: Entity<Catalog>, facet: Facet| {
                    move |cx: &mut App| {
                        if let Some(track) = track.clone() {
                            catalog.update(cx, |c, cx| c.open_of_track(track, facet, cx))
                        }
                    }
                };
                v_flex()
                    .id(("bar-text", step))
                    .min_w_0()
                    .items_start()
                    .when(has_track, |d| {
                        d.child(
                            link(
                                "bar-title",
                                title.clone(),
                                pal.fg,
                                pal,
                                open(to_album, catalog_a, Facet::Album),
                                window,
                                cx,
                            )
                            .into_any_element(),
                        )
                    })
                    .when(!has_track, |d| d.child(div().truncate().child(title)))
                    .when(has_track && !artist.is_empty(), |d| {
                        d.child(div().text_xs().max_w_full().child(link(
                            "bar-artist",
                            artist,
                            pal.secondary,
                            pal,
                            open(to_artist, catalog_b, Facet::Artist),
                            window,
                            cx,
                        )))
                    })
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(pal.fg)
                    .with_animation(("bar-text-enter", step), motion::enter(), |el, t| {
                        el.opacity(t).mt(px((1.0 - t) * 6.0))
                    })
            });
            let like = parts.like.then(|| {
                let shared = self.shared.clone();
                IconButton::new("bar-like", IconName::Heart, pal)
                    .tip(if starred { "Unlike" } else { "Like" })
                    .diameter(32.)
                    .selected(starred)
                    .disabled(!has_track)
                    .on_click(move |_, _, cx| {
                        if let Some(id) = id.clone() {
                            shared.update(cx, |s, cx| {
                                s.run(
                                    Job::Star {
                                        kind: Kind::Track,
                                        id,
                                        starred: !starred,
                                    },
                                    cx,
                                )
                            })
                        }
                    })
            });
            h_flex()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .gap(px(if parts.text { 12. } else { 6. }))
                .items_center()
                .child(cover)
                .children(text)
                .children(like)
                .when_some(menu, |d, (item, starred)| {
                    d.menu_starred(item, Spot::Player, Some(starred))
                })
        };

        let middle = {
            let (prev, toggle, next) = (
                self.shared.clone(),
                self.shared.clone(),
                self.shared.clone(),
            );
            v_flex()
                .w(px(parts.center))
                .flex_none()
                .items_center()
                .gap(px(bar::ROWS_GAP))
                .child(
                    h_flex()
                        .gap(px(gap))
                        .items_center()
                        .child(
                            IconButton::new("bar-prev", IconName::SkipBack, pal)
                                .tip("Previous")
                                .diameter(side_d)
                                .disabled(!has_track)
                                .on_click(move |_, _, cx| {
                                    prev.update(cx, |s, cx| s.send(Request::Prev, cx))
                                }),
                        )
                        .child(
                            IconButton::new("bar-play", IconName::Play, pal)
                                .morph(IconName::Pause, play)
                                .tip(if playing { "Pause" } else { "Play" })
                                .diameter(bar::PLAY)
                                .primary()
                                .on_click(move |_, _, cx| {
                                    toggle.update(cx, |s, cx| s.play_or_pause(cx))
                                }),
                        )
                        .child(
                            IconButton::new("bar-next", IconName::SkipForward, pal)
                                .tip("Next")
                                .diameter(side_d)
                                .disabled(!has_track)
                                .on_click(move |_, _, cx| {
                                    next.update(cx, |s, cx| s.send(Request::Next, cx))
                                }),
                        ),
                )
                .child(self.position.clone())
        };

        let right = {
            let (mute, queue, open) = (
                self.volume.clone(),
                self.shared.clone(),
                self.catalog.clone(),
            );
            h_flex()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .justify_end()
                .items_center()
                .gap(px(if small { 2. } else { 4. }))
                .when(parts.volume, |d| {
                    d.child(
                        IconButton::new("bar-mute", volume::icon(volume_now), pal)
                            .tip(if volume_now > 0.0 { "Mute" } else { "Unmute" })
                            .diameter(32.)
                            .on_click(move |_, _, cx| mute.update(cx, |v, cx| v.toggle_mute(cx))),
                    )
                    .child(
                        div().w(px(90.)).flex_none().mr_2().child(
                            Slider::new(self.volume.read(cx).slider())
                                .disabled(!following)
                                .bg(pal.secondary)
                                .text_color(pal.fg),
                        ),
                    )
                })
                .child(
                    IconButton::new("bar-queue", IconName::ListMusic, pal)
                        .tip("Queue")
                        .diameter(side_d)
                        .selected(queue_open)
                        .on_click(move |_, _, cx| queue.update(cx, |s, cx| s.toggle_queue(cx))),
                )
                .child(
                    IconButton::new("bar-open-player", IconName::Maximize2, pal)
                        .tip("Open the player")
                        .diameter(side_d)
                        .on_click(move |_, _, cx| {
                            open.update(cx, |c, cx| c.start_at(Place::Player, cx))
                        }),
                )
        };

        // Both sides take the same room, so that play and pause sit on the axis of the area.
        h_flex()
            .id("player-bar")
            .h(px(bar::HEIGHT))
            .w_full()
            .flex_none()
            .px(px(pad))
            .gap(px(gap))
            .items_center()
            .border_t_1()
            .border_color(Hsla { a: 0.08, ..pal.fg })
            .bg(mix(pal.bg, pal.accent, 0.08))
            .child(left)
            .child(middle)
            .child(right)
    }
}
