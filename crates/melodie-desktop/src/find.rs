//! The search of the window, without the window: what a result does when clicked, which buttons
//! it offers, and which answer is still wanted while the user types.

use crate::browse::Place;
use crate::launch::{Job, Kind};
use melodie_core::db;
use melodie_core::env::Env;
use melodie_core::search::{self, Group, Hit};
use std::sync::Arc;

/// Results shown in each group; the rest is one click away.
pub const SHOWN: usize = 5;

impl From<search::Kind> for Kind {
    fn from(k: search::Kind) -> Self {
        match k {
            search::Kind::Playlist => Self::Playlist,
            search::Kind::Artist => Self::Artist,
            search::Kind::Album => Self::Album,
            search::Kind::Track => Self::Track,
        }
    }
}

impl From<Kind> for search::Kind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Playlist => Self::Playlist,
            Kind::Artist => Self::Artist,
            Kind::Album => Self::Album,
            Kind::Track => Self::Track,
        }
    }
}

/// What the local copy says for a text.
pub struct Answer {
    pub text: String,
    pub groups: Vec<Group>,
    /// Every result of one kind, when the user asked to see them all.
    pub all: Option<(Kind, Arc<[Hit]>)>,
}

/// Reads the local copy for `text`: its groups, and every result of `all`. `None` without a local
/// copy. Blocking: run it away from the window.
pub fn answer(env: &Env, text: &str, all: Option<Kind>) -> anyhow::Result<Option<Answer>> {
    let Some(conn) = db::open_existing(env)? else {
        return Ok(None);
    };
    let all = match all {
        Some(k) => Some((k, search::of_kind(&conn, text, k.into())?.into())),
        None => None,
    };
    Ok(Some(Answer {
        text: text.to_string(),
        groups: search::grouped(&conn, text, SHOWN)?,
        all,
    }))
}

/// What a result can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Play,
    Shuffle,
    /// After the current track.
    Next,
    /// At the end of the queue.
    Last,
    /// Its page; a track's is its album's.
    Open,
    /// Chosen in the playlist picker.
    AddToPlaylist,
}

/// What a click on a result does: a track plays, alone, as in the lists; the rest open.
pub fn click(hit: &Hit) -> Action {
    if hit.kind == search::Kind::Track {
        Action::Play
    } else {
        Action::Open
    }
}

/// The buttons of a result, in order: its actions but the one its click does.
pub fn buttons(hit: &Hit) -> Vec<Action> {
    let track = hit.kind == search::Kind::Track;
    let mut all = vec![Action::Play];
    if !track {
        all.push(Action::Shuffle);
    }
    all.extend([Action::Next, Action::Last]);
    if page(hit).is_some() {
        all.push(Action::Open);
    }
    // `melodie playlist add` takes a track or an album
    if matches!(hit.kind, search::Kind::Track | search::Kind::Album) {
        all.push(Action::AddToPlaylist);
    }
    all.retain(|a| *a != click(hit));
    all
}

/// The page of a result.
pub fn page(hit: &Hit) -> Option<Place> {
    let id = hit.id.clone();
    match hit.kind {
        search::Kind::Album => Some(Place::Album(id)),
        search::Kind::Artist => Some(Place::Artist(id)),
        search::Kind::Playlist => Some(Place::Playlist(id)),
        search::Kind::Track => hit.album_id.clone().map(Place::Album),
    }
}

/// What `melodie` runs for an action; opening a page and choosing a playlist are not jobs.
pub fn job(action: Action, hit: &Hit) -> Option<Job> {
    let (kind, id) = (Kind::from(hit.kind), hit.id.clone());
    Some(match action {
        Action::Play | Action::Shuffle => Job::Play {
            kind,
            id,
            shuffle: action == Action::Shuffle,
        },
        Action::Next | Action::Last => Job::Queue {
            kind,
            id,
            next: action == Action::Next,
        },
        Action::Open | Action::AddToPlaylist => return None,
    })
}

/// The line under a result's name.
pub fn about(hit: &Hit) -> String {
    let plural = |n: i64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    match hit.kind {
        search::Kind::Artist => plural(hit.count, "album"),
        search::Kind::Album if hit.year > 0 => format!("{} · {}", hit.artist, hit.year),
        search::Kind::Album => hit.artist.clone(),
        search::Kind::Track => format!("{} · {}", hit.artist, hit.album),
        search::Kind::Playlist => format!(
            "{} · {}",
            plural(hit.count, "track"),
            search::human_duration(hit.duration)
        ),
    }
}

/// Numbers the questions asked as the user types: only the answer to the last one is shown, so
/// a slow answer to an older keystroke never replaces a newer one.
#[derive(Debug, Default)]
pub struct Latest {
    asked: u64,
}

impl Latest {
    pub fn ask(&mut self) -> u64 {
        self.asked += 1;
        self.asked
    }

