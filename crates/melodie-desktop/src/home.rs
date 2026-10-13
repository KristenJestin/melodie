//! The home view without a window: what it reads from the local copy, the highlights at its top,
//! its rows in their order, and the line that greets.

use crate::launch::Job;
use anyhow::Result;
use melodie_core::library::{self as lib, Album, AlbumBy, Artist, Playlist, Sort};
use melodie_core::mix;
use rusqlite::Connection;
use std::sync::Arc;

/// The most a row holds: its "see all" opens the rest.
pub const ROW: usize = 20;
/// The most highlights the top card goes through.
pub const HIGHLIGHTS: usize = 4;
/// Genre mixes, as many as `melodie mixes` offers.
const GENRES: usize = 6;

/// What the home reads from the local copy.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Home {
    pub recent: Vec<Album>,
    pub most_played: Vec<Album>,
    pub recently_played: Vec<Album>,
    pub artists: Vec<Artist>,
    /// The genres most present among the liked or frequent tracks, with their track counts.
    pub genres: Vec<(String, usize)>,
}

impl Home {
    /// Reads it; `now` in seconds, for the mixes.
    pub fn read(conn: &Connection, now: i64) -> Result<Self> {
        let library = mix::Library::load(conn, now)?;
        Ok(Self {
            recent: lib::recently_added(conn, ROW)?,
            most_played: lib::most_played(conn, ROW)?,
            recently_played: lib::recently_played(conn, ROW)?,
            artists: lib::top_artists(conn, ROW)?,
            // the genres `melodie mixes` offers, by the same code
            genres: mix::top_genres(&library, GENRES)
                .into_iter()
                .map(|(name, _, total)| (name, total))
                .collect(),
        })
    }
}

/// A mix to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mix {
    /// The personal mix: favourites, familiar tracks and discoveries.
    Mine,
    Rediscover,
    Genre {
        name: String,
        tracks: usize,
    },
}

impl Mix {
    pub fn job(&self) -> Job {
        match self {
            Self::Mine => Job::PlayMix,
            Self::Rediscover => Job::Rediscover,
            Self::Genre { name, .. } => Job::MixGenre { name: name.clone() },
        }
    }
}

/// Why an album is a highlight: the card says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    New,
    LastPlayed,
    MostPlayed,
}

/// Something worth playing now, shown large at the top.
#[derive(Debug, Clone, PartialEq)]
pub enum Highlight {
    Album {
        album: Album,
        why: Why,
    },
    /// The personal mix.
    Mix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    RecentlyAdded,
    Mixes,
    MostPlayed,
    RecentlyPlayed,
    Artists,
    Playlists,
}

/// The page a row's "see all" opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeeAll {
    Albums(Sort<AlbumBy>),
    Artists,
}

