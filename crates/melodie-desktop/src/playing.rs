//! Which rows of which lists are the music playing now: a track row, the card of its album, the
//! row of its artist, the playlist it was started from. No drawing here; the views ask. It is
//! worked out from what the window already knows (the player, followed through the daemon's
//! events), plus one read of the local copy for the album and the artist of the current track:
//! a queue entry has only their names.

use crate::browse::Place;
use crate::model::{PlayState, Player};
use melodie_core::db;
use melodie_core::env::Env;
use melodie_core::library::{self, Track};
use std::sync::Arc;

/// How a row shows that its music is the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Playing: the equaliser moves.
    Playing,
    /// Paused, or stopped with the track still current: the same mark, still.
    Paused,
}

/// Where the current track is listed: ids in the local copy, which a queue entry does not carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origin {
    pub album: Option<String>,
    pub artist: Option<String>,
}

impl Origin {
    pub fn of(track: &Track) -> Self {
        Self {
            album: track.album_id.clone(),
            artist: track.artist_id.clone(),
        }
    }
}

/// The bars of the equaliser.
pub const BARS: usize = 3;

/// How high bar `bar` of the equaliser stands at `t`, a turn of its animation (0 to 1, the end
/// meeting the start so that it repeats without a jump): from a fifth of its room to all of it.
/// Each bar goes at its own pace and from its own place, so that they do not rise together.
pub fn level(bar: usize, t: f32) -> f32 {
    const TURNS: [f32; BARS] = [2.0, 3.0, 1.0];
    const FROM: [f32; BARS] = [0.0, 0.35, 0.7];
    let wave = (std::f32::consts::TAU * (TURNS[bar] * t + FROM[bar])).sin();
    0.2 + 0.8 * (0.5 + 0.5 * wave)
}

/// The album and the artist of the track `id` in the local copy. Does file I/O: never call it
/// while drawing. `None` without a copy, or when the copy lacks the track.
pub fn read_origin(env: &Env, id: &str) -> Option<Origin> {
    let conn = db::open_existing(env).ok().flatten()?;
    library::track(&conn, id)
        .ok()
        .flatten()
        .map(|t| Origin::of(&t))
}

/// The playlist the window started the queue from, tied to that queue by the numbers of its
/// entries (unique for the daemon's life, kept when an entry moves).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Source {
    playlist: String,
    /// The highest entry number seen when the play was asked: the queue it makes holds only
    /// higher ones.
    after: u64,
    /// The entries that queue was made of, in the order it was made. Empty until it is seen.
    entries: Arc<[u64]>,
}

/// What is playing, as the lists need it.
#[derive(Debug, Clone, Default)]
pub struct Playing {
    state: PlayState,
    /// The current queue entry (its number) and the track it holds.
    current: Option<(u64, String)>,
    /// The highest entry number of the queue now.
    seen: u64,
    source: Option<Source>,
    /// The origin of the track with this id.
    origin: Option<(String, Origin)>,
    /// The track whose origin is being read.
    asked: Option<String>,
}

/// How the rows of one list stand against the current track.
#[derive(Debug, Clone)]
pub struct Marks {
    mark: Option<Mark>,
    track: Option<String>,
    /// The only rank of the list that plays, when the queue says which (a playlist holding the
    /// track twice); else every row of that track.
    only: Option<usize>,
}

impl Marks {
    /// The mark of the row of rank `rank` (in the list's own order, whatever its sort shows)
    /// holding the track `id`.
    pub fn of(&self, id: &str, rank: usize) -> Option<Mark> {
        self.mark.filter(|_| {
            self.track.as_deref() == Some(id) && self.only.is_none_or(|only| only == rank)
        })
    }
}

impl Playing {
    /// After the player changed.
    pub fn follow(&mut self, player: &Player) {
        self.state = player.state;
        self.current = player.track().map(|t| (t.entry, t.id.clone()));
        self.seen = player.tracks.iter().map(|t| t.entry).max().unwrap_or(0);
        let entries = || player.tracks.iter().map(|t| t.entry);
        if player.tracks.is_empty() {
            // a daemon that quits numbers its entries again: the numbers of the queue that went
            // mean nothing to the next one
            if self.source.as_ref().is_some_and(|s| !s.entries.is_empty()) {
                self.source = None;
            }
        } else if let Some(s) = self.source.as_mut()
            && s.entries.is_empty()
            && entries().all(|e| e > s.after)
        {
            s.entries = entries().collect();
        }
    }

