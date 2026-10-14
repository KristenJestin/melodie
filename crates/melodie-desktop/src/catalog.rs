//! The library as the window shows it: where the user is, what each view lists, the covers on
//! disk. Everything is read from the local copy away from the window's thread; a view shows what
//! it had while the new answer comes.

use crate::app::Shared;
use crate::cover_cache::CoverCache;
use crate::home_page::{CIRCLE, SQUARE};
use gpui_kit::*;
use melodie_core::db;
use melodie_core::env::Env;
use melodie_core::library::{self as lib, Album, AlbumBy, Artist, ArtistBy, Playlist, Sort, Track};
use melodie_core::store;
use melodie_core::time::now_secs;
use melodie_desktop::browse::{Nav, Place};
use melodie_desktop::covers::{Files, Use};
use melodie_desktop::home::{self, Highlight, Home, Row};
use melodie_desktop::launch::{self, Job};
use melodie_desktop::menu::{self, Item, Spot};
use melodie_desktop::playlists;
use melodie_desktop::selection::{Click, Selection};
use melodie_desktop::table::{self, ColSort, Shown, Sorts};
use melodie_desktop::theme::Tint;
use melodie_desktop::tint;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Top tracks on an artist's page.
const TOP_TRACKS: usize = 10;

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
    /// The library is being copied: what is not there yet is on its way.
    Copying,
    Failed,
}

