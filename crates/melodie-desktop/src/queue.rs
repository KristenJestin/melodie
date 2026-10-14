//! The queue panel: past tracks greyed, the current one marked, the next ones; play one, drag to
//! reorder, remove, clear what comes next.

use crate::app::Shared;
use crate::artist_links;
use crate::catalog::Catalog;
use crate::context_menu::Menued as _;
use crate::player::palette;
use crate::tips::Tipped as _;
use crate::widgets::{IconButton, Revealed as _, hover};
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::proto::{QueueEntry, Request};
use melodie_desktop::browse::Place;
use melodie_desktop::covers::Use;
use melodie_desktop::credits;
use melodie_desktop::figures::{Figures as _, clock_text, count_text};
use melodie_desktop::menu::{Item, Spot};
use melodie_desktop::model::Row;
use melodie_desktop::theme::Palette;
use melodie_desktop::thumbs;
use std::cell::RefCell;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Weak};

const ROW: f32 = 56.0;

pub struct QueuePanel {
    shared: Entity<Shared>,
    list: Entity<QueueList>,
}

impl QueuePanel {
    pub fn new(shared: Entity<Shared>, catalog: Entity<Catalog>, cx: &mut Context<Self>) -> Self {
        let list = cx.new(|_| QueueList {
            shared: shared.clone(),
            catalog,
            scroll: UniformListScrollHandle::new(),
            asked: Rc::default(),
        });
        Self { shared, list }
    }

    /// The panel opens: its rows ask for their covers again (those given up may be due), and the
    /// current track comes into view.
    pub fn opened(&self, cx: &App) {
        if let Some(asked) = self.list.read(cx).asked.borrow_mut().as_mut() {
            asked.round = None;
        }
        self.reveal_current(cx);
    }

    /// Brings the current track into view.
    fn reveal_current(&self, cx: &App) {
        if let Some(i) = self.shared.read(cx).player.current {
            self.list
                .read(cx)
                .scroll
                .scroll_to_item(i, ScrollStrategy::Center);
        }
    }
}

impl Render for QueuePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (count, has_next) = {
            let p = &self.shared.read(cx).player;
            (p.tracks.len(), p.has_next())
        };
        let pal = palette(&self.shared, window, cx);
        let (close, clear) = (self.shared.clone(), self.shared.clone());
        v_flex()
            .size_full()
            .bg(pal.surface)
            .border_l_1()
            .border_color(Hsla { a: 0.10, ..pal.fg })
            .shadow_xl()
            .child(
                h_flex()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .gap_2()
                    .items_center()
                    .child(
                        v_flex()
                            .flex_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(pal.fg)
                                    .child("Queue"),
                            )
                            .child(div().text_xs().text_color(pal.secondary).child(count_text(
                                match count {
                                    0 => "Empty".to_string(),
                                    1 => "1 track".to_string(),
                                    n => format!("{n} tracks"),
                                },
                            ))),
                    )
                    .child(
                        IconButton::new("clear-next", IconName::ListX, pal)
                            .disabled(!has_next)
                            .tip("Clear what comes next")
                            .on_click(move |_, _, cx| {
                                clear.update(cx, |s, cx| s.send(Request::QueueClearAfter, cx))
                            }),
                    )
                    .child(
                        IconButton::new("close-queue", IconName::X, pal)
                            .tip("Close the queue")
                            .on_click(move |_, _, cx| {
                                close.update(cx, |s, cx| s.show_queue(false, cx))
                            }),
                    ),
            )
            .child(self.list.clone())
    }
}

/// The rows: a view of their own, with nothing animated inside a row, so that GPUI Fast keeps
/// the list on its scroll layer (`docs/gui-toolkit.md`, "Retained mode and animations").
pub struct QueueList {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    scroll: UniformListScrollHandle,
    /// The queue and the rows whose covers were last asked, so that a frame showing the same
    /// rows asks nothing.
    asked: Rc<RefCell<Option<Asked>>>,
}

/// Rows of a queue whose covers were asked, and the retry round of the cover cache then (`None`
/// once the panel opens again): a new one may ask again what was given up. The queue is held
/// weakly: its address cannot be reused by another while this is kept, so a new queue is never
/// taken for the old one.
struct Asked {
    queue: Weak<[QueueEntry]>,
    rows: Range<usize>,
    round: Option<u64>,
}

impl Asked {
    fn of(&self, queue: &Arc<[QueueEntry]>) -> bool {
        std::ptr::addr_eq(self.queue.as_ptr(), Arc::as_ptr(queue))
    }
}

/// A row being dragged: what the drop needs and what the pointer carries.
#[derive(Clone)]
struct Dragged {
    from: usize,
    /// The entry at `from` when the drag began.
    entry: u64,
    title: SharedString,
    pal: Palette,
}

