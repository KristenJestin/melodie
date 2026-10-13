//! The one track table every list of tracks is drawn with (an album, a playlist, an artist's top
//! tracks, the library's tracks, the search): its columns, their widths, the columns a narrow
//! window drops, and the sort of its headers. Kept away from GPUI so that it is tested. The queue
//! is not one of these tables: its order is the play order.

use crate::browse::Place;
use crate::selection::Selection;
use melodie_core::library::{Sort, Track, TrackBy};
use melodie_core::search::Hit;
use std::sync::Arc;

/// A column, in the order every table shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Col {
    /// The row's rank in the list's own order.
    Number,
    Title,
    Artist,
    Album,
    Year,
    Plays,
    Time,
}

pub const COLS: [Col; 7] = [
    Col::Number,
    Col::Title,
    Col::Artist,
    Col::Album,
    Col::Year,
    Col::Plays,
    Col::Time,
];

/// Under this content width, the secondary columns go, in every table.
pub const NARROW: f32 = 640.0;

/// How wide a column is: fixed, or a share of what the fixed ones leave.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Width {
    Fixed(f32),
    Share(f32),
}

impl Col {
    pub fn label(self) -> &'static str {
        match self {
            Self::Number => "#",
            Self::Title => "TITLE",
            Self::Artist => "ARTIST",
            Self::Album => "ALBUM",
            Self::Year => "YEAR",
            Self::Plays => "PLAYS",
            Self::Time => "TIME",
        }
    }

    pub fn width(self) -> Width {
        match self {
            Self::Number => Width::Fixed(32.),
            Self::Title => Width::Share(3.),
            Self::Artist | Self::Album => Width::Share(2.),
            Self::Year => Width::Fixed(48.),
            Self::Plays | Self::Time => Width::Fixed(56.),
        }
    }

    /// Dropped on a narrow window.
    fn secondary(self) -> bool {
        matches!(self, Self::Album | Self::Year | Self::Plays)
    }

    /// The library's order for it, read from the local copy.
    fn by(self) -> Option<TrackBy> {
        match self {
            Self::Number => None,
            Self::Title => Some(TrackBy::Title),
            Self::Artist => Some(TrackBy::Artist),
            Self::Album => Some(TrackBy::Album),
            Self::Year => Some(TrackBy::Year),
            Self::Plays => Some(TrackBy::Plays),
            Self::Time => Some(TrackBy::Duration),
        }
    }
}

/// Where a table is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    /// Every track of the library.
    Tracks,
    Album,
    Playlist,
    /// An artist's top tracks.
    TopTracks,
    /// The tracks found by the search.
    Search,
}

pub const TABLES: [Table; 5] = [
    Table::Tracks,
    Table::Album,
    Table::Playlist,
    Table::TopTracks,
    Table::Search,
];

impl Table {
    /// The columns that make no sense here.
    fn leaves_out(self, col: Col) -> bool {
        match self {
            // the library's own order is by artist: a rank in it says nothing
            Self::Tracks => col == Col::Number,
            // the same for every row
            Self::Album => matches!(col, Col::Album | Col::Year),
            Self::Playlist => false,
            Self::TopTracks => col == Col::Artist,
            // a result's rank is its relevance; the search reads no year or plays of a track
            Self::Search => matches!(col, Col::Number | Col::Year | Col::Plays),
        }
    }

    /// The columns shown in a content `width` pixels wide, in order.
    pub fn cols(self, width: f32) -> Vec<Col> {
        COLS.into_iter()
            .filter(|c| !self.leaves_out(*c))
            .filter(|c| width >= NARROW || !c.secondary())
            .collect()
    }
}

/// A column and a direction; no sort is the list's own order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColSort {
    pub col: Col,
    pub descending: bool,
}

/// What a click on the header of `col` gives: ascending, then descending, then the list's own
/// order again.
pub fn clicked(current: Option<ColSort>, col: Col) -> Option<ColSort> {
    match current {
        Some(s) if s.col == col && !s.descending => Some(ColSort {
            col,
            descending: true,
        }),
        Some(s) if s.col == col => None,
        _ => Some(ColSort {
            col,
            descending: false,
        }),
    }
}

/// The library's tracks are sorted as the local copy is read; its own order is by artist.
pub fn library_sort(sort: Option<ColSort>) -> Sort<TrackBy> {
    match sort.and_then(|s| s.col.by().map(|by| (by, s.descending))) {
        Some((by, true)) => Sort::desc(by),
        Some((by, false)) => Sort::asc(by),
        None => Sort::asc(TrackBy::Artist),
    }
}

