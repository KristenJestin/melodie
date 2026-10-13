//! The menu a right click opens on an item, without the window: what it offers for each kind of
//! item and each place, what each choice does, how the keys move in it and where it is drawn.

use crate::browse::Place;
use crate::home::Mix;
use crate::launch::{Adding, Job, Kind};
use melodie_core::library::{self as lib, Album, Artist, Playlist, Track};
use melodie_core::proto::{QueueEntry, Request};
use melodie_core::search::{self, Hit};
use rusqlite::Connection;

/// What was right-clicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A track; its album and artist when the list knows them (a queue entry holds only names).
    Track {
        id: String,
        title: String,
        album: Option<String>,
        artist: Option<String>,
    },
    Album {
        id: String,
        name: String,
        artist: Option<String>,
    },
    Artist {
        id: String,
        name: String,
    },
    /// A playlist; `own` when the user may edit it (said when the menu opens, by the playlists
    /// the window knows).
    Playlist {
        id: String,
        name: String,
        own: bool,
        tracks: usize,
    },
    /// A mix card: the personal mix, the rediscoveries, a genre.
    Mix(Mix),
}

impl Item {
    pub fn track(t: &Track) -> Self {
        Self::Track {
            id: t.id.clone(),
            title: t.title.clone(),
            album: t.album_id.clone(),
            artist: t.artist_id.clone(),
        }
    }

    pub fn entry(e: &QueueEntry) -> Self {
        Self::Track {
            id: e.id.clone(),
            title: e.title.clone(),
            album: None,
            artist: None,
        }
    }

    pub fn album(a: &Album) -> Self {
        Self::Album {
            id: a.id.clone(),
            name: a.name.clone(),
            artist: a.artist_id.clone(),
        }
    }

    pub fn artist(a: &Artist) -> Self {
        Self::Artist {
            id: a.id.clone(),
            name: a.name.clone(),
        }
    }

    pub fn playlist(p: &Playlist) -> Self {
        Self::Playlist {
            id: p.id.clone(),
            name: p.name.clone(),
            own: false,
            tracks: p.songs.max(0) as usize,
        }
    }

    /// A search result.
    pub fn hit(h: &Hit) -> Self {
        let (id, name) = (h.id.clone(), h.name.clone());
        match h.kind {
            search::Kind::Track => Self::Track {
                id,
                title: name,
                album: h.album_id.clone(),
                artist: h.artist_id.clone(),
            },
            search::Kind::Album => Self::Album {
                id,
                name,
                artist: h.artist_id.clone(),
            },
            search::Kind::Artist => Self::Artist { id, name },
            search::Kind::Playlist => Self::Playlist {
                id,
                name,
                own: false,
                tracks: h.count.max(0) as usize,
            },
        }
    }

    /// What `melodie like` takes: a track, an album or an artist.
    pub fn likeable(&self) -> Option<(Kind, &str)> {
        match self {
            Self::Track { id, .. } => Some((Kind::Track, id)),
            Self::Album { id, .. } => Some((Kind::Album, id)),
            Self::Artist { id, .. } => Some((Kind::Artist, id)),
            Self::Playlist { .. } | Self::Mix(_) => None,
        }
    }
}

/// Where the item was right-clicked, as far as the menu cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spot {
    /// A list, a grid, a page, the home, the search, the sidebar.
    Library,
    /// The row at `rank` (from 0) of a playlist of the user's.
    Playlist {
        id: String,
        name: String,
        rank: usize,
    },
    /// The row `ix` of the queue, which held `entry`.
    Queue { ix: usize, entry: u64 },
    /// The track playing, in the player or the player bar.
    Player,
}

/// What the menu is opened on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub item: Item,
    pub spot: Spot,
    /// The place the window shows: a menu does not offer to go where the user is.
    pub here: Place,
    /// The item is a favourite; `None` while unknown (it is then not offered).
    pub starred: Option<bool>,
}

/// A line of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Play,
    Shuffle,
    /// After the current track.
    Next,
    /// At the end of the queue.
    Last,
    Mix,
    /// Opens the list of the user's playlists.
    AddToPlaylist,
    Like,
    Unlike,
    GoToAlbum,
    GoToArtist,
    RemoveFromPlaylist,
    RemoveFromQueue,
    Rename,
    Delete,
}

impl Action {
    pub fn label(self, item: &Item) -> &'static str {
        match self {
            Self::Play => "Play",
            Self::Shuffle => "Shuffle",
            Self::Next => "Play next",
            Self::Last => "Add to queue",
            Self::Mix => "Instant mix",
            Self::AddToPlaylist if matches!(item, Item::Playlist { .. }) => {
                "Add to another playlist"
            }
            Self::AddToPlaylist => "Add to playlist",
            Self::Like => "Like",
            Self::Unlike => "Unlike",
            Self::GoToAlbum => "Go to album",
            Self::GoToArtist => "Go to artist",
            Self::RemoveFromPlaylist => "Remove from this playlist",
            Self::RemoveFromQueue => "Remove from the queue",
            Self::Rename => "Rename…",
            Self::Delete => "Delete…",
        }
    }

    /// The line opens a second list beside it.
    pub fn has_sub(self) -> bool {
        self == Self::AddToPlaylist
    }

    /// Lines of one group stay together; a thin line separates the groups.
    fn group(self) -> u8 {
        match self {
            Self::Play | Self::Shuffle => 0,
            Self::Next | Self::Last | Self::Mix => 1,
            Self::AddToPlaylist | Self::Like | Self::Unlike => 2,
            Self::GoToAlbum | Self::GoToArtist => 3,
            Self::RemoveFromPlaylist | Self::RemoveFromQueue | Self::Rename | Self::Delete => 4,
        }
    }
}

