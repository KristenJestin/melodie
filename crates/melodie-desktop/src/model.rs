//! What the window shows of the player, kept in step with the daemon's event stream
//! (`docs/events.md`). No drawing here: the views read it.

use melodie_core::proto::{Event, QueueEntry, QueueView};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayState {
    Playing,
    Paused,
    #[default]
    Stopped,
}

impl PlayState {
    fn parse(s: &str) -> Self {
        match s {
            "playing" => Self::Playing,
            "paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }
}

/// What the last call to the server said (the `connection` of the stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connection {
    #[default]
    Ok,
    Unreachable,
    AuthRefused,
}

impl Connection {
    fn parse(s: &str) -> Self {
        match s {
            "unreachable" => Self::Unreachable,
            "auth-refused" => Self::AuthRefused,
            _ => Self::Ok,
        }
    }
}

/// Where a row of the queue stands against the current track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Past,
    Current,
    Next,
}

/// The player as the daemon last described it.
#[derive(Debug, Clone, Default)]
pub struct Player {
    /// An event stream is open: a daemon is there.
    pub following: bool,
    pub state: PlayState,
    /// Shared with the views, which take it without copying the tracks.
    pub tracks: Arc<[QueueEntry]>,
    pub current: Option<usize>,
    /// 0 to 1.3 (1 = 100 %).
    pub volume: f64,
    pub connection: Connection,
    /// Grows by one at every change of the current track, so a view can animate it.
    pub track_change: u64,
}

/// What an event changed, so that only the views that show it are rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Changed {
    pub player: bool,
    pub clock: bool,
}

impl Player {
    /// Applies one event of the stream. Every event carries the new value, so applying one twice
    /// changes nothing the second time.
    pub fn apply(&mut self, event: &Event, clock: &mut Clock, at: Instant) -> Changed {
        let was = self.track().map(|t| t.entry);
        let mut changed = Changed {
            player: true,
            clock: false,
        };
        match event {
            Event::Snapshot {
                now,
                queue,
                connection,
            } => {
                self.following = true;
                self.set_queue(queue);
                self.state = PlayState::parse(&now.state);
                self.volume = now.volume;
                self.connection = Connection::parse(connection);
                clock.duration = now.duration;
                clock.set(now.position, self.state, at);
                changed.clock = true;
            }
            Event::Queue { queue } => self.set_queue(queue),
            Event::Track { index, track } => {
                self.current = *index;
                if let (Some(i), Some(t)) = (index, track)
                    && i < &self.tracks.len()
                    && self.tracks[*i].entry == t.entry
                    && self.tracks[*i] != *t
                {
                    let mut tracks = self.tracks.to_vec();
                    tracks[*i] = t.clone();
                    self.tracks = tracks.into();
                }
            }
            Event::State { state, position } => {
                self.state = PlayState::parse(state);
                clock.set(*position, self.state, at);
                changed.clock = true;
            }
            Event::Position { position, duration } => {
                clock.duration = *duration;
                clock.set(*position, self.state, at);
                return Changed {
                    player: false,
                    clock: true,
                };
            }
            Event::Volume { volume } => self.volume = *volume,
            Event::Starred { id, starred } => {
                self.tracks = self
                    .tracks
                    .iter()
                    .map(|t| {
                        let mut t = t.clone();
                        if t.id == *id {
                            t.starred = *starred;
                        }
                        t
                    })
                    .collect();
            }
            Event::Connection { state } => self.connection = Connection::parse(state),
        }
        if self.track().map(|t| t.entry) != was {
            self.track_change += 1;
            // A `queue` event can change the current entry too (it is removed, or `play` replaces
            // the queue), and the `track` that follows then finds nothing new. A snapshot brings
            // its own position.
            if !matches!(event, Event::Snapshot { .. }) {
                clock.duration = self.track().map_or(0.0, |t| t.duration);
                clock.set(0.0, self.state, at);
                changed.clock = true;
            }
        }
        changed
    }

    fn set_queue(&mut self, queue: &QueueView) {
        self.current = queue.current;
        if *self.tracks != *queue.tracks {
            self.tracks = queue.tracks.clone().into();
        }
    }

