//! What plays, at the bottom of the library: the cover, the track, play and pause, next. A click
//! on it opens the player.

use crate::app::Shared;
use crate::catalog::Catalog;
use crate::player::palette;
use crate::widgets::IconButton;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::proto::Request;
use melodie_gui::browse::Place;
use melodie_gui::model::PlayState;

pub const HEIGHT: f32 = 68.0;

pub struct MiniPlayer {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
}

impl MiniPlayer {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>) -> Self {
        Self { shared, catalog }
    }
}

impl Render for MiniPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (track, playing, image) = {
            let s = self.shared.read(cx);
            (
                s.player.track().cloned(),
                s.player.state == PlayState::Playing,
                s.art.at(s.art.step).and_then(|a| a.image.clone()),
            )
        };
        let (open, toggle, next) = (
            self.catalog.clone(),
            self.shared.clone(),
            self.shared.clone(),
        );
        h_flex()
            .id("mini-player")
            .h(px(HEIGHT))
            .w_full()
            .flex_none()
            .px_4()
            .gap_3()
            .items_center()
            .border_t_1()
            .border_color(Hsla { a: 0.08, ..pal.fg })
            .bg(Hsla { a: 0.04, ..pal.fg })
            .cursor_pointer()
            .on_click(move |_, _, cx| open.update(cx, |c, cx| c.start_at(Place::Player, cx)))
            .child(
                div()
                    .size(px(44.))
                    .flex_none()
                    .rounded_md()
                    .overflow_hidden()
                    .bg(Hsla { a: 0.08, ..pal.fg })
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|d| match image {
                        Some(i) => {
                            d.child(img(i).size_full().rounded_md().object_fit(ObjectFit::Cover))
                        }
                        None => d.child(
                            Icon::new(IconName::Music)
                                .size(px(18.))
                                .text_color(pal.secondary),
                        ),
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(pal.fg)
                            .truncate()
                            .child(
                                track
                                    .as_ref()
                                    .map_or("Nothing playing".to_string(), |t| t.title.clone()),
                            ),
                    )
                    .when_some(track.as_ref(), |d, t| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(pal.secondary)
                                .truncate()
                                .child(t.artist.clone()),
                        )
                    }),
            )
            .child(
                IconButton::new(
                    "mini-play",
                    if playing {
                        IconName::Pause
                    } else {
                        IconName::Play
                    },
                    pal,
                )
                .primary()
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    toggle.update(cx, |s, cx| s.play_or_pause(cx))
                }),
            )
            .child(
                IconButton::new("mini-next", IconName::SkipForward, pal)
                    .disabled(track.is_none())
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        next.update(cx, |s, cx| s.send(Request::Next, cx))
                    }),
            )
    }
}
