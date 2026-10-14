//! The dialogs: choosing a playlist to add to (and the duplicate question that may follow),
//! naming a playlist, confirming a deletion or a removal, the duplicates of a playlist shown
//! before they go. One at a time, modal, above everything; the card fades and rises in, and its
//! content fades when one dialog leads to the next.

use crate::app::Shared;
use crate::catalog::Catalog;
use crate::motion;
use crate::player::palette;
use crate::widgets::IconButton;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_desktop::edit::{Checked, Found, Next, Question};
use melodie_desktop::launch::{Adding, Job};
use melodie_desktop::theme::Palette;

/// Tracks named in a question or a confirmation; the rest are counted.
const NAMED: usize = 6;

#[derive(Clone)]
pub enum Dialog {
    /// Which playlist to add to (or a new one).
    Pick {
        what: Adding,
        about: String,
    },
    /// The playlist is read to see what is in it already.
    Checking {
        name: String,
    },
    /// Some of what is added is there already.
    Ask {
        question: Question,
    },
    /// The name of a new playlist (with what is being added, if anything).
    Create {
        with: Option<Adding>,
    },
    Rename {
        id: String,
        name: String,
    },
    Delete {
        id: String,
        name: String,
        tracks: usize,
    },
    /// Removing rows of a playlist: by rank (from 0) and the id seen there.
    Remove {
        id: String,
        name: String,
        picks: Vec<(usize, String)>,
        titles: Vec<String>,
    },
    /// The duplicates of a playlist, read from the server, then removed on demand.
    Duplicates {
        id: String,
        name: String,
        found: Option<Result<Found, String>>,
    },
}

impl Dialog {
    pub fn has_field(&self) -> bool {
        matches!(self, Self::Create { .. } | Self::Rename { .. })
    }
}

/// The dialog every view can open (`open`), kept in one entity read by one view only.
pub struct Dialogs {
    pub open: Option<Dialog>,
    /// What the card shows: the last dialog, kept while it fades out.
    shown: Option<Dialog>,
    /// Grows with every dialog shown, so its content fades in.
    step: u64,
    pub input: Entity<InputState>,
    field: Entity<Field>,
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    _subs: Vec<Subscription>,
}

/// Where the views find the dialogs: set once, never written again (reading it costs nothing).
pub struct DialogsHandle(pub Entity<Dialogs>);

impl Global for DialogsHandle {}

/// Opens a dialog from anywhere.
pub fn open(dialog: Dialog, window: &mut Window, cx: &mut App) {
    let d = cx.global::<DialogsHandle>().0.clone();
    d.update(cx, |d, cx| d.show(dialog, window, cx));
}