pub struct Catalog {
    env: Env,
    pub nav: Nav,
    pub albums: Option<Arc<[Album]>>,
    pub album_sort: Sort<AlbumBy>,
    pub artists: Option<Arc<[Artist]>>,
    pub artist_sort: Sort<ArtistBy>,
    /// How each list of tracks is sorted for the view, by place; the library's tracks are sorted
    /// as the local copy is read.
    pub sorts: Sorts,
    /// Each list of tracks as its table shows it, by place.
    pub views: HashMap<Place, Shown>,
    home: Option<Home>,
    /// What the home shows, from what it read and the playlists.
    pub home_view: Option<Arc<home::View>>,
    /// The colour of the highlights' covers, by cover id, as far as it was read.
    pub tints: Arc<HashMap<String, Option<Tint>>>,
    tinting: HashSet<String>,
    pub playlists: Arc<[Playlist]>,
    pub pages: HashMap<Place, Arc<Page>>,
    /// Cover files by cover id, as far as they were looked up (a copy of the cache's, which
    /// the views read without a lock).
    pub covers: Arc<Files>,
    cover_cache: Entity<CoverCache>,
    /// The last read of the local copy failed this way.
    pub empty: Empty,
    /// A copy of the library runs: an empty view says it is coming.
    pub copying: bool,
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
            this.want_tints(cx);
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
            sorts: Sorts::default(),
            views: HashMap::new(),
            home: None,
            home_view: None,
            tints: Arc::default(),
            tinting: HashSet::new(),
            playlists: Arc::from([]),
            pages: HashMap::new(),
            covers: Arc::default(),
            cover_cache,
            empty: Empty::Loading,
            copying: false,
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
            self.moved(true, cx);
        }
    }

    /// Picks a filter of the search. All, from a filter that was opened from All, is a step back:
    /// the way forward and the sort of All's tracks stay as they were.
    pub fn filter(&mut self, to: Place, cx: &mut Context<Self>) {
        if to == Place::Search && matches!(self.nav.places(), [.., Place::Search, Place::Found(_)])
        {
            self.back(cx)
        } else {
            self.go(to, cx)
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
            self.moved(true, cx);
        }
    }

    pub fn back(&mut self, cx: &mut Context<Self>) {
        if self.nav.back() {
            self.moved(false, cx);
        }
    }

    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if self.nav.forward() {
            self.moved(false, cx);
        }
    }

    /// `anew`: opened, not come back to (see `Sorts::moved`).
    fn moved(&mut self, anew: bool, cx: &mut Context<Self>) {
        self.selection.clear();
        self.sorts.moved(&self.nav, anew);
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

    /// Sorts the view of the list of tracks at `place`; the list itself never changes.
    pub fn sort_tracks(&mut self, place: Place, sort: Option<ColSort>, cx: &mut Context<Self>) {
        self.selection.clear();
        self.sorts.set(place.clone(), sort);
        if place == Place::Tracks {
            self.load_tracks(cx);
        } else {
            self.show(&place);
        }
        cx.notify();
    }

    /// The view of the tracks of the page at `place`, sorted as asked.
    fn show(&mut self, place: &Place) {
        let tracks = match self.pages.get(place).map(Arc::as_ref) {
            Some(Page::Album { tracks, .. } | Page::Playlist { tracks, .. }) => tracks.clone(),
            Some(Page::Artist { top, .. }) => top.clone(),
            _ => {
                self.views.remove(place);
                return;
            }
        };
        let shown = Shown::new(tracks, self.sorts.get(place));
        self.selection.read_again(place, &shown);
        self.views.insert(place.clone(), shown);
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

    /// A left click on row `ix` of `shown`, the list at the current place: it picks, or plays.
    pub fn left_click(&mut self, shown: &Shown, ix: usize, click: Click, cx: &mut Context<Self>) {
        let here = self.here().clone();
        if let Some(played) = self.selection.left_click(&here, shown, ix, click) {
            self.shared
                .update(cx, |s, cx| s.play_from(&here, played, cx));
        }
        cx.notify();
    }

    /// A right click on row `ix` of `shown`: the menu of the selection when the row is in it,
    /// else of this row, picked alone. `playlist`: the user's playlist the list is.
    pub fn menu_on(
        &mut self,
        shown: &Shown,
        ix: usize,
        playlist: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Option<(Item, Spot)> {
        let here = self.here().clone();
        self.selection.right_click(&here, shown, ix);
        cx.notify();
        menu::of_picked(&self.selection.tracks(&here), playlist)
    }

    pub fn unselect(&mut self, cx: &mut Context<Self>) {
        self.selection.clear();
        cx.notify();
    }

    /// Unpicks the rows picked in the lists at these places.
    pub fn unselect_where(&mut self, read_again: impl Fn(&Place) -> bool, cx: &mut Context<Self>) {
        self.selection.clear_where(read_again);
        cx.notify();
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
        self.pages.insert(here.clone(), Arc::new(page));
        self.show(&here);
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
                        this.empty = if this.copying {
                            Empty::Copying
                        } else {
                            Empty::Loading
                        };
                        apply(this, v, cx);
                    }
                    Ok(None) if this.copying => this.empty = Empty::Copying,
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
            for (ids, usage) in playlists::cover_requests(&list, SQUARE) {
                this.want_covers(ids, usage, true, cx);
            }
            this.playlists = list.into();
            this.home_changed(cx);
        });
    }

    fn load_home(&mut self, cx: &mut Context<Self>) {
        self.load(
            "home",
            cx,
            |c| Home::read(c, now_secs() as i64),
            |this, home, cx| {
                this.home = Some(home);
                this.home_changed(cx);
            },
        );
        self.load_playlists(cx);
    }

    /// Builds the home again from what it read, and asks for what it shows.
    fn home_changed(&mut self, cx: &mut Context<Self>) {
        let Some(home) = &self.home else { return };
        let view = home::view(home, &self.playlists);
        let (mut albums, mut artists) = (Vec::new(), Vec::new());
        for section in &view.sections {
            match &section.row {
                Row::Albums(v) => albums.extend(v.iter().filter_map(|a| a.cover.clone())),
                Row::Artists(v) => artists.extend(v.iter().filter_map(|a| a.cover.clone())),
                Row::Mixes(_) | Row::Playlists(_) => {}
            }
        }
        let highlighted: Vec<String> = view
            .highlights
            .iter()
            .filter_map(|h| match h {
                Highlight::Album { album, .. } => album.cover.clone(),
                Highlight::Mix => None,
            })
            .collect();
        self.home_view = Some(Arc::new(view));
        // the home is what is on screen: its covers go before the library's
        self.want_covers(albums, Use::of(SQUARE), true, cx);
        self.want_covers(artists, Use::of(CIRCLE), true, cx);
        self.want_covers(highlighted, Use::CARD, true, cx);
        self.want_tints(cx);
    }

    /// Reads the colour of the highlights' covers that are on disk, away from the window.
    fn want_tints(&mut self, cx: &mut Context<Self>) {
        let Some(view) = &self.home_view else { return };
        let todo: Vec<(String, PathBuf)> = view
            .highlights
            .iter()
            .filter_map(|h| match h {
                Highlight::Album { album, .. } => album.cover.clone(),
                Highlight::Mix => None,
            })
            .filter(|id| !self.tints.contains_key(id) && !self.tinting.contains(id))
            .filter_map(|id| self.covers.smallest(&id).cloned().map(|p| (id, p)))
            .collect();
        if todo.is_empty() {
            return;
        }
        self.tinting.extend(todo.iter().map(|(id, _)| id.clone()));
        let task = cx.background_spawn(async move {
            todo.into_iter()
                .map(|(id, path)| (id, tint::of_file(&path)))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |this, cx| {
                let mut tints = (*this.tints).clone();
                for (id, t) in found {
                    this.tinting.remove(&id);
                    tints.insert(id, t);
                }
                this.tints = Arc::new(tints);
                cx.notify();
            });
        })
        .detach();
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
                    Use::SMALL,
                    false,
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
                        Use::SMALL,
                        false,
                        cx,
                    );
                    this.artists = Some(list.into());
                }
            },
        );
    }

    fn load_tracks(&mut self, cx: &mut Context<Self>) {
        let sort = self.sorts.get(&Place::Tracks);
        self.load(
            "tracks",
            cx,
            move |c| lib::tracks(c, table::library_sort(sort)),
            move |this, list, _| {
                if this.sorts.get(&Place::Tracks) == sort {
                    let shown = Shown::read_sorted(list.into(), sort);
                    this.selection.read_again(&Place::Tracks, &shown);
                    this.views.insert(Place::Tracks, shown);
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
                this.want_covers(covers, Use::TILE, true, cx);
                // Only the pages on the way back are kept.
                let kept: Vec<Place> = this.nav.places().to_vec();
                this.pages.retain(|p, _| kept.contains(p));
                this.views
                    .retain(|p, _| *p == Place::Tracks || kept.contains(p));
                this.pages.insert(key.clone(), Arc::new(page));
                this.show(&key);
            },
        );
    }

    /// Asks for the covers not asked yet (see `CoverCache`); `covers` is filled as they land.
    pub fn want_covers(
        &mut self,
        ids: impl IntoIterator<Item = String>,
        usage: Use,
        first: bool,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<String> = ids.into_iter().collect();
        self.cover_cache
            .update(cx, |c, cx| c.want(ids, usage, first, cx));
    }

    /// Runs `melodie` for an action; what it says is said in the window.
    pub fn run(&self, job: launch::Job, cx: &mut App) {
        self.shared.update(cx, |s, cx| s.run(job, cx));
    }
}

