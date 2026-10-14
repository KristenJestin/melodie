//! Which covers the window still has to get, and in what order: one `melodie cover-fetch` at a
//! time, fed by the covers (an id at a tier) that are missing on disk. Nothing here touches the
//! server or draws; the cover files known so far are looked up here too, for the views.

use melodie_core::cover::{Disk, Tier, Want};
use melodie_core::env::Env;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How many times a cover may come back from a finished `cover-fetch` still missing before the
/// window leaves it to what it has for a while (the server errs on it, or the process stopped
/// before it): see [`RETRY_AFTER`].
pub const MAX_TRIES: u32 = 3;
/// The covers a round hands to one `cover-fetch`.
const BATCH: usize = 48;
/// How long a cover given up waits before a view that shows it may ask for it again; doubled
/// each time it is given up again, up to [`RETRY_MAX`].
pub const RETRY_AFTER: Duration = Duration::from_secs(60);
pub const RETRY_MAX: Duration = Duration::from_secs(30 * 60);
/// How long to wait before asking again when another `cover-fetch` held the lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(2);

/// The side, in logical pixels, a cover is drawn at: which tier it needs depends on it and on
/// the screen's scale factor. Callers pass the side they draw; the constants are the usual ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Use(u32);

impl Use {
    /// The pictures of a list row.
    pub const SMALL: Use = Use(56);
    /// Albums, playlists and artists in rows and page headers.
    pub const TILE: Use = Use(180);
    /// The home's highlight.
    pub const CARD: Use = Use(212);

    /// A cover drawn `side` pixels wide.
    pub fn of(side: f32) -> Use {
        Use(side.ceil().max(0.0) as u32)
    }

    pub fn side(self) -> f32 {
        self.0 as f32
    }

    fn max_side(self, other: Use) -> Use {
        Use(self.0.max(other.0))
    }

    /// The tier that is not upscaled on a screen of this scale factor.
    pub fn tier(self, scale: f32) -> Tier {
        Tier::for_draw(self.side(), scale)
    }
}

/// How one `cover-fetch` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// It ran: whatever it could download is on disk.
    Done,
    /// Another one held the lock: nothing was done.
    Locked,
    /// It could not run at all.
    Failed,
}

/// How long a larger cover shows its smaller one under it, fading in. Longer than the fade, so
/// that it is still there for all of it.
pub const FADE_KEEP: Duration = Duration::from_millis(500);

/// The cover files known so far, by cover id.
#[derive(Default, Clone)]
pub struct Files {
    by_id: HashMap<String, Disk>,
    /// When a file landed beside a smaller one that was shown before it.
    upgraded: HashMap<(String, Tier), Instant>,
}

/// A cover to draw: its file, and the smaller one it replaces while that one fades out.
#[derive(Debug, Clone, PartialEq)]
pub struct Cover {
    pub path: PathBuf,
    pub under: Option<PathBuf>,
}

impl Files {
    /// Takes what a lookup found in. A larger file that lands where a smaller one was already
    /// shown is remembered, so that the view can fade from one to the other; true when one was.
    pub fn merge(&mut self, found: HashMap<String, Disk>, now: Instant) -> bool {
        let mut faded = false;
        self.upgraded
            .retain(|_, at| now.saturating_duration_since(*at) < FADE_KEEP);
        for (id, disk) in found {
            if let Some(old) = self.by_id.get(&id) {
                for tier in Tier::ALL {
                    let smaller_shown = Tier::ALL[..tier as usize]
                        .iter()
                        .any(|t| old.at(*t).is_some());
                    if disk.at(tier).is_some() && old.at(tier).is_none() && smaller_shown {
                        self.upgraded.insert((id.clone(), tier), now);
                        faded = true;
                    }
                }
            }
            self.by_id.insert(id, disk);
        }
        faded
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// The file to draw for a cover that needs `tier`: the smallest one that is not upscaled,
    /// else the largest below it, meanwhile.
    pub fn get(&self, id: &str, tier: Tier, now: Instant) -> Option<Cover> {
        let disk = self.by_id.get(id)?;
        let shown = disk.tier_of(tier)?;
        let path = disk.at(shown)?.clone();
        let fresh = self
            .upgraded
            .get(&(id.to_string(), shown))
            .is_some_and(|at| now.saturating_duration_since(*at) < FADE_KEEP);
        let under = fresh
            .then(|| {
                Tier::ALL[..shown as usize]
                    .iter()
                    .rev()
                    .find_map(|t| disk.at(*t))
                    .cloned()
            })
            .flatten();
        Some(Cover { path, under })
    }

    /// The cheapest file of a cover (to read its colour).
    pub fn smallest(&self, id: &str) -> Option<&PathBuf> {
        self.by_id.get(id)?.smallest()
    }
}

/// The files, and the scale factor of the screen: what a view needs to pick a cover by the side
/// it draws it at.
#[derive(Clone)]
pub struct Covers {
    pub files: std::sync::Arc<Files>,
    pub scale: f32,
}

impl Covers {
    pub fn new(files: std::sync::Arc<Files>, scale: f32) -> Self {
        Self { files, scale }
    }