impl Dialogs {
    pub fn new(
        shared: Entity<Shared>,
        catalog: Entity<Catalog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Playlist name"));
        let subs = vec![
            cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.confirm(window, cx);
                }
            }),
        ];
        let field = cx.new(|_| Field {
            input: input.clone(),
            shared: shared.clone(),
        });
        Self {
            open: None,
            shown: None,
            step: 0,
            input,
            field,
            shared,
            catalog,
            _subs: subs,
        }
    }

    pub fn show(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        let name = match &dialog {
            Dialog::Create { .. } => Some(String::new()),
            Dialog::Rename { name, .. } => Some(name.clone()),
            _ => None,
        };
        if let Dialog::Duplicates {
            id, found: None, ..
        } = &dialog
        {
            self.read_duplicates(id.clone(), cx);
        }
        self.step += 1;
        self.open = Some(dialog.clone());
        self.shown = Some(dialog);
        if let Some(name) = name {
            self.input.update(cx, |i, cx| {
                i.set_value(name, window, cx);
                i.focus(window, cx);
                i.select_all(window, cx)
            });
        }
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = None;
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn field_focused(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    /// What Enter does: the main button of the dialog.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.open.clone() else {
            return;
        };
        let name = self.input.read(cx).value().trim().to_string();
        let edit = |job: Job, said: Option<String>, this: &mut Self, cx: &mut Context<Self>| {
            this.close(cx);
            this.catalog.update(cx, |c, cx| c.edit(job, said, cx));
        };
        match dialog {
            Dialog::Create { with } if !name.is_empty() => {
                let said = format!("Created {name}");
                edit(Job::CreatePlaylist { name, with }, Some(said), self, cx)
            }
            Dialog::Rename { id, name: old } if !name.is_empty() && name != old => {
                let said = format!("Renamed {old} to {name}");
                edit(Job::RenamePlaylist { id, name }, Some(said), self, cx)
            }
            Dialog::Rename { .. } if !name.is_empty() => self.close(cx),
            Dialog::Delete { id, name, .. } => {
                let said = format!("Deleted {name}");
                edit(Job::DeletePlaylist { id }, Some(said), self, cx)
            }
            Dialog::Remove {
                id, name, picks, ..
            } => {
                let said = match picks.len() {
                    1 => format!("Removed 1 track from {name}"),
                    n => format!("Removed {n} tracks from {name}"),
                };
                edit(Job::RemoveFromPlaylist { id, picks }, Some(said), self, cx)
            }
            // the ranks shown, each checked to hold the track shown: nothing else goes
            Dialog::Duplicates {
                id,
                found: Some(Ok(f)),
                ..
            } => {
                if let Some((job, said)) = f.remove(&id) {
                    edit(job, Some(said), self, cx)
                }
            }
            // skipping is the answer that adds nothing twice
            Dialog::Ask { question } => self.answer(question.skip.1, window, cx),
            _ => {}
        }
    }

    fn answer(&mut self, job: Option<Job>, _: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
        if let Some(job) = job {
            self.catalog.update(cx, |c, cx| c.edit(job, None, cx));
        }
    }

    /// A playlist was chosen (here or in a menu): what is in it already is read first, then
    /// added or asked about.
    pub fn chose(
        &mut self,
        playlist: String,
        name: String,
        what: Adding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show(Dialog::Checking { name }, window, cx);
        let this = cx.entity();
        let job = Job::CheckAdd {
            playlist: playlist.clone(),
            what: what.clone(),
        };
        self.shared.update(cx, |s, cx| {
            s.run_then(job, cx, move |r, cx| {
                this.update(cx, |d, cx| {
                    // closed meanwhile: the user changed their mind
                    if !matches!(d.open, Some(Dialog::Checking { .. })) {
                        return;
                    }
                    match r.and_then(|out| Checked::read(&out)) {
                        Ok(c) => match c.next(&playlist, &what) {
                            Next::Run(job) => {
                                d.close(cx);
                                d.catalog.update(cx, |c, cx| c.edit(job, None, cx));
                            }
                            Next::Ask(question) => {
                                d.step += 1;
                                d.open = Some(Dialog::Ask {
                                    question: question.clone(),
                                });
                                d.shown = d.open.clone();
                                cx.notify();
                            }
                        },
                        Err(why) => {
                            d.close(cx);
                            d.shared.update(cx, |s, cx| s.say(why, cx));
                        }
                    }
                })
            })
        });
    }

    fn read_duplicates(&mut self, id: String, cx: &mut Context<Self>) {
        let this = cx.entity();
        let job = Job::Duplicates {
            id: id.clone(),
            remove: false,
        };
        self.shared.update(cx, |s, cx| {
            s.run_then(job, cx, move |r, cx| {
                this.update(cx, |d, cx| {
                    if let Some(Dialog::Duplicates {
                        id: open, found, ..
                    }) = &mut d.open
                        && *open == id
                    {
                        *found = Some(r.and_then(|out| Found::read(&out)));
                        d.step += 1;
                        d.shown = d.open.clone();
                        cx.notify();
                    }
                })
            })
        });
    }
}

/// The name field, a view of its own: its caret blinks without redrawing the card.
struct Field {
    input: Entity<InputState>,
    shared: Entity<Shared>,
}

impl Render for Field {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        Input::new(&self.input)
            .prefix(
                Icon::new(IconName::ListMusic)
                    .size(px(16.))
                    .text_color(pal.secondary),
            )
            .h(px(40.))
    }
}

fn title(text: impl Into<SharedString>, pal: Palette) -> Div {
    div()
        .text_lg()
        .font_weight(FontWeight::BOLD)
        .text_color(pal.fg)
        .child(text.into())
}

fn line(text: impl Into<SharedString>, pal: Palette) -> Div {
    div().text_sm().text_color(pal.secondary).child(text.into())
}

/// Some names, one per line, and how many more.
fn names(list: &[String], pal: Palette) -> Div {
    let more = list.len().saturating_sub(NAMED);
    v_flex()
        .gap_1()
        .py_1()
        .children(list.iter().take(NAMED).map(|t| {
            div()
                .text_sm()
                .text_color(pal.fg)
                .truncate()
                .child(format!("· {t}"))
        }))
        .when(more > 0, |d| d.child(line(format!("and {more} more"), pal)))
}