    /// The stream ended: no daemon, so nothing plays and there is no queue.
    pub fn gone(&mut self, clock: &mut Clock) -> Changed {
        if !self.following && self.tracks.is_empty() {
            return Changed::default();
        }
        let had_track = self.track().is_some();
        *self = Self {
            track_change: self.track_change + u64::from(had_track),
            volume: self.volume,
            // The daemon quits when the server refuses the login: the window keeps saying why.
            connection: match self.connection {
                Connection::AuthRefused => Connection::AuthRefused,
                _ => Connection::Ok,
            },
            ..Self::default()
        };
        *clock = Clock::default();
        Changed {
            player: true,
            clock: true,
        }
    }

    pub fn track(&self) -> Option<&QueueEntry> {
        self.current.and_then(|i| self.tracks.get(i))
    }

    pub fn row(&self, index: usize) -> Row {
        match self.current {
            Some(c) if index < c => Row::Past,
            Some(c) if index == c => Row::Current,
            _ => Row::Next,
        }
    }

    /// Whether row `ix` still holds `entry`. A row's actions carry the position and the entry
    /// seen at render time; another client may have changed the queue since, and the daemon
    /// only knows positions.
    pub fn holds(&self, ix: usize, entry: u64) -> bool {
        self.tracks.get(ix).is_some_and(|t| t.entry == entry)
    }

    /// The `(from, to)` of a `queue_move` for dropping the dragged row on another, `None` when
    /// either row is not where the window saw it.
    pub fn move_for(&self, dragged: (usize, u64), onto: (usize, u64)) -> Option<(usize, usize)> {
        if !self.holds(dragged.0, dragged.1) || !self.holds(onto.0, onto.1) {
            return None;
        }
        drop_move(dragged.0, onto.0, self.tracks.len())
    }

    /// Whether there is something after the current track (what "clear what comes next" removes).
    pub fn has_next(&self) -> bool {
        self.current.is_some_and(|c| c + 1 < self.tracks.len())
    }

    /// Whether shuffling what comes next can change anything: at least two tracks come after
    /// the current one.
    pub fn can_shuffle_next(&self) -> bool {
        self.current.is_some_and(|c| c + 2 < self.tracks.len())
    }
}

/// The playback position. The daemon says it at most once a second; between two of its
/// messages the window counts on its own clock, so the bar can move four times a second.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Clock {
    /// Seconds at `since`.
    pub position: f64,
    pub duration: f64,
    pub running: bool,
    pub since: Option<Instant>,
}

impl Clock {
    /// Seconds into the track at `at`, never past its end.
    pub fn at(&self, at: Instant) -> f64 {
        let ran = match (self.running, self.since) {
            (true, Some(since)) => at.saturating_duration_since(since).as_secs_f64(),
            _ => 0.0,
        };
        let p = self.position + ran;
        if self.duration > 0.0 {
            p.min(self.duration)
        } else {
            p
        }
    }

    fn set(&mut self, position: f64, state: PlayState, at: Instant) {
        self.position = position;
        self.running = state == PlayState::Playing;
        self.since = Some(at);
    }
}

/// The slider's end, in percent: what the daemon accepts (`docs/events.md`, `volume` up to 1.3).
pub const VOLUME_SLIDER_MAX: f32 = melodie_core::prefs::MAX_VOLUME as f32;

/// The volume slider's value (percent) for the daemon's volume (1 = 100 %).
pub fn volume_percent(v: f64) -> f32 {
    (v * 100.0).clamp(0.0, f64::from(VOLUME_SLIDER_MAX)) as f32
}

/// What a mute sets the volume to, and the volume to keep for the next click. The daemon has no
/// mute: the window mutes by setting 0, and a second click brings back what was there before.
pub fn mute(now: f64, before: Option<f64>) -> (f64, Option<f64>) {
    if now > 0.0 {
        (0.0, Some(now))
    } else {
        (before.unwrap_or(0.5), None)
    }
}