    /// The cover `id` to draw `side` logical pixels wide.
    pub fn get(&self, id: &str, side: f32) -> Option<Cover> {
        self.files
            .get(id, Tier::for_draw(side, self.scale), Instant::now())
    }
}

/// The files on disk for these covers, and the covers still to fetch: those with no file as
/// large as wanted (a smaller one is found all the same, to show meanwhile), except the server's
/// default image, which is never fetched. Checks files: run it away from the window's thread.
pub fn resolve<'a>(
    env: &Env,
    wants: impl IntoIterator<Item = &'a Want>,
) -> (HashMap<String, Disk>, Vec<Want>) {
    let mut disks: HashMap<String, Disk> = HashMap::new();
    let mut missing: Vec<Want> = Vec::new();
    for w in wants {
        let disk = disks
            .entry(w.id.clone())
            .or_insert_with(|| Disk::read(env, &w.id));
        let default_image = disk.smallest().is_none() && disk.placeholder();
        if !disk.has(w.tier) && !default_image && !missing.contains(w) {
            missing.push(w.clone());
        }
    }
    disks.retain(|_, d| d.smallest().is_some());
    (disks, missing)
}

/// What one round found.
#[derive(Debug)]
pub struct Round {
    /// The cover files on disk now.
    pub found: HashMap<String, Disk>,
    /// The covers still without a file as large as wanted or a marker.
    pub missing: Vec<Want>,
    /// How the fetch ended; `None` when nothing needed one.
    pub fetch: Option<Fetch>,
}

/// Looks up `batch` on disk, has `fetch` get what is missing, and looks up again. Checks files
/// and waits for the fetch: run it away from the window's thread.
pub fn round(env: &Env, batch: &[Want], fetch: impl FnOnce(&[Want]) -> Fetch) -> Round {
    let (mut found, missing) = resolve(env, batch);
    if missing.is_empty() {
        return Round {
            found,
            missing,
            fetch: None,
        };
    }
    let fetch = fetch(&missing);
    let (arrived, missing) = resolve(env, &missing);
    found.extend(arrived);
    Round {
        found,
        missing,
        fetch: Some(fetch),
    }
}

/// The covers asked, and those still to get.
#[derive(Default)]
pub struct Queue {
    seen: HashSet<Want>,
    pending: Vec<Want>,
    /// What `pending` holds, to queue a cover once without scanning it.
    waiting: HashSet<Want>,
    tries: HashMap<Want, u32>,
    running: bool,
    /// How many times each cover was given up, and when the last time lets it be asked again.
    given_up: HashMap<Want, u32>,
    retry_at: HashMap<Want, Instant>,
    /// The largest side each cover was asked at: asked again at the right tier when the scale
    /// factor changes. One entry per cover, whatever the sides it was drawn at (a window being
    /// resized draws the grid at a different side at every step).
    asked: HashMap<String, Use>,
}

impl Queue {
    /// The covers not asked before (each is looked up once, then queued if missing), and those
    /// given up whose wait is over: a cover is never left to the glyph for good.
    pub fn see(&mut self, wants: impl IntoIterator<Item = Want>, now: Instant) -> Vec<Want> {
        wants
            .into_iter()
            .filter(|w| self.seen.insert(w.clone()) || self.due(w, now))
            .collect()
    }

    /// A cover given up whose wait is over: true once.
    fn due(&mut self, w: &Want, now: Instant) -> bool {
        let due = self.retry_at.get(w).is_some_and(|at| *at <= now);
        if due {
            self.retry_at.remove(w);
        }
        due
    }

