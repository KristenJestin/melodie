//! Which covers the window still has to get, and in what order: one `melodie cover-fetch` at a
//! time, fed by the ids that are missing on disk. Nothing here touches the server or draws.

use crate::browse::resolve_covers;
use melodie_core::env::Env;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

/// How many times an id may come back from a finished `cover-fetch` still missing before the
/// window leaves it to the glyph (the server errs on it, or the process stopped before it).
pub const MAX_TRIES: u32 = 3;
/// The ids a round hands to one `cover-fetch`.
const BATCH: usize = 48;
/// How long to wait before asking again when another `cover-fetch` held the lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(2);

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

/// What one round found.
#[derive(Debug)]
pub struct Round {
    /// The cover files on disk now.
    pub found: HashMap<String, PathBuf>,
    /// The ids still without a file or a marker.
    pub missing: Vec<String>,
    /// How the fetch ended; `None` when nothing needed one.
    pub fetch: Option<Fetch>,
}

/// Looks up `batch` on disk, has `fetch` get what is missing, and looks up again. Checks files
/// and waits for the fetch: run it away from the window's thread.
pub fn round(env: &Env, batch: &[String], fetch: impl FnOnce(&[String]) -> Fetch) -> Round {
    let (mut found, missing) = resolve_covers(env, batch.iter().map(String::as_str));
    if missing.is_empty() {
        return Round {
            found,
            missing,
            fetch: None,
        };
    }
    let fetch = fetch(&missing);
    let (arrived, missing) = resolve_covers(env, missing.iter().map(String::as_str));
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
    seen: HashSet<String>,
    pending: Vec<String>,
    tries: HashMap<String, u32>,
    running: bool,
}

impl Queue {
    /// The ids not asked before (each is looked up once, then queued if missing).
    pub fn see(&mut self, ids: impl IntoIterator<Item = String>) -> Vec<String> {
        ids.into_iter()
            .filter(|id| self.seen.insert(id.clone()))
            .collect()
    }