/// The `to` of a `queue_move` when the row `from` is dropped on the row `onto`; `None` when it
/// does not move.
pub fn drop_move(from: usize, onto: usize, len: usize) -> Option<(usize, usize)> {
    if from >= len {
        return None;
    }
    let to = onto.min(len - 1);
    (to != from).then_some((from, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use melodie_core::proto::Now;
    use std::time::Duration;

    fn entry(n: u64, id: &str, title: &str) -> QueueEntry {
        QueueEntry {
            entry: n,
            id: id.into(),
            title: title.into(),
            artist: "An Artist".into(),
            album: "An Album".into(),
            duration: 200.0,
            quality: "FLAC · 16 bit / 44.1 kHz".into(),
            ..QueueEntry::default()
        }
    }

    fn queue(current: Option<usize>, tracks: Vec<QueueEntry>) -> QueueView {
        QueueView { current, tracks }
    }

    fn snapshot() -> Event {
        Event::Snapshot {
            now: Now {
                state: "playing".into(),
                id: "t2".into(),
                title: "Second".into(),
                duration: 200.0,
                position: 12.0,
                volume: 0.8,
                ..Now::default()
            },
            queue: queue(
                Some(1),
                vec![
                    entry(1, "t1", "First"),
                    entry(2, "t2", "Second"),
                    entry(3, "t3", "Third"),
                ],
            ),
            connection: "ok".into(),
        }
    }

    /// Feeds a scripted stream and returns the model and its clock.
    fn feed(events: &[Event]) -> (Player, Clock, Instant) {
        let t0 = Instant::now();
        let (mut p, mut c) = (Player::default(), Clock::default());
        for e in events {
            p.apply(e, &mut c, t0);
        }
        (p, c, t0)
    }

    #[test]
    fn a_snapshot_shows_the_current_state_at_once() {
        let (p, c, t0) = feed(&[snapshot()]);
        assert!(p.following);
        assert_eq!(p.state, PlayState::Playing);
        assert_eq!(p.track().unwrap().title, "Second");
        assert_eq!(p.tracks.len(), 3);
        assert_eq!(p.volume, 0.8);
        assert_eq!(p.connection, Connection::Ok);
        assert_eq!(c.duration, 200.0);
        assert_eq!(c.at(t0), 12.0);
    }

    #[test]
    fn a_track_change_moves_the_current_track_and_restarts_the_clock() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        let before = p.track_change;
        let ch = p.apply(
            &Event::Track {
                index: Some(2),
                track: Some(entry(3, "t3", "Third")),
            },
            &mut c,
            t0,
        );
        assert!(ch.player && ch.clock);
        assert_eq!(p.track().unwrap().title, "Third");
        assert_eq!(p.track_change, before + 1);
        assert_eq!(c.at(t0), 0.0);
        assert_eq!(c.duration, 200.0);
        // the same event again (the cover arrived) changes the entry, not the track
        let mut with_cover = entry(3, "t3", "Third");
        with_cover.cover = Some("/tmp/c3.img".into());
        p.apply(
            &Event::Track {
                index: Some(2),
                track: Some(with_cover),
            },
            &mut c,
            t0,
        );
        assert_eq!(p.track_change, before + 1);
        assert_eq!(p.track().unwrap().cover.as_deref(), Some("/tmp/c3.img"));
    }

    #[test]
    fn a_queue_event_that_changes_the_current_track_restarts_the_clock() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        let before = p.track_change;
        // the current track was removed: the next one slides into its place, then `track` follows
        let mut third = entry(3, "t3", "Third");
        third.duration = 90.0;
        let ch = p.apply(
            &Event::Queue {
                queue: queue(Some(1), vec![entry(1, "t1", "First"), third.clone()]),
            },
            &mut c,
            t0,
        );
        assert!(ch.player && ch.clock);
        assert_eq!(p.track().unwrap().entry, 3);
        assert_eq!(p.track_change, before + 1);
        assert_eq!(c.at(t0), 0.0);
        assert_eq!(c.duration, 90.0);
        // the `track` event that follows finds the same entry: nothing more to restart
        let ch = p.apply(
            &Event::Track {
                index: Some(1),
                track: Some(third),
            },
            &mut c,
            t0,
        );
        assert!(!ch.clock);
        assert_eq!(p.track_change, before + 1);
    }

    #[test]
    fn no_current_track_after_the_end() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        p.apply(
            &Event::Track {
                index: None,
                track: None,
            },
            &mut c,
            t0,
        );
        assert!(p.track().is_none());
        assert_eq!(p.tracks.len(), 3, "the rest of the queue stays");
    }

    #[test]
    fn a_queue_change_replaces_the_rows() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        let before = p.track_change;
        // the current track moved to the front: same track, new index
        let ch = p.apply(
            &Event::Queue {
                queue: queue(
                    Some(0),
                    vec![
                        entry(2, "t2", "Second"),
                        entry(1, "t1", "First"),
                        entry(3, "t3", "Third"),
                    ],
                ),
            },
            &mut c,
            t0,
        );
        assert!(ch.player);
        assert!(!ch.clock, "moving the current track does not restart it");
        assert_eq!(p.current, Some(0));
        assert_eq!(p.track().unwrap().entry, 2);
        assert_eq!(p.track_change, before, "the same entry is still playing");
        assert_eq!(p.row(0), Row::Current);
        assert_eq!(p.row(1), Row::Next);
        assert!(p.has_next());
        // clear what comes next
        p.apply(
            &Event::Queue {
                queue: queue(Some(0), vec![entry(2, "t2", "Second")]),
            },
            &mut c,
            t0,
        );
        assert!(!p.has_next());
    }

    #[test]
    fn shuffling_what_comes_next_needs_two_tracks_after_the_current_one() {
        let at = |current, n: u64| {
            let tracks = (0..n).map(|i| entry(i + 1, "t", "T")).collect();
            let (mut p, mut c) = (Player::default(), Clock::default());
            p.apply(
                &Event::Queue {
                    queue: queue(current, tracks),
                },
                &mut c,
                Instant::now(),
            );
            p.can_shuffle_next()
        };
        assert!(!at(None, 4), "nothing is current");
        assert!(!at(Some(3), 4), "the current track is the last");
        assert!(!at(Some(2), 4), "one track next");
        assert!(at(Some(1), 4));
        assert!(at(Some(0), 3));
    }

    #[test]
    fn past_current_and_next_rows() {
        let (p, _, _) = feed(&[snapshot()]);
        assert_eq!(p.row(0), Row::Past);
        assert_eq!(p.row(1), Row::Current);
        assert_eq!(p.row(2), Row::Next);
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        p.apply(
            &Event::Track {
                index: None,
                track: None,
            },
            &mut c,
            t0,
        );
        assert_eq!(
            p.row(0),
            Row::Next,
            "nothing current: every row can be played"
        );
    }

    #[test]
    fn state_position_volume_starred_and_connection() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        let ch = p.apply(
            &Event::State {
                state: "paused".into(),
                position: 30.0,
            },
            &mut c,
            t0,
        );
        assert!(ch.player && ch.clock);
        assert_eq!(p.state, PlayState::Paused);
        assert_eq!(c.at(t0 + Duration::from_secs(5)), 30.0, "paused: still");
        let ch = p.apply(
            &Event::Position {
                position: 31.0,
                duration: 210.0,
            },
            &mut c,
            t0,
        );
        assert_eq!(
            ch,
            Changed {
                player: false,
                clock: true
            },
            "a tick rebuilds the bar only"
        );
        assert_eq!(c.duration, 210.0);
        p.apply(&Event::Volume { volume: 0.5 }, &mut c, t0);
        assert_eq!(p.volume, 0.5);
        p.apply(
            &Event::Starred {
                id: "t3".into(),
                starred: true,
            },
            &mut c,
            t0,
        );
        assert!(p.tracks[2].starred);
        assert!(!p.tracks[1].starred);
        p.apply(
            &Event::Connection {
                state: "unreachable".into(),
            },
            &mut c,
            t0,
        );
        assert_eq!(p.connection, Connection::Unreachable);
        p.apply(
            &Event::Connection {
                state: "auth-refused".into(),
            },
            &mut c,
            t0,
        );
        assert_eq!(p.connection, Connection::AuthRefused);
    }

    #[test]
    fn applying_an_event_twice_is_harmless() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        let e = Event::Track {
            index: Some(0),
            track: Some(entry(1, "t1", "First")),
        };
        p.apply(&e, &mut c, t0);
        let n = p.track_change;
        let ch = p.apply(&e, &mut c, t0);
        assert_eq!(p.track_change, n);
        assert!(!ch.clock, "the same track again does not restart the clock");
    }

    #[test]
    fn the_clock_runs_while_playing_and_stops_at_the_end() {
        let (_, c, t0) = feed(&[snapshot()]);
        assert_eq!(c.at(t0 + Duration::from_millis(1500)), 13.5);
        assert_eq!(c.at(t0 + Duration::from_secs(900)), 200.0);
    }

    #[test]
    fn daemon_gone_and_back() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        let ch = p.gone(&mut c);
        assert!(ch.player && ch.clock);
        assert!(!p.following);
        assert_eq!(p.state, PlayState::Stopped);
        assert!(p.track().is_none());
        assert!(p.tracks.is_empty());
        assert_eq!(c.at(t0), 0.0);
        // a second end changes nothing
        assert_eq!(p.gone(&mut c), Changed::default());
        // a new daemon: a new snapshot
        p.apply(&snapshot(), &mut c, t0);
        assert!(p.following);
        assert_eq!(p.track().unwrap().title, "Second");
    }

    #[test]
    fn a_refused_login_is_still_said_after_the_daemon_quits() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        p.apply(
            &Event::Connection {
                state: "auth-refused".into(),
            },
            &mut c,
            t0,
        );
        p.gone(&mut c);
        assert_eq!(p.connection, Connection::AuthRefused);
        // an unreachable server is not said once the daemon has gone: it may be back
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        p.apply(
            &Event::Connection {
                state: "unreachable".into(),
            },
            &mut c,
            t0,
        );
        p.gone(&mut c);
        assert_eq!(p.connection, Connection::Ok);
    }

    #[test]
    fn a_snapshot_after_a_reconnection_counts_as_a_track_change_only_if_the_track_differs() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        let n = p.track_change;
        p.apply(&snapshot(), &mut c, t0);
        assert_eq!(p.track_change, n);
    }

    #[test]
    fn a_volume_above_100_percent_set_from_the_command_line_shows_as_it_is() {
        assert_eq!(volume_percent(0.0), 0.0);
        assert_eq!(volume_percent(0.8), 80.0);
        assert_eq!(volume_percent(1.2), 120.0);
        assert_eq!(
            volume_percent(1.3),
            VOLUME_SLIDER_MAX,
            "the slider reaches what the daemon allows"
        );
        assert_eq!(volume_percent(2.0), VOLUME_SLIDER_MAX);
        assert_eq!(volume_percent(-1.0), 0.0);
    }

    #[test]
    fn a_mute_sets_zero_and_the_next_click_brings_the_volume_back() {
        let (muted, kept) = mute(0.8, None);
        assert_eq!((muted, kept), (0.0, Some(0.8)));
        assert_eq!(mute(muted, kept), (0.8, None));
    }

    #[test]
    fn a_mute_with_nothing_to_come_back_to_unmutes_to_half() {
        // the window opened with the volume at zero, or the volume was dragged to zero
        assert_eq!(mute(0.0, None), (0.5, None));
    }

    #[test]
    fn a_volume_changed_after_the_mute_is_muted_again_not_restored() {
        // muted at 0.8, then the slider moved to 0.3: a click mutes, it does not jump to 0.8
        assert_eq!(mute(0.3, Some(0.8)), (0.0, Some(0.3)));
    }

    #[test]
    fn a_second_click_on_remove_does_not_remove_the_neighbour() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        // the cross of the first row was clicked: position 0, entry 1
        assert!(p.holds(0, 1));
        // the queue event of that removal arrives
        p.apply(
            &Event::Queue {
                queue: queue(
                    Some(0),
                    vec![entry(2, "t2", "Second"), entry(3, "t3", "Third")],
                ),
            },
            &mut c,
            t0,
        );
        // a second click with what was captured before: the row at 0 is now another track
        assert!(!p.holds(0, 1));
        assert!(p.holds(0, 2));
        assert!(!p.holds(5, 1), "a position past the end holds nothing");
    }

    #[test]
    fn a_drag_during_another_clients_change_moves_nothing() {
        let (mut p, mut c, t0) = feed(&[snapshot()]);
        assert_eq!(p.move_for((0, 1), (2, 3)), Some((0, 2)));
        // another client removed the first track while the row was dragged
        p.apply(
            &Event::Queue {
                queue: queue(
                    Some(0),
                    vec![entry(2, "t2", "Second"), entry(3, "t3", "Third")],
                ),
            },
            &mut c,
            t0,
        );
        assert_eq!(p.move_for((0, 1), (1, 3)), None, "the dragged row is gone");
        assert_eq!(p.move_for((0, 2), (2, 3)), None, "the target moved");
        assert_eq!(p.move_for((0, 2), (1, 3)), Some((0, 1)));
    }

    #[test]
    fn dropping_a_row_on_another_moves_it_there() {
        assert_eq!(drop_move(4, 1, 6), Some((4, 1)));
        assert_eq!(drop_move(0, 5, 6), Some((0, 5)));
        assert_eq!(drop_move(2, 2, 6), None);
        assert_eq!(drop_move(2, 9, 6), Some((2, 5)), "past the end: last");
        assert_eq!(drop_move(7, 1, 6), None, "a row that is gone");
    }
}
