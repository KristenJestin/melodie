//! The sidebar: the player, the library's sections, the playlists; the queue and the theme at
//! the bottom. Under `COMPACT` pixels it keeps only the icons.

use crate::app::Shared;
use crate::catalog::Catalog;
use crate::context_menu::Menued as _;
use crate::dialogs::{self, Dialog};
use crate::equaliser::{Equaliser, badge};
use crate::motion;
use crate::player::palette;
use crate::widgets::{IconButton, follow_pointer, pointer};
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_desktop::browse::Place;
use melodie_desktop::covers::{Cover, Covers};
use melodie_desktop::menu::{Item, Spot};
use melodie_desktop::playing::Mark;
use melodie_desktop::playlists::{self, PICTURE};
use melodie_desktop::theme::{Palette, mix};

/// The side of the badge on the cover of the source of the music, when only the icons show.
const BADGE: f32 = 14.;

/// A window narrower than this has the sidebar of icons.
const COMPACT: f32 = 760.0;

pub fn width(viewport: f32) -> f32 {
    if viewport < COMPACT { 64.0 } else { 220.0 }
}

pub struct Sidebar {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    scroll: ScrollHandle,
}

impl Sidebar {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>) -> Self {
        Self {
            shared,
            catalog,
            scroll: ScrollHandle::new(),
        }
    }
}

/// An entry: its icon (a playlist's own picture instead, when it has one), its name unless
/// compact, marked when it is where the user is.
#[derive(IntoElement)]
struct Entry {
    id: ElementId,
    icon: IconName,
    picture: Option<Cover>,
    label: SharedString,
    compact: bool,
    on: bool,
    pal: Palette,
    to: Place,
    catalog: Entity<Catalog>,
    /// The music playing was started from this playlist.
    mark: Option<Mark>,
}

