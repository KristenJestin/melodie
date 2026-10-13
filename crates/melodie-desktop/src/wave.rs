//! The waveform of the progress bar, kept away from GPUI so that it is tested: which tracks still
//! need one (the current track and the next, never the library), how its levels become the bars
//! of a bar of some width, where a position on it lands in the track, and what shows while it is
//! missing. The levels are computed by `melodie waveform-fetch`; the window only reads them.

use crate::covers::Fetch;
use crate::model::Player;
use melodie_core::env::Env;
use melodie_core::waveform;
use std::collections::HashMap;
use std::sync::Arc;

/// How many times a waveform may come back from a finished `waveform-fetch` still missing
/// before the window keeps the plain bar for that track.
pub const MAX_TRIES: u32 = 2;
/// The width of a bar of the waveform and the gap after it, in logical pixels.
pub const BAR: f32 = 2.0;
pub const GAP: f32 = 1.5;

/// The levels of a track: [`waveform::LEVELS`] values from 0 to 255.
pub type Levels = Arc<[u8]>;

/// The waveforms in hand, and those the window waits for.
#[derive(Default)]
pub struct Waves {
    levels: HashMap<String, Levels>,
    /// The tracks wanted now, the current one first.
    wanted: Vec<String>,
    tries: HashMap<String, u32>,
    running: bool,
}

/// What one round of `waveform-fetch` gave.
#[derive(Debug)]
pub struct Round {
    pub found: Vec<(String, Levels)>,
    pub missing: Vec<String>,
    /// How the fetch ended; `None` when everything was on disk already.
    pub fetch: Option<Fetch>,
}

impl Waves {
    /// The levels of the track `id`, when they are in hand.
    pub fn get(&self, id: &str) -> Option<&Levels> {
        self.levels.get(id)
    }

    /// The tracks the window wants a waveform for now (what it wanted before no longer counts).
    pub fn want(&mut self, ids: Vec<String>) {
        self.wanted = ids;
    }

    /// The tracks for the next round, unless one is running or nothing is missing.
    pub fn start(&mut self) -> Option<Vec<String>> {
        if self.running {
            return None;
        }
        let batch: Vec<String> = self
            .wanted
            .iter()
            .filter(|id| !self.levels.contains_key(*id))
            .filter(|id| self.tries.get(*id).copied().unwrap_or(0) < MAX_TRIES)
            .cloned()
            .collect();
        self.running = !batch.is_empty();
        self.running.then_some(batch)
    }

    /// Takes a finished round in; true when it hit the lock and should be asked again later.
    pub fn finish(&mut self, round: Round) -> bool {
        self.running = false;
        self.levels.extend(round.found);
        match round.fetch {
            None => false,
            // nothing was done: the tracks keep their tries
            Some(Fetch::Locked) => true,
            Some(Fetch::Done | Fetch::Failed) => {
                for id in round.missing {
                    *self.tries.entry(id).or_default() += 1;
                }
                false
            }
        }
    }
}

/// The tracks the player wants a waveform for: the current one and the next, whatever changed
/// (the queue, or only the current track within it).
pub fn wanted(player: &Player) -> Vec<String> {
    let Some(current) = player.current else {
        return Vec::new();
    };
    player
        .tracks
        .iter()
        .skip(current)
        .take(2)
        .map(|t| t.id.clone())
        .collect()
}

/// Reads `batch` on disk, has `fetch` compute what is missing, and reads again. Reads files and
/// waits for the fetch: run it away from the window's thread.
pub fn round(env: &Env, batch: &[String], fetch: impl FnOnce(&[String]) -> Fetch) -> Round {
    let (mut found, missing) = read(env, batch);
    if missing.is_empty() {
        return Round {
            found,
            missing,
            fetch: None,
        };
    }
    let fetch = fetch(&missing);
    let (arrived, missing) = read(env, &missing);
    found.extend(arrived);
    Round {
        found,
        missing,
        fetch: Some(fetch),
    }
}

/// The waveforms of `ids` kept on disk, and the ids without one.
fn read(env: &Env, ids: &[String]) -> (Vec<(String, Levels)>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for id in ids {
        match waveform::read(env, id) {
            Some(levels) => found.push((id.clone(), Levels::from(levels))),
            None => missing.push(id.clone()),
        }
    }
    (found, missing)
}

/// How many bars a waveform `width` pixels wide draws.
pub fn count(width: f32) -> usize {
    (((width + GAP) / (BAR + GAP)).floor() as usize).max(1)
}

