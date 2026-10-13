//! The player, in the spirit of Amberol: the cover large, the track, the position, a few buttons.

use crate::app::{Shared, Shown};
use crate::dialogs::{self, Dialog};
use crate::motion;
use crate::widgets::IconButton;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::proto::Request;
use melodie_core::track::clock;
use melodie_desktop::launch::{Adding, Job};
use melodie_desktop::layout;
use melodie_desktop::model::{Clock, PlayState, VOLUME_SLIDER_MAX, volume_percent};
use melodie_desktop::theme::Palette;
use std::time::{Duration, Instant};

/// The palette on screen now, moving towards the target of the model.
pub fn palette(shared: &Entity<Shared>, window: &mut Window, cx: &mut App) -> Palette {
    let target = shared.read(cx).target_palette();
    motion::to("palette", target, motion::COVER, window, cx)
}

/// How often the position moves while playing: often enough for the eye, cheap enough to
/// leave the window open for hours (`docs/gui-toolkit.md`, "Frame times").
const TICK: Duration = Duration::from_millis(250);
/// The volume is sent at most this often while it is dragged.
const VOLUME_EVERY: Duration = Duration::from_millis(60);

pub struct PlayerView {
    shared: Entity<Shared>,
    bar: Entity<Bar>,
    volume: Entity<SliderState>,
    volume_dragged: bool,
    volume_sent: Option<Instant>,
    _subs: Vec<Subscription>,
}

impl PlayerView {
    pub fn new(
        shared: Entity<Shared>,
        clock: Entity<Clock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let bar = cx.new(|cx| Bar::new(shared.clone(), clock, window, cx));
        let v = shared.read(cx).player.volume;
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(VOLUME_SLIDER_MAX)
                .step(1.)
                .default_value(volume_percent(v))
        });
        let subs = vec![
            cx.subscribe_in(&volume, window, |this, _, ev: &SliderEvent, _, cx| {
                let (value, done) = match ev {
                    SliderEvent::Change(v) => (v.end(), false),
                    SliderEvent::Release(v) => (v.end(), true),
                };
                this.volume_dragged = !done;
                let due = this.volume_sent.is_none_or(|t| t.elapsed() >= VOLUME_EVERY);
                if done || due {
                    this.volume_sent = Some(Instant::now());
                    let volume = f64::from(value) / 100.0;
                    this.shared
                        .update(cx, |s, cx| s.send(Request::SetVolume { volume }, cx));
                }
            }),
            cx.observe_in(&shared, window, |this, shared, window, cx| {
                if this.volume_dragged {
                    return;
                }
                let v = volume_percent(shared.read(cx).player.volume);
                if this.volume.read(cx).value().end() != v {
                    this.volume.update(cx, |s, cx| s.set_value(v, window, cx));
                }
            }),
        ];
        Self {
            shared,
            bar,
            volume,
            volume_dragged: false,
            volume_sent: None,
            _subs: subs,
        }
    }
}

/// A cover, or its place while there is none.
fn cover(shown: Option<&Shown>, size: Pixels, pal: Palette) -> AnyElement {
    match shown.and_then(|s| s.image.clone()) {
        Some(image) => img(image)
            .size(size)
            .object_fit(ObjectFit::Cover)
            .rounded(px(14.))
            .into_any_element(),
        None => div()
            .size(size)
            .rounded(px(14.))
            .bg(pal.surface)
            .flex()
            .items_center()
            .justify_center()
            .child(
                Icon::new(IconName::Music)
                    .size(size * 0.28)
                    .text_color(Hsla { a: 0.35, ..pal.fg }),
            )
            .into_any_element(),
    }
}

