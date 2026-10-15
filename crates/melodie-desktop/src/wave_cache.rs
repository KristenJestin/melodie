//! The waveforms of the progress bar: asked for the current track and the next one, computed by
//! one `melodie waveform-fetch` at a time, and the bar told when they land. The window never
//! touches the server nor the audio.

use gpui_kit::*;
use melodie_core::env::Env;
use melodie_core::waveform::Ask;
use melodie_desktop::covers::{Fetch, LOCK_WAIT};
use melodie_desktop::launch;
use melodie_desktop::wave::{self, Levels, Waves};

pub struct WaveCache {
    env: Env,
    waves: Waves,
}

impl WaveCache {
    pub fn new(env: Env) -> Self {
        Self {
            env,
            waves: Waves::default(),
        }
    }

    pub fn get(&self, id: &str) -> Option<Levels> {
        self.waves.get(id).cloned()
    }

    /// The tracks to have a waveform for now: the current one, then the next.
    pub fn want(&mut self, asks: Vec<Ask>, cx: &mut Context<Self>) {
        self.waves.want(asks);
        self.pump(cx);
    }

    /// Starts a round if none runs; the next one follows when it is done (after a pause when
    /// another `waveform-fetch` held the lock).
    fn pump(&mut self, cx: &mut Context<Self>) {
        let Some(batch) = self.waves.start() else {
            return;
        };
        let env = self.env.clone();
        let task = cx.background_spawn(async move { wave::round(&env, &batch, fetch_waves) });
        cx.spawn(async move |this, cx| {
            let round = task.await;
            let landed = !round.found.is_empty();
            let Ok(wait) = this.update(cx, |this, cx| {
                let wait = this.waves.finish(round);
                if landed {
                    cx.notify();
                }
                wait
            }) else {
                return;
            };
            if wait {
                cx.background_executor().timer(LOCK_WAIT).await;
            }
            let _ = this.update(cx, |this, cx| this.pump(cx));
        })
        .detach();
    }
}

/// `melodie waveform-fetch` with the tracks on its standard input. Blocking: run it away from the
/// window.
fn fetch_waves(asks: &[Ask]) -> Fetch {
    let program = launch::program(std::env::current_exe().ok().as_deref());
    wave::fetch(&program, asks, wave::FETCH_LIMIT)
}
