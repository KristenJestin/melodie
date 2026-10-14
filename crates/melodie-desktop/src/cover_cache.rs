//! The cover files every view of the window shows (grids, pages, search, the queue, the player):
//! one place asks for them, one `melodie cover-fetch` at a time gets the missing ones, and the
//! views are told when a file lands. The window never talks to the server.

use gpui_kit::*;
use melodie_core::cover::{Disk, Want};
use melodie_core::env::Env;
use melodie_desktop::covers::{self, FADE_KEEP, Fetch, Files, Queue, ROUND_LIMIT, Use, resolve};
use melodie_desktop::launch;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

pub struct CoverCache {
    env: Env,
    queue: Queue,
    /// Cover files by cover id, at every tier, as far as they were looked up.
    pub files: Arc<Files>,
    /// The screen's scale factor: a cover is asked at the pixels it is drawn with.
    scale: f32,
}

impl CoverCache {
    pub fn new(env: Env, scale: f32) -> Self {
        Self {
            env,
            queue: Queue::default(),
            files: Arc::default(),
            scale,
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
        let new = self.queue.ask_again(scale, Instant::now());
        self.look_up(new, false, cx);
    }

    /// Looks up the covers not asked before; those with no file as large as `usage` draws them
    /// are fetched, a few at a time, until they are on disk, are the server's default image or
    /// have failed for good. With `first`, the missing ones go before those already waiting:
    /// what is on screen does not wait for a library.
    pub fn want(
        &mut self,
        ids: impl IntoIterator<Item = String>,
        usage: Use,
        first: bool,
        cx: &mut Context<Self>,
    ) {
        let new = self.queue.ask(
            ids.into_iter().collect(),
            usage,
            self.scale,
            Instant::now(),
            first,
        );
        self.look_up(new, first, cx);
    }

    fn look_up(&mut self, new: Vec<Want>, first: bool, cx: &mut Context<Self>) {
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
    /// another `cover-fetch` held the lock, or this one had to be stopped).
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
                let wait = this.queue.finish(&batch, &round, Instant::now());
                this.add(round.found, cx);
                wait
            }) else {
                return;
            };
            if let Some(wait) = wait {
                cx.background_executor().timer(wait).await;
            }
            let _ = this.update(cx, |this, cx| {
                this.queue.resume();
                this.pump(cx)
            });
        })
        .detach();
    }

    fn add(&mut self, found: HashMap<String, Disk>, cx: &mut Context<Self>) {
        if found.is_empty() {
            return;
        }
        let mut files = (*self.files).clone();
        let faded = files.merge(found, Instant::now());
        self.files = Arc::new(files);
        cx.notify();
        if faded {
            // nothing else repaints when the smaller file leaves the tree: it would stay drawn
            // under a transparent picture until the next change
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(FADE_KEEP).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
    }
}

/// `melodie cover-fetch` with the covers on its standard input. Blocking: run it away from the
/// window.
fn fetch_covers(wants: &[Want]) -> Fetch {
    let program = launch::program(std::env::current_exe().ok().as_deref());
    covers::fetch(&program, wants, ROUND_LIMIT)
}
