//! The library as the window shows it: where the user is, what each view lists, the covers on
//! disk. Everything is read from the local copy away from the window's thread; a view shows what
//! it had while the new answer comes.

use crate::app::Shared;
use crate::cover_cache::CoverCache;
use gpui_kit::*;
use melodie_core::db;
use melodie_core::env::Env;
use melodie_core::library::{
    self as lib, Album, AlbumBy, Artist, ArtistBy, Playlist, Sort, Track, TrackBy,
};
use melodie_core::mix;
use melodie_core::store;
use melodie_core::time::now_secs;
use melodie_desktop::browse::{Nav, Place};
use melodie_desktop::launch::{self, Job};
use melodie_desktop::selection::{How, Selection};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Albums shown on the home page in each section.
const HOME_ALBUMS: usize = 24;
/// Top tracks on an artist's page.
const TOP_TRACKS: usize = 10;
/// Genre mixes on the home page, as many as `melodie mixes` offers.
const GENRES: usize = 6;

pub struct Home {
    pub recent: Arc<[Album]>,
    pub most_played: Arc<[Album]>,
    /// The genres most present among the liked or frequent tracks, with their track counts.
    pub genres: Vec<(String, usize)>,
}

pub enum Page {
    Album {
        album: Album,
        tracks: Arc<[Track]>,
    },
    Artist {
        artist: Artist,
        albums: Arc<[Album]>,
        top: Arc<[Track]>,
    },
    Playlist {
        playlist: Playlist,
        tracks: Arc<[Track]>,
    },
    /// Not (or no longer) in the local copy.
    Gone,
}

/// Which page of a track to open.
#[derive(Clone, Copy)]
pub enum Facet {
    Album,
    Artist,
}

/// Why a view has nothing to list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Empty {
    Loading,
    /// No local copy yet.
    NoCopy,
    Failed,
}

pub struct Catalog {
    env: Env,
    pub nav: Nav,
    pub albums: Option<Arc<[Album]>>,
    pub album_sort: Sort<AlbumBy>,
    pub artists: Option<Arc<[Artist]>>,
    pub artist_sort: Sort<ArtistBy>,
    pub tracks: Option<Arc<[Track]>>,
    pub track_sort: Sort<TrackBy>,
    pub home: Option<Arc<Home>>,
    pub playlists: Arc<[Playlist]>,
    pub pages: HashMap<Place, Arc<Page>>,
    /// Cover files by cover id, as far as they were looked up (a copy of the cache's, which
    /// the views read without a lock).
    pub covers: Arc<HashMap<String, PathBuf>>,
    cover_cache: Entity<CoverCache>,
    /// The last read of the local copy failed this way.
    pub empty: Empty,
    /// The user logged in: the playlists that are theirs can be added to.
    pub user: Option<String>,
    /// The rows picked in the current list.
    pub selection: Selection,
    shared: Entity<Shared>,
}

impl Catalog {
    pub fn new(env: Env, shared: Entity<Shared>, cx: &mut Context<Self>) -> Self {
        let user = store::Settings::load(&env)
            .ok()
            .and_then(|s| s.login().map(|(_, u)| u.to_string()));
        let cover_cache = shared.read(cx).covers.clone();
        // a file that lands shows in every view of the catalog, without a restart
        cx.observe(&cover_cache, |this, cache, cx| {
            this.covers = cache.read(cx).files.clone();
            cx.notify();
        })
        .detach();
        let mut this = Self {
            env,
            nav: Nav::new(Place::Home),
            albums: None,
            album_sort: Sort::desc(AlbumBy::Added),
            artists: None,
            artist_sort: Sort::asc(ArtistBy::Name),
            tracks: None,
            track_sort: Sort::asc(TrackBy::Artist),
            home: None,
            playlists: Arc::from([]),
            pages: HashMap::new(),
            covers: Arc::default(),
            cover_cache,
            empty: Empty::Loading,
            user,
            selection: Selection::default(),
            shared,
        };
        this.load_playlists(cx);
        this.refresh(cx);
        this
    }

    pub fn here(&self) -> &Place {
        self.nav.here()
    }