impl RenderOnce for Entry {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (ptr, h, d) = pointer(&self.id, true, window, cx);
        let s = motion::flag((self.id.clone(), "on"), self.on, motion::HOVER, window, cx);
        let pal = self.pal;
        let mark_id = (self.id.clone(), "equaliser");
        let outline_id = format!("{}-outline", self.id);
        let fg = mix(pal.secondary, pal.fg, s.max(h));
        let (catalog, to) = (self.catalog, self.to);
        follow_pointer(div().id(self.id), ptr)
            .h(px(36.))
            .w_full()
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .rounded_lg()
            .cursor_pointer()
            .when(self.compact, |d| d.justify_center().px_0())
            .bg(Hsla {
                a: 0.06 * h + 0.06 * d + 0.12 * s,
                ..mix(pal.fg, pal.accent, s)
            })
            .on_click(move |_, _, cx| catalog.update(cx, |c, cx| c.start_at(to.clone(), cx)))
            .child(
                div()
                    .relative()
                    .flex_none()
                    // an icon is smaller than a picture: the outline goes round the room of one
                    .when(self.mark.is_some() && self.compact, |d| {
                        d.size(px(PICTURE)).flex().items_center().justify_center()
                    })
                    .child(match self.picture {
                        // GPUI does not clip children to rounded corners: the image is rounded
                        // itself
                        Some(cover) => img(cover.path)
                            .size(px(PICTURE))
                            .flex_none()
                            .rounded(px(4.))
                            .object_fit(ObjectFit::Cover)
                            .into_any_element(),
                        None => Icon::new(self.icon)
                            .size(px(18.))
                            .flex_none()
                            .text_color(mix(fg, pal.accent, s))
                            .into_any_element(),
                    })
                    // icons only: no name to turn to the accent, so the cover the music plays
                    // from is outlined and wears the badge
                    .when_some(self.mark.filter(|_| self.compact), |d, mark| {
                        d.child(
                            div()
                                .debug_selector(|| outline_id)
                                .absolute()
                                .top(px(-1.))
                                .left(px(-1.))
                                .size(px(PICTURE + 2.))
                                .rounded(px(5.))
                                .border_1()
                                .border_color(pal.accent),
                        )
                        .child(badge(
                            mark_id.clone(),
                            mark,
                            BADGE,
                            pal.accent,
                        ))
                    }),
            )
            .when(!self.compact, |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(if self.mark.is_some() { pal.accent } else { fg })
                        .when(self.on, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .child(self.label),
                )
            })
            .when_some(self.mark.filter(|_| !self.compact), |d, mark| {
                d.child(Equaliser::new(mark_id, mark, pal.accent))
            })
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let compact = width(window.viewport_size().width.as_f32()) < 100.0;
        let (section, list, covers) = {
            let c = self.catalog.read(cx);
            (
                c.nav.section().clone(),
                c.playlists.clone(),
                Covers::new(c.covers.clone(), window.scale_factor()),
            )
        };
        let (queue_open, settings_open, playing) = {
            let s = self.shared.read(cx);
            (s.queue_open, s.settings_open, s.playing.clone())
        };
        let entry = |id: &'static str, icon, label: &'static str, to: Place| Entry {
            id: id.into(),
            icon,
            picture: None,
            label: label.into(),
            compact,
            on: section == to,
            pal,
            to,
            catalog: self.catalog.clone(),
            mark: None,
        };
        let (queue, gear) = (self.shared.clone(), self.shared.clone());
        v_flex()
            .h_full()
            .w(px(width(window.viewport_size().width.as_f32())))
            .flex_none()
            .px_2()
            .pt_3()
            .pb_2()
            .gap_1()
            .bg(Hsla { a: 0.04, ..pal.fg })
            .border_r_1()
            .border_color(Hsla { a: 0.08, ..pal.fg })
            .child(entry(
                "nav-search",
                IconName::Search,
                "Search",
                Place::Search,
            ))
            .child(entry(
                "nav-player",
                IconName::AudioLines,
                "Now playing",
                Place::Player,
            ))
            .child(entry("nav-home", IconName::House, "Home", Place::Home))
            .child(entry(
                "nav-albums",
                IconName::Disc3,
                "Albums",
                Place::Albums,
            ))
            .child(entry(
                "nav-artists",
                IconName::MicVocal,
                "Artists",
                Place::Artists,
            ))
            .child(entry(
                "nav-tracks",
                IconName::Music,
                "Tracks",
                Place::Tracks,
            ))
            .when(!compact, |d| {
                d.child(
                    h_flex()
                        .pl_3()
                        .pt_3()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(pal.secondary)
                                .child("PLAYLISTS"),
                        )
                        .child(
                            IconButton::new("nav-new-playlist", IconName::Plus, pal)
                                .tip("New playlist")
                                .diameter(28.)
                                .on_click(|_, window, cx| {
                                    dialogs::open(Dialog::Create { with: None }, window, cx)
                                }),
                        ),
                )
            })
            .child(
                v_flex()
                    .id("nav-playlists")
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(list.iter().map(|p| {
                        div()
                            .w_full()
                            .menu(Item::playlist(p), Spot::Library)
                            .child(Entry {
                                id: ElementId::Name(format!("nav-pl-{}", p.id).into()),
                                icon: IconName::ListMusic,
                                picture: playlists::picture(p, &covers),
                                label: p.name.clone().into(),
                                compact,
                                on: section == Place::Playlist(p.id.clone()),
                                pal,
                                to: Place::Playlist(p.id.clone()),
                                catalog: self.catalog.clone(),
                                mark: playing.playlist(&p.id),
                            })
                    })),
            )
            .child(
                h_flex()
                    .pt_2()
                    .gap_1()
                    .when(compact, |d| d.flex_col())
                    .justify_center()
                    .child(
                        IconButton::new("nav-queue", IconName::ListMusic, pal)
                            .tip("Queue")
                            .selected(queue_open)
                            .on_click(move |_, _, cx| queue.update(cx, |s, cx| s.toggle_queue(cx))),
                    )
                    .child(
                        IconButton::new("settings", IconName::Settings, pal)
                            .tip("Settings")
                            .selected(settings_open)
                            .on_click(move |_, _, cx| {
                                gear.update(cx, |s, cx| {
                                    s.settings_open = !s.settings_open;
                                    cx.notify()
                                })
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::Sidebar;
    use crate::app;
    use crate::catalog::Catalog;
    use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext, px, size};
    use melodie_core::env::Env;
    use melodie_core::library::Playlist;
    use melodie_core::proto::QueueEntry;
    use melodie_desktop::model::{PlayState, Player};
    use melodie_desktop::playing::Ranks;
    use std::sync::Arc;

    fn playlist(id: &str) -> Playlist {
        Playlist {
            id: id.into(),
            name: format!("Playlist {id}"),
            songs: 3,
            duration: 600,
            owner: None,
            cover: None,
        }
    }

    /// A sidebar of the playlists `p1` and `p2`, `width` pixels wide (a window under 760 keeps
    /// only the icons).
    fn sidebar(
        app: &mut TestAppContext,
        width: f32,
    ) -> (Entity<app::Shared>, &mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        let program = app::fake_melodie(dir.path());
        let env = Env::with_home(dir.path().join("home"));
        let mut made = None;
        let (_, cx) = app.add_window_view(|_, cx| {
            let s = app::bare(env.clone(), program.clone(), cx);
            let catalog = cx.new(|cx| Catalog::new(env.clone(), s.clone(), cx));
            made = Some((s.clone(), catalog.clone()));
            Sidebar::new(s, catalog)
        });
        let (shared, catalog) = made.unwrap();
        cx.simulate_resize(size(px(width), px(700.)));
        cx.run_until_parked();
        // after the read of the copy, which holds no playlist
        catalog.update(cx, |c, _| {
            c.playlists = Arc::from([playlist("p1"), playlist("p2")])
        });
        cx.refresh().unwrap();
        cx.run_until_parked();
        (shared, cx)
    }

    /// The music plays (or is paused) from the playlist `p1`; no music when `None`.
    fn play(shared: &Entity<app::Shared>, state: Option<PlayState>, cx: &mut VisualTestContext) {
        shared.update(cx, |s, _| {
            let Some(state) = state else {
                return s.playing.follow(&Player::default());
            };
            // a new start asks for the queue that follows: not again for a pause of this one
            if s.playing.playlist("p1").is_none() {
                s.playing.start_playlist("p1", Ranks::Own);
            }
            s.playing.follow(&Player {
                following: true,
                state,
                tracks: vec![QueueEntry {
                    entry: 1,
                    id: "a".into(),
                    ..QueueEntry::default()
                }]
                .into(),
                current: Some(0),
                ..Player::default()
            });
        });
        cx.refresh().unwrap();
        cx.run_until_parked();
        // an update draws the window that is dirty; reading a bound does it only after the read
        cx.update(|_, _| {});
    }

    #[test]
    fn the_collapsed_sidebar_outlines_and_badges_the_cover_the_music_plays_from() {
        let mut app = TestAppContext::single();
        let (shared, cx) = sidebar(&mut app, 600.);
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("nav-pl-p1-outline").is_none(),
            "nothing plays"
        );
        play(&shared, Some(PlayState::Playing), cx);
        let outline = cx.debug_bounds("nav-pl-p1-outline").expect("its cover");
        let badge = cx
            .debug_bounds("nav-pl-p1-equaliser-badge")
            .expect("the badge");
        assert!(
            cx.debug_bounds("nav-pl-p1-equaliser").is_some(),
            "holding the bars"
        );
        let near = |a: gpui_kit::Pixels, b: gpui_kit::Pixels| (a - b).abs() <= px(4.);
        assert!(
            near(badge.right(), outline.right()) && near(badge.bottom(), outline.bottom()),
            "on its bottom-right corner: {badge:?} on {outline:?}"
        );
        assert!(badge.size.width < outline.size.width, "small");
        assert!(
            cx.debug_bounds("nav-pl-p2-outline").is_none()
                && cx.debug_bounds("nav-pl-p2-equaliser").is_none(),
            "another playlist is not marked"
        );
        play(&shared, Some(PlayState::Paused), cx);
        assert!(
            cx.debug_bounds("nav-pl-p1-outline").is_some(),
            "paused: still marked"
        );
        play(&shared, None, cx);
        assert!(
            cx.debug_bounds("nav-pl-p1-outline").is_none()
                && cx.debug_bounds("nav-pl-p1-equaliser-badge").is_none(),
            "the music stopped"
        );
    }

    #[test]
    fn the_full_sidebar_keeps_the_name_in_the_accent_and_no_outline() {
        let mut app = TestAppContext::single();
        let (shared, cx) = sidebar(&mut app, 1280.);
        play(&shared, Some(PlayState::Playing), cx);
        assert!(
            cx.debug_bounds("nav-pl-p1-equaliser").is_some(),
            "at the right of its name"
        );
        assert!(
            cx.debug_bounds("nav-pl-p1-outline").is_none()
                && cx.debug_bounds("nav-pl-p1-equaliser-badge").is_none(),
            "the outline and the badge are for the icons"
        );
    }
}