impl Kind {
    pub fn title(self) -> &'static str {
        match self {
            Self::RecentlyAdded => "Recently added",
            Self::Mixes => "Your mixes",
            Self::MostPlayed => "Most played",
            Self::RecentlyPlayed => "Recently played",
            Self::Artists => "Artists you play most",
            Self::Playlists => "Playlists",
        }
    }

    /// Where its "see all" goes: the mixes and the playlists have no page of their own (the
    /// playlists are in the sidebar).
    pub fn see_all(self) -> Option<SeeAll> {
        match self {
            Self::RecentlyAdded => Some(SeeAll::Albums(Sort::desc(AlbumBy::Added))),
            Self::MostPlayed => Some(SeeAll::Albums(Sort::desc(AlbumBy::Plays))),
            Self::RecentlyPlayed => Some(SeeAll::Albums(Sort::desc(AlbumBy::Played))),
            Self::Artists => Some(SeeAll::Artists),
            Self::Mixes | Self::Playlists => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Albums(Arc<[Album]>),
    Artists(Arc<[Artist]>),
    Mixes(Arc<[Mix]>),
    Playlists(Arc<[Playlist]>),
}

impl Row {
    pub fn len(&self) -> usize {
        match self {
            Self::Albums(v) => v.len(),
            Self::Artists(v) => v.len(),
            Self::Mixes(v) => v.len(),
            Self::Playlists(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub kind: Kind,
    pub row: Row,
}

/// What the home shows, top to bottom.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    pub highlights: Vec<Highlight>,
    pub sections: Vec<Section>,
}

/// The highlights and the rows: a row with nothing in it is left out, and an empty library has
/// neither highlights nor mixes.
pub fn view(home: &Home, playlists: &[Playlist]) -> View {
    let any = !home.recent.is_empty();
    let mut highlights: Vec<Highlight> = Vec::new();
    let album = |a: Option<&Album>, why: Why, highlights: &mut Vec<Highlight>| {
        let Some(a) = a else { return };
        let seen = highlights
            .iter()
            .any(|h| matches!(h, Highlight::Album { album, .. } if album.id == a.id));
        if !seen {
            highlights.push(Highlight::Album {
                album: a.clone(),
                why,
            });
        }
    };
    album(home.recent.first(), Why::New, &mut highlights);
    if any {
        highlights.push(Highlight::Mix);
    }
    album(
        home.recently_played.first(),
        Why::LastPlayed,
        &mut highlights,
    );
    album(home.most_played.first(), Why::MostPlayed, &mut highlights);
    highlights.truncate(HIGHLIGHTS);

    let mixes: Vec<Mix> = if any {
        [Mix::Mine, Mix::Rediscover]
            .into_iter()
            .chain(home.genres.iter().map(|(name, tracks)| Mix::Genre {
                name: name.clone(),
                tracks: *tracks,
            }))
            .collect()
    } else {
        Vec::new()
    };
    let albums = |v: &[Album]| Row::Albums(v[..v.len().min(ROW)].into());
    let sections = [
        (Kind::RecentlyAdded, albums(&home.recent)),
        (Kind::Mixes, Row::Mixes(mixes.into())),
        (Kind::MostPlayed, albums(&home.most_played)),
        (Kind::RecentlyPlayed, albums(&home.recently_played)),
        (
            Kind::Artists,
            Row::Artists(home.artists[..home.artists.len().min(ROW)].into()),
        ),
        (
            Kind::Playlists,
            Row::Playlists(playlists[..playlists.len().min(ROW)].into()),
        ),
    ]
    .into_iter()
    .filter(|(_, row)| !row.is_empty())
    .map(|(kind, row)| Section { kind, row })
    .collect();
    View {
        highlights,
        sections,
    }
}

/// The line above everything, by the hour of the day (0 to 23).
pub fn greeting(hour: u32) -> &'static str {
    match hour {
        5..12 => "Good morning",
        12..18 => "Good afternoon",
        _ => "Good evening",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melodie_core::db;
    use serde_json::json;

    fn kinds(v: &View) -> Vec<Kind> {
        v.sections.iter().map(|s| s.kind).collect()
    }

    fn album(id: &str) -> Album {
        Album {
            id: id.into(),
            name: format!("Album {id}"),
            ..Album::default()
        }
    }

    /// A test copy of `albums` albums, each by its own artist with one track, added in their
    /// order; the first `played` of them played, the earlier ones more often but less lately.
    fn copy(albums: usize, played: usize) -> (tempfile::TempDir, Connection) {
        let d = tempfile::tempdir().unwrap();
        let mut conn = db::create(&d.path().join("l.db")).unwrap();
        let tx = conn.transaction().unwrap();
        for n in 0..albums {
            let plays = if n < played { 100 - n } else { 0 };
            let ar = format!("ar{n}");
            db::insert_artist(
                &tx,
                &json!({"id":ar,"name":format!("Artist {n}"),"albumCount":1}),
            )
            .unwrap();
            let mut a = json!({"id":format!("al{n}"),"name":format!("Album {n}"),"artist":format!("Artist {n}"),
                "artistId":ar,"playCount":plays,"created":format!("2026-01-01T00:{:02}:00Z", n % 60)});
            if n < played {
                a["played"] = json!(format!("2026-09-01T{:02}:{:02}:00Z", n / 60, n % 60));
            }
            db::insert_album(&tx, &a).unwrap();
            db::insert_track(
                &tx,
                &json!({"id":format!("t{n}"),"title":format!("Track {n}"),"artistId":ar,
                        "albumId":format!("al{n}"),"playCount":plays,"genre":"Folk"}),
            )
            .unwrap();
        }
        tx.commit().unwrap();
        (d, conn)
    }

    #[test]
    fn an_empty_library_has_no_highlight_and_no_row() {
        let (_d, conn) = copy(0, 0);
        let home = Home::read(&conn, 1_790_000_000).unwrap();
        assert_eq!(home, Home::default());
        assert_eq!(view(&home, &[]), View::default());
    }

    #[test]
    fn a_small_library_shows_what_it_has_and_leaves_out_empty_rows() {
        // three albums, one played
        let (_d, conn) = copy(3, 1);
        let home = Home::read(&conn, 1_790_000_000).unwrap();
        let v = view(&home, &[]);
        assert_eq!(
            kinds(&v),
            [
                Kind::RecentlyAdded,
                Kind::Mixes,
                Kind::MostPlayed,
                Kind::RecentlyPlayed,
                Kind::Artists
            ]
        );
        let lens: Vec<usize> = v.sections.iter().map(|s| s.row.len()).collect();
        assert_eq!(lens[0], 3);
        assert_eq!(&lens[2..], [1, 1, 1]);
        // the newest first: al2 was added last
        let Row::Albums(recent) = &v.sections[0].row else {
            panic!("albums")
        };
        assert_eq!(recent[0].id, "al2");
    }

    #[test]
    fn a_large_library_fills_each_row_up_to_its_length() {
        let (_d, conn) = copy(45, 30);
        let home = Home::read(&conn, 1_790_000_000).unwrap();
        let playlists: Vec<Playlist> = (0..30)
            .map(|n| Playlist {
                id: format!("p{n}"),
                name: format!("Playlist {n}"),
                ..Playlist::default()
            })
            .collect();
        let v = view(&home, &playlists);
        assert_eq!(
            kinds(&v),
            [
                Kind::RecentlyAdded,
                Kind::Mixes,
                Kind::MostPlayed,
                Kind::RecentlyPlayed,
                Kind::Artists,
                Kind::Playlists
            ]
        );
        for s in &v.sections {
            if s.kind == Kind::Mixes {
                continue;
            }
            assert_eq!(s.row.len(), ROW, "{:?}", s.kind);
        }
        assert_eq!(v.highlights.len(), HIGHLIGHTS);
    }

    #[test]
    fn the_mixes_are_mine_then_rediscover_then_the_genres() {
        let (_d, conn) = copy(3, 3);
        let home = Home::read(&conn, 1_790_000_000).unwrap();
        let v = view(&home, &[]);
        let Row::Mixes(mixes) = &v.sections[1].row else {
            panic!("mixes")
        };
        assert_eq!(mixes[0], Mix::Mine);
        assert_eq!(mixes[1], Mix::Rediscover);
        assert!(matches!(&mixes[2], Mix::Genre { name, .. } if name == "Folk"));
        assert_eq!(
            mixes[2].job(),
            Job::MixGenre {
                name: "Folk".into()
            }
        );
    }

    #[test]
    fn highlights_start_with_the_newest_album_then_the_mix_without_repeats() {
        let home = Home {
            recent: vec![album("new"), album("b")],
            most_played: vec![album("top"), album("last")],
            recently_played: vec![album("new"), album("top")],
            ..Home::default()
        };
        let v = view(&home, &[]);
        assert_eq!(
            v.highlights,
            [
                Highlight::Album {
                    album: album("new"),
                    why: Why::New
                },
                Highlight::Mix,
                // the album played last is the newest one: the next one played is not taken
                // in its place, the most played one comes
                Highlight::Album {
                    album: album("top"),
                    why: Why::MostPlayed
                },
            ]
        );
    }

    #[test]
    fn see_all_opens_the_albums_in_the_rows_order_or_the_artists() {
        assert_eq!(
            Kind::RecentlyAdded.see_all(),
            Some(SeeAll::Albums(Sort::desc(AlbumBy::Added)))
        );
        assert_eq!(
            Kind::MostPlayed.see_all(),
            Some(SeeAll::Albums(Sort::desc(AlbumBy::Plays)))
        );
        assert_eq!(
            Kind::RecentlyPlayed.see_all(),
            Some(SeeAll::Albums(Sort::desc(AlbumBy::Played)))
        );
        assert_eq!(Kind::Artists.see_all(), Some(SeeAll::Artists));
        assert_eq!(Kind::Mixes.see_all(), None);
        assert_eq!(Kind::Playlists.see_all(), None);
    }

    #[test]
    fn the_greeting_follows_the_time_of_day() {
        assert_eq!(greeting(5), "Good morning");
        assert_eq!(greeting(11), "Good morning");
        assert_eq!(greeting(12), "Good afternoon");
        assert_eq!(greeting(17), "Good afternoon");
        assert_eq!(greeting(18), "Good evening");
        assert_eq!(greeting(23), "Good evening");
        assert_eq!(greeting(2), "Good evening");
    }
}
