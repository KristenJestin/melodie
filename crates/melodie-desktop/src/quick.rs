//! The search palette (Ctrl+K), without the window: its groups of results, how the arrows move
//! through them, what Enter and its modifiers do, the keys it shows, and what it lists before
//! anything is typed.

use crate::browse::Place;
use crate::find::Action;
use crate::launch::{Job, Kind};
use melodie_core::db;
use melodie_core::env::Env;
use melodie_core::library as lib;
use melodie_core::search::{self, Genre, Hit};

/// Genres shown, as many as the results of each kind.
pub const GENRES: usize = crate::find::SHOWN;
/// What the palette lists before anything is typed.
pub const RECENT: usize = 8;

/// Which of a result's actions a key runs: Enter, Shift+Enter, Ctrl+Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    Main,
    /// Adds it to the queue.
    Queue,
    /// What the click does not: a track's album opens, an album, an artist or a playlist plays.
    Other,
}

/// A row of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Found(Hit),
    Genre(Genre),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Recent,
    Kind(search::Kind),
    Genres,
}

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Self::Recent => "Recent",
            Self::Kind(search::Kind::Playlist) => "Playlists",
            Self::Kind(search::Kind::Artist) => "Artists",
            Self::Kind(search::Kind::Album) => "Albums",
            Self::Kind(search::Kind::Track) => "Tracks",
            Self::Genres => "Genres",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub section: Section,
    pub items: Vec<Item>,
}

/// The groups of the search page's answer, in its order, the genres after them.
pub fn groups(found: &[search::Group], genres: Vec<Genre>) -> Vec<Group> {
    let mut out: Vec<Group> = found
        .iter()
        .map(|g| Group {
            section: Section::Kind(g.kind),
            items: g.hits.iter().cloned().map(Item::Found).collect(),
        })
        .collect();
    out.push(Group {
        section: Section::Genres,
        items: genres.into_iter().map(Item::Genre).collect(),
    });
    out.retain(|g| !g.items.is_empty());
    out
}

/// How many rows the groups hold.
pub fn count(groups: &[Group]) -> usize {
    groups.iter().map(|g| g.items.len()).sum()
}

/// The row at `ix`, counted across the groups.
pub fn nth(groups: &[Group], ix: usize) -> Option<&Item> {
    groups.iter().flat_map(|g| &g.items).nth(ix)
}

/// The row an arrow goes to from `from`, among `len` rows: round the ends, so the last one is
/// one key away from the first.
pub fn step(from: usize, len: usize, down: bool) -> usize {
    match (len, down) {
        (0, _) => 0,
        (_, true) => (from + 1) % len,
        (_, false) => (from + len - 1) % len,
    }
}

/// What a key does to a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Do {
    /// One of the search page's actions.
    Hit(Action),
    /// The genre's mix.
    Mix,
}

/// What `how` does to the row; `None` when it does nothing to it (a track with no album, a
/// genre added to the queue).
pub fn what(item: &Item, how: How) -> Option<Do> {
    let hit = match item {
        Item::Genre(_) => return (how == How::Main).then_some(Do::Mix),
        Item::Found(hit) => hit,
    };
    let track = hit.kind == search::Kind::Track;
    let action = match how {
        How::Main => crate::find::click(hit),
        How::Queue => Action::Last,
        How::Other if track => {
            crate::find::page(hit)?;
            Action::Open
        }
        How::Other => Action::Play,
    };
    Some(Do::Hit(action))
}

/// The keys shown at the bottom of the palette, for the row chosen: the key and what it does.
pub fn hints(item: Option<&Item>) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if let Some(item) = item {
        out.push(("↑↓", "Move"));
        for (key, how) in [
            ("Enter", How::Main),
            ("Shift+Enter", How::Queue),
            ("Ctrl+Enter", How::Other),
        ] {
            let does = match what(item, how) {
                None => continue,
                Some(Do::Mix) => "Play mix",
                Some(Do::Hit(Action::Last)) => "Add to queue",
                Some(Do::Hit(Action::Open)) if how == How::Other => "Open album",
                Some(Do::Hit(Action::Open)) => "Open",
                Some(Do::Hit(_)) => "Play",
            };
            out.push((key, does));
        }
    }
    out.push(("Esc", "Close"));
    out
}

