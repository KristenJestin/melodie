//! The cover files every view of the window shows (grids, pages, search, the queue, the player):
//! one place asks for them, one `melodie cover-fetch` at a time gets the missing ones, and the
//! views are told when a file lands. The window never talks to the server.

use gpui_kit::*;
use melodie_core::cover::LOCKED_EXIT;
use melodie_core::cover::{Disk, Want};
use melodie_core::env::Env;
use melodie_desktop::covers::{self, Fetch, Files, LOCK_WAIT, Queue, Use, resolve};
use melodie_desktop::launch;
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

pub struct CoverCache {
    env: Env,
    queue: Queue,
    /// Cover files by cover id, at every tier, as far as they were looked up.
    pub files: Arc<Files>,
    /// The screen's scale factor: a cover is asked at the pixels it is drawn with.
    scale: f32,
    /// What was asked for, to ask again at the right tier when the scale factor changes.
    asked: HashSet<(String, Use)>,
}

impl CoverCache {
    pub fn new(env: Env, scale: f32) -> Self {
        Self {
            env,
            queue: Queue::default(),
            files: Arc::default(),
            scale,
            asked: HashSet::new(),
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The window moved to a screen of another scale factor: what was asked is asked again at
    /// the tier that suits it (the files already there keep serving meanwhile).
    pub fn set_scale(&mut self, scale: f32, cx: &mut Context<Self>) {
        if (scale - self.scale).abs() < 0.01 {
            return;
        }
        self.scale = scale;
        let asked: Vec<(String, Use)> = self.asked.iter().cloned().collect();
        for usage in [Use::Small, Use::Tile, Use::Card] {
            let ids = asked
                .iter()
                .filter(|(_, u)| *u == usage)
                .map(|(id, _)| id.clone());
            self.want(ids, usage, false, cx);
        }
    }

    /// Looks up the covers not asked before; those with no file as large as `usage` draws them
    /// are fetched, a few at a time, until they are on disk, are the server's default image or
    /// have failed for good. With `first`, the missing ones go before those already waiting.
    pub fn want(
        &mut self,
        ids: impl IntoIterator<Item = String>,
        usage: Use,
        first: bool,
        cx: &mut Context<Self>,
    ) {
        let tier = usage.tier(self.scale);
        let ids: Vec<String> = ids.into_iter().collect();
        self.asked.extend(ids.iter().map(|id| (id.clone(), usage)));
        let new = self
            .queue
            .see(ids.into_iter().map(|id| Want::new(id, tier)));
        if new.is_empty() {
            return;
        }
        let env = self.env.clone();
        let task = cx.background_spawn(async move { resolve(&env, &new) });
        cx.spawn(async move |this, cx| {
            let (found, missing) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.add(found, cx);
                if first {
                    this.queue.missing_first(missing);
                } else {
                    this.queue.missing(missing);
                }
                this.pump(cx);
            });
        })
        .detach();
    }

    /// Starts a round if none runs; the next one follows when it is done (after a pause when
    /// another `cover-fetch` held the lock).
    fn pump(&mut self, cx: &mut Context<Self>) {
        let Some(batch) = self.queue.start() else {
            return;
        };
        let env = self.env.clone();
        let task = cx.background_spawn({
            let batch = batch.clone();
            async move { covers::round(&env, &batch, fetch_covers) }
        });
        cx.spawn(async move |this, cx| {
            let round = task.await;
            let Ok(wait) = this.update(cx, |this, cx| {
                let wait = this.queue.finish(&batch, &round);
                this.add(round.found, cx);
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

    fn add(&mut self, found: HashMap<String, Disk>, cx: &mut Context<Self>) {
        if found.is_empty() {
            return;
        }
        let mut files = (*self.files).clone();
        files.merge(found, Instant::now());
        self.files = Arc::new(files);
        cx.notify();
    }
}

/// `melodie cover-fetch` with the covers on its standard input; waits for it. Blocking: run it
/// away from the window.
fn fetch_covers(wants: &[Want]) -> Fetch {
    let program = launch::program(std::env::current_exe().ok().as_deref());
    let child = std::process::Command::new(program)
        .arg("cover-fetch")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return Fetch::Failed;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let lines: Vec<String> = wants.iter().map(Want::line).collect();
        let _ = stdin.write_all(lines.join("\n").as_bytes());
    }
    match child.wait().map(|s| s.code()) {
        Ok(Some(0)) => Fetch::Done,
        Ok(Some(code)) if code == i32::from(LOCKED_EXIT) => Fetch::Locked,
        _ => Fetch::Failed,
    }
}
