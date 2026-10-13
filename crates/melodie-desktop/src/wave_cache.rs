//! The waveforms of the progress bar: asked for the current track and the next one, computed by
//! one `melodie waveform-fetch` at a time, and the bar told when they land. The window never
//! touches the server nor the audio.

use gpui_kit::*;
use melodie_core::cover::LOCKED_EXIT;
use melodie_core::env::Env;
use melodie_desktop::covers::{Fetch, LOCK_WAIT};
use melodie_desktop::launch;
use melodie_desktop::wave::{self, Levels, Waves};
use std::io::Write as _;

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
    pub fn want(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        self.waves.want(ids);
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

/// `melodie waveform-fetch` with the track ids on its standard input; waits for it. Blocking: run
/// it away from the window.
fn fetch_waves(ids: &[String]) -> Fetch {
    let program = launch::program(std::env::current_exe().ok().as_deref());
    let child = std::process::Command::new(program)
        .arg("waveform-fetch")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return Fetch::Failed;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(ids.join("\n").as_bytes());
    }
    match child.wait().map(|s| s.code()) {
        Ok(Some(0)) => Fetch::Done,
        Ok(Some(code)) if code == i32::from(LOCKED_EXIT) => Fetch::Locked,
        _ => Fetch::Failed,
    }
}