/// Something opened or played lately.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Seen {
    Album(String),
    Artist(String),
    Playlist(String),
    Track(String),
    Genre(String),
}

impl Seen {
    /// The page opened, when it is one the palette can name.
    pub fn of_place(place: &Place) -> Option<Self> {
        match place {
            Place::Album(id) => Some(Self::Album(id.clone())),
            Place::Artist(id) => Some(Self::Artist(id.clone())),
            Place::Playlist(id) => Some(Self::Playlist(id.clone())),
            _ => None,
        }
    }

    /// What a job the user started makes them remember: what they played, never what the daemon
    /// went on to play by itself nor what they only queued.
    pub fn of_job(job: &Job) -> Option<Self> {
        match job {
            Job::Play { kind, id, .. } => {
                let id = id.clone();
                Some(match kind {
                    Kind::Track => Self::Track(id),
                    Kind::Album => Self::Album(id),
                    Kind::Artist => Self::Artist(id),
                    Kind::Playlist => Self::Playlist(id),
                })
            }
            Job::PlayTracks { ids, from } => ids.get(*from).cloned().map(Self::Track),
            Job::MixGenre { name } => Some(Self::Genre(name.clone())),
            _ => None,
        }
    }
}

/// What was opened or played in the window, the last one first, without repeats.
#[derive(Debug, Clone, Default)]
pub struct Recent {
    list: Vec<Seen>,
}

impl Recent {
    pub fn note(&mut self, seen: Seen) {
        self.list.retain(|s| *s != seen);
        self.list.insert(0, seen);
        self.list.truncate(RECENT);
    }

    pub fn list(&self) -> &[Seen] {
        &self.list
    }
}