/// The lines of the menu for `t`, in order. What cannot apply is left out, not greyed.
pub fn actions(t: &Target) -> Vec<Action> {
    use Action::*;
    let mut rows = Vec::new();
    let heart = |rows: &mut Vec<Action>| match t.starred {
        Some(true) => rows.push(Unlike),
        Some(false) => rows.push(Like),
        None => {}
    };
    let not_here = |to: Place| t.here != to;
    match &t.item {
        Item::Track { album, artist, .. } => {
            // the track playing is playing already
            if t.spot != Spot::Player {
                rows.push(Play);
            }
            rows.extend([Next, Last, Mix, AddToPlaylist]);
            heart(&mut rows);
            // a queue entry or the track playing holds names only: the copy says which
            let looked_up = matches!(t.spot, Spot::Queue { .. } | Spot::Player);
            if looked_up || album.clone().is_some_and(|a| not_here(Place::Album(a))) {
                rows.push(GoToAlbum);
            }
            if looked_up || artist.clone().is_some_and(|a| not_here(Place::Artist(a))) {
                rows.push(GoToArtist);
            }
            match t.spot {
                Spot::Playlist { .. } => rows.push(RemoveFromPlaylist),
                Spot::Queue { .. } => rows.push(RemoveFromQueue),
                Spot::Library | Spot::Player => {}
            }
        }
        Item::Album { artist, .. } => {
            rows.extend([Play, Shuffle, Next, Last, Mix, AddToPlaylist]);
            heart(&mut rows);
            if artist.clone().is_some_and(|a| not_here(Place::Artist(a))) {
                rows.push(GoToArtist);
            }
        }
        Item::Artist { .. } => {
            rows.extend([Play, Shuffle, Next, Last, Mix, AddToPlaylist]);
            heart(&mut rows);
        }
        Item::Playlist { own, .. } => {
            rows.extend([Play, Shuffle, Next, Last, AddToPlaylist]);
            if *own {
                rows.extend([Rename, Delete]);
            }
        }
        // `melodie mix` plays or adds at the end: there is no "next" for a mix
        Item::Mix(_) => rows.extend([Play, Last]),
    }
    rows
}

/// A separator goes before line `ix`.
pub fn separated(rows: &[Action], ix: usize) -> bool {
    ix > 0 && rows[ix - 1].group() != rows[ix].group()
}

/// What is added to a playlist: ready, or the tracks of an artist or a playlist, read from the
/// local copy when the playlist is chosen (`melodie playlist add` takes tracks or an album).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Ready(Adding),
    Artist(String),
    Playlist(String),
}

impl Source {
    /// What goes in, read from the local copy when it must be. Blocking: away from the window.
    pub fn read(&self, conn: &Connection) -> anyhow::Result<Adding> {
        let tracks: Vec<String> = match self {
            Self::Ready(what) => return Ok(what.clone()),
            Self::Artist(id) => {
                let mut ids = Vec::new();
                for album in lib::artist_albums(conn, id)? {
                    ids.extend(
                        lib::album_tracks(conn, &album.id)?
                            .into_iter()
                            .map(|t| t.id),
                    );
                }
                ids
            }
            Self::Playlist(id) => lib::playlist_tracks(conn, id)?
                .into_iter()
                .map(|t| t.id)
                .collect(),
        };
        anyhow::ensure!(!tracks.is_empty(), "nothing to add");
        Ok(Adding::Tracks(tracks))
    }
}

/// What a line does.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// A job of `melodie`.
    Run(Job),
    /// A request to the daemon about row `ix` of the queue, sent only while it holds `entry`.
    Row {
        ix: usize,
        entry: u64,
        req: Request,
    },
    Go(Place),
    /// The page of the album (or the artist) of a track the list knows only by its id.
    GoOfTrack {
        id: String,
        artist: bool,
    },
    /// A change of a playlist, and what a success says.
    Edit {
        job: Job,
        said: String,
    },
    Rename {
        id: String,
        name: String,
    },
    Delete {
        id: String,
        name: String,
        tracks: usize,
    },
    /// Adds to the playlist `to` (its id and name), or to a new one when `None`.
    Add {
        to: Option<(String, String)>,
        what: Source,
    },
}