    /// The window asks for the playlist `id` to be played: the queue that follows is its.
    pub fn start_playlist(&mut self, id: &str) {
        self.source = Some(Source {
            playlist: id.to_string(),
            after: self.seen,
            entries: Arc::default(),
        });
    }

    /// The track whose album and artist are to be read, once; `None` when they are known, being
    /// read, or nothing plays.
    pub fn wants_origin(&mut self) -> Option<String> {
        let (_, id) = self.current.as_ref()?;
        let known = self.origin.as_ref().is_some_and(|(known, _)| known == id);
        if known || self.asked.as_ref() == Some(id) {
            return None;
        }
        self.asked = Some(id.clone());
        self.asked.clone()
    }

    /// What the read of the origin of track `id` found. Late news about another track is dropped.
    pub fn set_origin(&mut self, id: &str, origin: Option<Origin>) {
        if self.current.as_ref().is_some_and(|(_, now)| now == id) {
            // a track the copy does not know is remembered as listed nowhere, not asked again
            self.origin = Some((id.to_string(), origin.unwrap_or_default()));
        }
        if self.asked.as_deref() == Some(id) {
            self.asked = None;
        }
    }

    fn mark(&self) -> Option<Mark> {
        self.current.as_ref().map(|_| match self.state {
            PlayState::Playing => Mark::Playing,
            PlayState::Paused | PlayState::Stopped => Mark::Paused,
        })
    }

    /// The marks of the rows of `tracks`, the list shown at `list`.
    pub fn marks(&self, list: &Place, tracks: &[Track]) -> Marks {
        // The rank the queue is at, when the list is the playlist it was started from and the
        // queue still tells which row each entry came from: the entry's place among the entries
        // the play made, if that row holds the same track.
        let only = match (list, &self.source, &self.current) {
            (Place::Playlist(p), Some(s), Some((entry, id))) if s.playlist == *p => s
                .entries
                .iter()
                .position(|e| e == entry)
                .filter(|&rank| tracks.get(rank).is_some_and(|t| t.id == *id)),
            _ => None,
        };
        Marks {
            mark: self.mark(),
            track: self.current.as_ref().map(|(_, id)| id.clone()),
            only,
        }
    }

    fn origin(&self) -> Option<&Origin> {
        let (id, origin) = self.origin.as_ref()?;
        self.current
            .as_ref()
            .is_some_and(|(_, now)| now == id)
            .then_some(origin)
    }

    /// The mark of the album `id`.
    pub fn album(&self, id: &str) -> Option<Mark> {
        self.origin()
            .filter(|o| o.album.as_deref() == Some(id))
            .and_then(|_| self.mark())
    }

    /// The mark of the artist `id`.
    pub fn artist(&self, id: &str) -> Option<Mark> {
        self.origin()
            .filter(|o| o.artist.as_deref() == Some(id))
            .and_then(|_| self.mark())
    }

    /// The mark of the playlist `id`: only when the current entry is one of those the play of
    /// that playlist made.
    pub fn playlist(&self, id: &str) -> Option<Mark> {
        let (entry, _) = self.current.as_ref()?;
        self.source
            .as_ref()
            .filter(|s| s.playlist == id && s.entries.contains(entry))
            .and_then(|_| self.mark())
    }
}

#[cfg(test)]
mod tests {
    use super::{BARS, Mark, Origin, Playing, level, read_origin};
    use crate::browse::Place;
    use crate::model::{PlayState, Player};
    use melodie_core::db;
    use melodie_core::env::Env;
    use melodie_core::library::Track;
    use melodie_core::proto::QueueEntry;
    use serde_json::json;

    fn track(id: &str) -> Track {
        Track {
            id: id.into(),
            title: format!("Song {id}"),
            ..Track::default()
        }
    }

    fn entry(n: u64, id: &str) -> QueueEntry {
        QueueEntry {
            entry: n,
            id: id.into(),
            title: format!("Song {id}"),
            ..QueueEntry::default()
        }
    }

    /// A player whose queue holds `entries` (number, track id), at `current`.
    fn player(entries: &[(u64, &str)], current: Option<usize>, state: PlayState) -> Player {
        Player {
            following: true,
            state,
            tracks: entries.iter().map(|(n, id)| entry(*n, id)).collect(),
            current,
            ..Player::default()
        }
    }