impl Render for Dragged {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        h_flex()
            .w(px(300.))
            .h(px(ROW - 8.))
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

impl Render for QueueList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (tracks, player, thumbs, credited) = {
            let s = self.shared.read(cx);
            let p = &s.player;
            (
                p.tracks.clone(),
                p.clone(),
                s.thumbs.clone(),
                s.credits.clone(),
            )
        };
        let catalog = self.catalog.clone();
        let pal = palette(&self.shared, window, cx);
        let shared = self.shared.clone();
        let len = tracks.len();
        let asked = self.asked.clone();
        let covers = self.shared.read(cx).covers.clone();
        let round = covers.read(cx).retry_round();
        // the rows asked last ask again when covers given up may be due, or when the panel opens
        // again: a list that keeps its rows drawn (gpui-fast) may not render them again
        if let Some(a) = asked.borrow_mut().as_mut()
            && a.of(&tracks)
            && a.round != Some(round)
        {
            a.round = Some(round);
            let ids = thumbs::shown(&tracks, a.rows.clone());
            cx.defer(move |cx| covers.update(cx, |c, cx| c.want(ids, Use::SMALL, true, cx)));
        }
        uniform_list("queue-rows", len, move |range, window, cx| {
            // the rows shown ask their covers before anything that waits (#84). Asked later:
            // nothing here reads a file.
            let ids = thumbs::shown(&tracks, range.clone());
            let same = asked
                .borrow()
                .as_ref()
                .is_some_and(|a| a.of(&tracks) && a.rows == range);
            // the row measured alone asks nothing and leaves the key as it is
            if !ids.is_empty() && !same {
                *asked.borrow_mut() = Some(Asked {
                    queue: Arc::downgrade(&tracks),
                    rows: range.clone(),
                    round: Some(round),
                });
                let covers = shared.read(cx).covers.clone();
                cx.defer(move |cx| covers.update(cx, |c, cx| c.want(ids, Use::SMALL, true, cx)));
            }
            range
                .map(|ix| {
                    let t = &tracks[ix];
                    let row = player.row(ix);
                    // Found when the queue arrived (`Shared::follow_thumbs`); until then the
                    // cover the daemon gave, which costs no file access either.
                    let thumb: Option<PathBuf> = thumbs
                        .get(&t.entry)
                        .cloned()
                        .or_else(|| t.cover.clone().map(PathBuf::from));
                    let entry = t.entry;
                    let hover = hover(("queue-hover", entry), window, cx);
                    let (play, remove, dropped) = (shared.clone(), shared.clone(), shared.clone());
                    let accent = pal.accent;
                    hover
                        .track(h_flex().id(("queue-row", t.entry)))
                        .menu_starred(Item::entry(t), Spot::Queue { ix, entry }, Some(t.starred))
                        .h(px(ROW))
                        .w_full()
                        .px_2()
                        .gap_3()
                        .items_center()
                        .rounded_lg()
                        .cursor_pointer()
                        .when(row == Row::Current, |d| {
                            d.bg(Hsla {
                                a: 0.16,
                                ..pal.accent
                            })
                        })
                        .hover(|d| d.bg(Hsla { a: 0.07, ..pal.fg }))
                        .on_click(move |_, _, cx| {
                            play.update(cx, |s, cx| {
                                s.send_for_row(ix, entry, Request::QueuePlay { index: ix }, cx)
                            })
                        })
                        .on_drag(
                            Dragged {
                                from: ix,
                                entry,
                                title: t.title.clone().into(),
                                pal,
                            },
                            |d, _, _, cx| cx.new(|_| d.clone()),
                        )
                        // the line shows where the row will land
                        .drag_over::<Dragged>(move |style, d, _, _| {
                            if d.from < ix {
                                style.border_b_2().border_color(accent)
                            } else if d.from > ix {
                                style.border_t_2().border_color(accent)
                            } else {
                                style
                            }
                        })
                        .on_drop(move |d: &Dragged, _, cx| {
                            dropped.update(cx, |s, cx| {
                                if let Some((from, to)) =
                                    s.player.move_for((d.from, d.entry), (ix, entry))
                                {
                                    s.send(Request::QueueMove { from, to }, cx)
                                }
                            })
                        })
                        .child(
                            div()
                                .w(px(22.))
                                .flex()
                                .justify_center()
                                .tabular()
                                .text_xs()
                                .text_color(pal.secondary)
                                .map(|d| match row {
                                    Row::Current => d.child(
                                        Icon::new(IconName::AudioLines)
                                            .size(px(16.))
                                            .text_color(pal.accent),
                                    ),
                                    _ => d.child((ix + 1).to_string()),
                                }),
                        )
                        .child(match thumb {
                            Some(p) => img(p)
                                .size(px(40.))
                                .flex_none()
                                .rounded_md()
                                .object_fit(ObjectFit::Cover)
                                .into_any_element(),
                            None => div()
                                .size(px(40.))
                                .flex_none()
                                .rounded_md()
                                .bg(Hsla { a: 0.08, ..pal.fg })
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(IconName::Music)
                                        .size(px(16.))
                                        .text_color(pal.secondary),
                                )
                                .into_any_element(),
                        })
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_sm()
                                        .truncate()
                                        .text_color(pal.fg)
                                        .when(row == Row::Current, |d| {
                                            d.font_weight(FontWeight::SEMIBOLD)
                                        })
                                        .child(t.title.clone()),
                                )
                                .child({
                                    let artists = credits::of(&credited, &t.id, &t.artist, None);
                                    artist_links::line(&artists, |i, a| {
                                        let id = format!("queue-artist-{entry}-{i}");
                                        crate::tables::link(
                                            ElementId::Name(id.clone().into()),
                                            a.name.clone(),
                                            a.id.clone().map(Place::Artist),
                                            pal,
                                            &catalog,
                                        )
                                        .min_w_0()
                                        .debug_selector(move || id.clone())
                                        .into_any_element()
                                    })
                                    .text_xs()
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(pal.secondary)
                                .child(clock_text(t.duration)),
                        )
                        .child(
                            div()
                                .id(("queue-remove", t.entry))
                                .size(px(28.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .revealed(hover.look, true)
                                .hover(|s| s.bg(Hsla { a: 0.10, ..pal.fg }))
                                .tip("Remove from the queue", pal)
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    remove.update(cx, |s, cx| {
                                        s.send_for_row(
                                            ix,
                                            entry,
                                            Request::QueueRemove { index: ix },
                                            cx,
                                        )
                                    })
                                })
                                .child(
                                    Icon::new(IconName::X)
                                        .size(px(14.))
                                        .text_color(pal.secondary),
                                ),
                        )
                        .when(row == Row::Past, |d| d.opacity(0.5))
                })
                .collect()
        })
        .track_scroll(&self.scroll)
        .flex_1()
        .px_2()
        .pb_2()
    }
}