/// The rows before anything is typed: what was opened or played in the window, named from the
/// local copy (what left it is left out), then the albums played last by the copy; `RECENT` of
/// them, without repeats. `None` without a local copy. Blocking: run it away from the window.
pub fn recent(env: &Env, seen: &[Seen]) -> anyhow::Result<Option<Vec<Item>>> {
    let Some(conn) = db::open_existing(env)? else {
        return Ok(None);
    };
    let mut genres = None;
    let mut out = Vec::new();
    for s in seen {
        let item = match s {
            Seen::Album(id) => lib::album(&conn, id)?.map(album),
            Seen::Artist(id) => lib::artist(&conn, id)?.map(|a| Hit {
                count: a.albums,
                cover: a.cover,
                ..blank(search::Kind::Artist, a.id, a.name)
            }),
            Seen::Playlist(id) => lib::playlist(&conn, id)?.map(|p| Hit {
                count: p.songs,
                duration: p.duration,
                cover: p.cover,
                ..blank(search::Kind::Playlist, p.id, p.name)
            }),
            Seen::Track(id) => lib::track(&conn, id)?.map(|t| Hit {
                artist: t.artist,
                album: t.album,
                year: t.year,
                duration: t.duration,
                cover: t.cover,
                album_id: t.album_id,
                artist_id: t.artist_id,
                ..blank(search::Kind::Track, t.id, t.title)
            }),
            Seen::Genre(name) => {
                let all = match &genres {
                    Some(all) => all,
                    None => genres.insert(search::genres(&conn)?),
                };
                let found = all.iter().find(|g: &&Genre| g.name == *name);
                out.extend(found.cloned().map(Item::Genre));
                continue;
            }
        };
        out.extend(item.map(Item::Found));
    }
    // more than are missing: some are already there
    for a in lib::recently_played(&conn, RECENT * 2)? {
        if out.len() >= RECENT {
            break;
        }
        let item = Item::Found(album(a));
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out.truncate(RECENT);
    Ok(Some(out))
}

fn blank(kind: search::Kind, id: String, name: String) -> Hit {
    Hit {
        kind,
        id,
        name,
        artist: String::new(),
        album: String::new(),
        year: 0,
        count: 0,
        duration: 0,
        cover: None,
        starred: false,
        album_id: None,
        artist_id: None,
    }
}

fn album(a: lib::Album) -> Hit {
    Hit {
        artist: a.artist,
        year: a.year,
        count: a.songs,
        duration: a.duration,
        cover: a.cover,
        artist_id: a.artist_id,
        ..blank(search::Kind::Album, a.id.clone(), a.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hit(kind: search::Kind, id: &str) -> Hit {
        Hit {
            kind,
            id: id.into(),
            name: id.into(),
            artist: String::new(),
            album: String::new(),
            year: 0,
            count: 0,
            duration: 0,
            cover: None,
            starred: false,
            album_id: None,
            artist_id: None,
        }
    }

    fn genre(name: &str) -> Item {
        Item::Genre(Genre {
            name: name.into(),
            tracks: 3,
        })
    }

    fn sample() -> Vec<Group> {
        let found = [
            search::Group {
                kind: search::Kind::Artist,
                hits: vec![hit(search::Kind::Artist, "ar1")],
                more: false,
            },
            search::Group {
                kind: search::Kind::Track,
                hits: vec![
                    hit(search::Kind::Track, "t1"),
                    hit(search::Kind::Track, "t2"),
                ],
                more: true,
            },
        ];
        groups(
            &found,
            vec![Genre {
                name: "Rain Pop".into(),
                tracks: 3,
            }],
        )
    }

    #[test]
    fn the_results_are_grouped_as_on_the_search_page_with_the_genres_last() {
        let g = sample();
        let sections: Vec<Section> = g.iter().map(|g| g.section).collect();
        assert_eq!(
            sections,
            [
                Section::Kind(search::Kind::Artist),
                Section::Kind(search::Kind::Track),
                Section::Genres
            ]
        );
        assert_eq!(count(&g), 4);
        assert!(groups(&[], Vec::new()).is_empty(), "no empty group");
    }

    #[test]
    fn the_arrows_move_across_the_groups_and_round_the_ends() {
        let g = sample();
        let len = count(&g);
        let mut at = 0;
        let mut seen = Vec::new();
        for _ in 0..len {
            seen.push(nth(&g, at).cloned().unwrap());
            at = step(at, len, true);
        }
        assert_eq!(at, 0, "down from the last row comes back to the first");
        assert_eq!(
            seen,
            [
                Item::Found(hit(search::Kind::Artist, "ar1")),
                Item::Found(hit(search::Kind::Track, "t1")),
                Item::Found(hit(search::Kind::Track, "t2")),
                genre("Rain Pop"),
            ]
        );
        assert_eq!(
            step(0, len, false),
            len - 1,
            "up from the first row goes to the last"
        );
        assert_eq!(step(2, len, false), 1);
        assert_eq!(nth(&g, len), None);
        // nothing to move through
        assert_eq!(step(0, 0, true), 0);
        assert_eq!(step(0, 0, false), 0);
    }

    #[test]
    fn enter_plays_a_track_and_opens_the_rest() {
        let t = Item::Found(hit(search::Kind::Track, "t1"));
        assert_eq!(what(&t, How::Main), Some(Do::Hit(Action::Play)));
        for kind in [
            search::Kind::Album,
            search::Kind::Artist,
            search::Kind::Playlist,
        ] {
            let it = Item::Found(hit(kind, "x"));
            assert_eq!(
                what(&it, How::Main),
                Some(Do::Hit(Action::Open)),
                "{kind:?}"
            );
        }
        assert_eq!(what(&genre("Rock"), How::Main), Some(Do::Mix));
    }

    #[test]
    fn shift_enter_adds_to_the_queue_and_ctrl_enter_does_the_other_thing() {
        let mut t = hit(search::Kind::Track, "t1");
        let track = Item::Found(t.clone());
        assert_eq!(what(&track, How::Queue), Some(Do::Hit(Action::Last)));
        // a track without an album has no page
        assert_eq!(what(&track, How::Other), None);
        t.album_id = Some("al1".into());
        let track = Item::Found(t);
        assert_eq!(what(&track, How::Other), Some(Do::Hit(Action::Open)));
        for kind in [
            search::Kind::Album,
            search::Kind::Artist,
            search::Kind::Playlist,
        ] {
            let it = Item::Found(hit(kind, "x"));
            assert_eq!(
                what(&it, How::Queue),
                Some(Do::Hit(Action::Last)),
                "{kind:?}"
            );
            assert_eq!(
                what(&it, How::Other),
                Some(Do::Hit(Action::Play)),
                "{kind:?}"
            );
        }
        assert_eq!(what(&genre("Rock"), How::Queue), None);
        assert_eq!(what(&genre("Rock"), How::Other), None);
    }

    #[test]
    fn the_keys_shown_are_those_of_the_row_chosen() {
        let mut t = hit(search::Kind::Track, "t1");
        assert_eq!(
            hints(Some(&Item::Found(t.clone()))),
            [
                ("↑↓", "Move"),
                ("Enter", "Play"),
                ("Shift+Enter", "Add to queue"),
                ("Esc", "Close")
            ]
        );
        t.album_id = Some("al1".into());
        assert_eq!(
            hints(Some(&Item::Found(t))),
            [
                ("↑↓", "Move"),
                ("Enter", "Play"),
                ("Shift+Enter", "Add to queue"),
                ("Ctrl+Enter", "Open album"),
                ("Esc", "Close")
            ]
        );
        assert_eq!(
            hints(Some(&Item::Found(hit(search::Kind::Artist, "ar1")))),
            [
                ("↑↓", "Move"),
                ("Enter", "Open"),
                ("Shift+Enter", "Add to queue"),
                ("Ctrl+Enter", "Play"),
                ("Esc", "Close")
            ]
        );
        assert_eq!(
            hints(Some(&genre("Rock"))),
            [("↑↓", "Move"), ("Enter", "Play mix"), ("Esc", "Close")]
        );
        assert_eq!(hints(None), [("Esc", "Close")]);
    }

    #[test]
    fn what_was_seen_last_comes_first_once() {
        let mut r = Recent::default();
        for n in 0..RECENT + 3 {
            r.note(Seen::Album(format!("al{n}")));
        }
        r.note(Seen::Track("t1".into()));
        r.note(Seen::Album(format!("al{}", RECENT + 1)));
        let list = r.list();
        assert_eq!(list.len(), RECENT, "only the last ones are kept");
        assert_eq!(list[0], Seen::Album(format!("al{}", RECENT + 1)));
        assert_eq!(list[1], Seen::Track("t1".into()));
        assert_eq!(list[2], Seen::Album(format!("al{}", RECENT + 2)));
        assert_eq!(
            list.iter()
                .filter(|s| **s == Seen::Album(format!("al{}", RECENT + 1)))
                .count(),
            1
        );
    }

    #[test]
    fn the_pages_named_in_the_recent_list_are_those_of_the_library() {
        assert_eq!(
            Seen::of_place(&Place::Album("al1".into())),
            Some(Seen::Album("al1".into()))
        );
        assert_eq!(
            Seen::of_place(&Place::Artist("ar1".into())),
            Some(Seen::Artist("ar1".into()))
        );
        assert_eq!(
            Seen::of_place(&Place::Playlist("p1".into())),
            Some(Seen::Playlist("p1".into()))
        );
        for section in [Place::Home, Place::Albums, Place::Search, Place::Player] {
            assert_eq!(Seen::of_place(&section), None);
        }
    }

    #[test]
    fn only_what_the_user_started_is_remembered() {
        let play = |kind, id: &str| Job::Play {
            kind,
            id: id.into(),
            shuffle: false,
        };
        assert_eq!(
            Seen::of_job(&play(Kind::Track, "t1")),
            Some(Seen::Track("t1".into()))
        );
        assert_eq!(
            Seen::of_job(&play(Kind::Album, "al1")),
            Some(Seen::Album("al1".into()))
        );
        assert_eq!(
            Seen::of_job(&play(Kind::Artist, "ar1")),
            Some(Seen::Artist("ar1".into()))
        );
        assert_eq!(
            Seen::of_job(&play(Kind::Playlist, "p1")),
            Some(Seen::Playlist("p1".into()))
        );
        assert_eq!(
            Seen::of_job(&Job::MixGenre {
                name: "Ambient".into()
            }),
            Some(Seen::Genre("Ambient".into()))
        );
        // adding to the queue plays nothing
        for kind in [Kind::Track, Kind::Album] {
            let queue = Job::Queue {
                kind,
                id: "x".into(),
                next: false,
            };
            assert_eq!(Seen::of_job(&queue), None);
        }
        let mix = Job::Mix {
            kind: Kind::Album,
            id: "al1".into(),
        };
        assert_eq!(Seen::of_job(&mix), None);
        let star = Job::Star {
            kind: Kind::Track,
            id: "t1".into(),
            starred: true,
        };
        assert_eq!(Seen::of_job(&star), None);
        assert_eq!(Seen::of_job(&Job::PlayMix), None);
    }

    #[test]
    fn the_list_fills_when_the_albums_seen_were_also_played_lately() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        let path = db::db_path(&env);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut conn = db::create(&path).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_artist(&tx, &json!({"id":"ar1","name":"Nightfall","albumCount":20})).unwrap();
        for n in 1..=20 {
            let mut a = json!({"id":format!("al{n:02}"),"name":format!("Album {n}"),"artist":"Nightfall","artistId":"ar1"});
            a["played"] = json!(format!("2026-09-01T10:{n:02}:00Z"));
            db::insert_album(&tx, &a).unwrap();
        }
        tx.commit().unwrap();
        drop(conn);
        // seen: the albums played last but one, and the ones just before them
        let seen: Vec<Seen> = (13..=19)
            .rev()
            .map(|n| Seen::Album(format!("al{n:02}")))
            .collect();
        let items = recent(&env, &seen).unwrap().unwrap();
        assert_eq!(items.len(), RECENT);
    }

    #[test]
    fn the_empty_field_lists_the_recent_items() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        assert!(recent(&env, &[]).unwrap().is_none(), "no copy yet");
        let path = db::db_path(&env);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut conn = db::create(&path).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_artist(&tx, &json!({"id":"ar1","name":"Nightfall","albumCount":2})).unwrap();
        for n in 1..=12 {
            let mut a = json!({"id":format!("al{n}"),"name":format!("Album {n}"),"artist":"Nightfall","artistId":"ar1","coverArt":format!("c{n}")});
            if n > 2 {
                // al12 played last
                a["played"] = json!(format!("2026-09-01T10:{n:02}:00Z"));
            }
            db::insert_album(&tx, &a).unwrap();
        }
        db::insert_track(&tx, &json!({"id":"t1","title":"Low Tide","artist":"Nightfall","album":"Album 1","albumId":"al1","genres":[{"name":"Ambient"}]})).unwrap();
        db::replace_playlists(&tx, &[json!({"id":"p1","name":"Evening","songCount":4})]).unwrap();
        tx.commit().unwrap();
        drop(conn);
        let seen = [
            Seen::Track("t1".into()),
            Seen::Album("gone".into()),
            Seen::Playlist("p1".into()),
            Seen::Genre("Ambient".into()),
            Seen::Artist("ar1".into()),
            Seen::Album("al11".into()),
        ];
        let items = recent(&env, &seen).unwrap().unwrap();
        let names: Vec<String> = items
            .iter()
            .map(|i| match i {
                Item::Found(h) => format!("{:?} {}", h.kind, h.name),
                Item::Genre(g) => format!("Genre {} {}", g.name, g.tracks),
            })
            .collect();
        // what was seen, then the albums played last that are not there yet
        assert_eq!(
            names,
            [
                "Track Low Tide",
                "Playlist Evening",
                "Genre Ambient 1",
                "Artist Nightfall",
                "Album Album 11",
                "Album Album 12",
                "Album Album 10",
                "Album Album 9",
            ]
        );
        let Item::Found(track) = &items[0] else {
            panic!("a track")
        };
        assert_eq!(track.album_id.as_deref(), Some("al1"), "its album opens");
        let Item::Found(album) = &items[4] else {
            panic!("an album")
        };
        assert_eq!(album.cover.as_deref(), Some("c11"), "with its cover");
    }
}