    fn following(player: &Player) -> Playing {
        let mut p = Playing::default();
        p.follow(player);
        p
    }

    /// The marks of a list of tracks, in its own order.
    fn marked(p: &Playing, list: &Place, ids: &[&str]) -> Vec<Option<Mark>> {
        let tracks: Vec<Track> = ids.iter().map(|id| track(id)).collect();
        let marks = p.marks(list, &tracks);
        ids.iter()
            .enumerate()
            .map(|(rank, id)| marks.of(id, rank))
            .collect()
    }

    const PLAYING: Option<Mark> = Some(Mark::Playing);
    const PAUSED: Option<Mark> = Some(Mark::Paused);

    #[test]
    fn nothing_playing_marks_nothing() {
        let p = Playing::default();
        assert_eq!(marked(&p, &Place::Tracks, &["a", "b"]), [None, None]);
        assert_eq!(p.album("al1"), None);
        assert_eq!(p.artist("ar1"), None);
        assert_eq!(p.playlist("pl1"), None);
        // a queue with no current track: still nothing
        let p = following(&player(&[(1, "a")], None, PlayState::Stopped));
        assert_eq!(marked(&p, &Place::Tracks, &["a"]), [None]);
    }

    #[test]
    fn the_row_of_the_current_track_is_marked_in_any_table() {
        let p = following(&player(
            &[(1, "a"), (2, "b"), (3, "c")],
            Some(1),
            PlayState::Playing,
        ));
        for list in [
            Place::Tracks,
            Place::Album("al1".into()),
            Place::Artist("ar1".into()),
            Place::Playlist("pl1".into()),
            Place::Search,
        ] {
            assert_eq!(
                marked(&p, &list, &["a", "b", "c"]),
                [None, PLAYING, None],
                "{list:?}"
            );
        }
    }

    #[test]
    fn a_pause_keeps_the_mark_and_stills_it() {
        let q = &[(1, "a"), (2, "b")];
        let mut p = following(&player(q, Some(0), PlayState::Playing));
        assert_eq!(marked(&p, &Place::Tracks, &["a", "b"]), [PLAYING, None]);
        p.follow(&player(q, Some(0), PlayState::Paused));
        assert_eq!(marked(&p, &Place::Tracks, &["a", "b"]), [PAUSED, None]);
        // a stop keeps the track current, and the player keeps showing it: so do the lists
        p.follow(&player(q, Some(0), PlayState::Stopped));
        assert_eq!(marked(&p, &Place::Tracks, &["a", "b"]), [PAUSED, None]);
        p.follow(&player(q, Some(0), PlayState::Playing));
        assert_eq!(marked(&p, &Place::Tracks, &["a", "b"]), [PLAYING, None]);
    }

    #[test]
    fn the_mark_follows_the_next_track_and_the_end_of_the_queue() {
        let q = &[(1, "a"), (2, "b")];
        let mut p = following(&player(q, Some(0), PlayState::Playing));
        p.follow(&player(q, Some(1), PlayState::Playing));
        assert_eq!(marked(&p, &Place::Tracks, &["a", "b"]), [None, PLAYING]);
        // the daemon quits: nothing is current
        p.follow(&Player::default());
        assert_eq!(marked(&p, &Place::Tracks, &["a", "b"]), [None, None]);
    }

    #[test]
    fn a_track_listed_twice_is_marked_at_each_occurrence_when_the_queue_does_not_say() {
        let p = following(&player(&[(1, "x"), (2, "a")], Some(1), PlayState::Playing));
        assert_eq!(
            marked(&p, &Place::Playlist("pl1".into()), &["a", "b", "a"]),
            [PLAYING, None, PLAYING]
        );
    }

