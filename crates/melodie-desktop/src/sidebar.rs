//! The sidebar: the player, the library's sections, the playlists; the queue and the theme at
//! the bottom. Under `COMPACT` pixels it keeps only the icons.

use crate::app::Shared;
use crate::catalog::Catalog;
use crate::dialogs::{self, Dialog};
use crate::motion;
use crate::player::palette;
use crate::widgets::{IconButton, follow_pointer, pointer};
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_desktop::browse::Place;
use melodie_desktop::theme::{Palette, mix};

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

/// An entry: its icon, its name unless compact, marked when it is where the user is.
#[derive(IntoElement)]
struct Entry {
    id: ElementId,
    icon: IconName,
    label: SharedString,
    compact: bool,
    on: bool,
    pal: Palette,
    to: Place,
    catalog: Entity<Catalog>,
}

impl RenderOnce for Entry {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (ptr, h, d) = pointer(&self.id, true, window, cx);
        let s = motion::flag((self.id.clone(), "on"), self.on, motion::HOVER, window, cx);
        let pal = self.pal;
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
                Icon::new(self.icon)
                    .size(px(18.))
                    .flex_none()
                    .text_color(mix(fg, pal.accent, s)),
            )
            .when(!self.compact, |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(fg)
                        .when(self.on, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .child(self.label),
                )
            })
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let compact = width(window.viewport_size().width.as_f32()) < 100.0;
        let (section, playlists) = {
            let c = self.catalog.read(cx);
            (c.nav.section().clone(), c.playlists.clone())
        };
        let (queue_open, settings_open) = {
            let s = self.shared.read(cx);
            (s.queue_open, s.settings_open)
        };
        let entry = |id: &'static str, icon, label: &'static str, to: Place| Entry {
            id: id.into(),
            icon,
            label: label.into(),
            compact,
            on: section == to,
            pal,
            to,
            catalog: self.catalog.clone(),
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
                    .children(playlists.iter().map(|p| Entry {
                        id: ElementId::Name(format!("nav-pl-{}", p.id).into()),
                        icon: IconName::ListMusic,
                        label: p.name.clone().into(),
                        compact,
                        on: section == Place::Playlist(p.id.clone()),
                        pal,
                        to: Place::Playlist(p.id.clone()),
                        catalog: self.catalog.clone(),
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
                            .on_click(move |_, _, cx| {
                                queue.update(cx, |s, cx| {
                                    s.queue_open = !s.queue_open;
                                    s.settings_open = false;
                                    cx.notify()
                                })
                            }),
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