    /// Asks for covers drawn as `usage` on a screen of this scale factor: those not asked before
    /// at that tier, to look up on disk. With `first` (they are on screen), those already waiting
    /// go before the rest: asked long ago, they would wait behind everything asked since.
    pub fn ask(
        &mut self,
        ids: Vec<String>,
        usage: Use,
        scale: f32,
        now: Instant,
        first: bool,
    ) -> Vec<Want> {
        let tier = usage.tier(scale);
        for id in &ids {
            let largest = self.asked.entry(id.clone()).or_insert(usage);
            *largest = (*largest).max_side(usage);
        }
        let wants: Vec<Want> = ids.into_iter().map(|id| Want::new(id, tier)).collect();
        if first {
            let waiting: Vec<Want> = wants
                .iter()
                .filter(|w| self.waiting.contains(w))
                .cloned()
                .collect();
            if !waiting.is_empty() {
                self.missing_first(waiting);
            }
        }
        self.see(wants, now)
    }

    /// The screen's scale factor changed: what was asked, at the tier that suits it now (what the
    /// new tier already has is not asked twice).
    pub fn ask_again(&mut self, scale: f32, now: Instant) -> Vec<Want> {
        let again: Vec<Want> = self
            .asked
            .iter()
            .map(|(id, usage)| Want::new(id.clone(), usage.tier(scale)))
            .collect();
        self.see(again, now)
    }

    /// Queues covers that have no file yet, behind those already waiting.
    pub fn missing(&mut self, wants: impl IntoIterator<Item = Want>) {
        for w in wants {
            if self.waiting.insert(w.clone()) {
                self.pending.push(w);
            }
        }
    }

    /// Queues covers that have no file yet, before those already waiting: what is playing, what
    /// is on screen.
    pub fn missing_first(&mut self, wants: Vec<Want>) {
        let rest = std::mem::take(&mut self.pending);
        self.waiting.clear();
        self.missing(wants);
        self.missing(rest);
    }

    fn forget(&mut self, gone: impl Fn(&Want) -> bool) {
        let waiting = &mut self.waiting;
        self.pending.retain(|w| {
            let keep = !gone(w);
            if !keep {
                waiting.remove(w);
            }
            keep
        });
    }

    /// The covers for the next round, unless one is running or nothing waits. A round takes a
    /// bounded share, so that what is asked meanwhile does not wait for a whole library.
    pub fn start(&mut self) -> Option<Vec<Want>> {
        if self.running || self.pending.is_empty() {
            return None;
        }
        self.running = true;
        Some(self.pending.iter().take(BATCH).cloned().collect())
    }

