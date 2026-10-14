//! The player, in the spirit of Amberol: the cover large, the track, the position, a few buttons.

use crate::app::{Shared, Shown};
use crate::artist_links;
use crate::catalog::Catalog;
use crate::context_menu::Menued as _;
use crate::dialogs::{self, Dialog};
use crate::motion;
use crate::volume::Volume;
use crate::wave_cache::WaveCache;
use crate::widgets::IconButton;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::proto::Request;
use melodie_desktop::bar;
use melodie_desktop::browse::Place;
use melodie_desktop::credits;
use melodie_desktop::figures::clock_text;
use melodie_desktop::launch::{Adding, Job, Kind};
use melodie_desktop::layout;
use melodie_desktop::menu::{Item, Spot};
use melodie_desktop::model::{Clock, PlayState};
use melodie_desktop::theme::Palette;
use melodie_desktop::wave::{self, Levels};
use std::time::{Duration, Instant};

/// The palette on screen now, moving towards the target of the model.
pub fn palette(shared: &Entity<Shared>, window: &mut Window, cx: &mut App) -> Palette {
    let target = shared.read(cx).target_palette();
    motion::to("palette", target, motion::COVER, window, cx)
}

/// How often the position moves while playing: often enough for the eye, cheap enough to
/// leave the window open for hours (`docs/gui-toolkit.md`, "Frame times").
const TICK: Duration = Duration::from_millis(250);
/// Room for the buttons on each side of the three at the middle: the widest side holds two
/// buttons of 40 px and the gap between them. Both sides get it, so play/pause stays centered.
const SIDE: f32 = 2.0 * 40.0 + 12.0;

pub struct PlayerView {
    shared: Entity<Shared>,
    catalog: Entity<Catalog>,
    bar: Entity<Bar>,
    volume: Entity<Volume>,
}