/// The ranks of `tracks`, in the order `sort` shows them.
pub fn order(tracks: &[Track], sort: Option<ColSort>) -> Vec<usize> {
    let mut ranks: Vec<usize> = (0..tracks.len()).collect();
    let Some(sort) = sort else { return ranks };
    let lower = |f: fn(&Track) -> &str| -> Vec<String> {
        tracks.iter().map(|t| f(t).to_lowercase()).collect()
    };
    let (title, artist, album) = (
        lower(|t| &t.title),
        lower(|t| &t.artist),
        lower(|t| &t.album),
    );
    let first = |a: usize, b: usize| {
        let (x, y) = (&tracks[a], &tracks[b]);
        match sort.col {
            Col::Number => a.cmp(&b),
            Col::Title => title[a].cmp(&title[b]),
            Col::Artist => artist[a].cmp(&artist[b]),
            Col::Album => album[a].cmp(&album[b]),
            Col::Year => x.year.cmp(&y.year),
            Col::Plays => x.plays.cmp(&y.plays),
            Col::Time => x.duration.cmp(&y.duration),
        }
    };
    // ties: as the library sorts them, then the list's own order
    let then = |a: usize, b: usize| {
        let (x, y) = (&tracks[a], &tracks[b]);
        match sort.col {
            Col::Artist => album[a].cmp(&album[b]).then(x.number.cmp(&y.number)),
            Col::Album => x.number.cmp(&y.number),
            Col::Year => album[a].cmp(&album[b]).then(x.number.cmp(&y.number)),
            Col::Plays | Col::Time => title[a].cmp(&title[b]),
            Col::Number | Col::Title => std::cmp::Ordering::Equal,
        }
        .then(a.cmp(&b))
    };
    ranks.sort_by(|&a, &b| {
        let o = first(a, b);
        if sort.descending { o.reverse() } else { o }.then_with(|| then(a, b))
    });
    ranks
}

/// A list of tracks as a table shows it: the list in its own order, never changed, and the rank
/// of the row shown at each place.
#[derive(Debug, Clone)]
pub struct Shown {
    pub tracks: Arc<[Track]>,
    pub sort: Option<ColSort>,
    ranks: Arc<[usize]>,
}

impl Shown {
    /// `tracks` in their own order, sorted by `sort` for the view.
    pub fn new(tracks: Arc<[Track]>, sort: Option<ColSort>) -> Self {
        let ranks = order(&tracks, sort).into();
        Self {
            tracks,
            sort,
            ranks,
        }
    }

    /// `tracks` read already in the order `sort` asks (the library's).
    pub fn read_sorted(tracks: Arc<[Track]>, sort: Option<ColSort>) -> Self {
        Self {
            ranks: (0..tracks.len()).collect(),
            tracks,
            sort,
        }
    }

    pub fn len(&self) -> usize {
        self.ranks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranks.is_empty()
    }

    /// The row shown at `ix`: its rank in the list's own order, and its track.
    pub fn row(&self, ix: usize) -> Option<(usize, &Track)> {
        let rank = *self.ranks.get(ix)?;
        Some((rank, &self.tracks[rank]))
    }

    /// The rows picked in `list`, each by its rank in the list's own order with the id it holds
    /// (what a removal names), in that order.
    pub fn picks(&self, selection: &Selection, list: &Place) -> Vec<(usize, String)> {
        let mut picks: Vec<(usize, String)> = selection
            .ranks(list)
            .into_iter()
            .filter_map(|ix| self.row(ix).map(|(rank, t)| (rank, t.id.clone())))
            .collect();
        picks.sort();
        picks
    }
}