#[cfg(test)]
mod tests {
    use super::QueueList;
    use crate::catalog::Catalog;
    use gpui_kit::{
        AppContext as _, Entity, Modifiers, TestAppContext, UniformListScrollHandle,
        VisualTestContext,
    };
    use melodie_core::env::Env;
    use melodie_core::library::Credit;
    use melodie_core::proto::QueueEntry;
    use melodie_desktop::browse::Place;
    use melodie_desktop::model::Player;
    use std::sync::Arc;

    #[test]
    fn a_queued_track_of_three_artists_shows_three_links_each_to_its_own_page() {
        let dir = tempfile::tempdir().unwrap();
        let program = crate::app::fake_melodie(dir.path());
        let env = Env::with_home(dir.path().join("home"));
        let mut app = TestAppContext::single();
        let (view, cx): (Entity<QueueList>, &mut VisualTestContext) =
            app.add_window_view(|_, cx| {
                let shared = crate::app::bare(env.clone(), program.clone(), cx);
                let entry = |n: u64, id: &str, artist: &str| QueueEntry {
                    entry: n,
                    id: id.into(),
                    title: format!("Song {id}"),
                    artist: artist.into(),
                    ..QueueEntry::default()
                };
                shared.update(cx, |s, _| {
                    s.player = Player {
                        tracks: vec![
                            entry(1, "t1", "Nova Reyes, Lior Castell, Ada Marlow"),
                            entry(2, "t2", "Ilse Varga"),
                        ]
                        .into(),
                        ..Player::default()
                    };
                    let credit = |id: &str, name: &str| Credit {
                        id: Some(id.into()),
                        name: name.into(),
                    };
                    let three = vec![
                        credit("ar5", "Nova Reyes"),
                        credit("ar1", "Lior Castell"),
                        credit("ar3", "Ada Marlow"),
                    ];
                    s.credits = Arc::new([("t1".to_string(), three)].into());
                });
                let catalog = cx.new(|cx| Catalog::new(env.clone(), shared.clone(), cx));
                QueueList {
                    shared,
                    catalog,
                    scroll: UniformListScrollHandle::new(),
                    asked: Default::default(),
                }
            });
        cx.run_until_parked();
        let catalog = view.read_with(cx, |v, _| v.catalog.clone());
        let link = |cx: &mut VisualTestContext, entry: u64, ix: usize| {
            cx.debug_bounds(Box::leak(
                format!("queue-artist-{entry}-{ix}").into_boxed_str(),
            ))
        };
        assert!(link(cx, 1, 2).is_some() && link(cx, 1, 3).is_none());
        assert!(link(cx, 1, 0).unwrap().right() < link(cx, 1, 1).unwrap().left());
        // a track the copy does not credit: the daemon's name, one artist, no page to open
        assert!(link(cx, 2, 0).is_some() && link(cx, 2, 1).is_none());
        for (ix, id) in [(0, "ar5"), (1, "ar1"), (2, "ar3")] {
            let at = link(cx, 1, ix).unwrap().center();
            cx.simulate_click(at, Modifiers::none());
            cx.run_until_parked();
            let here = catalog.read_with(cx, |c, _| c.here().clone());
            assert_eq!(here, Place::Artist(id.into()), "artist {ix}");
        }
    }
}
