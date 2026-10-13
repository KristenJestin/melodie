//! Where the window is in the library, and the pieces of browsing that need no window: the way
//! back from a page, how many covers a row holds.

use melodie_core::library::Album;
use std::ops::Range;

/// A place of the window: a section of the sidebar, or a page opened from one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Place {
    Player,
    Home,
    Albums,
    Artists,
    Tracks,
    Album(String),
    Artist(String),
    Playlist(String),
    /// The search.
    Search,
    /// Every result of one kind for the text searched.
    Found(crate::launch::Kind),
}

impl Place {
    /// A section of the sidebar (a page is opened from one).
    pub fn is_section(&self) -> bool {
        matches!(
            self,
            Self::Player | Self::Home | Self::Albums | Self::Artists | Self::Tracks | Self::Search
        )
    }
}

/// The places opened since the last section, the current one last. `step` grows at every move,
/// so the view that appears can be animated.
#[derive(Debug, Clone)]
pub struct Nav {
    stack: Vec<Place>,
    /// The pages left by going back, the next one last: what the forward button comes back to.
    ahead: Vec<Place>,
    pub step: u64,
}

impl Nav {
    pub fn new(start: Place) -> Self {
        Self {
            stack: vec![start],
            ahead: Vec::new(),
            step: 0,
        }
    }

    pub fn here(&self) -> &Place {
        self.stack.last().expect("never empty")
    }

    /// A section starts over; a page goes on top, unless it is already the current one. Returns
    /// whether the place changed.
    pub fn go(&mut self, to: Place) -> bool {
        if *self.here() == to {
            return false;
        }
        if to.is_section() {
            self.stack.clear();
        }
        self.ahead.clear();
        self.stack.push(to);
        self.step += 1;
        true
    }

    /// What the sidebar opens (a section or one of its playlists) starts over from it.
    pub fn start_at(&mut self, to: Place) -> bool {
        if self.stack.len() == 1 && *self.here() == to {
            return false;
        }
        self.stack = vec![to];
        self.ahead.clear();
        self.step += 1;
        true
    }

    /// The places on the way back, the first one first.
    pub fn places(&self) -> &[Place] {
        &self.stack
    }

    /// The pages left by going back, the next one last.
    pub fn ahead(&self) -> &[Place] {
        &self.ahead
    }

    pub fn can_go_back(&self) -> bool {
        self.stack.len() > 1
    }

    pub fn back(&mut self) -> bool {
        if !self.can_go_back() {
            return false;
        }
        let left = self.stack.pop().expect("more than one place");
        self.ahead.push(left);
        self.step += 1;
        true
    }

    pub fn can_go_forward(&self) -> bool {
        !self.ahead.is_empty()
    }

    /// Comes back to the page `back` left.
    pub fn forward(&mut self) -> bool {
        let Some(page) = self.ahead.pop() else {
            return false;
        };
        self.stack.push(page);
        self.step += 1;
        true
    }

    /// Where the current place was opened from: the entry the sidebar marks.
    pub fn section(&self) -> &Place {
        &self.stack[0]
    }
}

/// How many covers fit across `width` pixels, each at least `min` wide with `gap` between them.
pub fn columns(width: f32, min: f32, gap: f32) -> usize {
    (((width + gap) / (min + gap)).floor() as usize).max(1)
}