/// A track found by the search, as a table row holds it. The search does not read the plays.
pub fn track_of(hit: &Hit) -> Track {
    Track {
        id: hit.id.clone(),
        title: hit.name.clone(),
        artist: hit.artist.clone(),
        artist_id: hit.artist_id.clone(),
        album: hit.album.clone(),
        album_id: hit.album_id.clone(),
        year: hit.year,
        duration: hit.duration,
        cover: hit.cover.clone(),
        ..Track::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::How;

    const WIDE: f32 = 1000.0;

    fn track(id: &str, title: &str, artist: &str, album: &str, n: i64) -> Track {
        Track {
            id: id.into(),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            number: n,
            ..Track::default()
        }
    }

    /// Four tracks in a list's own order, each column putting them in another order.
    fn list() -> Vec<Track> {
        vec![
            Track {
                year: 2001,
                plays: 5,
                duration: 200,
                ..track("t0", "Cedar", "Bob", "Moss", 2)
            },
            Track {
                year: 1999,
                plays: 9,
                duration: 100,
                ..track("t1", "apple", "Cid", "Fern", 1)
            },
            Track {
                year: 2010,
                plays: 1,
                duration: 300,
                ..track("t2", "Birch", "ada", "Moss", 1)
            },
            Track {
                year: 2005,
                plays: 0,
                duration: 250,
                ..track("t3", "Dune", "Bob", "Clay", 3)
            },
        ]
    }

    fn ids(tracks: &[Track], ranks: &[usize]) -> Vec<String> {
        ranks.iter().map(|&r| tracks[r].id.clone()).collect()
    }

    #[test]
    fn each_place_builds_the_same_table_definition() {
        for table in TABLES {
            let cols = table.cols(WIDE);
            // the same columns in the same order: a part of the one list, nothing added
            let mut all = COLS.iter();
            for c in &cols {
                assert!(all.any(|a| a == c), "{table:?}: {cols:?} out of order");
            }
            assert!(cols.contains(&Col::Title) && cols.contains(&Col::Time));
        }
        // what is left out makes no sense there
        assert_eq!(
            Table::Album.cols(WIDE),
            [Col::Number, Col::Title, Col::Artist, Col::Plays, Col::Time]
        );
        assert_eq!(Table::Playlist.cols(WIDE), COLS);
        assert_eq!(
            Table::TopTracks.cols(WIDE),
            [
                Col::Number,
                Col::Title,
                Col::Album,
                Col::Year,
                Col::Plays,
                Col::Time
            ]
        );
        assert_eq!(
            Table::Tracks.cols(WIDE),
            [
                Col::Title,
                Col::Artist,
                Col::Album,
                Col::Year,
                Col::Plays,
                Col::Time
            ]
        );
        assert_eq!(
            Table::Search.cols(WIDE),
            [Col::Title, Col::Artist, Col::Album, Col::Time]
        );
    }

    #[test]
    fn the_widths_follow_one_rule() {
        for c in [Col::Number, Col::Year, Col::Plays, Col::Time] {
            assert!(matches!(c.width(), Width::Fixed(_)), "{c:?} is fixed");
        }
        for c in [Col::Title, Col::Artist, Col::Album] {
            assert!(
                matches!(c.width(), Width::Share(_)),
                "{c:?} shares the rest"
            );
        }
    }

    #[test]
    fn narrow_windows_drop_the_same_columns_everywhere() {
        for table in TABLES {
            let narrow = table.cols(NARROW - 1.);
            for c in [Col::Album, Col::Year, Col::Plays] {
                assert!(!narrow.contains(&c), "{table:?} keeps {c:?}");
            }
            let kept: Vec<Col> = table
                .cols(WIDE)
                .into_iter()
                .filter(|c| !matches!(c, Col::Album | Col::Year | Col::Plays))
                .collect();
            assert_eq!(narrow, kept, "{table:?}");
            assert_eq!(
                table.cols(NARROW),
                table.cols(WIDE),
                "{table:?} at the edge"
            );
        }
    }

    #[test]
    fn the_sort_cycle_of_each_column_goes_back_to_the_list_s_own_order() {
        for col in COLS {
            let asc = clicked(None, col);
            assert_eq!(
                asc,
                Some(ColSort {
                    col,
                    descending: false
                })
            );
            let desc = clicked(asc, col);
            assert_eq!(
                desc,
                Some(ColSort {
                    col,
                    descending: true
                })
            );
            assert_eq!(clicked(desc, col), None, "{col:?}: the third click");
        }
        // another column starts ascending, whatever the current one
        let by_title = Some(ColSort {
            col: Col::Title,
            descending: true,
        });
        assert_eq!(
            clicked(by_title, Col::Plays).map(|s| s.descending),
            Some(false)
        );
    }

    #[test]
    fn each_column_sorts_both_ways_and_no_sort_is_the_list_s_own_order() {
        let t = list();
        let by = |col, descending| ids(&t, &order(&t, Some(ColSort { col, descending })));
        assert_eq!(ids(&t, &order(&t, None)), ["t0", "t1", "t2", "t3"]);
        assert_eq!(by(Col::Number, false), ["t0", "t1", "t2", "t3"]);
        assert_eq!(by(Col::Number, true), ["t3", "t2", "t1", "t0"]);
        // whatever the case
        assert_eq!(by(Col::Title, false), ["t1", "t2", "t0", "t3"]);
        assert_eq!(by(Col::Title, true), ["t3", "t0", "t2", "t1"]);
        // one artist's tracks by album, then by number
        assert_eq!(by(Col::Artist, false), ["t2", "t3", "t0", "t1"]);
        assert_eq!(by(Col::Artist, true), ["t1", "t3", "t0", "t2"]);
        assert_eq!(by(Col::Album, false), ["t3", "t1", "t2", "t0"]);
        assert_eq!(by(Col::Album, true), ["t2", "t0", "t1", "t3"]);
        assert_eq!(by(Col::Year, false), ["t1", "t0", "t3", "t2"]);
        assert_eq!(by(Col::Year, true), ["t2", "t3", "t0", "t1"]);
        assert_eq!(by(Col::Plays, false), ["t3", "t2", "t0", "t1"]);
        assert_eq!(by(Col::Plays, true), ["t1", "t0", "t2", "t3"]);
        assert_eq!(by(Col::Time, false), ["t1", "t0", "t3", "t2"]);
        assert_eq!(by(Col::Time, true), ["t2", "t3", "t0", "t1"]);
    }

    #[test]
    fn the_library_reads_its_tracks_in_the_order_of_the_header() {
        assert_eq!(library_sort(None), Sort::asc(TrackBy::Artist));
        let s = |col, descending| library_sort(Some(ColSort { col, descending }));
        assert_eq!(s(Col::Title, false), Sort::asc(TrackBy::Title));
        assert_eq!(s(Col::Plays, true), Sort::desc(TrackBy::Plays));
        assert_eq!(s(Col::Time, true), Sort::desc(TrackBy::Duration));
        assert_eq!(s(Col::Year, false), Sort::asc(TrackBy::Year));
        assert_eq!(s(Col::Album, false), Sort::asc(TrackBy::Album));
    }

    #[test]
    fn sorting_a_playlist_changes_the_view_never_the_playlist() {
        // a playlist that holds a track twice
        let mut t = list();
        t.push(t[1].clone());
        let tracks: Arc<[Track]> = t.into();
        let shown = Shown::new(
            tracks.clone(),
            Some(ColSort {
                col: Col::Title,
                descending: false,
            }),
        );
        assert!(Arc::ptr_eq(&shown.tracks, &tracks), "the list is untouched");
        let rows: Vec<(usize, String)> = (0..shown.len())
            .map(|ix| shown.row(ix).map(|(r, t)| (r, t.id.clone())).unwrap())
            .collect();
        assert_eq!(
            rows,
            [
                (1, "t1".into()),
                (4, "t1".into()),
                (2, "t2".into()),
                (0, "t0".into()),
                (3, "t3".into())
            ],
            "each row keeps its rank in the playlist"
        );
        assert!(shown.row(5).is_none());
    }

    #[test]
    fn the_rows_picked_in_a_sorted_playlist_name_their_real_ranks() {
        let here = Place::Playlist("p1".into());
        let tracks: Arc<[Track]> = list().into();
        let shown = Shown::new(
            tracks,
            Some(ColSort {
                col: Col::Title,
                descending: true,
            }),
        );
        // shown: t3, t0, t2, t1; the first and the last rows are picked
        let mut s = Selection::default();
        s.click(&here, 0, How::Toggle);
        s.click(&here, 3, How::Toggle);
        assert_eq!(
            shown.picks(&s, &here),
            [(1, "t1".to_string()), (3, "t3".to_string())]
        );
        // unsorted, a rank is the place shown
        let plain = Shown::new(shown.tracks.clone(), None);
        assert_eq!(
            plain.picks(&s, &here),
            [(0, "t0".to_string()), (3, "t3".to_string())]
        );
    }

    #[test]
    fn a_list_read_sorted_keeps_the_order_it_was_read_in() {
        let tracks: Arc<[Track]> = list().into();
        let sort = Some(ColSort {
            col: Col::Plays,
            descending: true,
        });
        let shown = Shown::read_sorted(tracks, sort);
        assert_eq!(shown.sort, sort);
        assert_eq!(
            shown.row(2).map(|(r, t)| (r, t.id.as_str())),
            Some((2, "t2"))
        );
    }

    #[test]
    fn a_track_found_by_the_search_is_a_row_of_the_table() {
        let hit = Hit {
            kind: melodie_core::search::Kind::Track,
            id: "t9".into(),
            name: "Glass".into(),
            artist: "Ada".into(),
            album: "Moss".into(),
            year: 2003,
            count: 0,
            duration: 181,
            cover: Some("c9".into()),
            starred: false,
            album_id: Some("al9".into()),
            artist_id: Some("ar9".into()),
        };
        let t = track_of(&hit);
        assert_eq!(
            (
                t.id.as_str(),
                t.title.as_str(),
                t.artist.as_str(),
                t.album.as_str()
            ),
            ("t9", "Glass", "Ada", "Moss")
        );
        assert_eq!((t.year, t.duration), (2003, 181));
        assert_eq!(t.album_id.as_deref(), Some("al9"));
        assert_eq!(t.artist_id.as_deref(), Some("ar9"));
        assert_eq!(t.cover.as_deref(), Some("c9"));
    }
}
