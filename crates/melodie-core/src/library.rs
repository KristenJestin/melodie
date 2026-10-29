//! Browsing the local copy: the albums, artists, tracks and playlists in the orders a window
//! shows them, and what an album, an artist or a playlist holds. Reads only: no network.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub year: i64,
    pub songs: i64,
    /// Seconds.
    pub duration: i64,
    pub plays: i64,
    pub cover: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub albums: i64,
    pub tracks: i64,
    pub cover: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub album: String,
    pub album_id: Option<String>,
    pub year: i64,
    /// Its number on the album.
    pub number: i64,
    /// Seconds.
    pub duration: i64,
    pub plays: i64,
    pub cover: Option<String>,
    /// Every artist credited on it, in the server's order, the main one first; at least its own
    /// artist when it has one, for a server that credits no others.
    pub artists: Vec<Credit>,
}

/// An artist credited on a track. No id: the copy does not know that artist.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Credit {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub songs: i64,
    /// Seconds.
    pub duration: i64,
    pub owner: Option<String>,
    pub cover: Option<String>,
}

/// A column and a direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort<C> {
    pub by: C,
    pub descending: bool,
}

impl<C: Copy + PartialEq> Sort<C> {
    pub fn asc(by: C) -> Self {
        Self {
            by,
            descending: false,
        }
    }

    pub fn desc(by: C) -> Self {
        Self {
            by,
            descending: true,
        }
    }