/// What `action` does on `t`. Adding to a playlist waits for the playlist: see `add_to`.
pub fn effect(action: Action, t: &Target) -> Option<Effect> {
    use Action::*;
    // a row of the queue is played and removed where it is, while it still holds its entry
    if let Spot::Queue { ix, entry } = t.spot {
        let req = match action {
            Play => Some(Request::QueuePlay { index: ix }),
            RemoveFromQueue => Some(Request::QueueRemove { index: ix }),
            _ => None,
        };
        if let Some(req) = req {
            return Some(Effect::Row { ix, entry, req });
        }
    }
    if let Item::Mix(m) = &t.item {
        return match action {
            Play => Some(Effect::Run(m.job())),
            Last => Some(Effect::Run(Job::QueueMix(Box::new(m.job())))),
            _ => None,
        };
    }
    let (kind, id) = match &t.item {
        Item::Track { id, .. } => (Kind::Track, id),
        Item::Album { id, .. } => (Kind::Album, id),
        Item::Artist { id, .. } => (Kind::Artist, id),
        Item::Playlist { id, .. } => (Kind::Playlist, id),
        Item::Mix(_) => return None,
    };
    let id = id.clone();
    let run = |job| Some(Effect::Run(job));
    match action {
        Play | Shuffle => run(Job::Play {
            kind,
            id,
            shuffle: action == Shuffle,
        }),
        Next | Last => run(Job::Queue {
            kind,
            id,
            next: action == Next,
        }),
        Mix => run(Job::Mix { kind, id }),
        Like | Unlike => run(Job::Star {
            kind,
            id,
            starred: action == Like,
        }),
        GoToAlbum | GoToArtist => {
            let artist = action == GoToArtist;
            let known = match &t.item {
                Item::Track {
                    album, artist: a, ..
                } => {
                    if artist {
                        a.clone()
                    } else {
                        album.clone()
                    }
                }
                Item::Album { artist: a, .. } if artist => a.clone(),
                _ => None,
            };
            match known {
                Some(to) if artist => Some(Effect::Go(Place::Artist(to))),
                Some(to) => Some(Effect::Go(Place::Album(to))),
                None if kind == Kind::Track => Some(Effect::GoOfTrack { id, artist }),
                None => None,
            }
        }
        RemoveFromPlaylist => match (&t.spot, &t.item) {
            (
                Spot::Playlist {
                    id: pid,
                    name,
                    rank,
                },
                Item::Track { title, .. },
            ) => Some(Effect::Edit {
                job: Job::RemoveFromPlaylist {
                    id: pid.clone(),
                    picks: vec![(*rank, id)],
                },
                said: format!("Removed {title} from {name}"),
            }),
            _ => None,
        },
        Rename | Delete => match &t.item {
            Item::Playlist { name, .. } if action == Rename => Some(Effect::Rename {
                id,
                name: name.clone(),
            }),
            Item::Playlist { name, tracks, .. } => Some(Effect::Delete {
                id,
                name: name.clone(),
                tracks: *tracks,
            }),
            _ => None,
        },
        AddToPlaylist | RemoveFromQueue => None,
    }
}

/// What choosing a playlist (`None`: "New playlist…") in the list beside "Add to playlist" does.
pub fn add_to(t: &Target, to: Option<(String, String)>) -> Option<Effect> {
    let what = match &t.item {
        Item::Track { id, .. } => Source::Ready(Adding::Tracks(vec![id.clone()])),
        Item::Album { id, .. } => Source::Ready(Adding::Album(id.clone())),
        Item::Artist { id, .. } => Source::Artist(id.clone()),
        Item::Playlist { id, .. } => Source::Playlist(id.clone()),
        Item::Mix(_) => return None,
    };
    Some(Effect::Add { to, what })
}

/// The playlists the item may be added to: the user's, but itself.
pub fn playlists_for(t: &Target, own: &[Playlist]) -> Vec<(String, String)> {
    let itself = match &t.item {
        Item::Playlist { id, .. } => Some(id.as_str()),
        _ => None,
    };
    own.iter()
        .filter(|p| Some(p.id.as_str()) != itself)
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect()
}

/// The keys that move in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
}

impl MenuKey {
    /// `key` as GPUI names it.
    pub fn read(key: &str) -> Option<Self> {
        Some(match key {
            "up" => Self::Up,
            "down" => Self::Down,
            "left" => Self::Left,
            "right" => Self::Right,
            "enter" => Self::Enter,
            "escape" => Self::Escape,
            _ => return None,
        })
    }
}

/// What a key or a click picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chosen {
    Row(usize),
    /// A line of the list beside: 0 is "New playlist…", then the playlists.
    Sub(usize),
    Close,
}

/// The highlighted line, the list beside it open or not, and its highlighted line. The pointer
/// and the keys move the same highlight.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Focus {
    pub row: Option<usize>,
    pub sub_open: bool,
    pub sub: Option<usize>,
}

impl Focus {
    /// A key: moves the highlight, or says what was picked. `sub_len` counts "New playlist…".
    pub fn key(&mut self, key: MenuKey, rows: &[Action], sub_len: usize) -> Option<Chosen> {
        let step = |at: Option<usize>, len: usize, down: bool| match (at, down) {
            _ if len == 0 => None,
            (None, true) => Some(0),
            (None, false) => Some(len - 1),
            (Some(i), true) => Some((i + 1) % len),
            (Some(i), false) => Some((i + len - 1) % len),
        };
        let has_sub = self
            .row
            .and_then(|i| rows.get(i))
            .is_some_and(|a| a.has_sub());
        match key {
            // the list beside first, as Left does; then the menu
            MenuKey::Escape if self.sub_open => {
                self.sub_open = false;
                self.sub = None;
            }
            MenuKey::Escape => return Some(Chosen::Close),
            MenuKey::Up | MenuKey::Down if self.sub_open => {
                self.sub = step(self.sub, sub_len, key == MenuKey::Down)
            }
            MenuKey::Up | MenuKey::Down => {
                self.row = step(self.row, rows.len(), key == MenuKey::Down)
            }
            MenuKey::Right | MenuKey::Enter if has_sub && !self.sub_open => {
                self.sub_open = true;
                self.sub = Some(0);
            }
            MenuKey::Left => {
                self.sub_open = false;
                self.sub = None;
            }
            MenuKey::Enter if self.sub_open => return self.sub.map(Chosen::Sub),
            MenuKey::Enter => return self.row.map(Chosen::Row),
            MenuKey::Right => {}
        }
        None
    }

    /// The pointer rests on line `ix`: the list beside opens on the line that has one.
    pub fn hover_row(&mut self, ix: usize, rows: &[Action]) {
        if self.row == Some(ix) && self.sub_open {
            return;
        }
        self.row = Some(ix);
        self.sub_open = rows.get(ix).is_some_and(|a| a.has_sub());
        self.sub = None;
    }

    pub fn hover_sub(&mut self, ix: usize) {
        self.sub = Some(ix);
    }
}