    /// Takes a finished round in; true when it hit the lock and should be asked again after
    /// [`LOCK_WAIT`].
    pub fn finish(&mut self, batch: &[Want], round: &Round, now: Instant) -> bool {
        self.running = false;
        let gone: HashSet<&Want> = batch
            .iter()
            .filter(|w| !round.missing.contains(w))
            .collect();
        self.forget(|w| gone.contains(w));
        match round.fetch {
            None => {}
            // nothing was done: the covers keep their tries
            Some(Fetch::Locked) => return true,
            Some(Fetch::Done | Fetch::Failed) => {
                for w in &round.missing {
                    let tries = self.tries.entry(w.clone()).or_default();
                    *tries += 1;
                    if *tries >= MAX_TRIES {
                        self.forget(|p| p == w);
                        self.tries.remove(w);
                        let times = self.given_up.entry(w.clone()).or_default();
                        *times += 1;
                        let wait = RETRY_AFTER
                            .saturating_mul(1 << (*times - 1).min(16))
                            .min(RETRY_MAX);
                        self.retry_at.insert(w.clone(), now + wait);
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melodie_core::cover::{self, Tier};

    fn env() -> (tempfile::TempDir, Env) {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        (dir, env)
    }

    /// Thumbnails, as the window asked for them before it had tiers.
    fn ids(list: &[&str]) -> Vec<Want> {
        at(list, Tier::Thumb)
    }

    fn at(list: &[&str], tier: Tier) -> Vec<Want> {
        list.iter().map(|s| Want::new(*s, tier)).collect()
    }

    fn put_tier(env: &Env, id: &str, tier: Tier) {
        let file = cover::tier_path(env, id, tier);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "").unwrap();
    }

    fn put_want(env: &Env, w: &Want) {
        put_tier(env, &w.id, w.tier);
    }

    fn put_placeholder(env: &Env, id: &str) {
        let thumb = cover::thumb_path(env, id);
        std::fs::create_dir_all(thumb.parent().unwrap()).unwrap();
        std::fs::write(format!("{}.none", thumb.display()), "").unwrap();
    }

    /// Plays rounds until nothing waits; returns how many fetches were run.
    fn drain(
        env: &Env,
        queue: &mut Queue,
        fetch: impl FnMut(&[Want]) -> Fetch,
    ) -> (usize, HashMap<String, Disk>) {
        drain_at(env, queue, Instant::now(), fetch)
    }

    fn drain_at(
        env: &Env,
        queue: &mut Queue,
        now: Instant,
        mut fetch: impl FnMut(&[Want]) -> Fetch,
    ) -> (usize, HashMap<String, Disk>) {
        let (mut fetches, mut all) = (0, HashMap::new());
        while let Some(batch) = queue.start() {
            let r = round(env, &batch, |wants| {
                fetches += 1;
                fetch(wants)
            });
            all.extend(r.found.clone());
            queue.finish(&batch, &r, now);
            assert!(fetches < 50, "never ends");
        }
        (fetches, all)
    }

    #[test]
    fn ids_asked_while_another_fetch_holds_the_lock_are_fetched_afterwards() {
        let (_d, env) = env();
        let mut q = Queue::default();
        q.missing(ids(&["a", "b"]));
        let batch = q.start().unwrap();
        let r = round(&env, &batch, |_| Fetch::Locked);
        assert!(r.found.is_empty());
        assert!(
            q.finish(&batch, &r, Instant::now()),
            "told to wait, then ask again"
        );
        let batch = q.start().expect("the ids are still wanted");
        assert_eq!(batch, ids(&["a", "b"]));
        let r = round(&env, &batch, |wants| {
            wants.iter().for_each(|w| put_want(&env, w));
            Fetch::Done
        });
        assert_eq!(r.found.len(), 2);
        assert!(!q.finish(&batch, &r, Instant::now()));
        assert!(q.start().is_none(), "nothing is lost, nothing is left");
    }

    #[test]
    fn a_locked_fetch_does_not_use_up_the_tries() {
        let (_d, env) = env();
        let mut q = Queue::default();
        q.missing(ids(&["a"]));
        for _ in 0..(MAX_TRIES * 4) {
            let batch = q.start().unwrap();
            let r = round(&env, &batch, |_| Fetch::Locked);
            q.finish(&batch, &r, Instant::now());
        }
        let (_, found) = drain(&env, &mut q, |wants| {
            wants.iter().for_each(|w| put_want(&env, w));
            Fetch::Done
        });
        assert!(found.contains_key("a"));
    }

    #[test]
    fn ids_that_arrive_on_disk_show_without_a_restart() {
        let (_d, env) = env();
        let mut q = Queue::default();
        q.missing(ids(&["a", "b"]));
        // `a` is downloaded by the fetch of the round, `b` by someone else's meanwhile
        let (fetches, found) = drain(&env, &mut q, |wants| {
            assert_eq!(wants, ids(&["a", "b"]));
            put_tier(&env, "a", Tier::Thumb);
            put_tier(&env, "b", Tier::Thumb);
            Fetch::Done
        });
        assert_eq!(fetches, 1);
        assert_eq!(
            found["a"].at(Tier::Thumb),
            Some(&cover::thumb_path(&env, "a"))
        );
        assert_eq!(
            found["b"].at(Tier::Thumb),
            Some(&cover::thumb_path(&env, "b"))
        );

        // a file that came while the window waited needs no fetch of its own
        q.missing(ids(&["c"]));
        put_tier(&env, "c", Tier::Thumb);
        let (fetches, found) = drain(&env, &mut q, |_| panic!("already on disk"));
        assert_eq!(fetches, 0);
        assert!(found.contains_key("c"));
    }

    #[test]
    fn a_placeholder_is_not_asked_again_at_any_size() {
        let (_d, env) = env();
        let mut q = Queue::default();
        assert_eq!(q.see(ids(&["p"]), Instant::now()), ids(&["p"]));
        put_placeholder(&env, "p");
        for tier in Tier::ALL {
            q.missing(at(&["p"], tier));
        }
        let (fetches, found) = drain(&env, &mut q, |_| panic!("a placeholder is not fetched"));
        assert_eq!(fetches, 0);
        assert!(found.is_empty(), "the glyph stays");
        assert!(
            q.see(ids(&["p"]), Instant::now()).is_empty(),
            "and it is not looked up again"
        );

        // the fetch itself finds the server's default image: a marker, no file
        q.missing(at(&["q"], Tier::Row));
        let (fetches, found) = drain(&env, &mut q, |wants| {
            put_placeholder(&env, &wants[0].id);
            Fetch::Done
        });
        assert_eq!(fetches, 1);
        assert!(found.is_empty());
        assert!(q.start().is_none(), "no second fetch for it");
        q.missing(at(&["q"], Tier::Large));
        let (fetches, _) = drain(&env, &mut q, |_| panic!("not at another size either"));
        assert_eq!(fetches, 0);
    }

    #[test]
    fn a_download_that_fails_is_retried_a_bounded_number_of_times() {
        let (_d, env) = env();
        let mut q = Queue::default();
        q.missing(ids(&["bad", "good"]));
        let mut asked = Vec::new();
        let (fetches, found) = drain(&env, &mut q, |wants| {
            asked.push(wants.to_vec());
            if wants.iter().any(|w| w.id == "good") {
                put_tier(&env, "good", Tier::Thumb);
            }
            Fetch::Done
        });
        assert_eq!(fetches as u32, MAX_TRIES);
        assert_eq!(asked[0], ids(&["bad", "good"]));
        assert_eq!(asked[1], ids(&["bad"]), "what arrived is not asked again");
        assert!(found.contains_key("good"));
        assert!(
            q.start().is_none(),
            "left to the glyph until shown again later"
        );
    }

    /// Issue #84: the server failed a while (a long queue, a slow or unreachable server); the
    /// covers of the rows were given up, and a queue showing them later stayed on the glyph
    /// until the window was restarted.
    #[test]
    fn a_cover_given_up_is_asked_again_when_shown_after_a_while() {
        let (_d, env) = env();
        let mut q = Queue::default();
        let t0 = Instant::now();
        let row = |q: &mut Queue, at: Instant| q.ask(vec!["c1".into()], Use::SMALL, 1.0, at, false);
        let new = row(&mut q, t0);
        q.missing(new);
        let (fetches, _) = drain_at(&env, &mut q, t0, |_| Fetch::Done);
        assert_eq!(fetches as u32, MAX_TRIES, "the server errs: given up");

        assert!(
            row(&mut q, t0 + Duration::from_secs(5)).is_empty(),
            "shown again at once: no request storm"
        );
        let later = t0 + RETRY_AFTER;
        let again = row(&mut q, later);
        assert_eq!(again, at(&["c1"], Tier::Thumb), "shown after a while");
        assert!(row(&mut q, later).is_empty(), "asked once, not per frame");
        q.missing(again);
        let (fetches, found) = drain_at(&env, &mut q, later, |wants| {
            wants.iter().for_each(|w| put_want(&env, w));
            Fetch::Done
        });
        assert_eq!(fetches, 1);
        assert!(found.contains_key("c1"), "the row gets its cover back");
    }

    #[test]
    fn a_cover_given_up_again_waits_longer_each_time() {
        let (_d, env) = env();
        let mut q = Queue::default();
        let mut now = Instant::now();
        let row =
            |q: &mut Queue, at: Instant| q.ask(vec!["bad".into()], Use::SMALL, 1.0, at, false);
        let new = row(&mut q, now);
        q.missing(new);
        let mut waits = Vec::new();
        for _ in 0..8 {
            drain_at(&env, &mut q, now, |_| Fetch::Failed);
            let mut wait = Duration::ZERO;
            let again = loop {
                wait += Duration::from_secs(30);
                assert!(wait < Duration::from_secs(4 * 3600), "never asked again");
                let again = row(&mut q, now + wait);
                if !again.is_empty() {
                    break again;
                }
            };
            waits.push(wait);
            now += wait;
            q.missing(again);
        }
        assert_eq!(waits[0], RETRY_AFTER);
        assert_eq!(waits[1], RETRY_AFTER * 2);
        assert!(waits.windows(2).all(|w| w[0] <= w[1]), "{waits:?}");
        assert_eq!(*waits.last().unwrap(), RETRY_MAX, "but not for ever");
    }

    #[test]
    fn a_fetch_that_cannot_run_counts_as_a_try() {
        let (_d, env) = env();
        let mut q = Queue::default();
        q.missing(ids(&["a"]));
        let (fetches, _) = drain(&env, &mut q, |_| Fetch::Failed);
        assert_eq!(fetches as u32, MAX_TRIES);
    }

    #[test]
    fn one_fetch_at_a_time_fed_by_the_ids_that_come_meanwhile() {
        let mut q = Queue::default();
        q.missing(ids(&["a"]));
        let first = q.start().unwrap();
        q.missing(ids(&["b"]));
        assert!(q.start().is_none(), "a fetch is running: no second process");
        let (_d, env) = env();
        let r = round(&env, &first, |wants| {
            wants.iter().for_each(|w| put_want(&env, w));
            Fetch::Done
        });
        q.finish(&first, &r, Instant::now());
        assert_eq!(
            q.start().unwrap(),
            ids(&["b"]),
            "the next round takes what came"
        );
    }

    #[test]
    fn what_plays_is_fetched_before_the_rest_and_a_round_is_bounded() {
        let mut q = Queue::default();
        let many: Vec<Want> = (0..BATCH * 2)
            .map(|i| Want::new(format!("c{i}"), Tier::Row))
            .collect();
        q.missing(many);
        q.missing_first(ids(&["now"]));
        let batch = q.start().unwrap();
        assert_eq!(batch.len(), BATCH, "a round does not take a whole library");
        assert_eq!(batch[0], Want::new("now", Tier::Thumb));
        assert_eq!(batch[1], Want::new("c0", Tier::Row));
    }

    #[test]
    fn an_id_asked_twice_is_queued_once() {
        let mut q = Queue::default();
        assert_eq!(
            q.see(ids(&["a", "b", "a"]), Instant::now()),
            ids(&["a", "b"])
        );
        assert!(q.see(ids(&["b"]), Instant::now()).is_empty());
        q.missing(ids(&["a", "a"]));
        assert_eq!(q.start().unwrap(), ids(&["a"]));
    }

    #[test]
    fn the_same_cover_asked_at_another_tier_is_a_new_ask() {
        let mut q = Queue::default();
        assert_eq!(q.see(ids(&["a"]), Instant::now()).len(), 1);
        assert_eq!(
            q.see(at(&["a"], Tier::Row), Instant::now()),
            at(&["a"], Tier::Row),
            "a larger size is not the same cover to fetch"
        );
    }

    #[test]
    fn a_row_cover_is_drawn_from_the_tier_that_is_not_upscaled() {
        let (_d, env) = env();
        put_tier(&env, "a", Tier::Thumb);
        put_tier(&env, "a", Tier::Row);
        put_tier(&env, "a", Tier::Large);
        let (found, missing) = resolve(&env, &at(&["a"], Tier::Row));
        assert!(missing.is_empty());
        let files = Files {
            by_id: found,
            ..Files::default()
        };
        let now = Instant::now();
        let get = |side: f32, scale: f32| {
            files
                .get("a", Tier::for_draw(side, scale), now)
                .map(|c| c.path)
        };
        // the home's rows (168 px) and card (212 px) at scale 1
        assert_eq!(get(168., 1.), Some(cover::tier_path(&env, "a", Tier::Row)));
        assert_eq!(get(212., 1.), Some(cover::tier_path(&env, "a", Tier::Row)));
        // a small row of a list
        assert_eq!(get(40., 1.), Some(cover::thumb_path(&env, "a")));
        // the same row on a screen at scale 2
        assert_eq!(get(168., 2.), Some(cover::path(&env, "a")));
    }

    #[test]
    fn a_cover_with_only_a_thumbnail_is_wanted_larger_and_shown_meanwhile() {
        let (_d, env) = env();
        put_tier(&env, "a", Tier::Thumb);
        let wants = at(&["a"], Tier::Row);
        let (found, missing) = resolve(&env, &wants);
        assert_eq!(missing, wants, "the thumbnail is not the size wanted");
        let files = Files {
            by_id: found,
            ..Files::default()
        };
        let shown = files.get("a", Tier::Row, Instant::now()).unwrap();
        assert_eq!(shown.path, cover::thumb_path(&env, "a"));
        assert_eq!(shown.under, None);

        // a larger file on disk already answers a smaller wish: nothing to fetch
        put_tier(&env, "b", Tier::Large);
        assert!(resolve(&env, &at(&["b"], Tier::Row)).1.is_empty());
        assert!(resolve(&env, &at(&["b"], Tier::Thumb)).1.is_empty());
    }

    #[test]
    fn a_larger_tier_replaces_a_smaller_one_when_it_lands_with_a_fade() {
        let (_d, env) = env();
        let (t0, mut files) = (Instant::now(), Files::default());
        put_tier(&env, "a", Tier::Thumb);
        assert!(!files.merge(resolve(&env, &at(&["a"], Tier::Thumb)).0, t0));
        let before = files.get("a", Tier::Row, t0).unwrap();
        assert_eq!(before.path, cover::thumb_path(&env, "a"));

        put_tier(&env, "a", Tier::Row);
        let t1 = t0 + Duration::from_secs(5);
        assert!(
            files.merge(resolve(&env, &at(&["a"], Tier::Row)).0, t1),
            "told that a fade starts, so that a repaint can end it"
        );
        let landed = files.get("a", Tier::Row, t1).unwrap();
        assert_eq!(landed.path, cover::tier_path(&env, "a", Tier::Row));
        assert_eq!(
            landed.under,
            Some(cover::thumb_path(&env, "a")),
            "the smaller one is still under it while it fades in"
        );
        let later = files.get("a", Tier::Row, t1 + FADE_KEEP).unwrap();
        assert_eq!(later.under, None, "then only the new one is drawn");
    }

    #[test]
    fn a_cover_that_lands_first_does_not_fade_from_a_thumbnail_nobody_saw() {
        let (_d, env) = env();
        let (t0, mut files) = (Instant::now(), Files::default());
        // the fetch gets the thumbnail (for the default image) and the tier together
        put_tier(&env, "a", Tier::Thumb);
        put_tier(&env, "a", Tier::Row);
        files.merge(resolve(&env, &at(&["a"], Tier::Row)).0, t0);
        let shown = files.get("a", Tier::Row, t0).unwrap();
        assert_eq!(shown.path, cover::tier_path(&env, "a", Tier::Row));
        assert_eq!(shown.under, None);
    }

    #[test]
    fn the_window_asks_for_the_tier_of_what_it_draws_times_the_scale() {
        assert_eq!(Use::SMALL.tier(1.0), Tier::Thumb);
        assert_eq!(Use::TILE.tier(1.0), Tier::Row);
        assert_eq!(Use::CARD.tier(1.0), Tier::Row);
        assert_eq!(Use::TILE.tier(2.0), Tier::Large);
        assert_eq!(Use::SMALL.tier(2.0), Tier::Thumb);
    }

    #[test]
    fn a_tile_is_not_fetched_larger_than_it_is_drawn() {
        // the sides drawn are 168 to 180 px (up to ~250 in a two-column grid): at a scale of 1.25
        // a 300 px file is enough for the first, and not for the last
        for scale in [1.0, 1.25] {
            assert_eq!(Use::TILE.tier(scale), Tier::Row, "scale {scale}");
            assert_eq!(Use::of(168.0).tier(scale), Tier::Row, "scale {scale}");
        }
        assert_eq!(Use::TILE.tier(2.0), Tier::Large);
        assert_eq!(Use::of(250.0).tier(1.0), Tier::Row);
        assert_eq!(Use::of(250.0).tier(1.25), Tier::Large, "313 px are drawn");
        // what is asked is what is drawn from
        for side in [148.0, 168.0, 180.0, 212.0, 250.0] {
            for scale in [1.0, 1.25, 2.0] {
                assert_eq!(Use::of(side).tier(scale), Tier::for_draw(side, scale));
            }
        }
    }

    #[test]
    fn a_scale_change_asks_again_what_was_seen_at_the_tier_that_suits_it() {
        let mut q = Queue::default();
        let ids = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            q.ask(ids(&["a", "b"]), Use::TILE, 1.0, Instant::now(), false),
            at(&["a", "b"], Tier::Row)
        );
        q.ask(ids(&["s"]), Use::SMALL, 1.0, Instant::now(), false);
        assert!(
            q.ask(ids(&["a"]), Use::TILE, 1.0, Instant::now(), false)
                .is_empty(),
            "once"
        );
        assert!(
            q.ask_again(1.0, Instant::now()).is_empty(),
            "same scale, nothing new"
        );
        let mut again = q.ask_again(2.0, Instant::now());
        again.sort_by_key(|w| w.id.clone());
        assert_eq!(
            again,
            at(&["a", "b"], Tier::Large),
            "the tiles need more pixels; the small one is still a thumbnail"
        );
        assert!(q.ask_again(2.0, Instant::now()).is_empty(), "and only once");
        assert_eq!(
            q.ask_again(1.0, Instant::now()).len(),
            0,
            "back on the first screen: seen already"
        );
    }

    #[test]
    fn only_what_was_seen_is_asked_again_and_a_resize_does_not_grow_it() {
        let mut q = Queue::default();
        q.ask(
            vec!["seen".to_string()],
            Use::TILE,
            1.0,
            Instant::now(),
            false,
        );
        assert_eq!(q.ask_again(2.0, Instant::now()), at(&["seen"], Tier::Large));
        // a window being resized draws its grid at a different side at every step
        for side in 160..260 {
            q.ask(
                vec!["seen".to_string()],
                Use::of(side as f32),
                1.0,
                Instant::now(),
                false,
            );
        }
        assert_eq!(q.asked.len(), 1);
    }

    #[test]
    fn a_visible_ask_jumps_ahead_of_a_backlog() {
        let mut q = Queue::default();
        let backlog: Vec<Want> = (0..BATCH * 3)
            .map(|i| Want::new(format!("lib{i}"), Tier::Thumb))
            .collect();
        q.missing(backlog);
        q.missing_first(at(&["seen-now-1", "seen-now-2"], Tier::Row));
        let batch = q.start().unwrap();
        assert_eq!(batch[0], Want::new("seen-now-1", Tier::Row));
        assert_eq!(batch[1], Want::new("seen-now-2", Tier::Row));
        assert_eq!(
            batch[2],
            Want::new("lib0", Tier::Thumb),
            "the backlog follows"
        );
        // what was seen later goes before what was seen earlier (it is the one on screen)
        let mut q = Queue::default();
        q.missing(at(&["old"], Tier::Row));
        q.missing_first(at(&["new"], Tier::Row));
        assert_eq!(q.start().unwrap(), at(&["new", "old"], Tier::Row));
    }

    /// Issue #84: a long queue left a thousand covers waiting; a short queue played next showed
    /// some of them, which waited behind all the rest (and behind every grid asked since).
    #[test]
    fn a_cover_already_waiting_jumps_ahead_when_it_shows() {
        let (mut q, now) = (Queue::default(), Instant::now());
        let long: Vec<String> = (0..1000).map(|i| format!("t{i}")).collect();
        let new = q.ask(long, Use::SMALL, 1.0, now, false);
        q.missing(new);
        let shown = vec!["t900".to_string(), "t950".to_string()];
        assert!(
            q.ask(shown, Use::SMALL, 1.0, now, true).is_empty(),
            "looked up already"
        );
        let batch = q.start().unwrap();
        assert_eq!(batch[..2], at(&["t900", "t950"], Tier::Thumb));
        assert_eq!(batch[2], Want::new("t0", Tier::Thumb), "the rest follows");
    }

    #[test]
    fn a_cover_waiting_is_queued_once_even_after_the_queue_was_reordered() {
        let mut q = Queue::default();
        q.missing(at(&["a", "b"], Tier::Row));
        q.missing_first(at(&["b", "c"], Tier::Row));
        q.missing(at(&["a", "c"], Tier::Row));
        assert_eq!(q.start().unwrap(), at(&["b", "c", "a"], Tier::Row));
    }

    #[test]
    fn a_queue_of_thousands_is_deduplicated_without_a_quadratic_scan() {
        let mut q = Queue::default();
        let many: Vec<Want> = (0..20_000)
            .map(|i| Want::new(format!("c{i}"), Tier::Row))
            .collect();
        let t = Instant::now();
        q.missing(many.clone());
        q.missing(many.clone());
        q.missing_first(many);
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert_eq!(q.start().unwrap().len(), BATCH);
    }

    #[test]
    fn the_default_image_the_daemon_wrote_is_not_a_cover() {
        // an album without art that was played: the daemon wrote the server's default image to its
        // own file, beside the marker the thumbnail fetch left
        let (_d, env) = env();
        put_placeholder(&env, "p");
        put_tier(&env, "p", Tier::Large);
        let (found, missing) = resolve(&env, &at(&["p"], Tier::Row));
        assert!(found.is_empty(), "the glyph, not the server's image");
        assert!(missing.is_empty(), "and nothing to fetch");
    }
}