/// The buttons at the bottom of a card, the main one last.
fn buttons(children: Vec<IconButton>) -> Div {
    h_flex()
        .pt_3()
        .gap_2()
        .justify_end()
        .flex_wrap()
        .children(children)
}

impl Render for Dialogs {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = motion::flag(
            "dialog-open",
            self.open.is_some(),
            motion::PANEL,
            window,
            cx,
        );
        // 0 is also where an opening starts: only a closed dialog is forgotten
        if t == 0.0 && self.open.is_none() {
            self.shown = None;
            return div().into_any_element();
        }
        let Some(dialog) = self.shown.clone() else {
            return div().into_any_element();
        };
        let pal = palette(&self.shared, window, cx);
        let me = cx.entity();
        let close = {
            let me = me.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| me.update(cx, |d, cx| d.close(cx))
        };
        let confirm = {
            let me = me.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                me.update(cx, |d, cx| d.confirm(window, cx))
            }
        };
        let cancel = |label: &'static str| {
            IconButton::new("dialog-cancel", IconName::X, pal)
                .label(label)
                .diameter(36.)
                .on_click(close.clone())
        };
        let body: Div = match dialog {
            Dialog::Pick { what, about } => {
                let lists = self.catalog.read(cx).own_playlists();
                let new = {
                    let (me, what) = (me.clone(), what.clone());
                    h_flex()
                        .id("pick-new")
                        .px_2()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .rounded_lg()
                        .cursor_pointer()
                        .text_sm()
                        .text_color(pal.accent)
                        .hover(|s| s.bg(Hsla { a: 0.08, ..pal.fg }))
                        .on_click(move |_, window, cx| {
                            let with = Some(what.clone());
                            me.update(cx, |d, cx| d.show(Dialog::Create { with }, window, cx))
                        })
                        .child(Icon::new(IconName::Plus).size(px(16.)))
                        .child("New playlist…")
                };
                v_flex()
                    .gap_1()
                    .child(title("Add to playlist", pal))
                    .child(line(about, pal))
                    .child(
                        v_flex()
                            .id("pick-list")
                            .mt_2()
                            .max_h(px(300.))
                            .overflow_y_scroll()
                            .gap_1()
                            .child(new)
                            .children(lists.into_iter().map(|p| {
                                let (me, what) = (me.clone(), what.clone());
                                let (pid, name) = (p.id.clone(), p.name.clone());
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
                                    .on_click(move |_, window, cx| {
                                        let (pid, name, what) =
                                            (pid.clone(), name.clone(), what.clone());
                                        me.update(cx, |d, cx| d.chose(pid, name, what, window, cx))
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
                    .child(buttons(vec![cancel("Cancel")]))
            }
            Dialog::Checking { name } => v_flex()
                .gap_1()
                .child(title("Add to playlist", pal))
                .child(line(format!("Looking at what {name} holds already…"), pal))
                .child(buttons(vec![cancel("Cancel")])),
            Dialog::Ask { question } => {
                let (me1, me2) = (me.clone(), me.clone());
                let (skip, anyway) = (question.skip.clone(), question.anyway.clone());
                v_flex()
                    .gap_1()
                    .child(title(question.title.clone(), pal))
                    .child(line("Already there:", pal))
                    .child(names(&question.already, pal))
                    .child(buttons(vec![
                        IconButton::new("ask-anyway", IconName::Copy, pal)
                            .label(anyway.0)
                            .diameter(36.)
                            .on_click(move |_, window, cx| {
                                let job = Some(anyway.1.clone());
                                me1.update(cx, |d, cx| d.answer(job, window, cx))
                            }),
                        IconButton::new("ask-skip", IconName::Check, pal)
                            .label(skip.0)
                            .primary()
                            .diameter(36.)
                            .on_click(move |_, window, cx| {
                                let job = skip.1.clone();
                                me2.update(cx, |d, cx| d.answer(job, window, cx))
                            }),
                    ]))
            }
            Dialog::Create { .. } | Dialog::Rename { .. } => {
                let rename = matches!(dialog, Dialog::Rename { .. });
                v_flex()
                    .gap_2()
                    .child(title(
                        if rename {
                            "Rename the playlist"
                        } else {
                            "New playlist"
                        },
                        pal,
                    ))
                    .when_some(
                        match &dialog {
                            Dialog::Create { with: Some(_) } => {
                                Some("What you are adding goes in it")
                            }
                            _ => None,
                        },
                        |d, about| d.child(line(about, pal)),
                    )
                    .child(self.field.clone())
                    .child(buttons(vec![
                        cancel("Cancel"),
                        IconButton::new("dialog-ok", IconName::Check, pal)
                            .label(if rename { "Rename" } else { "Create" })
                            .primary()
                            .diameter(36.)
                            .on_click(confirm.clone()),
                    ]))
            }
            Dialog::Delete { name, tracks, .. } => v_flex()
                .gap_1()
                .child(title(format!("Delete {name}?"), pal))
                .child(line(
                    match tracks {
                        0 => "The playlist goes from the server. Its tracks stay in the library."
                            .to_string(),
                        1 => "The playlist and its 1 track go from the server. The track stays in the library."
                            .to_string(),
                        n => format!(
                            "The playlist and its {n} tracks go from the server. The tracks stay in the library."
                        ),
                    },
                    pal,
                ))
                .child(buttons(vec![
                    cancel("Keep it"),
                    IconButton::new("dialog-ok", IconName::Trash, pal)
                        .label("Delete")
                        .primary()
                        .diameter(36.)
                        .on_click(confirm.clone()),
                ])),
            Dialog::Remove {
                name,
                picks,
                titles,
                ..
            } => v_flex()
                .gap_1()
                .child(title(
                    match picks.len() {
                        1 => format!("Remove 1 track from {name}?"),
                        n => format!("Remove {n} tracks from {name}?"),
                    },
                    pal,
                ))
                .child(names(&titles, pal))
                .child(line("They stay in the library.", pal))
                .child(buttons(vec![
                    cancel("Cancel"),
                    IconButton::new("dialog-ok", IconName::ListMinus, pal)
                        .label("Remove")
                        .primary()
                        .diameter(36.)
                        .on_click(confirm.clone()),
                ])),
            Dialog::Duplicates { name, found, .. } => {
                let head = title(format!("Duplicates in {name}"), pal);
                match found {
                    None => v_flex()
                        .gap_1()
                        .child(head)
                        .child(line("Reading the playlist…", pal))
                        .child(buttons(vec![cancel("Cancel")])),
                    Some(Err(why)) => v_flex()
                        .gap_1()
                        .child(head)
                        .child(line(why, pal))
                        .child(buttons(vec![cancel("Close")])),
                    Some(Ok(f)) => {
                        let row = |r: &melodie_desktop::edit::Repeat| {
                            format!(
                                "#{} {} · {} (as #{})",
                                r.rank, r.title, r.artist, r.first
                            )
                        };
                        let exact: Vec<String> = f.exact.iter().map(row).collect();
                        let similar: Vec<String> = f.similar.iter().map(row).collect();
                        let label = f.remove_label();
                        v_flex()
                            .gap_1()
                            .child(head)
                            .when(exact.is_empty() && similar.is_empty(), |d| {
                                d.child(line(
                                    format!("No duplicates among its {} tracks.", f.total),
                                    pal,
                                ))
                            })
                            .when(!exact.is_empty(), |d| {
                                d.child(line(
                                    "The same track again: these go, the first one stays.",
                                    pal,
                                ))
                                .child(names(&exact, pal))
                            })
                            .when(!similar.is_empty(), |d| {
                                d.child(line(
                                    "Likely the same (same title and artist, another track): these stay.",
                                    pal,
                                ))
                                .child(names(&similar, pal))
                            })
                            .child(buttons(match label {
                                None => vec![cancel("Close")],
                                Some(label) => vec![
                                    cancel("Cancel"),
                                    IconButton::new("dialog-ok", IconName::CopyX, pal)
                                        .label(label)
                                        .primary()
                                        .diameter(36.)
                                        .on_click(confirm.clone()),
                                ],
                            }))
                    }
                }
            }
        };
        let step = self.step;
        div()
            .id("dialog-scrim")
            .absolute()
            .inset_0()
            // modal: nothing under it takes the pointer
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.40 * t))
            .on_mouse_down(MouseButton::Left, {
                let me = me.clone();
                move |_, _, cx| me.update(cx, |d, cx| d.close(cx))
            })
            .child(
                div()
                    .id("dialog-card")
                    .w(px(440.))
                    .max_w(relative(0.92))
                    .mt(px(16. * (1. - t)))
                    .opacity(t)
                    .p_5()
                    .rounded_xl()
                    .bg(pal.surface)
                    .border_1()
                    .border_color(Hsla { a: 0.10, ..pal.fg })
                    .shadow_xl()
                    // a click inside does not close it
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(div().id(("dialog-body", step)).child(body).with_animation(
                        ("dialog-body", step),
                        motion::enter(),
                        |el, t| el.opacity(t),
                    )),
            )
            .into_any_element()
    }
}