#[cfg(test)]
mod tests {
    // not `super::*`: it would bring the `test` macro of GPUI along
    use super::Catalog;
    use gpui_kit::{AppContext as _, TestAppContext};
    use melodie_core::db;
    use melodie_core::env::Env;
    use melodie_desktop::browse::Place;
    use melodie_desktop::selection::How;

    /// A local copy holding the tracks `ids`.
    fn copy_of(env: &Env, ids: &[&str]) {
        let path = db::db_path(env);
        let conn = db::open_existing(env)
            .unwrap()
            .unwrap_or_else(|| db::create(&path).unwrap());
        for id in ids {
            conn.execute("INSERT INTO tracks(id, title) VALUES (?1, ?1)", [id])
                .unwrap();
        }
    }

    #[test]
    fn a_new_step_of_the_copy_keeps_the_selection_unless_its_rows_may_shift() {
        let mut app = TestAppContext::single();
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        copy_of(&env, &["b", "d", "f", "h", "j", "l"]);
        let catalog = app.update(|cx| {
            let shared = crate::app::for_tests(env.clone(), cx);
            cx.new(|cx| Catalog::new(env.clone(), shared, cx))
        });
        catalog.update(&mut app, |c, cx| c.start_at(Place::Tracks, cx));
        app.run_until_parked();
        catalog.update(&mut app, |c, _| {
            let shown = c.views[&Place::Tracks].clone();
            c.selection.click(&Place::Tracks, &shown, 2, How::Toggle);
            c.selection.click(&Place::Tracks, &shown, 4, How::Range);
            assert_eq!(c.selection.count(&Place::Tracks), 3);
        });
        // a step that brought nothing to this list: the rows stay picked
        catalog.update(&mut app, |c, cx| c.reload(cx));
        app.run_until_parked();
        assert_eq!(
            catalog.read_with(&app, |c, _| c.selection.count(&Place::Tracks)),
            3
        );
        // tracks came in before the picked rows: their ranks now hold others
        copy_of(&env, &["a", "c"]);
        catalog.update(&mut app, |c, cx| c.reload(cx));
        app.run_until_parked();
        catalog.read_with(&app, |c, _| {
            assert_eq!(c.views[&Place::Tracks].len(), 8);
            assert_eq!(c.selection.count(&Place::Tracks), 0);
        });
    }
}