impl PlayerView {
    pub fn new(
        shared: Entity<Shared>,
        catalog: Entity<Catalog>,
        bar: Entity<Bar>,
        volume: Entity<Volume>,
    ) -> Self {
        Self {
            shared,
            catalog,
            bar,
            volume,
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
        let (track, artists, state, following, has_tracks, step, queue_open) = {
            let s = self.shared.read(cx);
            let artists = s
                .player
                .track()
                .map(|t| credits::of(&s.credits, &t.id, &t.artist, None));
            (
                s.player.track().cloned(),
                artists,
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
        let menu = track.as_ref().map(|t| (Item::entry(t), t.starred));

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
                    .when_some(menu, |d, (item, starred)| {
                        d.menu_starred(item, Spot::Player, Some(starred))
                    })
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
                    .child(match (&track, artists) {
                        // each artist a link of its own, the album after them
                        (Some(t), Some(artists)) => h_flex()
                            .max_w_full()
                            .text_color(pal.secondary)
                            .child(artist_links::line(&artists, |i, a| {
                                let id = format!("player-artist-{i}");
                                crate::tables::link(
                                    ElementId::Name(id.clone().into()),
                                    a.name.clone(),
                                    a.id.clone().map(Place::Artist),
                                    pal,
                                    &self.catalog,
                                )
                                .min_w_0()
                                .debug_selector(move || id.clone())
                                .into_any_element()
                            }))
                            .when(!t.album.is_empty(), |d| {
                                d.when(!artists.is_empty(), |d| {
                                    d.child(div().flex_none().px(px(5.)).child("·"))
                                })
                                .child(div().min_w_0().truncate().child(t.album.clone()))
                            }),
                        _ => div()
                            .text_color(pal.secondary)
                            .max_w_full()
                            .truncate()
                            .child(byline),
                    })
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
                    // the same room on both sides of play/pause, whatever they hold, so that it
                    // sits on the axis of the cover and the title
                    .child(
                        h_flex()
                            .w(px(SIDE))
                            .flex_none()
                            .gap_3()
                            .justify_end()
                            .child({
                                let shared = shared.clone();
                                IconButton::new("heart", IconName::Heart, pal)
                                    .tip(if starred { "Unlike" } else { "Like" })
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
                            })
                            .child({
                                let (id, title) = picked;
                                IconButton::new("add-current", IconName::ListPlus, pal)
                                    .tip("Add to playlist")
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
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child({
                                let shared = shared.clone();
                                IconButton::new("prev", IconName::SkipBack, pal)
                                    .tip("Previous")
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
                                .tip(if playing { "Pause" } else { "Play" })
                                .diameter(64.)
                                .primary()
                                .on_click(play),
                            )
                            .child({
                                let shared = shared.clone();
                                IconButton::new("next", IconName::SkipForward, pal)
                                    .tip("Next")
                                    .diameter(48.)
                                    .disabled(!has_track)
                                    .on_click(move |_, _, cx| {
                                        shared.update(cx, |s, cx| s.send(Request::Next, cx))
                                    })
                            }),
                    )
                    .child(h_flex().w(px(SIDE)).flex_none().gap_3().child({
                        let shared = shared.clone();
                        IconButton::new("queue", IconName::ListMusic, pal)
                            .tip("Queue")
                            .selected(queue_open)
                            .on_click(move |_, _, cx| shared.update(cx, |s, cx| s.toggle_queue(cx)))
                    })),
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
                            Slider::new(self.volume.read(cx).slider())
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
    waves: Entity<WaveCache>,
    /// On one line, between the elapsed and the total time (the bar at the bottom of the
    /// window); else under the slider, as in the player.
    inline: bool,
    slider: Entity<SliderState>,
    /// Where the thumb is held, while it is dragged (0 to 1000).
    held: Option<f32>,
    _subs: Vec<Subscription>,
    _tick: Task<()>,
}

const BAR_STEPS: f32 = 1000.0;
/// The tallest bar of the waveform: in the bottom bar it keeps to the row of the position.
const WAVE_INLINE: f32 = bar::POSITION - 2.0;
const WAVE: f32 = 24.0;

/// The waveform under the slider: the bars played so far in the accent, the rest dimmed, the bar
/// under the position cut where it is. Painted from the levels in hand: no allocation, no disk.
fn waveform(
    levels: Levels,
    split: f32,
    height: f32,
    opacity: f32,
    pal: Palette,
) -> impl IntoElement {
    let played = Hsla {
        a: pal.accent.a * opacity,
        ..pal.accent
    };
    let rest = Hsla {
        a: 0.28 * opacity,
        ..pal.fg
    };
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let width = bounds.size.width.as_f32();
            let n = wave::count(width);
            // spread over the whole width, so the bars end where the slider ends
            let step = width / n as f32;
            let (left, middle) = (bounds.origin.x.as_f32(), bounds.center().y.as_f32());
            let cut = left + width * split;
            for i in 0..n {
                let h = (wave::bar(&levels, i, n) * height).max(2.0);
                let x = left + i as f32 * step;
                let quad = |from: f32, to: f32, color: Hsla| {
                    fill(
                        Bounds::new(
                            point(px(from), px(middle - h / 2.0)),
                            size(px(to - from), px(h)),
                        ),
                        color,
                    )
                    .corner_radii(px(1.0))
                };
                let end = x + wave::BAR;
                if end <= cut {
                    window.paint_quad(quad(x, end, played));
                } else if x >= cut {
                    window.paint_quad(quad(x, end, rest));
                } else {
                    window.paint_quad(quad(x, cut, played));
                    window.paint_quad(quad(cut, end, rest));
                }
            }
        },
    )
    .absolute()
    .inset_0()
}

impl Bar {
    pub fn new(
        shared: Entity<Shared>,
        clock: Entity<Clock>,
        inline: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let slider = cx.new(|_| SliderState::new().min(0.).max(BAR_STEPS).step(1.));
        let mut subs = vec![
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
        let waves = shared.read(cx).waves.clone();
        subs.push(cx.observe(&waves, |_, _, cx| cx.notify()));
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
            waves,
            inline,
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
        let position = wave::time_at(v, BAR_STEPS, self.clock.read(cx).duration);
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
        let id = self.shared.read(cx).player.track().map(|t| t.id.clone());
        let has_track = id.is_some();
        let c = *self.clock.read(cx);
        let position = match self.held {
            Some(v) => wave::time_at(v, BAR_STEPS, c.duration),
            None => c.at(Instant::now()),
        };
        // The waveform fades in over the plain bar when it lands; one already there shows at
        // once (a transition adopts its first value), as a track change starts a new one.
        let levels = id.as_deref().and_then(|id| self.waves.read(cx).get(id));
        let shown = motion::flag(
            format!("wave-{}-{}", self.inline, id.unwrap_or_default()),
            levels.is_some(),
            motion::ENTER,
            window,
            cx,
        );
        let look = wave::look(levels.is_some(), shown);
        let split = if c.duration > 0.0 {
            (position / c.duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        // The slider stays on top for the click and the drag; its own bar fades out.
        let slider = Slider::new(&self.slider)
            .disabled(!has_track)
            .bg(Hsla {
                a: pal.accent.a * look.plain,
                ..pal.accent
            })
            .text_color(pal.fg);
        let height = if self.inline { WAVE_INLINE } else { WAVE };
        let slider = div()
            .relative()
            .w_full()
            .when_some(levels.filter(|_| look.wave > 0.0), |d, levels| {
                d.child(waveform(levels, split, height, look.wave, pal))
            })
            .child(slider);
        if self.inline {
            let times = crate::playbar::parts(&self.shared, window, cx).times;
            // fixed widths: the slider does not shift when 9:59 becomes 10:00; the elapsed time
            // leans against the slider, the total time too
            let time = |t: f64| div().w(px(40.)).flex_none().child(clock_text(t));
            return h_flex()
                .w_full()
                .h(px(bar::POSITION))
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(pal.secondary)
                .when(times, |d| d.child(time(position).flex().justify_end()))
                .child(div().flex_1().min_w_0().child(slider))
                .when(times, |d| d.child(time(c.duration)))
                .into_any_element();
        }
        v_flex()
            .w_full()
            .gap_1()
            .child(slider)
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(pal.secondary)
                    .child(clock_text(position))
                    .child(clock_text(c.duration)),
            )
            .into_any_element()
    }
}
