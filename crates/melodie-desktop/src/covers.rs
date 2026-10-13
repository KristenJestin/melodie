//! Which covers the window still has to get, and in what order: one `melodie cover-fetch` at a
//! time, fed by the covers (an id at a tier) that are missing on disk. Nothing here touches the
//! server or draws; the cover files known so far are looked up here too, for the views.

use melodie_core::cover::{Disk, Tier, Want};
use melodie_core::env::Env;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How many times a cover may come back from a finished `cover-fetch` still missing before the
/// window leaves it to what it has (the server errs on it, or the process stopped before it).
pub const MAX_TRIES: u32 = 3;
/// The covers a round hands to one `cover-fetch`.
const BATCH: usize = 48;
/// How long to wait before asking again when another `cover-fetch` held the lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(2);

/// What the window draws a cover for, which tells the tier it wants. The sides are the largest
/// the views draw (the grids' cells grow with the window).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Use {
    /// The pictures of a list row.
    Small,
    /// Albums, playlists and artists in grids, rows and page headers.
    Tile,
    /// The home's highlight.
    Card,
}

impl Use {
    pub fn side(self) -> f32 {
        match self {
            Use::Small => 56.0,
            Use::Tile => 260.0,
            Use::Card => 212.0,
        }
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
    /// shown is remembered, so that the view can fade from one to the other.
    pub fn merge(&mut self, found: HashMap<String, Disk>, now: Instant) {
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
                    }
                }
            }
            self.by_id.insert(id, disk);
        }
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
    tries: HashMap<Want, u32>,
    running: bool,
}

impl Queue {
    /// The covers not asked before (each is looked up once, then queued if missing).
    pub fn see(&mut self, wants: impl IntoIterator<Item = Want>) -> Vec<Want> {
        wants
            .into_iter()
            .filter(|w| self.seen.insert(w.clone()))
            .collect()
    }

    /// Queues covers that have no file yet, behind those already waiting.
    pub fn missing(&mut self, wants: impl IntoIterator<Item = Want>) {
        for w in wants {
            if !self.pending.contains(&w) {
                self.pending.push(w);
            }
        }
    }

    /// Queues covers that have no file yet, before those already waiting: what is playing.
    pub fn missing_first(&mut self, wants: Vec<Want>) {
        let rest = std::mem::take(&mut self.pending);
        self.missing(wants);
        self.missing(rest);
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
    pub fn finish(&mut self, batch: &[Want], round: &Round) -> bool {
        self.running = false;
        let gone: HashSet<&Want> = batch
            .iter()
            .filter(|w| !round.missing.contains(w))
            .collect();
        self.pending.retain(|w| !gone.contains(w));
        match round.fetch {
            None => {}
            // nothing was done: the covers keep their tries
            Some(Fetch::Locked) => return true,
            Some(Fetch::Done | Fetch::Failed) => {
                for w in &round.missing {
                    let tries = self.tries.entry(w.clone()).or_default();
                    *tries += 1;
                    if *tries >= MAX_TRIES {
                        self.pending.retain(|p| p != w);
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
        mut fetch: impl FnMut(&[Want]) -> Fetch,
    ) -> (usize, HashMap<String, Disk>) {
        let (mut fetches, mut all) = (0, HashMap::new());
        while let Some(batch) = queue.start() {
            let r = round(env, &batch, |wants| {
                fetches += 1;
                fetch(wants)
            });
            all.extend(r.found.clone());
            queue.finish(&batch, &r);
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
        assert!(q.finish(&batch, &r), "told to wait, then ask again");
        let batch = q.start().expect("the ids are still wanted");
        assert_eq!(batch, ids(&["a", "b"]));
        let r = round(&env, &batch, |wants| {
            wants.iter().for_each(|w| put_want(&env, w));
            Fetch::Done
        });
        assert_eq!(r.found.len(), 2);
        assert!(!q.finish(&batch, &r));
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
            q.finish(&batch, &r);
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
        assert_eq!(q.see(ids(&["p"])), ids(&["p"]));
        put_placeholder(&env, "p");
        for tier in Tier::ALL {
            q.missing(at(&["p"], tier));
        }
        let (fetches, found) = drain(&env, &mut q, |_| panic!("a placeholder is not fetched"));
        assert_eq!(fetches, 0);
        assert!(found.is_empty(), "the glyph stays");
        assert!(
            q.see(ids(&["p"])).is_empty(),
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
        assert!(q.start().is_none(), "left to the glyph for good");
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
        q.finish(&first, &r);
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
        assert_eq!(q.see(ids(&["a", "b", "a"])), ids(&["a", "b"]));
        assert!(q.see(ids(&["b"])).is_empty());
        q.missing(ids(&["a", "a"]));
        assert_eq!(q.start().unwrap(), ids(&["a"]));
    }

    #[test]
    fn the_same_cover_asked_at_another_tier_is_a_new_ask() {
        let mut q = Queue::default();
        assert_eq!(q.see(ids(&["a"])).len(), 1);
        assert_eq!(
            q.see(at(&["a"], Tier::Row)),
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
        files.merge(resolve(&env, &at(&["a"], Tier::Thumb)).0, t0);
        let before = files.get("a", Tier::Row, t0).unwrap();
        assert_eq!(before.path, cover::thumb_path(&env, "a"));

        put_tier(&env, "a", Tier::Row);
        let t1 = t0 + Duration::from_secs(5);
        files.merge(resolve(&env, &at(&["a"], Tier::Row)).0, t1);
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
        assert_eq!(Use::Small.tier(1.0), Tier::Thumb);
        assert_eq!(Use::Tile.tier(1.0), Tier::Row);
        assert_eq!(Use::Card.tier(1.0), Tier::Row);
        assert_eq!(Use::Tile.tier(2.0), Tier::Large);
        assert_eq!(Use::Small.tier(2.0), Tier::Thumb);
    }
}