/// The height of the bar `i` of `count`, from 0 to 1: the loudest level it covers.
pub fn bar(levels: &[u8], i: usize, count: usize) -> f32 {
    let n = levels.len();
    if n == 0 || count == 0 {
        return 0.0;
    }
    let from = (i * n / count).min(n - 1);
    let to = ((i + 1) * n / count).clamp(from + 1, n);
    let top = levels[from..to].iter().copied().max().unwrap_or(0);
    f32::from(top) / 255.0
}

/// The time a position of the bar lands on: `value` of `steps`, in a track of `duration` seconds.
pub fn time_at(value: f32, steps: f32, duration: f64) -> f64 {
    if duration <= 0.0 || steps <= 0.0 {
        return 0.0;
    }
    f64::from((value / steps).clamp(0.0, 1.0)) * duration
}

/// How much of the plain bar and of the waveform shows: `shown` goes from 0 to 1 as the waveform
/// fades in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub plain: f32,
    pub wave: f32,
}

/// What the bar shows, with or without the levels of the track playing.
pub fn look(has_levels: bool, shown: f32) -> Look {
    if !has_levels {
        return Look {
            plain: 1.0,
            wave: 0.0,
        };
    }
    let wave = shown.clamp(0.0, 1.0);
    Look {
        plain: 1.0 - wave,
        wave,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Clock;
    use melodie_core::proto::{Event, QueueEntry, QueueView};
    use melodie_core::waveform::LEVELS;
    use std::cell::Cell;
    use std::time::Instant;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn found(id: &str) -> (String, Levels) {
        (id.to_string(), Arc::from(vec![9; LEVELS]))
    }

    #[test]
    fn the_current_track_and_the_next_one_are_asked_and_nothing_else() {
        let mut w = Waves::default();
        w.want(ids(&["t1", "t2"]));
        assert_eq!(w.start(), Some(ids(&["t1", "t2"])));
        // one round at a time
        w.want(ids(&["t2", "t3"]));
        assert_eq!(w.start(), None);
        let wait = w.finish(Round {
            found: vec![found("t1"), found("t2")],
            missing: vec![],
            fetch: Some(Fetch::Done),
        });
        assert!(!wait);
        // the next track became current: only what is still missing of what is wanted now
        assert_eq!(w.start(), Some(ids(&["t3"])));
    }

    #[test]
    fn advancing_within_the_same_tracks_changes_the_wanted_waveforms() {
        let tracks: Vec<QueueEntry> = (1..=10)
            .map(|n| QueueEntry {
                entry: n,
                id: format!("t{n}"),
                ..QueueEntry::default()
            })
            .collect();
        let (mut p, mut c, at) = (Player::default(), Clock::default(), Instant::now());
        assert_eq!(wanted(&p), Vec::<String>::new());
        p.apply(
            &Event::Queue {
                queue: QueueView {
                    current: Some(0),
                    tracks: tracks.clone(),
                },
            },
            &mut c,
            at,
        );
        assert_eq!(wanted(&p), ids(&["t1", "t2"]));
        let queue = p.tracks.clone();
        // the album goes on, then a later track is clicked: the queue itself does not change
        for (index, want) in [(2, ["t3", "t4"]), (6, ["t7", "t8"])] {
            p.apply(
                &Event::Track {
                    index: Some(index),
                    track: Some(tracks[index].clone()),
                },
                &mut c,
                at,
            );
            assert!(Arc::ptr_eq(&queue, &p.tracks));
            assert_eq!(wanted(&p), ids(&want));
        }
        // the last track: no next one
        p.apply(
            &Event::Track {
                index: Some(9),
                track: Some(tracks[9].clone()),
            },
            &mut c,
            at,
        );
        assert_eq!(wanted(&p), ids(&["t10"]));
    }

    #[test]
    fn a_waveform_in_hand_is_not_asked_again() {
        let mut w = Waves::default();
        w.want(ids(&["t1"]));
        let batch = w.start().unwrap();
        w.finish(Round {
            found: vec![found(&batch[0])],
            missing: vec![],
            fetch: None,
        });
        assert!(w.get("t1").is_some());
        w.want(ids(&["t1"]));
        assert_eq!(w.start(), None);
    }

    #[test]
    fn a_waveform_that_cannot_be_computed_is_tried_a_bounded_number_of_times() {
        let mut w = Waves::default();
        w.want(ids(&["t1"]));
        for _ in 0..MAX_TRIES {
            assert_eq!(w.start(), Some(ids(&["t1"])));
            w.finish(Round {
                found: vec![],
                missing: ids(&["t1"]),
                fetch: Some(Fetch::Done),
            });
        }
        assert_eq!(w.start(), None);
        assert!(w.get("t1").is_none());
    }

    #[test]
    fn a_locked_fetch_is_asked_again_without_using_up_the_tries() {
        let mut w = Waves::default();
        w.want(ids(&["t1"]));
        for _ in 0..MAX_TRIES + 2 {
            assert_eq!(w.start(), Some(ids(&["t1"])));
            assert!(w.finish(Round {
                found: vec![],
                missing: ids(&["t1"]),
                fetch: Some(Fetch::Locked),
            }));
        }
        assert_eq!(w.start(), Some(ids(&["t1"])));
    }

    #[test]
    fn the_cache_is_reused_without_a_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        let kept: Vec<u8> = (0..LEVELS).map(|i| (i % 256) as u8).collect();
        waveform::write(&env, "t1", &kept).unwrap();
        let fetched = Cell::new(false);
        let r = round(&env, &ids(&["t1"]), |_| {
            fetched.set(true);
            Fetch::Done
        });
        assert!(!fetched.get());
        assert_eq!(r.fetch, None);
        assert_eq!(r.found.len(), 1);
        assert_eq!(&*r.found[0].1, kept.as_slice());
    }

    #[test]
    fn what_the_fetch_computed_is_read_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        let r = round(&env, &ids(&["t1", "t2"]), |missing| {
            assert_eq!(missing, ids(&["t1", "t2"]).as_slice());
            waveform::write(&env, "t2", &[5; LEVELS]).unwrap();
            Fetch::Done
        });
        assert_eq!(r.fetch, Some(Fetch::Done));
        assert_eq!(r.missing, ids(&["t1"]));
        assert_eq!(r.found.len(), 1);
        assert_eq!(r.found[0].0, "t2");
    }

    #[test]
    fn the_bar_falls_back_while_the_waveform_is_missing() {
        let w = Waves::default();
        assert!(w.get("t1").is_none());
        // whatever the fade says, no levels: the plain bar alone
        for shown in [0.0, 0.5, 1.0] {
            let l = look(false, shown);
            assert_eq!((l.plain, l.wave), (1.0, 0.0));
        }
        // the levels land: the waveform fades in over the plain bar, which fades out
        assert_eq!(
            look(true, 0.0),
            Look {
                plain: 1.0,
                wave: 0.0
            }
        );
        assert_eq!(
            look(true, 0.25),
            Look {
                plain: 0.75,
                wave: 0.25
            }
        );
        assert_eq!(
            look(true, 1.0),
            Look {
                plain: 0.0,
                wave: 1.0
            }
        );
    }

    #[test]
    fn seeking_maps_a_position_to_a_time() {
        assert_eq!(time_at(0.0, 1000.0, 240.0), 0.0);
        assert_eq!(time_at(500.0, 1000.0, 240.0), 120.0);
        assert_eq!(time_at(1000.0, 1000.0, 240.0), 240.0);
        assert_eq!(time_at(250.0, 1000.0, 200.0), 50.0);
        // past the ends, and a track whose length is not known yet
        assert_eq!(time_at(-3.0, 1000.0, 240.0), 0.0);
        assert_eq!(time_at(1200.0, 1000.0, 240.0), 240.0);
        assert_eq!(time_at(500.0, 1000.0, 0.0), 0.0);
    }

    #[test]
    fn a_bar_shows_the_loudest_level_it_covers() {
        let mut levels = vec![0u8; LEVELS];
        levels[LEVELS - 1] = 255;
        levels[0] = 51;
        // 4 bars of 100 levels each
        assert_eq!(bar(&levels, 0, 4), 0.2);
        assert_eq!(bar(&levels, 1, 4), 0.0);
        assert_eq!(bar(&levels, 3, 4), 1.0);
        // more bars than levels: each still shows one
        assert_eq!(bar(&levels, 2 * LEVELS - 1, 2 * LEVELS), 1.0);
        assert_eq!(bar(&[], 0, 4), 0.0);
    }

    #[test]
    fn the_number_of_bars_follows_the_width() {
        assert_eq!(count(0.0), 1);
        assert_eq!(count(BAR), 1);
        assert_eq!(count(BAR + GAP + BAR), 2);
        assert_eq!(count(350.0), (350.0 / (BAR + GAP)) as usize);
    }
}