/// The height of a line, of a separator, the room above and below the lines, the widths.
pub const ROW: f32 = 32.0;
pub const SEPARATOR: f32 = 9.0;
pub const PAD: f32 = 6.0;
pub const WIDTH: f32 = 232.0;
pub const SUB_WIDTH: f32 = 248.0;
/// The list beside shows this many lines, then scrolls.
pub const SUB_ROWS: usize = 10;
/// The room kept between a menu and the window's edge.
pub const MARGIN: f32 = 8.0;

/// Where line `ix` starts, from the top of the menu.
pub fn row_top(rows: &[Action], ix: usize) -> f32 {
    let separators = (0..=ix.min(rows.len().saturating_sub(1)))
        .filter(|&i| separated(rows, i))
        .count();
    PAD + ix as f32 * ROW + separators as f32 * SEPARATOR
}

pub fn height(rows: &[Action]) -> f32 {
    match rows.len() {
        0 => 2. * PAD,
        n => row_top(rows, n - 1) + ROW + PAD,
    }
}

pub fn sub_height(len: usize) -> f32 {
    2. * PAD + len.min(SUB_ROWS) as f32 * ROW
}

/// Where a menu of `size` opened at the pointer `at` goes in a window of `window`: below and
/// right of the pointer, on the other side of it when there is no room, always inside.
pub fn place(at: (f32, f32), size: (f32, f32), window: (f32, f32)) -> (f32, f32) {
    let axis = |at: f32, len: f32, room: f32| {
        let at = if at + len > room - MARGIN && at - len >= MARGIN {
            at - len
        } else {
            at
        };
        at.min(room - MARGIN - len)
            .max(MARGIN.min(room - len).max(0.))
    };
    (axis(at.0, size.0, window.0), axis(at.1, size.1, window.1))
}

/// Where the list beside a line goes: right of the menu `menu` (its left, top and width), its
/// first line level with the line at `row_top`; on the left when there is no room; inside.
pub fn place_sub(
    menu: (f32, f32, f32),
    row_top: f32,
    size: (f32, f32),
    window: (f32, f32),
) -> (f32, f32) {
    let (left, top, width) = menu;
    // a little over the menu's edge, so that the pointer crosses no gap
    let right = left + width - 4.;
    let x = if right + size.0 <= window.0 - MARGIN {
        right
    } else {
        (left - size.0 + 4.).max(MARGIN)
    };
    let y = (top + row_top - PAD)
        .min(window.1 - MARGIN - size.1)
        .max(MARGIN);
    (x, y)
}

/// The list a row sits in, as drawn: where its viewport is in the window and how far it is
/// scrolled (zero or negative, as GPUI keeps it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListView {
    pub left: f32,
    pub top: f32,
    pub height: f32,
    pub scrolled: f32,
    pub row: f32,
}

/// How far from the list's left edge a menu opened by a key starts: past the number and the cover.
pub const ROW_INSET: f32 = 48.;

/// Where the menu of row `ix` opens when a key asks for it (it has no pointer): level with the
/// middle of the row; at the top of the list when the row is scrolled out of sight.
pub fn row_anchor(list: &ListView, ix: usize) -> (f32, f32) {
    let top = list.top + list.scrolled + ix as f32 * list.row;
    let whole = top >= list.top && top + list.row <= list.top + list.height;
    let top = if whole { top } else { list.top };
    (list.left + ROW_INSET, top + list.row / 2.)
}

/// The height the menu takes in a window of `window_h`: its own, or the window's less the
/// margins, and then it scrolls.
pub fn capped(height: f32, window_h: f32) -> f32 {
    height
        .min(window_h - 2. * MARGIN)
        .max((ROW + 2. * PAD).min(height))
}

/// The position of line `ix` among the menu's children, which the separators are part of.
pub fn child_index(rows: &[Action], ix: usize) -> usize {
    ix + (0..=ix.min(rows.len().saturating_sub(1)))
        .filter(|&i| separated(rows, i))
        .count()
}

/// The menus asked for and not shown yet (their look-up is still running): an answer that
/// comes after a newer ask or a cancel opens nothing.
#[derive(Debug, Default)]
pub struct Asks {
    last: u64,
    pending: bool,
}

impl Asks {
    /// A new ask, which makes the previous ones stale. Its ticket comes back with the answer.
    pub fn ask(&mut self) -> u64 {
        self.last += 1;
        self.pending = true;
        self.last
    }

    /// Escape, a click or a page change: whatever is pending opens nothing.
    pub fn cancel(&mut self) {
        self.last += 1;
        self.pending = false;
    }

    /// The answer of `ticket` is in: `true` when the menu may still open.
    pub fn answer(&mut self, ticket: u64) -> bool {
        let wanted = self.pending && ticket == self.last;
        if wanted {
            self.pending = false;
        }
        wanted
    }