    /// What a click on the header of `by` gives: the other direction on the same column, else
    /// that column in its natural direction.
    pub fn clicked(self, by: C, natural_descending: bool) -> Self {
        if self.by == by {
            Self {
                by,
                descending: !self.descending,
            }
        } else {
            Self {
                by,
                descending: natural_descending,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumBy {
    /// When it came into the library.
    Added,
    Name,
    Artist,
    Year,
    /// How often it was played.
    Plays,
    /// When it was last played.
    Played,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistBy {
    Name,
    Albums,
    Tracks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackBy {
    Title,
    Artist,
    Album,
    Year,
    Duration,
    Plays,
    /// When it came into the library.
    Added,
    /// When it was last played.
    Played,
}

fn dir(descending: bool) -> &'static str {
    if descending { "DESC" } else { "ASC" }
}

const ALBUM: &str =
    "SELECT id, name, artist, artistId, year, songCount, duration, playCount, coverArt FROM albums";

fn album_row(r: &Row) -> rusqlite::Result<Album> {
    Ok(Album {
        id: r.get(0)?,
        name: r.get(1)?,
        artist: r.get(2)?,
        artist_id: r.get(3)?,
        year: r.get(4)?,
        songs: r.get(5)?,
        duration: r.get(6)?,
        plays: r.get(7)?,
        cover: r.get(8)?,
    })
}

/// The artists credited on the track `t`, in order: each as its id and its name, the two split by
/// a unit separator and the artists by a record separator (no name holds either). An artist the
/// copy does not list is left out. A macro, for `TRACK` to be one constant.
macro_rules! credits {
    () => {
        "(SELECT group_concat(c.artist || char(31) || ca.name, char(30) ORDER BY c.pos, c.rowid)
    FROM track_artists c JOIN artists ca ON ca.id = c.artist WHERE c.track = t.id)"
    };
}

const TRACK: &str = concat!(
    "SELECT t.id, t.title, t.artist, t.artistId, t.album, t.albumId, t.year, t.track,
    t.duration, t.playCount, t.coverArt, ",
    credits!(),
    " FROM tracks t"
);

/// The artists of a track from the `credits!` it was read with, else its own artist.
fn credited(joined: Option<String>, artist: &str, artist_id: Option<&str>) -> Vec<Credit> {
    let listed: Vec<Credit> = joined
        .iter()
        .flat_map(|j| j.split('\x1e'))
        .filter_map(|c| c.split_once('\x1f'))
        .map(|(id, name)| Credit {
            id: Some(id.to_string()),
            name: name.to_string(),
        })
        .collect();
    if !listed.is_empty() || (artist.is_empty() && artist_id.is_none()) {
        return listed;
    }
    vec![Credit {
        id: artist_id.map(str::to_string),
        name: artist.to_string(),
    }]
}

fn track_row(r: &Row) -> rusqlite::Result<Track> {
    let (artist, artist_id): (String, Option<String>) = (r.get(2)?, r.get(3)?);
    Ok(Track {
        id: r.get(0)?,
        title: r.get(1)?,
        artists: credited(r.get(11)?, &artist, artist_id.as_deref()),
        artist,
        artist_id,
        album: r.get(4)?,
        album_id: r.get(5)?,
        year: r.get(6)?,
        number: r.get(7)?,
        duration: r.get(8)?,
        plays: r.get(9)?,
        cover: r.get(10)?,
    })
}

fn all<T>(
    conn: &Connection,
    sql: &str,
    p: impl rusqlite::Params,
    f: fn(&Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut st = conn.prepare(sql)?;
    let rows = st.query_map(p, f)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every album. Added: the newest first in natural order; an album whose date is unknown (a copy
/// older than that date) comes after the dated ones.
pub fn albums(conn: &Connection, sort: Sort<AlbumBy>) -> Result<Vec<Album>> {
    let d = dir(sort.descending);
    let order = match sort.by {
        AlbumBy::Added => format!("created IS NULL, created {d}, rowid {d}"),
        AlbumBy::Name => format!("name COLLATE NOCASE {d}, artist COLLATE NOCASE"),
        AlbumBy::Artist => format!("artist COLLATE NOCASE {d}, year, name COLLATE NOCASE"),
        AlbumBy::Year => format!("year {d}, artist COLLATE NOCASE, name COLLATE NOCASE"),
        AlbumBy::Plays => format!("playCount {d}, name COLLATE NOCASE"),
        AlbumBy::Played => format!("played IS NULL, played {d}, name COLLATE NOCASE"),
    };
    all(
        conn,
        &format!("{ALBUM} ORDER BY {order}, id"),
        [],
        album_row,
    )
}

/// The `n` albums added last.
pub fn recently_added(conn: &Connection, n: usize) -> Result<Vec<Album>> {
    all(
        conn,
        &format!("{ALBUM} ORDER BY created IS NULL, created DESC, rowid DESC, id LIMIT ?1"),
        [n as i64],
        album_row,
    )
}

/// The `n` albums played most, among those played at all.
pub fn most_played(conn: &Connection, n: usize) -> Result<Vec<Album>> {
    all(
        conn,
        &format!(
            "{ALBUM} WHERE playCount > 0 ORDER BY playCount DESC, played DESC, name COLLATE NOCASE, id LIMIT ?1"
        ),
        [n as i64],
        album_row,
    )
}

/// The `n` albums played last, among those played at all.
pub fn recently_played(conn: &Connection, n: usize) -> Result<Vec<Album>> {
    all(
        conn,
        &format!(
            "{ALBUM} WHERE played IS NOT NULL ORDER BY played DESC, name COLLATE NOCASE, id LIMIT ?1"
        ),
        [n as i64],
        album_row,
    )
}

pub fn album(conn: &Connection, id: &str) -> Result<Option<Album>> {
    Ok(
        all(conn, &format!("{ALBUM} WHERE id = ?1"), [id], album_row)?
            .into_iter()
            .next(),
    )
}

/// The tracks of an album, in its order.
pub fn album_tracks(conn: &Connection, id: &str) -> Result<Vec<Track>> {
    all(
        conn,
        &format!("{TRACK} WHERE t.albumId = ?1 ORDER BY t.track, t.title COLLATE NOCASE, t.id"),
        [id],
        track_row,
    )
}

/// One track by its id, with the album and the artist it belongs to (a queue entry has only
/// their names).
pub fn track(conn: &Connection, id: &str) -> Result<Option<Track>> {
    Ok(
        all(conn, &format!("{TRACK} WHERE t.id = ?1"), [id], track_row)?
            .into_iter()
            .next(),
    )
}

/// The artists of the tracks of these ids, by id: for a list that knows only the tracks (the
/// queue, the search). A track the copy does not hold is left out.
pub fn credits(conn: &Connection, ids: &[String]) -> Result<HashMap<String, Vec<Credit>>> {
    let mut st = conn.prepare_cached(concat!(
        "SELECT t.artist, t.artistId, ",
        credits!(),
        " FROM tracks t WHERE t.id = ?1"
    ))?;
    let mut found = HashMap::new();
    for id in ids {
        let row = st
            .query_row([id], |r| {
                let (artist, artist_id): (String, Option<String>) = (r.get(0)?, r.get(1)?);
                Ok(credited(r.get(2)?, &artist, artist_id.as_deref()))
            })
            .optional()?;
        if let Some(artists) = row {
            found.insert(id.clone(), artists);
        }
    }
    Ok(found)
}

const ARTIST: &str = "SELECT a.id, a.name, a.albumCount, COALESCE(n.c, 0), a.coverArt FROM artists a
    LEFT JOIN (SELECT artistId, count(*) AS c FROM tracks GROUP BY artistId) n ON n.artistId = a.id";

/// A condition on an `artists a` row: the artist has something to play, an album of their own, a
/// track, or an appearance on one. Navidrome also lists the artists of the other credits
/// (composer, producer…), most of which have none of these; they stay in the copy, but the lists
/// and the search leave them out. The albums are looked up once for the whole query (they have no
/// index on the artist), the tracks and the appearances by their indexes.
pub const HAS_SOMETHING_TO_PLAY: &str =
    "(a.id IN (SELECT artistId FROM albums WHERE artistId IS NOT NULL)
    OR EXISTS (SELECT 1 FROM tracks WHERE artistId = a.id)
    OR EXISTS (SELECT 1 FROM track_artists WHERE artist = a.id))";

fn artist_row(r: &Row) -> rusqlite::Result<Artist> {
    Ok(Artist {
        id: r.get(0)?,
        name: r.get(1)?,
        albums: r.get(2)?,
        tracks: r.get(3)?,
        cover: r.get(4)?,
    })
}

/// The artists that have something to play (see `HAS_SOMETHING_TO_PLAY`).
pub fn artists(conn: &Connection, sort: Sort<ArtistBy>) -> Result<Vec<Artist>> {
    let d = dir(sort.descending);
    let order = match sort.by {
        ArtistBy::Name => format!("a.name COLLATE NOCASE {d}"),
        ArtistBy::Albums => format!("a.albumCount {d}, a.name COLLATE NOCASE"),
        ArtistBy::Tracks => format!("COALESCE(n.c, 0) {d}, a.name COLLATE NOCASE"),
    };
    all(
        conn,
        &format!("{ARTIST} WHERE {HAS_SOMETHING_TO_PLAY} ORDER BY {order}, a.id"),
        [],
        artist_row,
    )
}

pub fn artist(conn: &Connection, id: &str) -> Result<Option<Artist>> {
    Ok(
        all(conn, &format!("{ARTIST} WHERE a.id = ?1"), [id], artist_row)?
            .into_iter()
            .next(),
    )
}

/// The `n` artists whose tracks were played most, among those played at all.
pub fn top_artists(conn: &Connection, n: usize) -> Result<Vec<Artist>> {
    all(
        conn,
        &format!(
            "{ARTIST} JOIN (SELECT artistId, sum(playCount) AS p FROM tracks GROUP BY artistId) s
             ON s.artistId = a.id WHERE s.p > 0 ORDER BY s.p DESC, a.name COLLATE NOCASE, a.id LIMIT ?1"
        ),
        [n as i64],
        artist_row,
    )
}

/// The albums of an artist, the latest first.
pub fn artist_albums(conn: &Connection, id: &str) -> Result<Vec<Album>> {
    all(
        conn,
        &format!("{ALBUM} WHERE artistId = ?1 ORDER BY year DESC, name COLLATE NOCASE, id"),
        [id],
        album_row,
    )
}

/// The `n` tracks of an artist played most; among tracks played as often, the favourites first,
/// then by title.
pub fn top_tracks(conn: &Connection, id: &str, n: usize) -> Result<Vec<Track>> {
    all(
        conn,
        &format!(
            "{TRACK} WHERE t.artistId = ?1
             ORDER BY t.playCount DESC, t.starred IS NULL, t.title COLLATE NOCASE, t.id LIMIT ?2"
        ),
        params![id, n as i64],
        track_row,
    )
}

/// The tracks an artist appears on outside their own albums: credited on another artist's tracks
/// (featured), or on an album that is not theirs. The latest album first, then in its order.
pub fn artist_appearances(conn: &Connection, id: &str) -> Result<Vec<Track>> {
    all(
        conn,
        &format!(
            "{TRACK} LEFT JOIN albums a ON a.id = t.albumId
             WHERE (t.artistId = ?1 OR t.id IN (SELECT track FROM track_artists WHERE artist = ?1))
               AND a.artistId IS NOT ?1
             ORDER BY t.year DESC, t.album COLLATE NOCASE, t.albumId, t.track,
               t.title COLLATE NOCASE, t.id"
        ),
        [id],
        track_row,
    )
}

/// Every track.
pub fn tracks(conn: &Connection, sort: Sort<TrackBy>) -> Result<Vec<Track>> {
    let d = dir(sort.descending);
    let order = match sort.by {
        TrackBy::Title => format!("t.title COLLATE NOCASE {d}, t.artist COLLATE NOCASE"),
        TrackBy::Artist => {
            format!("t.artist COLLATE NOCASE {d}, t.album COLLATE NOCASE, t.track")
        }
        TrackBy::Album => format!("t.album COLLATE NOCASE {d}, t.track"),
        TrackBy::Year => format!("t.year {d}, t.album COLLATE NOCASE, t.track"),
        TrackBy::Duration => format!("t.duration {d}, t.title COLLATE NOCASE"),
        TrackBy::Plays => format!("t.playCount {d}, t.title COLLATE NOCASE"),
        // no date: last either way. Tracks added together keep their album's order.
        TrackBy::Added => {
            format!("t.created IS NULL, t.created {d}, t.album COLLATE NOCASE, t.track")
        }
        TrackBy::Played => format!("t.played IS NULL, t.played {d}, t.title COLLATE NOCASE"),
    };
    all(
        conn,
        &format!("{TRACK} ORDER BY {order}, t.id"),
        [],
        track_row,
    )
}

const PLAYLIST: &str = "SELECT id, name, songCount, duration, owner, coverArt FROM playlists";

fn playlist_row(r: &Row) -> rusqlite::Result<Playlist> {
    Ok(Playlist {
        id: r.get(0)?,
        name: r.get(1)?,
        songs: r.get(2)?,
        duration: r.get(3)?,
        owner: r.get(4)?,
        cover: r.get(5)?,
    })
}

/// The playlists, by name.
pub fn playlists(conn: &Connection) -> Result<Vec<Playlist>> {
    all(
        conn,
        &format!("{PLAYLIST} ORDER BY name COLLATE NOCASE, id"),
        [],
        playlist_row,
    )
}

pub fn playlist(conn: &Connection, id: &str) -> Result<Option<Playlist>> {
    Ok(all(
        conn,
        &format!("{PLAYLIST} WHERE id = ?1"),
        [id],
        playlist_row,
    )?
    .into_iter()
    .next())
}

/// The tracks of a playlist, in its order, as the last synchronisation saw it. A track the copy
/// does not hold (removed from the library since) is left out.
pub fn playlist_tracks(conn: &Connection, id: &str) -> Result<Vec<Track>> {
    all(
        conn,
        &format!(
            "{TRACK} JOIN playlist_tracks p ON p.track = t.id WHERE p.playlist = ?1 ORDER BY p.pos"
        ),
        [id],
        track_row,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use serde_json::{Value, json};

    fn ids<T>(v: &[T], f: fn(&T) -> &str) -> Vec<String> {
        v.iter().map(|x| f(x).to_string()).collect()
    }

    fn album_ids(v: &[Album]) -> Vec<String> {
        ids(v, |a| &a.id)
    }

    fn track_ids(v: &[Track]) -> Vec<String> {
        ids(v, |t| &t.id)
    }

    /// An invented library: three artists, four albums, a few tracks, two playlists.
    fn library() -> (tempfile::TempDir, Connection) {
        let d = tempfile::tempdir().unwrap();
        let mut conn = db::create(&d.path().join("l.db")).unwrap();
        let tx = conn.transaction().unwrap();
        for a in [
            json!({"id":"ar1","name":"Ada Marlow","albumCount":2}),
            json!({"id":"ar2","name":"the Quiet Engines","albumCount":1}),
            json!({"id":"ar3","name":"Ilse Varga","albumCount":1}),
        ] {
            db::insert_artist(&tx, &a).unwrap();
        }
        for a in [
            json!({"id":"al1","name":"Lanterns","artist":"Ada Marlow","artistId":"ar1","year":2019,
                   "playCount":4,"played":"2026-09-10T20:00:00Z","created":"2026-01-10T00:00:00Z"}),
            json!({"id":"al2","name":"copper Season","artist":"the Quiet Engines","artistId":"ar2",
                   "year":2021,"playCount":9,"played":"2026-08-01T09:00:00Z","created":"2026-03-02T00:00:00Z"}),
            json!({"id":"al3","name":"Northbound","artist":"Ilse Varga","artistId":"ar3","year":2008,
                   "created":"2026-02-20T00:00:00Z"}),
            json!({"id":"al4","name":"Small Hours","artist":"Ada Marlow","artistId":"ar1","year":2023}),
        ] {
            db::insert_album(&tx, &a).unwrap();
        }
        let t = |id: &str,
                 title: &str,
                 ar: (&str, &str),
                 al: (&str, &str),
                 n: i64,
                 plays: i64|
         -> Value {
            json!({"id":id,"title":title,"artist":ar.1,"artistId":ar.0,"album":al.1,"albumId":al.0,
                   "track":n,"duration":100 + n * 10,"playCount":plays,"year":2000 + n})
        };
        let ada = ("ar1", "Ada Marlow");
        let quiet = ("ar2", "the Quiet Engines");
        for v in [
            t("t1", "Harbour Lights", ada, ("al1", "Lanterns"), 2, 7),
            t("t2", "paper Boats", ada, ("al1", "Lanterns"), 1, 0),
            t("t3", "Slow Tide", ada, ("al1", "Lanterns"), 3, 7),
            t("t4", "Glasshouse", quiet, ("al2", "copper Season"), 1, 2),
            t("t5", "Afterglow", ada, ("al4", "Small Hours"), 1, 12),
        ] {
            db::insert_track(&tx, &v).unwrap();
        }
        db::apply_starred(
            &tx,
            &json!({"song":[{"id":"t3","starred":"2026-09-01T00:00:00Z"}]}),
        )
        .unwrap();
        db::replace_playlists(
            &tx,
            &[
                json!({"id":"p1","name":"road","songCount":3,"owner":"alice"}),
                json!({"id":"p2","name":"Evening","songCount":1,"owner":"bob"}),
            ],
        )
        .unwrap();
        db::replace_playlist_tracks(
            &tx,
            "p1",
            &["t4".into(), "t1".into(), "gone".into(), "t4".into()],
        )
        .unwrap();
        tx.commit().unwrap();
        (d, conn)
    }

    #[test]
    fn the_queries_of_the_window_work_on_an_older_copy() {
        let d = tempfile::tempdir().unwrap();
        let env = crate::env::Env::with_home(d.path());
        let old = db::create_before_browsing(&db::db_path(&env)).unwrap();
        old.execute_batch(
            "INSERT INTO artists(id, name) VALUES ('ar1', 'Ada Marlow');
             INSERT INTO albums(id, name, artist, artistId, playCount) VALUES ('al1', 'Lanterns', 'Ada Marlow', 'ar1', 3);
             INSERT INTO tracks(id, title, artist, artistId, album, albumId)
               VALUES ('t1', 'Harbour Lights', 'Ada Marlow', 'ar1', 'Lanterns', 'al1');
             INSERT INTO playlists(id, name) VALUES ('p1', 'road');",
        )
        .unwrap();
        drop(old);

        let conn = db::open_existing(&env).unwrap().unwrap();
        assert_eq!(album_ids(&recently_added(&conn, 10).unwrap()), ["al1"]);
        assert_eq!(album_ids(&most_played(&conn, 10).unwrap()), ["al1"]);
        assert!(recently_played(&conn, 10).unwrap().is_empty());
        assert!(top_artists(&conn, 10).unwrap().is_empty());
        assert_eq!(
            album_ids(&albums(&conn, Sort::desc(AlbumBy::Played)).unwrap()),
            ["al1"]
        );
        assert_eq!(
            album_ids(&albums(&conn, Sort::desc(AlbumBy::Added)).unwrap()),
            ["al1"]
        );
        assert_eq!(artists(&conn, Sort::asc(ArtistBy::Name)).unwrap().len(), 1);
        assert_eq!(tracks(&conn, Sort::asc(TrackBy::Title)).unwrap().len(), 1);
        assert_eq!(playlists(&conn).unwrap().len(), 1);
        assert!(playlist_tracks(&conn, "p1").unwrap().is_empty());
    }

    #[test]
    fn recently_added_albums_come_newest_first_and_undated_ones_last() {
        let (_d, conn) = library();
        assert_eq!(
            album_ids(&recently_added(&conn, 10).unwrap()),
            ["al2", "al3", "al1", "al4"]
        );
        assert_eq!(
            album_ids(&recently_added(&conn, 2).unwrap()),
            ["al2", "al3"]
        );
        let added = albums(&conn, Sort::desc(AlbumBy::Added)).unwrap();
        assert_eq!(album_ids(&added), ["al2", "al3", "al1", "al4"]);
    }

    #[test]
    fn albums_sort_by_name_artist_and_year() {
        let (_d, conn) = library();
        let by = |s| album_ids(&albums(&conn, s).unwrap());
        // case does not count
        assert_eq!(by(Sort::asc(AlbumBy::Name)), ["al2", "al1", "al3", "al4"]);
        assert_eq!(by(Sort::desc(AlbumBy::Name)), ["al4", "al3", "al1", "al2"]);
        // one artist's albums by year
        assert_eq!(by(Sort::asc(AlbumBy::Artist)), ["al1", "al4", "al3", "al2"]);
        assert_eq!(by(Sort::desc(AlbumBy::Year)), ["al4", "al2", "al1", "al3"]);
        assert_eq!(by(Sort::asc(AlbumBy::Year)), ["al3", "al1", "al2", "al4"]);
    }

    #[test]
    fn most_played_albums_leave_out_the_unplayed() {
        let (_d, conn) = library();
        assert_eq!(album_ids(&most_played(&conn, 10).unwrap()), ["al2", "al1"]);
    }

    #[test]
    fn recently_played_albums_come_last_played_first_and_leave_out_the_unplayed() {
        let (_d, conn) = library();
        assert_eq!(
            album_ids(&recently_played(&conn, 10).unwrap()),
            ["al1", "al2"]
        );
        assert_eq!(album_ids(&recently_played(&conn, 1).unwrap()), ["al1"]);
    }

    #[test]
    fn albums_sort_by_plays_and_by_last_played() {
        let (_d, conn) = library();
        let by = |s| album_ids(&albums(&conn, s).unwrap());
        assert_eq!(by(Sort::desc(AlbumBy::Plays)), ["al2", "al1", "al3", "al4"]);
        assert_eq!(by(Sort::asc(AlbumBy::Plays)), ["al3", "al4", "al1", "al2"]);
        // never played comes last, whichever the direction
        assert_eq!(
            by(Sort::desc(AlbumBy::Played)),
            ["al1", "al2", "al3", "al4"]
        );
        assert_eq!(by(Sort::asc(AlbumBy::Played)), ["al2", "al1", "al3", "al4"]);
    }

    #[test]
    fn top_artists_are_those_whose_tracks_are_played_most() {
        let (_d, conn) = library();
        let top = top_artists(&conn, 10).unwrap();
        // Ilse Varga has no track played
        assert_eq!(ids(&top, |a| &a.id), ["ar1", "ar2"]);
        assert_eq!(ids(&top_artists(&conn, 1).unwrap(), |a| &a.id), ["ar1"]);
    }

    #[test]
    fn artists_sort_by_their_columns() {
        let (_d, conn) = library();
        let by = |s| ids(&artists(&conn, s).unwrap(), |a| &a.id);
        assert_eq!(by(Sort::asc(ArtistBy::Name)), ["ar1", "ar3", "ar2"]);
        assert_eq!(by(Sort::desc(ArtistBy::Albums)), ["ar1", "ar3", "ar2"]);
        assert_eq!(by(Sort::desc(ArtistBy::Tracks)), ["ar1", "ar2", "ar3"]);
        assert_eq!(by(Sort::asc(ArtistBy::Tracks)), ["ar3", "ar2", "ar1"]);
        let ada = artist(&conn, "ar1").unwrap().unwrap();
        assert_eq!((ada.albums, ada.tracks), (2, 4));
        assert_eq!(artist(&conn, "nobody").unwrap(), None);
    }

    /// The invented library, plus: Bram Oduya is only credited on a track of Ada's (an
    /// appearance), Cleo Hart only on a track whose own artist is hers, Dov Reyes has nothing
    /// (a composer or a producer, say).
    fn library_with_credits_only() -> (tempfile::TempDir, Connection) {
        let (d, conn) = library();
        conn.execute_batch(
            "INSERT INTO artists(id, name) VALUES ('ar4', 'Bram Oduya'), ('ar5', 'Cleo Hart'),
               ('ar6', 'Dov Reyes');
             INSERT INTO tracks(id, title, artist, artistId, album, albumId)
               VALUES ('t6', 'Single Cut', 'Cleo Hart', 'ar5', 'Loose', NULL);
             INSERT INTO track_artists(track, artist) VALUES ('t1', 'ar4');",
        )
        .unwrap();
        (d, conn)
    }

    #[test]
    fn the_artists_page_leaves_out_artists_with_nothing_to_play() {
        let (_d, conn) = library_with_credits_only();
        let by = |s| ids(&artists(&conn, s).unwrap(), |a| &a.id);
        // an album of their own, a track of their own, an appearance: kept; nothing: left out
        assert_eq!(
            by(Sort::asc(ArtistBy::Name)),
            ["ar1", "ar4", "ar5", "ar3", "ar2"]
        );
        assert!(!by(Sort::desc(ArtistBy::Tracks)).contains(&"ar6".to_string()));
    }

    #[test]
    fn an_artist_with_nothing_to_play_stays_in_the_copy_and_opens() {
        let (_d, conn) = library_with_credits_only();
        let dov = artist(&conn, "ar6").unwrap().unwrap();
        assert_eq!((dov.albums, dov.tracks), (0, 0));
        assert!(artist_albums(&conn, "ar6").unwrap().is_empty());
        assert!(artist_appearances(&conn, "ar6").unwrap().is_empty());
        let kept: i64 = conn
            .query_row("SELECT count(*) FROM artists", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 6);
    }

    #[test]
    fn tracks_sort_by_their_columns() {
        let (_d, conn) = library();
        let by = |s| track_ids(&tracks(&conn, s).unwrap());
        assert_eq!(
            by(Sort::asc(TrackBy::Title)),
            ["t5", "t4", "t1", "t2", "t3"]
        );
        assert_eq!(
            by(Sort::asc(TrackBy::Artist)),
            ["t2", "t1", "t3", "t5", "t4"]
        );
        assert_eq!(
            by(Sort::asc(TrackBy::Album)),
            ["t4", "t2", "t1", "t3", "t5"]
        );
        assert_eq!(
            by(Sort::desc(TrackBy::Year)),
            ["t3", "t1", "t4", "t2", "t5"]
        );
        assert_eq!(
            by(Sort::desc(TrackBy::Duration)),
            ["t3", "t1", "t5", "t4", "t2"]
        );
        assert_eq!(
            by(Sort::desc(TrackBy::Plays)),
            ["t5", "t1", "t3", "t4", "t2"]
        );
    }

    #[test]
    fn tracks_sort_by_when_they_were_added_and_last_played_the_undated_last() {
        let (_d, conn) = library();
        conn.execute_batch(
            "UPDATE tracks SET created = '2026-03-01T00:00:00Z' WHERE id IN ('t1', 't2');
             UPDATE tracks SET created = '2026-04-01T00:00:00Z' WHERE id = 't4';
             UPDATE tracks SET created = '2026-02-01T00:00:00Z' WHERE id = 't5';
             UPDATE tracks SET played = '2026-09-10T20:00:00Z' WHERE id = 't1';
             UPDATE tracks SET played = '2026-08-01T09:00:00Z' WHERE id = 't4';
             UPDATE tracks SET played = '2026-09-20T09:00:00Z' WHERE id = 't5';",
        )
        .unwrap();
        let by = |s| track_ids(&tracks(&conn, s).unwrap());
        // t3 has no date: last either way; t1 and t2 came together: their album's order
        assert_eq!(
            by(Sort::desc(TrackBy::Added)),
            ["t4", "t2", "t1", "t5", "t3"]
        );
        assert_eq!(
            by(Sort::asc(TrackBy::Added)),
            ["t5", "t2", "t1", "t4", "t3"]
        );
        // t2 and t3 were never played: by title
        assert_eq!(
            by(Sort::desc(TrackBy::Played)),
            ["t5", "t1", "t4", "t2", "t3"]
        );
        assert_eq!(
            by(Sort::asc(TrackBy::Played)),
            ["t4", "t1", "t5", "t2", "t3"]
        );
    }

    #[test]
    fn a_click_on_a_header_flips_or_takes_the_column() {
        let s = Sort::asc(TrackBy::Title);
        assert_eq!(s.clicked(TrackBy::Title, false), Sort::desc(TrackBy::Title));
        assert_eq!(s.clicked(TrackBy::Plays, true), Sort::desc(TrackBy::Plays));
        assert_eq!(
            s.clicked(TrackBy::Artist, false),
            Sort::asc(TrackBy::Artist)
        );
    }

    #[test]
    fn an_album_lists_its_tracks_in_order() {
        let (_d, conn) = library();
        assert_eq!(
            track_ids(&album_tracks(&conn, "al1").unwrap()),
            ["t2", "t1", "t3"]
        );
        assert_eq!(album(&conn, "al1").unwrap().unwrap().name, "Lanterns");
    }

    #[test]
    fn a_track_is_found_by_its_id_with_its_album_and_artist_ids() {
        let (_d, conn) = library();
        let t = track(&conn, "t4").unwrap().unwrap();
        assert_eq!(t.title, "Glasshouse");
        assert_eq!(t.album_id.as_deref(), Some("al2"));
        assert_eq!(t.artist_id.as_deref(), Some("ar2"));
        assert_eq!(track(&conn, "nope").unwrap(), None);
    }

    #[test]
    fn an_artist_shows_its_albums_latest_first_and_its_top_tracks() {
        let (_d, conn) = library();
        assert_eq!(
            album_ids(&artist_albums(&conn, "ar1").unwrap()),
            ["al4", "al1"]
        );
        // t1 and t3 are played as often: the favourite first
        assert_eq!(
            track_ids(&top_tracks(&conn, "ar1", 3).unwrap()),
            ["t5", "t3", "t1"]
        );
        assert_eq!(top_tracks(&conn, "ar1", 10).unwrap().len(), 4);
    }

    #[test]
    fn an_artist_appears_on_the_tracks_credited_to_them_outside_their_albums() {
        let (_d, mut conn) = library();
        let tx = conn.transaction().unwrap();
        let credited = |id: &str, artist: (&str, &str), album: (&str, &str), year: i64| {
            json!({"id":id,"title":id,"artist":artist.1,"artistId":artist.0,"album":album.1,
                   "albumId":album.0,"year":year,"track":1,
                   "artists":[{"id":artist.0,"name":artist.1},{"id":"ar3","name":"Ilse Varga"}]})
        };
        for v in [
            // featured on the tracks of others
            credited("f1", ("ar1", "Ada Marlow"), ("al1", "Lanterns"), 2019),
            credited(
                "f2",
                ("ar2", "the Quiet Engines"),
                ("al2", "copper Season"),
                2021,
            ),
            // their own track, on another artist's album
            credited("f3", ("ar3", "Ilse Varga"), ("al2", "copper Season"), 2021),
            // their own track, on their own album: not an appearance
            credited("f4", ("ar3", "Ilse Varga"), ("al3", "Northbound"), 2008),
        ] {
            db::insert_track(&tx, &v).unwrap();
        }
        tx.commit().unwrap();
        // the latest album first, then in the album's order
        let appears = track_ids(&artist_appearances(&conn, "ar3").unwrap());
        assert_eq!(appears, ["f2", "f3", "f1"]);
        // an artist credited only on their own albums appears nowhere
        assert!(artist_appearances(&conn, "ar2").unwrap().is_empty());
        assert!(artist_appearances(&conn, "nobody").unwrap().is_empty());
    }

    fn names(t: &Track) -> Vec<(Option<&str>, &str)> {
        t.artists
            .iter()
            .map(|c| (c.id.as_deref(), c.name.as_str()))
            .collect()
    }

    /// A track credited to three artists, the main one first, as Navidrome sends it: their names
    /// joined in `artist`.
    fn three_artists(conn: &mut Connection) {
        let tx = conn.transaction().unwrap();
        db::insert_track(
            &tx,
            &json!({"id":"t9","title":"Crossing","artist":"Ilse Varga, Ada Marlow, the Quiet Engines",
                    "artistId":"ar3","album":"Northbound","albumId":"al3","track":2,
                    "artists":[{"id":"ar3","name":"Ilse Varga"},{"id":"ar1","name":"Ada Marlow"},
                               {"id":"ar2","name":"the Quiet Engines"}]}),
        )
        .unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn a_track_of_three_artists_is_read_with_the_three_in_order() {
        let (_d, mut conn) = library();
        three_artists(&mut conn);
        let want = [
            (Some("ar3"), "Ilse Varga"),
            (Some("ar1"), "Ada Marlow"),
            (Some("ar2"), "the Quiet Engines"),
        ];
        assert_eq!(names(&track(&conn, "t9").unwrap().unwrap()), want);
        // every list of tracks reads them with the rows
        let listed = album_tracks(&conn, "al3").unwrap();
        assert_eq!(names(&listed[0]), want);
        let all = tracks(&conn, Sort::asc(TrackBy::Title)).unwrap();
        let t9 = all.iter().find(|t| t.id == "t9").unwrap();
        assert_eq!(names(t9), want);
    }

    #[test]
    fn a_track_of_one_artist_is_read_with_that_artist() {
        let (_d, conn) = library();
        // a server that sends no `artists`: the track's own artist, as before
        let t = track(&conn, "t4").unwrap().unwrap();
        assert_eq!(names(&t), [(Some("ar2"), "the Quiet Engines")]);
        // an artist without an id is still named
        conn.execute_batch(
            "INSERT INTO tracks(id, title, artist) VALUES ('t0', 'Loose', 'Nova Reyes')",
        )
        .unwrap();
        let t = track(&conn, "t0").unwrap().unwrap();
        assert_eq!(names(&t), [(None, "Nova Reyes")]);
    }

    #[test]
    fn the_artists_of_tracks_are_read_by_their_ids() {
        let (_d, mut conn) = library();
        three_artists(&mut conn);
        let ids = ["t9", "t4", "gone"].map(String::from);
        let found = credits(&conn, &ids).unwrap();
        assert_eq!(found.len(), 2);
        let order: Vec<&str> = found["t9"].iter().map(|c| c.name.as_str()).collect();
        assert_eq!(order, ["Ilse Varga", "Ada Marlow", "the Quiet Engines"]);
        assert_eq!(found["t4"][0].id.as_deref(), Some("ar2"));
    }

    #[test]
    fn a_playlist_lists_its_tracks_in_its_order_with_repeats() {
        let (_d, conn) = library();
        assert_eq!(
            track_ids(&playlist_tracks(&conn, "p1").unwrap()),
            ["t4", "t1", "t4"]
        );
        assert!(playlist_tracks(&conn, "p2").unwrap().is_empty());
        assert_eq!(ids(&playlists(&conn).unwrap(), |p| &p.id), ["p2", "p1"]);
        assert_eq!(
            playlist(&conn, "p1").unwrap().unwrap().owner.as_deref(),
            Some("alice")
        );
    }

    #[test]
    fn browsing_an_older_copy_without_added_dates_works() {
        let (_d, conn) = library();
        conn.execute_batch("UPDATE albums SET created = NULL")
            .unwrap();
        // the order they were copied in, the last first
        assert_eq!(
            album_ids(&recently_added(&conn, 10).unwrap()),
            ["al4", "al3", "al2", "al1"]
        );
    }
}
