//! The search: one field, the results grouped by kind as the user types, and every result of one
//! kind on a page of its own. The local copy is read away from the window's thread, and the
//! answer to an older keystroke is dropped (`find::Latest`).

use crate::albums::{PAD, cover_box, empty_state};
use crate::app::Shared;
use crate::catalog::{Catalog, Empty};
use crate::motion;
use crate::pages::picker;
use crate::player::palette;
use crate::tables::row_button;
use crate::widgets::IconButton;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::env::Env;
use melodie_core::search::{self, Hit};
use melodie_desktop::browse::Place;
use melodie_desktop::find::{self, Action, Answer, Latest};
use melodie_desktop::launch::Kind;
use melodie_desktop::theme::Palette;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

const ROW: f32 = 56.0;

pub struct SearchView {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    env: Env,
    pub input: Entity<InputState>,
    field: Entity<Field>,
    /// The text of the field, as last changed.
    text: String,
    latest: Latest,
    /// The kind of every result asked last, to ask again when another is opened.
    asked_all: Option<Kind>,
    answer: Option<Arc<Answer>>,
    empty: Empty,
    /// The result whose playlist picker is open.
    pick: Option<(Kind, String)>,
    scroll: ScrollHandle,
    all: UniformListScrollHandle,
    /// The step of the place last seen, to focus the field when the search is opened.
    step: u64,
    _subs: Vec<Subscription>,
}