    /// Queues ids that have no file yet, behind those already waiting.
    pub fn missing(&mut self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            if !self.pending.contains(&id) {
                self.pending.push(id);
            }
        }
    }

    /// Queues ids that have no file yet, before those already waiting: what is playing.
    pub fn missing_first(&mut self, ids: Vec<String>) {
        let rest = std::mem::take(&mut self.pending);
        self.missing(ids);
        self.missing(rest);
    }

    /// The ids for the next round, unless one is running or nothing waits. A round takes a
    /// bounded share, so that what is asked meanwhile does not wait for a whole library.
    pub fn start(&mut self) -> Option<Vec<String>> {
        if self.running || self.pending.is_empty() {
            return None;
        }
        self.running = true;
        Some(self.pending.iter().take(BATCH).cloned().collect())
    }

    /// Takes a finished round in; true when it hit the lock and should be asked again after
    /// [`LOCK_WAIT`].
    pub fn finish(&mut self, batch: &[String], round: &Round) -> bool {
        self.running = false;
        let gone: HashSet<&String> = batch
            .iter()
            .filter(|id| !round.missing.contains(id))
            .collect();
        self.pending.retain(|id| !gone.contains(id));
        match round.fetch {
            None => {}
            // nothing was done: the ids keep their tries
            Some(Fetch::Locked) => return true,
            Some(Fetch::Done | Fetch::Failed) => {
                for id in &round.missing {
                    let tries = self.tries.entry(id.clone()).or_default();
                    *tries += 1;
                    if *tries >= MAX_TRIES {
                        self.pending.retain(|p| p != id);
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
    use melodie_core::cover;

    fn env() -> (tempfile::TempDir, Env) {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        (dir, env)
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn put(env: &Env, id: &str) {
        let thumb = cover::thumb_path(env, id);
        std::fs::create_dir_all(thumb.parent().unwrap()).unwrap();
        std::fs::write(thumb, "").unwrap();
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
        mut fetch: impl FnMut(&[String]) -> Fetch,
    ) -> (usize, HashMap<String, PathBuf>) {
        let (mut fetches, mut all) = (0, HashMap::new());
        while let Some(batch) = queue.start() {
            let r = round(env, &batch, |ids| {
                fetches += 1;
                fetch(ids)
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
        assert_eq!(batch, ["a", "b"]);
        let r = round(&env, &batch, |ids| {
            ids.iter().for_each(|id| put(&env, id));
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
        let (_, found) = drain(&env, &mut q, |ids| {
            ids.iter().for_each(|id| put(&env, id));
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
        let (fetches, found) = drain(&env, &mut q, |ids| {
            assert_eq!(ids, ["a", "b"]);
            put(&env, "a");
            put(&env, "b");
            Fetch::Done
        });
        assert_eq!(fetches, 1);
        assert_eq!(found["a"], cover::thumb_path(&env, "a"));
        assert_eq!(found["b"], cover::thumb_path(&env, "b"));

        // a file that came while the window waited needs no fetch of its own
        q.missing(ids(&["c"]));
        put(&env, "c");
        let (fetches, found) = drain(&env, &mut q, |_| panic!("already on disk"));
        assert_eq!(fetches, 0);
        assert!(found.contains_key("c"));
    }

    #[test]
    fn a_placeholder_is_not_asked_again() {
        let (_d, env) = env();
        let mut q = Queue::default();
        assert_eq!(q.see(ids(&["p"])), ["p"]);
        put_placeholder(&env, "p");
        let (fetches, found) = drain(&env, &mut q, |_| panic!("a placeholder is not fetched"));
        assert_eq!(fetches, 0);
        assert!(found.is_empty(), "the glyph stays");
        assert!(
            q.see(ids(&["p"])).is_empty(),
            "and it is not looked up again"
        );

        // the fetch itself finds the server's default image: a marker, no file
        q.missing(ids(&["q"]));
        let (fetches, found) = drain(&env, &mut q, |ids| {
            put_placeholder(&env, &ids[0]);
            Fetch::Done
        });
        assert_eq!(fetches, 1);
        assert!(found.is_empty());
        assert!(q.start().is_none(), "no second fetch for it");
    }

    #[test]
    fn a_download_that_fails_is_retried_a_bounded_number_of_times() {
        let (_d, env) = env();
        let mut q = Queue::default();
        q.missing(ids(&["bad", "good"]));
        let mut asked = Vec::new();
        let (fetches, found) = drain(&env, &mut q, |ids| {
            asked.push(ids.to_vec());
            if ids.iter().any(|i| i == "good") {
                put(&env, "good");
            }
            Fetch::Done
        });
        assert_eq!(fetches as u32, MAX_TRIES);
        assert_eq!(asked[0], ["bad", "good"]);
        assert_eq!(asked[1], ["bad"], "what arrived is not asked again");
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
        let r = round(&env, &first, |ids| {
            ids.iter().for_each(|id| put(&env, id));
            Fetch::Done
        });
        q.finish(&first, &r);
        assert_eq!(q.start().unwrap(), ["b"], "the next round takes what came");
    }

    #[test]
    fn what_plays_is_fetched_before_the_rest_and_a_round_is_bounded() {
        let mut q = Queue::default();
        let many: Vec<String> = (0..BATCH * 2).map(|i| format!("c{i}")).collect();
        q.missing(many.clone());
        q.missing_first(ids(&["now"]));
        let batch = q.start().unwrap();
        assert_eq!(batch.len(), BATCH, "a round does not take a whole library");
        assert_eq!(batch[0], "now");
        assert_eq!(batch[1], "c0");
    }

    #[test]
    fn an_id_asked_twice_is_queued_once() {
        let mut q = Queue::default();
        assert_eq!(q.see(ids(&["a", "b", "a"])), ["a", "b"]);
        assert!(q.see(ids(&["b"])).is_empty());
        q.missing(ids(&["a", "a"]));
        assert_eq!(q.start().unwrap(), ["a"]);
    }
}