    pub fn pending(&self) -> bool {
        self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(item: Item, spot: Spot) -> Target {
        Target {
            item,
            spot,
            here: Place::Home,
            starred: Some(false),
        }
    }

    fn track() -> Item {
        Item::Track {
            id: "-t1".into(),
            title: "Rain".into(),
            album: Some("al1".into()),
            artist: Some("ar1".into()),
        }
    }

    fn album() -> Item {
        Item::Album {
            id: "al1".into(),
            name: "Blue".into(),
            artist: Some("ar1".into()),
        }
    }

    fn artist() -> Item {
        Item::Artist {
            id: "ar1".into(),
            name: "Ana".into(),
        }
    }

    fn playlist(own: bool) -> Item {
        Item::Playlist {
            id: "p1".into(),
            name: "Road".into(),
            own,
            tracks: 12,
        }
    }

    use Action::{
        AddToPlaylist, Delete, GoToAlbum, GoToArtist, Last, Like, Next, Play, RemoveFromPlaylist,
        RemoveFromQueue, Rename, Shuffle, Unlike,
    };

    #[test]
    fn a_track_offers_play_the_queue_a_mix_a_playlist_its_heart_and_its_pages() {
        let t = target(track(), Spot::Library);
        assert_eq!(
            actions(&t),
            [
                Play,
                Next,
                Last,
                Action::Mix,
                AddToPlaylist,
                Like,
                GoToAlbum,
                GoToArtist
            ]
        );
        let liked = Target {
            starred: Some(true),
            ..t.clone()
        };
        assert!(actions(&liked).contains(&Unlike));
        assert!(!actions(&liked).contains(&Like));
    }

    #[test]
    fn a_heart_not_known_yet_is_not_offered() {
        let t = Target {
            starred: None,
            ..target(track(), Spot::Library)
        };
        let rows = actions(&t);
        assert!(!rows.contains(&Like) && !rows.contains(&Unlike));
    }

    #[test]
    fn a_track_in_a_playlist_of_the_users_can_be_removed_from_it() {
        let spot = Spot::Playlist {
            id: "p1".into(),
            name: "Road".into(),
            rank: 3,
        };
        let t = Target {
            here: Place::Playlist("p1".into()),
            ..target(track(), spot)
        };
        assert_eq!(actions(&t).last(), Some(&RemoveFromPlaylist));
        assert!(!actions(&target(track(), Spot::Library)).contains(&RemoveFromPlaylist));
    }

    #[test]
    fn a_track_in_the_queue_can_be_removed_from_it_and_its_pages_are_looked_up() {
        let entry = Item::Track {
            id: "t1".into(),
            title: "Rain".into(),
            album: None,
            artist: None,
        };
        let t = target(entry, Spot::Queue { ix: 4, entry: 9 });
        let rows = actions(&t);
        assert_eq!(rows.last(), Some(&RemoveFromQueue));
        // a queue entry holds names only: the local copy says which album and artist
        assert!(rows.contains(&GoToAlbum) && rows.contains(&GoToArtist));
    }

    #[test]
    fn the_track_playing_is_not_offered_to_play() {
        let rows = actions(&target(track(), Spot::Player));
        assert!(!rows.contains(&Play));
        assert_eq!(rows[0], Next);
        assert!(!rows.contains(&RemoveFromQueue));
    }

    #[test]
    fn a_menu_does_not_offer_to_go_where_the_user_is() {
        let on_album = Target {
            here: Place::Album("al1".into()),
            ..target(track(), Spot::Library)
        };
        let rows = actions(&on_album);
        assert!(!rows.contains(&GoToAlbum));
        assert!(rows.contains(&GoToArtist));
        let on_artist = Target {
            here: Place::Artist("ar1".into()),
            ..target(album(), Spot::Library)
        };
        assert!(!actions(&on_artist).contains(&GoToArtist));
        // without an id there is nowhere to go
        let lone = Item::Track {
            id: "t1".into(),
            title: "Rain".into(),
            album: None,
            artist: None,
        };
        let rows = actions(&target(lone, Spot::Library));
        assert!(!rows.contains(&GoToAlbum) && !rows.contains(&GoToArtist));
    }

    #[test]
    fn an_album_offers_play_shuffle_the_queue_a_mix_a_playlist_its_heart_and_its_artist() {
        assert_eq!(
            actions(&target(album(), Spot::Library)),
            [
                Play,
                Shuffle,
                Next,
                Last,
                Action::Mix,
                AddToPlaylist,
                Like,
                GoToArtist
            ]
        );
    }

    #[test]
    fn an_artist_offers_play_shuffle_the_queue_a_mix_a_playlist_and_its_heart() {
        assert_eq!(
            actions(&target(artist(), Spot::Library)),
            [Play, Shuffle, Next, Last, Action::Mix, AddToPlaylist, Like]
        );
    }

    #[test]
    fn a_playlist_of_the_users_can_be_renamed_and_deleted() {
        assert_eq!(
            actions(&target(playlist(true), Spot::Library)),
            [Play, Shuffle, Next, Last, AddToPlaylist, Rename, Delete]
        );
        // another person's is only read
        assert_eq!(
            actions(&target(playlist(false), Spot::Library)),
            [Play, Shuffle, Next, Last, AddToPlaylist]
        );
    }

    #[test]
    fn a_genre_or_a_mix_card_plays_or_goes_at_the_end_of_the_queue() {
        // `melodie mix` adds at the end only: no "play next"
        for m in [
            Mix::Mine,
            Mix::Rediscover,
            Mix::Genre {
                name: "Jazz".into(),
                tracks: 40,
            },
        ] {
            assert_eq!(actions(&target(Item::Mix(m), Spot::Library)), [Play, Last]);
        }
    }

    #[test]
    fn the_groups_are_separated() {
        let rows = actions(&target(album(), Spot::Library));
        let at: Vec<usize> = (0..rows.len()).filter(|&i| separated(&rows, i)).collect();
        // play | queue, mix | playlist, heart | artist
        assert_eq!(at, [2, 5, 7]);
    }

    #[test]
    fn each_action_starts_its_job() {
        let t = target(track(), Spot::Library);
        let run = |a: Action, t: &Target| match effect(a, t) {
            Some(Effect::Run(job)) => job.args(),
            other => panic!("{a:?}: {other:?}"),
        };
        assert_eq!(run(Play, &t), ["play", "track", "--", "-t1"]);
        assert_eq!(run(Next, &t), ["queue", "track", "--next", "--", "-t1"]);
        assert_eq!(run(Last, &t), ["queue", "track", "--", "-t1"]);
        assert_eq!(run(Action::Mix, &t), ["mix", "track", "--", "-t1"]);
        assert_eq!(run(Like, &t), ["like", "track", "--", "-t1"]);
        assert_eq!(run(Unlike, &t), ["unlike", "track", "--", "-t1"]);
        let a = target(album(), Spot::Library);
        assert_eq!(
            run(Shuffle, &a),
            ["play", "album", "--shuffle", "--", "al1"]
        );
        assert_eq!(run(Like, &a), ["like", "album", "--", "al1"]);
        let ar = target(artist(), Spot::Library);
        assert_eq!(run(Next, &ar), ["queue", "artist", "--next", "--", "ar1"]);
        assert_eq!(run(Action::Mix, &ar), ["mix", "artist", "--", "ar1"]);
        assert_eq!(run(Unlike, &ar), ["unlike", "artist", "--", "ar1"]);
        let p = target(playlist(true), Spot::Library);
        assert_eq!(run(Last, &p), ["queue", "playlist", "--", "p1"]);
        let genre = target(
            Item::Mix(Mix::Genre {
                name: "Jazz".into(),
                tracks: 40,
            }),
            Spot::Library,
        );
        assert_eq!(run(Play, &genre), ["mix", "genre", "--", "Jazz"]);
        assert_eq!(run(Last, &genre), ["mix", "--queue", "genre", "--", "Jazz"]);
        let mine = target(Item::Mix(Mix::Mine), Spot::Library);
        assert_eq!(run(Play, &mine), ["mix", "me"]);
        assert_eq!(run(Last, &mine), ["mix", "--queue", "me"]);
    }

    #[test]
    fn each_action_on_a_page_a_dialog_or_a_row_does_its_part() {
        let t = target(track(), Spot::Library);
        assert_eq!(
            effect(GoToAlbum, &t),
            Some(Effect::Go(Place::Album("al1".into())))
        );
        assert_eq!(
            effect(GoToArtist, &t),
            Some(Effect::Go(Place::Artist("ar1".into())))
        );
        assert_eq!(effect(AddToPlaylist, &t), None, "the playlist comes first");
        // in the queue: the row, checked to hold the entry still
        let q = target(track(), Spot::Queue { ix: 4, entry: 9 });
        assert_eq!(
            effect(Play, &q),
            Some(Effect::Row {
                ix: 4,
                entry: 9,
                req: Request::QueuePlay { index: 4 }
            })
        );
        assert_eq!(
            effect(RemoveFromQueue, &q),
            Some(Effect::Row {
                ix: 4,
                entry: 9,
                req: Request::QueueRemove { index: 4 }
            })
        );
        let entry = Item::Track {
            id: "t1".into(),
            title: "Rain".into(),
            album: None,
            artist: None,
        };
        let p = target(entry, Spot::Player);
        assert_eq!(
            effect(GoToArtist, &p),
            Some(Effect::GoOfTrack {
                id: "t1".into(),
                artist: true
            })
        );
        // a row of a playlist, by its rank and the id seen there
        let spot = Spot::Playlist {
            id: "p1".into(),
            name: "Road".into(),
            rank: 3,
        };
        assert_eq!(
            effect(RemoveFromPlaylist, &target(track(), spot)),
            Some(Effect::Edit {
                job: Job::RemoveFromPlaylist {
                    id: "p1".into(),
                    picks: vec![(3, "-t1".into())]
                },
                said: "Removed Rain from Road".into()
            })
        );
        let pl = target(playlist(true), Spot::Library);
        assert_eq!(
            effect(Rename, &pl),
            Some(Effect::Rename {
                id: "p1".into(),
                name: "Road".into()
            })
        );
        assert_eq!(
            effect(Delete, &pl),
            Some(Effect::Delete {
                id: "p1".into(),
                name: "Road".into(),
                tracks: 12
            })
        );
    }

    #[test]
    fn adding_to_a_playlist_takes_the_track_the_album_or_what_the_copy_holds() {
        let to = Some(("p2".to_string(), "Night".to_string()));
        let add = |item: Item| add_to(&target(item, Spot::Library), to.clone());
        assert_eq!(
            add(track()),
            Some(Effect::Add {
                to: to.clone(),
                what: Source::Ready(Adding::Tracks(vec!["-t1".into()])),
            })
        );
        assert_eq!(
            add(album()),
            Some(Effect::Add {
                to: to.clone(),
                what: Source::Ready(Adding::Album("al1".into())),
            })
        );
        assert!(matches!(
            add(artist()),
            Some(Effect::Add { what: Source::Artist(id), .. }) if id == "ar1"
        ));
        assert!(matches!(
            add(playlist(false)),
            Some(Effect::Add { what: Source::Playlist(id), .. }) if id == "p1"
        ));
        // a new one
        assert!(matches!(
            add_to(&target(track(), Spot::Library), None),
            Some(Effect::Add { to: None, .. })
        ));
        assert_eq!(add(Item::Mix(Mix::Mine)), None);
    }

    #[test]
    fn a_playlist_is_not_offered_to_itself() {
        let own = |id: &str, name: &str| Playlist {
            id: id.into(),
            name: name.into(),
            ..Playlist::default()
        };
        let lists = [own("p1", "Road"), own("p2", "Night")];
        assert_eq!(
            playlists_for(&target(playlist(true), Spot::Library), &lists),
            [("p2".to_string(), "Night".to_string())]
        );
        assert_eq!(
            playlists_for(&target(track(), Spot::Library), &lists).len(),
            2
        );
    }

    #[test]
    fn the_tracks_of_an_artist_or_a_playlist_are_read_from_the_copy() {
        use melodie_core::db;
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let mut conn = db::create(&dir.path().join("library.db")).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_artist(&tx, &json!({"id":"ar1","name":"Ana","albumCount":2})).unwrap();
        for (al, year) in [("al1", 2001), ("al2", 1999)] {
            db::insert_album(
                &tx,
                &json!({"id":al,"name":al,"artist":"Ana","artistId":"ar1","year":year}),
            )
            .unwrap();
        }
        for (t, al, n) in [("t1", "al1", 2), ("t2", "al1", 1), ("t3", "al2", 1)] {
            db::insert_track(
                &tx,
                &json!({"id":t,"title":t,"albumId":al,"artistId":"ar1","track":n}),
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let Adding::Tracks(ids) = Source::Artist("ar1".into()).read(&conn).unwrap() else {
            panic!("tracks")
        };
        // album by album, the latest first as on the artist's page, each in its order
        assert_eq!(ids, ["t2", "t1", "t3"]);
        assert_eq!(
            Source::Ready(Adding::Album("al1".into()))
                .read(&conn)
                .unwrap(),
            Adding::Album("al1".into())
        );
        assert!(
            Source::Artist("nobody".into()).read(&conn).is_err(),
            "nothing to add is said, not sent"
        );
    }

    #[test]
    fn arrows_move_the_highlight_round_and_enter_picks() {
        let rows = [Play, Next, AddToPlaylist, Like];
        let mut f = Focus::default();
        assert_eq!(f.key(MenuKey::Down, &rows, 3), None);
        assert_eq!(f.row, Some(0));
        f.key(MenuKey::Up, &rows, 3);
        assert_eq!(f.row, Some(3), "round to the last");
        f.key(MenuKey::Down, &rows, 3);
        assert_eq!(f.row, Some(0));
        f.key(MenuKey::Down, &rows, 3);
        assert_eq!(f.key(MenuKey::Enter, &rows, 3), Some(Chosen::Row(1)));
        let mut none = Focus::default();
        assert_eq!(
            none.key(MenuKey::Enter, &rows, 3),
            None,
            "nothing highlighted"
        );
        assert_eq!(none.key(MenuKey::Escape, &rows, 3), Some(Chosen::Close));
    }

    #[test]
    fn the_right_arrow_opens_the_playlists_and_the_left_one_closes_them() {
        let rows = [Play, AddToPlaylist, Like];
        let mut f = Focus {
            row: Some(0),
            ..Focus::default()
        };
        f.key(MenuKey::Right, &rows, 3);
        assert!(!f.sub_open, "play has no list beside it");
        f.key(MenuKey::Down, &rows, 3);
        f.key(MenuKey::Right, &rows, 3);
        assert!(f.sub_open);
        assert_eq!(f.sub, Some(0));
        f.key(MenuKey::Down, &rows, 3);
        f.key(MenuKey::Down, &rows, 3);
        f.key(MenuKey::Down, &rows, 3);
        assert_eq!(f.sub, Some(0), "round in the list beside");
        f.key(MenuKey::Down, &rows, 3);
        assert_eq!(f.key(MenuKey::Enter, &rows, 3), Some(Chosen::Sub(1)));
        f.key(MenuKey::Left, &rows, 3);
        assert!(!f.sub_open);
        assert_eq!(f.row, Some(1));
        // Enter on the line opens its list too
        assert_eq!(f.key(MenuKey::Enter, &rows, 3), None);
        assert!(f.sub_open);
    }

    #[test]
    fn the_pointer_opens_the_list_beside_its_line_and_closes_it_elsewhere() {
        let rows = [Play, AddToPlaylist, Like];
        let mut f = Focus::default();
        f.hover_row(1, &rows);
        assert!(f.sub_open);
        assert_eq!(f.sub, None, "the pointer has not gone in yet");
        f.hover_sub(2);
        assert_eq!((f.row, f.sub), (Some(1), Some(2)));
        f.hover_row(2, &rows);
        assert!(!f.sub_open);
        assert_eq!(f.row, Some(2));
    }

    #[test]
    fn keys_are_read_by_their_names() {
        assert_eq!(MenuKey::read("up"), Some(MenuKey::Up));
        assert_eq!(MenuKey::read("down"), Some(MenuKey::Down));
        assert_eq!(MenuKey::read("left"), Some(MenuKey::Left));
        assert_eq!(MenuKey::read("right"), Some(MenuKey::Right));
        assert_eq!(MenuKey::read("enter"), Some(MenuKey::Enter));
        assert_eq!(MenuKey::read("escape"), Some(MenuKey::Escape));
        assert_eq!(MenuKey::read("q"), None);
    }

    #[test]
    fn the_height_counts_the_lines_and_the_separators() {
        let rows = [Play, Shuffle, Next];
        assert_eq!(height(&rows), 2. * PAD + 3. * ROW + SEPARATOR);
        assert_eq!(row_top(&rows, 0), PAD);
        assert_eq!(row_top(&rows, 2), PAD + 2. * ROW + SEPARATOR);
        assert_eq!(sub_height(3), 2. * PAD + 3. * ROW);
        assert_eq!(
            sub_height(40),
            2. * PAD + SUB_ROWS as f32 * ROW,
            "then it scrolls"
        );
    }

    #[test]
    fn the_menu_opens_at_the_pointer_and_stays_inside_the_window() {
        let window = (1000., 700.);
        let size = (WIDTH, 300.);
        assert_eq!(place((100., 100.), size, window), (100., 100.));
        // no room on the right or below: on the other side of the pointer
        assert_eq!(place((900., 650.), size, window), (900. - WIDTH, 350.));
        // no room on either side: against the edge, never outside
        let (x, y) = place((100., 300.), (WIDTH, 690.), window);
        assert_eq!((x, y), (100., MARGIN));
        let tiny = (200., 150.);
        let (x, y) = place((10., 10.), size, tiny);
        assert!(
            x >= 0. && y >= 0.,
            "a window smaller than the menu keeps its corner"
        );
    }

    #[test]
    fn the_list_beside_goes_right_of_the_menu_or_left_when_there_is_no_room() {
        let window = (1000., 700.);
        let size = (SUB_WIDTH, 200.);
        assert_eq!(
            place_sub((100., 100., WIDTH), 70., size, window),
            (100. + WIDTH - 4., 100. + 70. - PAD)
        );
        let (x, _) = place_sub((800., 100., WIDTH), 70., size, window);
        assert_eq!(x, 800. - SUB_WIDTH + 4.);
        // near the bottom it goes up to stay inside
        let (_, y) = place_sub((100., 600., WIDTH), 70., size, window);
        assert_eq!(y, 700. - MARGIN - 200.);
    }

    #[test]
    fn escape_closes_the_list_beside_first_and_the_menu_the_second_time() {
        let rows = [Play, AddToPlaylist, Like];
        let mut f = Focus {
            row: Some(1),
            ..Focus::default()
        };
        f.key(MenuKey::Right, &rows, 3);
        assert!(f.sub_open);
        assert_eq!(f.key(MenuKey::Escape, &rows, 3), None);
        assert!(!f.sub_open, "the list beside is closed, as Left does");
        assert_eq!(f.row, Some(1), "the line stays highlighted");
        assert_eq!(f.key(MenuKey::Escape, &rows, 3), Some(Chosen::Close));
    }

    #[test]
    fn a_menu_asked_by_a_key_opens_level_with_its_row() {
        let list = ListView {
            left: 200.,
            top: 100.,
            height: 400.,
            scrolled: 0.,
            row: 40.,
        };
        assert_eq!(
            row_anchor(&list, 0),
            (200. + ROW_INSET, 100. + 20.),
            "the middle of the first row"
        );
        assert_eq!(
            row_anchor(&list, 3),
            (200. + ROW_INSET, 100. + 3. * 40. + 20.)
        );
        // scrolled two rows and a half down: the row moves up with the list
        let scrolled = ListView {
            scrolled: -100.,
            ..list
        };
        assert_eq!(
            row_anchor(&scrolled, 5),
            (200. + ROW_INSET, 100. + 200. - 100. + 20.)
        );
        // the last row that fits whole is still there
        assert_eq!(row_anchor(&list, 9), (200. + ROW_INSET, 100. + 360. + 20.));
    }

    #[test]
    fn a_row_out_of_sight_has_its_menu_at_the_top_of_the_list() {
        let list = ListView {
            left: 200.,
            top: 100.,
            height: 400.,
            scrolled: -400.,
            row: 40.,
        };
        let top = (200. + ROW_INSET, 100. + 20.);
        assert_eq!(row_anchor(&list, 2), top, "above the viewport");
        assert_eq!(row_anchor(&list, 30), top, "below the viewport");
        // cut by the edge: not whole, so not there
        assert_eq!(row_anchor(&list, 9), top, "cut at the top");
        assert_eq!(row_anchor(&list, 20), top, "below the last whole row");
        assert_eq!(
            row_anchor(&list, 11),
            (200. + ROW_INSET, 100. + 40. + 20.),
            "a whole row, second of the viewport"
        );
        assert_eq!(
            row_anchor(&list, 19),
            (200. + ROW_INSET, 100. + 360. + 20.),
            "the last whole row"
        );
    }

    #[test]
    fn a_menu_taller_than_the_window_is_capped_and_scrolls() {
        assert_eq!(capped(300., 700.), 300.);
        assert_eq!(capped(300., 300. + 2. * MARGIN), 300.);
        assert_eq!(capped(300., 200.), 200. - 2. * MARGIN);
        assert_eq!(capped(300., 20.), ROW + 2. * PAD, "never less than a line");
        // so it fits: opened at the pointer, it is against the margin, whole
        let (_, y) = place((100., 100.), (WIDTH, capped(300., 200.)), (800., 200.));
        assert_eq!(y, MARGIN);
    }

    #[test]
    fn a_line_is_found_among_the_separators() {
        let rows = [Play, Shuffle, Next, Last, AddToPlaylist, Like, GoToArtist];
        // groups: [Play, Shuffle] | [Next, Last] | [AddToPlaylist, Like] | [GoToArtist]
        assert_eq!(child_index(&rows, 0), 0);
        assert_eq!(child_index(&rows, 1), 1);
        assert_eq!(child_index(&rows, 2), 3, "after the first separator");
        assert_eq!(child_index(&rows, 4), 6);
        assert_eq!(child_index(&rows, 6), 9, "after three separators");
    }

    #[test]
    fn a_look_up_answered_after_a_cancel_or_a_newer_ask_opens_nothing() {
        let mut asks = Asks::default();
        let a = asks.ask();
        assert!(asks.pending());
        assert!(asks.answer(a), "nothing happened meanwhile");
        assert!(!asks.pending());
        assert!(!asks.answer(a), "an answer counts once");

        let b = asks.ask();
        asks.cancel();
        assert!(!asks.pending());
        assert!(
            !asks.answer(b),
            "Escape, a click or a page change came first"
        );

        let c = asks.ask();
        let d = asks.ask();
        assert!(!asks.answer(c), "a newer menu was asked for");
        assert!(asks.pending(), "the newer one is still being looked up");
        assert!(asks.answer(d));
    }
}