    pub fn is_latest(&self, n: u64) -> bool {
        n == self.asked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(kind: search::Kind, id: &str) -> Hit {
        Hit {
            kind,
            id: id.into(),
            name: "Name".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            year: 0,
            count: 0,
            duration: 0,
            cover: None,
            starred: false,
            album_id: None,
            artist_id: None,
        }
    }

    #[test]
    fn a_click_on_a_track_plays_it_and_on_the_rest_opens_its_page() {
        let t = hit(search::Kind::Track, "-t1");
        assert_eq!(click(&t), Action::Play);
        for (kind, place) in [
            (search::Kind::Album, Place::Album("x".into())),
            (search::Kind::Artist, Place::Artist("x".into())),
            (search::Kind::Playlist, Place::Playlist("x".into())),
        ] {
            let h = hit(kind, "x");
            assert_eq!(click(&h), Action::Open);
            assert_eq!(page(&h), Some(place));
        }
    }

    #[test]
    fn each_result_offers_its_actions() {
        let mut t = hit(search::Kind::Track, "t1");
        // no shuffle for one track; its page is its album's
        assert_eq!(
            buttons(&t),
            [Action::Next, Action::Last, Action::AddToPlaylist]
        );
        t.album_id = Some("al1".into());
        assert_eq!(
            buttons(&t),
            [
                Action::Next,
                Action::Last,
                Action::Open,
                Action::AddToPlaylist
            ]
        );
        assert_eq!(page(&t), Some(Place::Album("al1".into())));
        let all = [Action::Play, Action::Shuffle, Action::Next, Action::Last];
        let mut album = all.to_vec();
        album.push(Action::AddToPlaylist);
        assert_eq!(buttons(&hit(search::Kind::Album, "al1")), album);
        // `melodie playlist add` takes a track or an album only
        assert_eq!(buttons(&hit(search::Kind::Artist, "ar1")), all);
        assert_eq!(buttons(&hit(search::Kind::Playlist, "p1")), all);
    }

    #[test]
    fn the_jobs_of_a_result_pass_its_id_after_the_dashes() {
        let a = hit(search::Kind::Album, "-al1");
        assert_eq!(
            job(Action::Shuffle, &a).unwrap().args(),
            ["play", "album", "--shuffle", "--", "-al1"]
        );
        assert_eq!(
            job(Action::Next, &a).unwrap().args(),
            ["queue", "album", "--next", "--", "-al1"]
        );
        assert_eq!(
            job(Action::Last, &hit(search::Kind::Playlist, "p1"))
                .unwrap()
                .args(),
            ["queue", "playlist", "--", "p1"]
        );
        assert_eq!(
            job(Action::Play, &hit(search::Kind::Track, "t1"))
                .unwrap()
                .args(),
            ["play", "track", "--", "t1"]
        );
        assert_eq!(job(Action::Open, &a), None);
        assert_eq!(job(Action::AddToPlaylist, &a), None);
    }

    #[test]
    fn what_a_result_says_under_its_name() {
        let mut a = hit(search::Kind::Album, "al1");
        a.year = 2001;
        assert_eq!(about(&a), "Artist · 2001");
        let mut ar = hit(search::Kind::Artist, "ar1");
        ar.count = 1;
        assert_eq!(about(&ar), "1 album");
        let mut p = hit(search::Kind::Playlist, "p1");
        p.count = 42;
        p.duration = 7500;
        assert_eq!(about(&p), "42 tracks · 2 h 05");
        assert_eq!(about(&hit(search::Kind::Track, "t1")), "Artist · Album");
    }

    #[test]
    fn an_answer_holds_the_groups_and_every_result_of_the_kind_asked() {
        use melodie_core::db;
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        assert!(answer(&env, "rain", None).unwrap().is_none(), "no copy yet");
        let path = db::db_path(&env);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut conn = db::create(&path).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_artist(&tx, &json!({"id":"ar1","name":"Rain","albumCount":1})).unwrap();
        for n in 0..12 {
            db::insert_track(
                &tx,
                &json!({"id":format!("t{n}"),"title":format!("Rain {n}")}),
            )
            .unwrap();
        }
        tx.commit().unwrap();
        drop(conn);
        let a = answer(&env, "rain", None).unwrap().unwrap();
        assert_eq!(a.text, "rain");
        let kinds: Vec<_> = a.groups.iter().map(|g| g.kind).collect();
        assert_eq!(kinds, [search::Kind::Artist, search::Kind::Track]);
        assert_eq!(a.groups[1].hits.len(), SHOWN);
        assert!(a.all.is_none());
        let a = answer(&env, "rain", Some(Kind::Track)).unwrap().unwrap();
        let (kind, all) = a.all.unwrap();
        assert_eq!(kind, Kind::Track);
        assert_eq!(all.len(), 12);
        // drawn on every frame: a clone is a pointer, not the thousands of results
        let drawn = Arc::clone(&all);
        assert!(std::ptr::eq(drawn.as_ptr(), all.as_ptr()));
        let a = answer(&env, " ", Some(Kind::Track)).unwrap().unwrap();
        assert!(a.groups.is_empty());
        assert!(a.all.unwrap().1.is_empty());
    }

    #[test]
    fn the_answer_to_an_older_keystroke_is_dropped() {
        let mut latest = Latest::default();
        let first = latest.ask();
        let second = latest.ask();
        assert!(
            !latest.is_latest(first),
            "a slow answer to « r » after « ra »"
        );
        assert!(latest.is_latest(second));
    }
}
