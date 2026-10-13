//! The volume: one slider and one mute, shared by the player and the bar at the bottom of the
//! window (only one of them is on screen at a time).

use crate::app::Shared;
use gpui_kit::assets::IconName;
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::*;
use melodie_core::proto::Request;
use melodie_desktop::model::{VOLUME_SLIDER_MAX, mute, volume_percent};
use std::time::{Duration, Instant};

/// The volume is sent at most this often while it is dragged.
const EVERY: Duration = Duration::from_millis(60);

pub struct Volume {
    shared: Entity<Shared>,
    slider: Entity<SliderState>,
    dragged: bool,
    sent: Option<Instant>,
    /// The volume before the mute, which a second click brings back.
    before_mute: Option<f64>,
    _subs: Vec<Subscription>,
}

impl Volume {
    pub fn new(shared: Entity<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let v = shared.read(cx).player.volume;
        let slider = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(VOLUME_SLIDER_MAX)
                .step(1.)
                .default_value(volume_percent(v))
        });
        let subs = vec![
            cx.subscribe_in(&slider, window, |this, _, ev: &SliderEvent, _, cx| {
                let (value, done) = match ev {
                    SliderEvent::Change(v) => (v.end(), false),
                    SliderEvent::Release(v) => (v.end(), true),
                };
                this.dragged = !done;
                let due = this.sent.is_none_or(|t| t.elapsed() >= EVERY);
                if done || due {
                    this.sent = Some(Instant::now());
                    this.set(f64::from(value) / 100.0, cx);
                }
            }),
            cx.observe_in(&shared, window, |this, shared, window, cx| {
                if this.dragged {
                    return;
                }
                let v = volume_percent(shared.read(cx).player.volume);
                if this.slider.read(cx).value().end() != v {
                    this.slider.update(cx, |s, cx| s.set_value(v, window, cx));
                }
            }),
        ];
        Self {
            shared,
            slider,
            dragged: false,
            sent: None,
            before_mute: None,
            _subs: subs,
        }
    }

    pub fn slider(&self) -> &Entity<SliderState> {
        &self.slider
    }

    fn set(&self, volume: f64, cx: &mut Context<Self>) {
        self.shared
            .update(cx, |s, cx| s.send(Request::SetVolume { volume }, cx));
    }

    /// Mutes, or brings back the volume the mute took away.
    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        let now = self.shared.read(cx).player.volume;
        let (volume, before) = mute(now, self.before_mute);
        self.before_mute = before;
        self.set(volume, cx);
    }
}

/// The icon for a volume (1 = 100 %): silent, low or loud.
pub fn icon(volume: f64) -> IconName {
    if volume <= 0.0 {
        IconName::VolumeX
    } else if volume < 0.5 {
        IconName::Volume1
    } else {
        IconName::Volume2
    }
}