/// The covers of the albums in rows `rows` of a grid `cols` across: what a virtual list shows.
pub fn row_covers(albums: &[Album], rows: Range<usize>, cols: usize) -> Vec<String> {
    let end = (rows.end * cols).min(albums.len());
    let start = (rows.start * cols).min(end);
    albums[start..end]
        .iter()
        .filter_map(|a| a.cover.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_starts_over_and_a_page_goes_on_top() {
        let mut nav = Nav::new(Place::Home);
        assert!(nav.go(Place::Album("al1".into())));
        assert!(nav.go(Place::Artist("ar1".into())));
        assert_eq!(nav.section(), &Place::Home);
        assert!(nav.back());
        assert_eq!(nav.here(), &Place::Album("al1".into()));
        assert!(nav.go(Place::Albums));
        assert!(!nav.can_go_back(), "a section forgets the pages");
        assert!(!nav.back());
        assert_eq!(nav.here(), &Place::Albums);
    }

    #[test]
    fn forward_comes_back_to_the_page_back_left() {
        let mut nav = Nav::new(Place::Home);
        assert!(!nav.can_go_forward(), "nothing to go forward to yet");
        assert!(!nav.forward());
        nav.go(Place::Album("al1".into()));
        nav.go(Place::Artist("ar1".into()));
        nav.back();
        nav.back();
        assert!(nav.can_go_forward());
        let step = nav.step;
        assert!(nav.forward());
        assert_eq!(nav.here(), &Place::Album("al1".into()));
        assert!(nav.step > step, "going forward animates too");
        assert!(nav.forward());
        assert_eq!(nav.here(), &Place::Artist("ar1".into()));
        assert!(!nav.can_go_forward(), "the end of the way");
        assert!(!nav.forward());
        assert_eq!(nav.places().len(), 3, "back sees them all again");
    }

    #[test]
    fn a_new_place_forgets_what_was_ahead() {
        let mut nav = Nav::new(Place::Home);
        nav.go(Place::Album("al1".into()));
        nav.back();
        nav.go(Place::Artist("ar1".into()));
        assert!(!nav.can_go_forward(), "a page opened after going back");
        nav.back();
        nav.go(Place::Albums);
        assert!(!nav.can_go_forward(), "a section");
        nav.go(Place::Album("al2".into()));
        nav.back();
        nav.start_at(Place::Playlist("p1".into()));
        assert!(!nav.can_go_forward(), "the sidebar");
        // staying where one is changes nothing, forward included
        nav.go(Place::Album("al3".into()));
        nav.back();
        assert!(!nav.go(Place::Playlist("p1".into())));
        assert!(nav.can_go_forward(), "staying keeps what is ahead");
        assert!(!nav.start_at(Place::Playlist("p1".into())));
        assert!(
            nav.can_go_forward(),
            "so does the sidebar entry already open"
        );
    }

    #[test]
    fn the_current_place_again_changes_nothing() {
        let mut nav = Nav::new(Place::Home);
        nav.go(Place::Album("al1".into()));
        let step = nav.step;
        assert!(!nav.go(Place::Album("al1".into())));
        assert_eq!(nav.step, step, "no animation for staying");
        assert!(nav.can_go_back());
        nav.back();
        assert!(nav.step > step, "going back animates too");
    }

    #[test]
    fn a_playlist_opened_from_the_sidebar_starts_over() {
        let mut nav = Nav::new(Place::Albums);
        nav.go(Place::Album("al1".into()));
        assert!(nav.start_at(Place::Playlist("p1".into())));
        assert!(!nav.can_go_back());
        assert_eq!(nav.section(), &Place::Playlist("p1".into()));
        assert!(!nav.start_at(Place::Playlist("p1".into())));
        // opened from a page, it goes on top
        nav.go(Place::Album("al2".into()));
        nav.go(Place::Playlist("p2".into()));
        assert_eq!(nav.section(), &Place::Playlist("p1".into()));
    }

    #[test]
    fn the_search_is_a_section_and_all_of_one_kind_goes_on_top() {
        let mut nav = Nav::new(Place::Albums);
        nav.go(Place::Album("al1".into()));
        assert!(nav.go(Place::Search));
        assert!(!nav.can_go_back(), "the search starts over");
        nav.go(Place::Found(crate::launch::Kind::Track));
        nav.go(Place::Album("al2".into()));
        assert_eq!(nav.section(), &Place::Search);
        nav.back();
        nav.back();
        assert_eq!(nav.here(), &Place::Search, "back to the groups");
    }

    /// The views are drawn on every frame of a scroll or a fade: reading the local copy and
    /// looking for cover files belong to the catalog's background tasks. A guard on the source,
    /// as drawing cannot be observed without a window.
    #[test]
    fn the_library_views_do_not_touch_the_disk_while_drawing() {
        let views = [
            ("albums.rs", include_str!("albums.rs")),
            ("tables.rs", include_str!("tables.rs")),
            ("pages.rs", include_str!("pages.rs")),
            ("sidebar.rs", include_str!("sidebar.rs")),
            ("playbar.rs", include_str!("playbar.rs")),
            ("search.rs", include_str!("search.rs")),
            ("dialogs.rs", include_str!("dialogs.rs")),
        ];
        for (file, src) in views {
            for call in [
                "cover::local",
                "covers::resolve",
                "Disk::read",
                "is_file",
                "std::fs",
                "exists()",
                "db::",
                "lib::",
            ] {
                assert!(
                    !src.contains(call),
                    "{file} calls {call} on the render path"
                );
            }
        }
    }

    #[test]
    fn covers_fit_the_width() {
        assert_eq!(columns(1000.0, 160.0, 20.0), 5);
        assert_eq!(columns(159.0, 160.0, 20.0), 1, "always one");
        assert_eq!(columns(340.0, 160.0, 20.0), 2);
    }

    #[test]
    fn a_list_asks_for_the_covers_of_the_rows_it_shows() {
        let albums: Vec<Album> = (0..10)
            .map(|i| Album {
                id: format!("al{i}"),
                cover: (i != 4).then(|| format!("c{i}")),
                ..Album::default()
            })
            .collect();
        // 3 columns: row 1 holds albums 3 to 5 (the fifth has no cover)
        assert_eq!(row_covers(&albums, 1..2, 3), ["c3", "c5"]);
        assert_eq!(row_covers(&albums, 2..4, 3), ["c6", "c7", "c8", "c9"]);
        assert!(row_covers(&albums, 4..9, 3).is_empty(), "past the end");
    }
}