    /// A playlist `pl1` of a, b, a, c played from its start: the queue's entries 11 to 14.
    fn playlist_played() -> (Playing, Vec<(u64, &'static str)>) {
        let mut p = following(&player(&[(3, "z")], Some(0), PlayState::Playing));
        p.start_playlist("pl1");
        let queue = vec![(11, "a"), (12, "b"), (13, "a"), (14, "c")];
        p.follow(&player(&queue, Some(0), PlayState::Playing));
        (p, queue)
    }

    #[test]
    fn a_track_listed_twice_marks_the_occurrence_the_queue_is_at() {
        let (mut p, queue) = playlist_played();
        let list = Place::Playlist("pl1".into());
        let ids = ["a", "b", "a", "c"];
        assert_eq!(marked(&p, &list, &ids), [PLAYING, None, None, None]);
        p.follow(&player(&queue, Some(2), PlayState::Playing));
        assert_eq!(marked(&p, &list, &ids), [None, None, PLAYING, None]);
        // another list holding the same track marks it wherever it is
        assert_eq!(
            marked(&p, &Place::Playlist("pl2".into()), &ids),
            [PLAYING, None, PLAYING, None]
        );
        assert_eq!(
            marked(&p, &Place::Tracks, &ids),
            [PLAYING, None, PLAYING, None]
        );
    }

    #[test]
    fn the_occurrence_is_followed_through_a_queue_that_was_moved_or_grew() {
        let (mut p, _) = playlist_played();
        // the user queued a track next, then dragged the last one first
        let moved = [(14, "c"), (11, "a"), (99, "n"), (12, "b"), (13, "a")];
        p.follow(&player(&moved, Some(4), PlayState::Playing));
        assert_eq!(
            marked(&p, &Place::Playlist("pl1".into()), &["a", "b", "a", "c"]),
            [None, None, PLAYING, None]
        );
    }

    #[test]
    fn a_shuffled_playlist_marks_every_occurrence() {
        let mut p = Playing::default();
        p.start_playlist("pl1");
        // the queue is not in the playlist's order: the entry at rank 1 holds "c", not "b"
        let q = [(1, "c"), (2, "a"), (3, "b")];
        p.follow(&player(&q, Some(1), PlayState::Playing));
        assert_eq!(
            marked(&p, &Place::Playlist("pl1".into()), &["a", "b", "a"]),
            [PLAYING, None, PLAYING]
        );
    }

    #[test]
    fn a_playlist_is_marked_only_for_the_queue_it_started() {
        let (mut p, queue) = playlist_played();
        assert_eq!(p.playlist("pl1"), PLAYING);
        assert_eq!(p.playlist("pl2"), None);
        p.follow(&player(&queue, Some(0), PlayState::Paused));
        assert_eq!(p.playlist("pl1"), PAUSED);
        // a track queued next is not the playlist's, even if it is in it
        let more = [(11, "a"), (50, "b"), (12, "b")];
        p.follow(&player(&more, Some(1), PlayState::Playing));
        assert_eq!(p.playlist("pl1"), None);
        // another client replaces the queue: no more playlist, although "a" is in it
        p.follow(&player(&[(60, "a")], Some(0), PlayState::Playing));
        assert_eq!(p.playlist("pl1"), None);
        assert_eq!(
            marked(&p, &Place::Playlist("pl1".into()), &["a", "b"]),
            [PLAYING, None]
        );
    }

    #[test]
    fn a_play_asked_is_tied_to_the_queue_it_makes_not_to_the_old_one() {
        let mut p = following(&player(&[(5, "a"), (6, "b")], Some(0), PlayState::Playing));
        p.start_playlist("pl1");
        // events about the old queue (a pause, a position) are not its queue
        p.follow(&player(&[(5, "a"), (6, "b")], Some(0), PlayState::Paused));
        assert_eq!(p.playlist("pl1"), None);
        p.follow(&player(&[(7, "a"), (8, "b")], Some(0), PlayState::Playing));
        assert_eq!(p.playlist("pl1"), PLAYING);
    }

    #[test]
    fn a_new_daemon_numbers_its_entries_again() {
        let mut p = following(&player(&[(40, "a")], Some(0), PlayState::Playing));
        // the daemon quits, another one starts and is asked for the playlist
        p.follow(&Player::default());
        p.start_playlist("pl1");
        p.follow(&player(&[(1, "a"), (2, "b")], Some(0), PlayState::Playing));
        assert_eq!(p.playlist("pl1"), PLAYING);
        // and the playlist of the daemon that quit is not the new daemon's queue
        p.follow(&Player::default());
        p.follow(&player(&[(1, "a")], Some(0), PlayState::Playing));
        assert_eq!(p.playlist("pl1"), None);
    }

    #[test]
    fn the_album_and_the_artist_of_the_current_track_are_marked_once_read() {
        let mut p = following(&player(&[(1, "a"), (2, "b")], Some(0), PlayState::Playing));
        assert_eq!(p.wants_origin(), Some("a".into()));
        // asked once, not at every event
        assert_eq!(p.wants_origin(), None);
        assert_eq!(p.album("al1"), None);
        let origin = Origin {
            album: Some("al1".into()),
            artist: Some("ar1".into()),
        };
        p.set_origin("a", Some(origin));
        assert_eq!((p.album("al1"), p.artist("ar1")), (PLAYING, PLAYING));
        assert_eq!((p.album("al2"), p.artist("ar2")), (None, None));
        p.follow(&player(&[(1, "a"), (2, "b")], Some(0), PlayState::Paused));
        assert_eq!((p.album("al1"), p.artist("ar1")), (PAUSED, PAUSED));
        assert_eq!(p.wants_origin(), None);
    }

    #[test]
    fn the_origin_of_the_previous_track_is_never_shown_for_the_next() {
        let q = &[(1, "a"), (2, "b")];
        let mut p = following(&player(q, Some(0), PlayState::Playing));
        p.wants_origin();
        p.set_origin(
            "a",
            Some(Origin {
                album: Some("al1".into()),
                artist: None,
            }),
        );
        p.follow(&player(q, Some(1), PlayState::Playing));
        assert_eq!(p.album("al1"), None, "gone as soon as the track changes");
        assert_eq!(p.wants_origin(), Some("b".into()));
        // the read of "a" ends late: dropped
        p.set_origin(
            "a",
            Some(Origin {
                album: Some("al1".into()),
                artist: None,
            }),
        );
        assert_eq!(p.album("al1"), None);
        p.set_origin(
            "b",
            Some(Origin {
                album: Some("al2".into()),
                artist: None,
            }),
        );
        assert_eq!(p.album("al2"), PLAYING);
    }

    #[test]
    fn a_track_the_copy_does_not_know_marks_no_album() {
        let mut p = following(&player(&[(1, "a")], Some(0), PlayState::Playing));
        p.wants_origin();
        p.set_origin("a", None);
        assert_eq!(p.album("al1"), None);
        // and it is not asked again at every event
        p.follow(&player(&[(1, "a")], Some(0), PlayState::Paused));
        assert_eq!(p.wants_origin(), None);
    }

    #[test]
    fn the_same_track_played_again_is_read_again_only_when_it_changes() {
        let q = &[(1, "a"), (2, "a")];
        let mut p = following(&player(q, Some(0), PlayState::Playing));
        p.wants_origin();
        p.set_origin(
            "a",
            Some(Origin {
                album: Some("al1".into()),
                artist: None,
            }),
        );
        // the next entry holds the same track: the album stays, no new read
        p.follow(&player(q, Some(1), PlayState::Playing));
        assert_eq!(p.album("al1"), PLAYING);
        assert_eq!(p.wants_origin(), None);
    }

    #[test]
    fn the_bars_of_the_equaliser_stay_in_their_room_and_loop_without_a_jump() {
        for bar in 0..BARS {
            for step in 0..=100 {
                let h = level(bar, step as f32 / 100.0);
                assert!((0.2..=1.0).contains(&h), "bar {bar}: {h}");
            }
            let (start, end) = (level(bar, 0.0), level(bar, 1.0));
            assert!((start - end).abs() < 1e-4, "bar {bar}: {start} then {end}");
        }
        // they do not move together
        let at = |t: f32| (0..BARS).map(|b| level(b, t)).collect::<Vec<_>>();
        assert!((at(0.1)[0] - at(0.1)[1]).abs() > 0.05);
        assert!((at(0.1)[1] - at(0.1)[2]).abs() > 0.05);
    }

    #[test]
    fn the_origin_is_read_from_the_local_copy() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        // no copy yet: nothing to say
        assert_eq!(read_origin(&env, "t1"), None);
        let path = db::db_path(&env);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut conn = db::create(&path).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_artist(&tx, &json!({"id":"ar1","name":"Nightfall","albumCount":1})).unwrap();
        db::insert_album(
            &tx,
            &json!({"id":"al1","name":"Album 1","artist":"Nightfall","artistId":"ar1"}),
        )
        .unwrap();
        db::insert_track(&tx, &json!({"id":"t1","title":"Low Tide","artist":"Nightfall","artistId":"ar1","album":"Album 1","albumId":"al1"})).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            read_origin(&env, "t1"),
            Some(Origin {
                album: Some("al1".into()),
                artist: Some("ar1".into())
            })
        );
        assert_eq!(read_origin(&env, "gone"), None, "a track the copy lacks");
    }
}