impl Render for PlayerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (track, state, following, has_tracks, step, queue_open) = {
            let s = self.shared.read(cx);
            (
                s.player.track().cloned(),
                s.player.state,
                s.player.following,
                !s.player.tracks.is_empty(),
                s.art.step,
                s.queue_open,
            )
        };
        let pal = palette(&self.shared, window, cx);
        // The cover cross-fades through the steps: two quick changes keep moving, never jump.
        let at = motion::to("cover-step", step as f32, motion::COVER, window, cx);
        let (lo, hi) = (at.floor() as u64, at.ceil() as u64);
        let t = at - at.floor();
        let (under, over) = {
            let art = &self.shared.read(cx).art;
            (art.at(lo).cloned(), art.at(hi).cloned())
        };

        let viewport = window.viewport_size();
        let w = viewport.width.as_f32();
        let width = layout::main_width(w, crate::sidebar::width(w), queue_open);
        let side = (width - 64.0)
            .min(viewport.height.as_f32() - 340.0)
            .clamp(160.0, 440.0);
        let size = px(side);

        let playing = state == PlayState::Playing;
        let shared = self.shared.clone();
        let (title, byline, quality, starred, id) = match &track {
            Some(t) => {
                let byline = [t.artist.as_str(), t.album.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                (
                    t.title.clone(),
                    byline,
                    t.quality.clone(),
                    t.starred,
                    Some(t.id.clone()),
                )
            }
            None => (
                "Nothing playing".to_string(),
                if following && has_tracks {
                    "Press play to start the queue again".to_string()
                } else {
                    "Press play for your mix".to_string()
                },
                String::new(),
                false,
                None,
            ),
        };
        let picked = (id.clone(), title.clone());

        let play = {
            let shared = shared.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                shared.update(cx, |s, cx| s.play_or_pause(cx))
            }
        };
        let has_track = track.is_some();

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .px_8()
            .gap_5()
            .child(
                div()
                    .relative()
                    .size(size)
                    .rounded(px(14.))
                    .shadow_xl()
                    .child(cover(under.as_ref(), size, pal))
                    .when(hi != lo, |d| {
                        d.child(div().absolute().top_0().left_0().opacity(t).child(cover(
                            over.as_ref(),
                            size,
                            pal,
                        )))
                    }),
            )
            .child(
                v_flex()
                    .id(("track-text", step))
                    .w_full()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(pal.fg)
                            .max_w_full()
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_color(pal.secondary)
                            .max_w_full()
                            .truncate()
                            .child(byline),
                    )
                    .when(!quality.is_empty(), |d| {
                        d.child(
                            div()
                                .mt_1()
                                .px_2()
                                .py(px(2.))
                                .rounded_full()
                                .border_1()
                                .border_color(Hsla { a: 0.18, ..pal.fg })
                                .text_xs()
                                .text_color(pal.secondary)
                                .child(quality),
                        )
                    })
                    .with_animation(("track-text-enter", step), motion::enter(), |el, t| {
                        el.opacity(t).mt(px((1.0 - t) * 8.0))
                    }),
            )
            .child(div().w(size.max(px(280.))).child(self.bar.clone()))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child({
                        let shared = shared.clone();
                        IconButton::new("heart", IconName::Heart, pal)
                            .selected(starred)
                            .disabled(!has_track)
                            .on_click(move |_, _, cx| {
                                if let Some(id) = id.clone() {
                                    shared.update(cx, |s, cx| {
                                        s.run(
                                            Job::Star {
                                                id,
                                                starred: !starred,
                                            },
                                            cx,
                                        )
                                    })
                                }
                            })
                    })
                    .child({
                        let (id, title) = picked;
                        IconButton::new("add-current", IconName::ListPlus, pal)
                            .disabled(!has_track)
                            .on_click(move |_, window, cx| {
                                // the id seen now, not "current": the track may change meanwhile
                                if let Some(id) = id.clone() {
                                    let dialog = Dialog::Pick {
                                        what: Adding::Tracks(vec![id]),
                                        about: title.to_string(),
                                    };
                                    dialogs::open(dialog, window, cx)
                                }
                            })
                    })
                    .child({
                        let shared = shared.clone();
                        IconButton::new("prev", IconName::SkipBack, pal)
                            .diameter(48.)
                            .disabled(!has_track)
                            .on_click(move |_, _, cx| {
                                shared.update(cx, |s, cx| s.send(Request::Prev, cx))
                            })
                    })
                    .child(
                        IconButton::new(
                            "play",
                            if playing {
                                IconName::Pause
                            } else {
                                IconName::Play
                            },
                            pal,
                        )
                        .diameter(64.)
                        .primary()
                        .on_click(play),
                    )
                    .child({
                        let shared = shared.clone();
                        IconButton::new("next", IconName::SkipForward, pal)
                            .diameter(48.)
                            .disabled(!has_track)
                            .on_click(move |_, _, cx| {
                                shared.update(cx, |s, cx| s.send(Request::Next, cx))
                            })
                    })
                    .child({
                        let shared = shared.clone();
                        IconButton::new("queue", IconName::ListMusic, pal)
                            .selected(queue_open)
                            .on_click(move |_, _, cx| shared.update(cx, |s, cx| s.toggle_queue(cx)))
                    }),
            )
            .child(
                h_flex()
                    .w(px(220.))
                    .gap_3()
                    .items_center()
                    .child(
                        Icon::new(IconName::Volume2)
                            .size(px(16.))
                            .text_color(pal.secondary),
                    )
                    .child(
                        div().flex_1().child(
                            Slider::new(&self.volume)
                                .disabled(!following)
                                .bg(pal.secondary)
                                .text_color(pal.fg),
                        ),
                    ),
            )
    }
}