impl SearchView {
    pub fn new(
        shared: Entity<Shared>,
        catalog: Entity<Catalog>,
        env: Env,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Artists, albums, tracks, playlists")
        });
        let subs = vec![
            cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.text = input.read(cx).value().to_string();
                    this.ask(cx)
                }
            }),
            cx.observe_in(&catalog, window, |this, catalog, window, cx| {
                let (here, step) = {
                    let c = catalog.read(cx);
                    (c.here().clone(), c.nav.step)
                };
                if step == this.step {
                    return;
                }
                this.step = step;
                match here {
                    Place::Search => this.focus(window, cx),
                    Place::Found(kind) => {
                        this.all.scroll_to_item(0, ScrollStrategy::Top);
                        if this.asked_all != Some(kind) {
                            this.ask(cx)
                        }
                    }
                    _ => {}
                }
            }),
        ];
        let field = cx.new(|_| Field {
            input: input.clone(),
            shared: shared.clone(),
        });
        Self {
            shared,
            catalog,
            env,
            input,
            field,
            text: String::new(),
            latest: Latest::default(),
            asked_all: None,
            answer: None,
            empty: Empty::Loading,
            pick: None,
            scroll: ScrollHandle::new(),
            all: UniformListScrollHandle::new(),
            step: 0,
            _subs: subs,
        }
    }

    /// Puts the cursor in the field, its text selected: typing replaces it.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx)
        });
    }

    pub fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Empties the field; false when it was empty already.
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.text.is_empty() {
            return false;
        }
        // no change event for this one
        self.input.update(cx, |i, cx| i.set_value("", window, cx));
        self.text.clear();
        self.ask(cx);
        true
    }

    /// Reads the local copy for the text of the field, away from the window.
    fn ask(&mut self, cx: &mut Context<Self>) {
        let text = self.text.clone();
        let all = match self.catalog.read(cx).here() {
            Place::Found(kind) => Some(*kind),
            _ => None,
        };
        self.asked_all = all;
        let n = self.latest.ask();
        let env = self.env.clone();
        let started = Instant::now();
        let task = cx.background_spawn(async move { find::answer(&env, &text, all) });
        cx.spawn(async move |this, cx| {
            let answer = task.await;
            let _ = this.update(cx, |this, cx| {
                if !this.latest.is_latest(n) {
                    return;
                }
                match answer {
                    Ok(Some(a)) => {
                        this.want_covers(&a, cx);
                        this.answer = Some(Arc::new(a));
                        this.empty = Empty::Loading;
                    }
                    Ok(None) => this.empty = Empty::NoCopy,
                    Err(_) => this.empty = Empty::Failed,
                }
                if crate::timing::on() {
                    crate::timing::note("search", started.elapsed());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The covers of what is shown; of every track only those known already (there may be
    /// thousands).
    fn want_covers(&self, a: &Answer, cx: &mut Context<Self>) {
        let shown = a.groups.iter().flat_map(|g| g.hits.iter());
        let all = a
            .all
            .iter()
            .filter(|(kind, _)| *kind != Kind::Track)
            .flat_map(|(_, hits)| hits.iter());
        let ids: Vec<String> = shown.chain(all).filter_map(|h| h.cover.clone()).collect();
        self.catalog.update(cx, |c, cx| c.want_covers(ids, cx));
    }
}

/// The field, a view of its own: its blinking caret redraws it alone, not the results beside it.
struct Field {
    input: Entity<InputState>,
    shared: Entity<Shared>,
}

impl Render for Field {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        Input::new(&self.input)
            .prefix(
                Icon::new(IconName::Search)
                    .size(px(18.))
                    .text_color(pal.secondary),
            )
            .cleanable(true)
            .h(px(44.))
    }
}

fn kind_title(kind: search::Kind) -> &'static str {
    match kind {
        search::Kind::Playlist => "Playlists",
        search::Kind::Artist => "Artists",
        search::Kind::Album => "Albums",
        search::Kind::Track => "Tracks",
    }
}

fn icon(action: Action) -> IconName {
    match action {
        Action::Play => IconName::Play,
        Action::Shuffle => IconName::Shuffle,
        Action::Next => IconName::ListStart,
        Action::Last => IconName::ListEnd,
        Action::Open => IconName::Disc3,
        Action::AddToPlaylist => IconName::ListPlus,
    }
}

/// Does what a result's action says: a job of `melodie`, its page, or the playlist picker.
fn act(
    action: Action,
    hit: &Hit,
    view: &Entity<SearchView>,
    catalog: &Entity<Catalog>,
    cx: &mut App,
) {
    if let Some(job) = find::job(action, hit) {
        catalog.update(cx, |c, cx| c.run(job, cx));
    } else if action == Action::Open {
        if let Some(place) = find::page(hit) {
            catalog.update(cx, |c, cx| c.go(place, cx));
        }
    } else {
        let pick = (Kind::from(hit.kind), hit.id.clone());
        view.update(cx, |v, cx| {
            v.pick = Some(pick);
            cx.notify()
        });
        catalog.update(cx, |c, cx| {
            c.picker_open = true;
            cx.notify()
        });
    }
}

/// One result: its picture, its name, what it is; a click plays a track and opens the rest; on
/// hover, its other actions. Nothing in it is animated, so that a long list keeps its scroll
/// layer.
fn result_row(
    list: &'static str,
    ix: usize,
    hit: &Hit,
    covers: &HashMap<String, PathBuf>,
    pal: Palette,
    view: &Entity<SearchView>,
    catalog: &Entity<Catalog>,
) -> Stateful<Div> {
    let key = |what: &str| ElementId::Name(format!("{list}-{what}-{ix}").into());
    let round = if hit.kind == search::Kind::Artist {
        20.
    } else {
        6.
    };
    let (h, v, c) = (Arc::new(hit.clone()), view.clone(), catalog.clone());
    h_flex()
        .id(key("row"))
        .group(list)
        .h(px(ROW))
        .flex_none()
        .w_full()
        .px(px(PAD))
        .on_click(move |_, _, cx| act(find::click(&h), &h, &v, &c, cx))
        .child(
            h_flex()
                .size_full()
                .px_2()
                .gap_3()
                .items_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(Hsla { a: 0.07, ..pal.fg }))
                .child(cover_box(
                    hit.cover.as_ref().and_then(|c| covers.get(c)),
                    40.,
                    round,
                    pal,
                ))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .text_color(pal.fg)
                                .child(hit.name.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(pal.secondary)
                                .child(find::about(hit)),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .invisible()
                        .group_hover(list, |s| s.visible())
                        .children(find::buttons(hit).into_iter().map(|a| {
                            let (h, v, c) = (Arc::new(hit.clone()), view.clone(), catalog.clone());
                            row_button(key(&format!("{a:?}")), icon(a), pal, move |cx| {
                                act(a, &h, &v, &c, cx)
                            })
                        })),
                ),
        )
}

fn hint(id: &'static str, text: String, pal: Palette) -> AnyElement {
    div()
        .id(id)
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(pal.secondary)
        .child(text)
        .with_animation(id, motion::enter(), |el, t| el.opacity(t))
        .into_any_element()
}

impl Render for SearchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let (here, covers, can_back, picker_open) = {
            let c = self.catalog.read(cx);
            (
                c.here().clone(),
                c.covers.clone(),
                c.nav.can_go_back(),
                c.picker_open,
            )
        };
        let text = self.text.clone();
        let view = cx.entity();
        let catalog = &self.catalog;
        let pk = motion::flag(
            "search-picker",
            picker_open && self.pick.is_some(),
            motion::PANEL,
            window,
            cx,
        );
        let field = h_flex()
            .px(px(PAD))
            .pt_4()
            .pb_3()
            .gap_2()
            .items_center()
            .when(can_back, |d| {
                let catalog = catalog.clone();
                d.child(
                    IconButton::new("search-back", IconName::ChevronLeft, pal)
                        .on_click(move |_, _, cx| catalog.update(cx, |c, cx| c.back(cx))),
                )
            })
            .child(div().flex_1().min_w_0().child(self.field.clone()));
        let answer = self.answer.clone().filter(|_| !text.trim().is_empty());
        let body: AnyElement = match (&here, answer) {
            _ if text.trim().is_empty() => hint(
                "search-hint",
                "Search artists, albums, tracks and playlists (Ctrl+F or / from anywhere)".into(),
                pal,
            ),
            (_, None) => empty_state(self.empty, "", pal).into_any_element(),
            (Place::Found(kind), Some(a)) => match a.all.as_ref().filter(|(k, _)| k == kind) {
                None => div().into_any_element(),
                Some((kind, hits)) => {
                    let hits = Arc::clone(hits);
                    let (v, c) = (view.clone(), catalog.clone());
                    v_flex()
                        .size_full()
                        .child(
                            h_flex()
                                .px(px(PAD))
                                .pb_2()
                                .gap_3()
                                .items_baseline()
                                .child(
                                    div()
                                        .text_2xl()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(pal.fg)
                                        .child(kind_title((*kind).into())),
                                )
                                .child(div().text_sm().text_color(pal.secondary).child(format!(
                                    "{} for “{}”",
                                    hits.len(),
                                    a.text
                                ))),
                        )
                        .child(
                            uniform_list("search-all", hits.len(), move |range, _, _| {
                                range
                                    .map(|ix| {
                                        result_row(
                                            "search-all",
                                            ix,
                                            &hits[ix],
                                            &covers,
                                            pal,
                                            &v,
                                            &c,
                                        )
                                    })
                                    .collect()
                            })
                            .track_scroll(&self.all)
                            .flex_1(),
                        )
                        .into_any_element()
                }
            },
            (_, Some(a)) if a.groups.is_empty() => hint(
                "search-none",
                format!("Nothing found for “{}”", a.text),
                pal,
            ),
            (_, Some(a)) => v_flex()
                .id("search-groups")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .pb_6()
                .children(a.groups.iter().map(|g| {
                    let list = match g.kind {
                        search::Kind::Playlist => "found-playlist",
                        search::Kind::Artist => "found-artist",
                        search::Kind::Album => "found-album",
                        search::Kind::Track => "found-track",
                    };
                    let (kind, c) = (Kind::from(g.kind), catalog.clone());
                    v_flex()
                        .flex_none()
                        .child(
                            h_flex()
                                .px(px(PAD))
                                .pt_4()
                                .pb_2()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(pal.fg)
                                        .child(kind_title(g.kind)),
                                )
                                .when(g.more, |d| {
                                    d.child(
                                        div()
                                            .id(ElementId::Name(format!("{list}-all").into()))
                                            .px_3()
                                            .py_1()
                                            .rounded_full()
                                            .cursor_pointer()
                                            .text_sm()
                                            .text_color(pal.secondary)
                                            .hover(|s| {
                                                s.text_color(pal.fg).bg(Hsla { a: 0.07, ..pal.fg })
                                            })
                                            .on_click(move |_, _, cx| {
                                                c.update(cx, |c, cx| c.go(Place::Found(kind), cx))
                                            })
                                            .child("See all"),
                                    )
                                }),
                        )
                        .children(
                            g.hits.iter().enumerate().map(|(ix, h)| {
                                result_row(list, ix, h, &covers, pal, &view, catalog)
                            }),
                        )
                }))
                .with_animation("search-groups", motion::enter(), |el, t| el.opacity(t))
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .relative()
            .child(field)
            .child(div().flex_1().min_h_0().child(body))
            .when(pk > 0.0, |d| {
                let close = catalog.clone();
                let (kind, id) = self.pick.clone().unwrap_or((Kind::Track, String::new()));
                d.child(
                    div()
                        .id("search-picker-scrim")
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
                                .top(px(72.))
                                .left_0()
                                .child(picker(kind, &id, pk, pal, catalog, cx)),
                        ),
                )
            })
    }
}