    /// Goes to a place from within the views (a page goes on top).
    pub fn go(&mut self, to: Place, cx: &mut Context<Self>) {
        if self.nav.go(to) {
            self.moved(cx);
        }
    }

    /// Opens the page of the album or the artist of a track. A queue entry holds only their
    /// names, so the local copy says which they are.
    pub fn open_of_track(&mut self, track: String, facet: Facet, cx: &mut Context<Self>) {
        self.load(
            "track",
            cx,
            move |c| lib::track(c, &track),
            move |this, found, cx| {
                let to = found.and_then(|t| match facet {
                    Facet::Album => t.album_id.map(Place::Album),
                    Facet::Artist => t.artist_id.map(Place::Artist),
                });
                match to {
                    Some(to) => this.go(to, cx),
                    None => this
                        .shared
                        .update(cx, |s, cx| s.say("It is not in the library", cx)),
                }
            },
        );
    }

    /// Goes to what the sidebar holds: it starts over.
    pub fn start_at(&mut self, to: Place, cx: &mut Context<Self>) {
        if self.nav.start_at(to) {
            self.moved(cx);
        }
    }

    pub fn back(&mut self, cx: &mut Context<Self>) {
        if self.nav.back() {
            self.moved(cx);
        }
    }

    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if self.nav.forward() {
            self.moved(cx);
        }
    }

    fn moved(&mut self, cx: &mut Context<Self>) {
        self.selection.clear();
        self.refresh(cx);
        cx.notify();
    }

    pub fn sort_albums(&mut self, sort: Sort<AlbumBy>, cx: &mut Context<Self>) {
        self.album_sort = sort;
        self.load_albums(cx);
        cx.notify();
    }

    pub fn sort_artists(&mut self, sort: Sort<ArtistBy>, cx: &mut Context<Self>) {
        self.artist_sort = sort;
        self.load_artists(cx);
        cx.notify();
    }

    pub fn sort_tracks(&mut self, sort: Sort<TrackBy>, cx: &mut Context<Self>) {
        self.track_sort = sort;
        self.selection.clear();
        self.load_tracks(cx);
        cx.notify();
    }

    /// The playlists the user may add to.
    pub fn own_playlists(&self) -> Vec<Playlist> {
        self.playlists
            .iter()
            .filter(|p| self.is_own(p))
            .cloned()
            .collect()
    }

    /// Reads the local copy again after a new one came: the playlists and the current place.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load_playlists(cx);
        self.refresh(cx);
    }

    /// The user may edit it (other people's playlists are only read).
    pub fn is_own(&self, p: &Playlist) -> bool {
        match (&p.owner, &self.user) {
            (Some(o), Some(u)) => o == u,
            _ => true,
        }
    }

    /// A Ctrl+click or a Shift+click on row `ix` of the current list.
    pub fn select(&mut self, ix: usize, how: How, cx: &mut Context<Self>) {
        let here = self.here().clone();
        self.selection.click(&here, ix, how);
        cx.notify();
    }

    pub fn unselect(&mut self, cx: &mut Context<Self>) {
        self.selection.clear();
        cx.notify();
    }

    /// The tracks of the list shown at the current place.
    pub fn shown_tracks(&self) -> Option<Arc<[Track]>> {
        match self.here() {
            Place::Tracks => self.tracks.clone(),
            here => match self.pages.get(here).map(Arc::as_ref) {
                Some(Page::Album { tracks, .. } | Page::Playlist { tracks, .. }) => {
                    Some(tracks.clone())
                }
                _ => None,
            },
        }
    }

    /// Runs `melodie` to change a playlist, then reads the playlists and the current place again
    /// and says what happened. `said` is what a success says; without it, the output says it.
    pub fn edit(&mut self, job: Job, said: Option<String>, cx: &mut Context<Self>) {
        let this = cx.entity();
        let shared = self.shared.clone();
        let deleted = match &job {
            Job::DeletePlaylist { id } => Some(Place::Playlist(id.clone())),
            _ => None,
        };
        let read: fn(&str) -> Result<String, String> = match &job {
            Job::AddToPlaylist { .. } => launch::added_message,
            _ => |_| Ok(String::new()),
        };
        self.shared.update(cx, |s, cx| {
            s.run_then(job, cx, move |r, cx| {
                let msg = match r {
                    Ok(out) => match read(&out) {
                        Ok(m) => said.unwrap_or(m),
                        Err(why) => why,
                    },
                    Err(why) => format!("Could not change the playlist: {why}"),
                };
                this.update(cx, |c, cx| {
                    c.selection.clear();
                    if deleted.as_ref() == Some(c.here()) && !c.nav.back() {
                        c.nav.start_at(Place::Home);
                    }
                    c.load_playlists(cx);
                    c.refresh(cx);
                    cx.notify();
                });
                if !msg.is_empty() {
                    shared.update(cx, |s, cx| s.say(msg, cx));
                }
            })
        });
    }

    /// Moves a track of the playlist shown at once, then asks `melodie` to (the list is read
    /// again when it is done, or when it failed).
    pub fn move_in_playlist(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let here = self.here().clone();
        let Place::Playlist(id) = &here else { return };
        let Some(Page::Playlist { playlist, tracks }) = self.pages.get(&here).map(Arc::as_ref)
        else {
            return;
        };
        if from == to || from >= tracks.len() || to >= tracks.len() {
            return;
        }
        let mut list = tracks.to_vec();
        let t = list.remove(from);
        let track = t.id.clone();
        list.insert(to, t);
        let page = Page::Playlist {
            playlist: playlist.clone(),
            tracks: list.into(),
        };
        let job = Job::MoveInPlaylist {
            id: id.clone(),
            from,
            to,
            track,
        };
        self.pages.insert(here, Arc::new(page));
        self.selection.clear();
        cx.notify();
        self.edit(job, Some(String::new()), cx);
    }

    /// Reads again what the current place shows; until it comes, the place shows what it had.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        match self.here().clone() {
            Place::Player | Place::Search | Place::Found(_) => {}
            Place::Home => self.load_home(cx),
            Place::Albums => self.load_albums(cx),
            Place::Artists => self.load_artists(cx),
            Place::Tracks => self.load_tracks(cx),
            p @ (Place::Album(_) | Place::Artist(_) | Place::Playlist(_)) => self.load_page(p, cx),
        }
    }

    /// Runs `query` on the local copy away from the window, then `apply` with its answer.
    fn load<T: Send + 'static>(
        &mut self,
        what: &'static str,
        cx: &mut Context<Self>,
        query: impl FnOnce(&Connection) -> anyhow::Result<T> + Send + 'static,
        apply: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        let env = self.env.clone();
        let started = Instant::now();
        let task = cx.background_spawn(async move {
            let Some(conn) = db::open_existing(&env)? else {
                return Ok(None);
            };
            query(&conn).map(Some)
        });
        cx.spawn(async move |this, cx| {
            let answer: anyhow::Result<Option<T>> = task.await;
            let _ = this.update(cx, |this, cx| {
                match answer {
                    Ok(Some(v)) => {
                        this.empty = Empty::Loading;
                        apply(this, v, cx);
                    }
                    Ok(None) => this.empty = Empty::NoCopy,
                    Err(_) => this.empty = Empty::Failed,
                }
                if crate::timing::on() {
                    crate::timing::note(what, started.elapsed());
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_playlists(&mut self, cx: &mut Context<Self>) {
        self.load("playlists", cx, lib::playlists, |this, list, cx| {
            this.want_covers(list.iter().filter_map(|p| p.cover.clone()), cx);
            this.playlists = list.into();
        });
    }

    fn load_home(&mut self, cx: &mut Context<Self>) {
        self.load(
            "home",
            cx,
            |c| {
                // the genres `melodie mixes` offers, by the same code
                let library = mix::Library::load(c, now_secs() as i64)?;
                Ok(Home {
                    recent: lib::recently_added(c, HOME_ALBUMS)?.into(),
                    most_played: lib::most_played(c, HOME_ALBUMS)?.into(),
                    genres: mix::top_genres(&library, GENRES)
                        .into_iter()
                        .map(|(name, _, total)| (name, total))
                        .collect(),
                })
            },
            |this, home, cx| {
                let ids = home.recent.iter().chain(home.most_played.iter());
                this.want_covers(ids.filter_map(|a| a.cover.clone()).collect::<Vec<_>>(), cx);
                this.home = Some(Arc::new(home));
            },
        );
        self.load_playlists(cx);
    }

    fn load_albums(&mut self, cx: &mut Context<Self>) {
        let sort = self.album_sort;
        self.load(
            "albums",
            cx,
            move |c| lib::albums(c, sort),
            move |this, list, cx| {
                if this.album_sort != sort {
                    return; // another sort was asked meanwhile
                }
                this.want_covers(
                    list.iter()
                        .filter_map(|a| a.cover.clone())
                        .collect::<Vec<_>>(),
                    cx,
                );
                this.albums = Some(list.into());
            },
        );
    }

    fn load_artists(&mut self, cx: &mut Context<Self>) {
        let sort = self.artist_sort;
        self.load(
            "artists",
            cx,
            move |c| lib::artists(c, sort),
            move |this, list, cx| {
                if this.artist_sort == sort {
                    this.want_covers(
                        list.iter()
                            .filter_map(|a| a.cover.clone())
                            .collect::<Vec<_>>(),
                        cx,
                    );
                    this.artists = Some(list.into());
                }
            },
        );
    }

    fn load_tracks(&mut self, cx: &mut Context<Self>) {
        let sort = self.track_sort;
        self.load(
            "tracks",
            cx,
            move |c| lib::tracks(c, sort),
            move |this, list, _| {
                if this.track_sort == sort {
                    this.tracks = Some(list.into());
                }
            },
        );
    }

    fn load_page(&mut self, place: Place, cx: &mut Context<Self>) {
        let key = place.clone();
        self.load(
            "page",
            cx,
            move |c| {
                Ok(match &place {
                    Place::Album(id) => match lib::album(c, id)? {
                        Some(album) => Page::Album {
                            album,
                            tracks: lib::album_tracks(c, id)?.into(),
                        },
                        None => Page::Gone,
                    },
                    Place::Artist(id) => match lib::artist(c, id)? {
                        Some(artist) => Page::Artist {
                            artist,
                            albums: lib::artist_albums(c, id)?.into(),
                            top: lib::top_tracks(c, id, TOP_TRACKS)?.into(),
                        },
                        None => Page::Gone,
                    },
                    Place::Playlist(id) => match lib::playlist(c, id)? {
                        Some(playlist) => Page::Playlist {
                            playlist,
                            tracks: lib::playlist_tracks(c, id)?.into(),
                        },
                        None => Page::Gone,
                    },
                    _ => Page::Gone,
                })
            },
            move |this, page, cx| {
                let covers: Vec<String> = match &page {
                    Page::Album { album, .. } => album.cover.clone().into_iter().collect(),
                    Page::Artist { artist, albums, .. } => artist
                        .cover
                        .iter()
                        .cloned()
                        .chain(albums.iter().filter_map(|a| a.cover.clone()))
                        .collect(),
                    Page::Playlist { playlist, .. } => playlist.cover.clone().into_iter().collect(),
                    Page::Gone => vec![],
                };
                this.want_covers(covers, cx);
                // Only the pages on the way back are kept.
                let kept: Vec<Place> = this.nav.places().to_vec();
                this.pages.retain(|p, _| kept.contains(p));
                this.pages.insert(key, Arc::new(page));
            },
        );
    }

    /// Asks for the covers not asked yet (see `CoverCache`); `covers` is filled as they land.
    pub fn want_covers(&mut self, ids: impl IntoIterator<Item = String>, cx: &mut Context<Self>) {
        let ids: Vec<String> = ids.into_iter().collect();
        self.cover_cache.update(cx, |c, cx| c.want(ids, false, cx));
    }

    /// Runs `melodie` for an action; what it says is said in the window.
    pub fn run(&self, job: launch::Job, cx: &mut App) {
        self.shared.update(cx, |s, cx| s.run(job, cx));
    }
}