/// The position: its own view, rebuilt by its tick alone (a tick in the shared model would
/// rebuild every view that reads it).
pub struct Bar {
    shared: Entity<Shared>,
    clock: Entity<Clock>,
    slider: Entity<SliderState>,
    /// Where the thumb is held, while it is dragged (0 to 1000).
    held: Option<f32>,
    _subs: Vec<Subscription>,
    _tick: Task<()>,
}

const BAR_STEPS: f32 = 1000.0;

impl Bar {
    fn new(
        shared: Entity<Shared>,
        clock: Entity<Clock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let slider = cx.new(|_| SliderState::new().min(0.).max(BAR_STEPS).step(1.));
        let subs = vec![
            cx.subscribe_in(&slider, window, |this, _, ev: &SliderEvent, window, cx| {
                match ev {
                    SliderEvent::Change(v) => this.held = Some(v.end()),
                    SliderEvent::Release(v) => {
                        this.held = None;
                        this.seek(v.end(), window, cx);
                    }
                }
                cx.notify();
            }),
            cx.observe_in(&clock, window, |this, _, window, cx| this.sync(window, cx)),
        ];
        let tick = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let alive = this.update_in(cx, |this, window, cx| {
                    if this.clock.read(cx).running {
                        this.sync(window, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            shared,
            clock,
            slider,
            held: None,
            _subs: subs,
            _tick: tick,
        }
    }

    fn fraction(&self, cx: &App) -> f32 {
        let c = self.clock.read(cx);
        if c.duration <= 0.0 {
            return 0.0;
        }
        (c.at(Instant::now()) / c.duration).clamp(0.0, 1.0) as f32
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.held.is_some() {
            return;
        }
        let v = (self.fraction(cx) * BAR_STEPS).round();
        if self.slider.read(cx).value().end() != v {
            self.slider.update(cx, |s, cx| s.set_value(v, window, cx));
        } else {
            cx.notify(); // the clock text moves before the thumb does
        }
    }

    fn seek(&mut self, v: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.shared.read(cx).player.track().map(|t| t.entry) else {
            return;
        };
        let position = f64::from(v / BAR_STEPS) * self.clock.read(cx).duration;
        // Moves at once; the daemon's next position confirms it.
        self.clock.update(cx, |c, cx| {
            c.position = position;
            c.since = Some(Instant::now());
            cx.notify();
        });
        self.shared.update(cx, |s, cx| {
            s.send(
                Request::SetPosition {
                    track: entry,
                    position_us: (position * 1e6) as i64,
                },
                cx,
            )
        });
        self.sync(window, cx);
    }
}

impl Render for Bar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let has_track = self.shared.read(cx).player.track().is_some();
        let c = *self.clock.read(cx);
        let position = match self.held {
            Some(v) => f64::from(v / BAR_STEPS) * c.duration,
            None => c.at(Instant::now()),
        };
        v_flex()
            .w_full()
            .gap_1()
            .child(
                Slider::new(&self.slider)
                    .disabled(!has_track)
                    .bg(pal.accent)
                    .text_color(pal.fg),
            )
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(pal.secondary)
                    .child(clock(position))
                    .child(clock(c.duration)),
            )
    }
}
