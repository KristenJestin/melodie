//! Where the window is in the library, and the pieces of browsing that need no window: the way
//! back from a page, how many covers a row holds, which covers are on disk.

use melodie_core::cover;
use melodie_core::env::Env;
use std::collections::HashMap;
use std::path::PathBuf;

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
}

impl Place {
    /// A section of the sidebar (a page is opened from one).
    pub fn is_section(&self) -> bool {
        matches!(
            self,
            Self::Player | Self::Home | Self::Albums | Self::Artists | Self::Tracks
        )
    }
}

/// The places opened since the last section, the current one last. `step` grows at every move,
/// so the view that appears can be animated.
#[derive(Debug, Clone)]
pub struct Nav {
    stack: Vec<Place>,
    pub step: u64,
}

impl Nav {
    pub fn new(start: Place) -> Self {
        Self {
            stack: vec![start],
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
        self.step += 1;
        true
    }

    pub fn can_go_back(&self) -> bool {
        self.stack.len() > 1
    }

    pub fn back(&mut self) -> bool {
        if !self.can_go_back() {
            return false;
        }
        self.stack.pop();
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

/// The cover files on disk for these cover ids, and the ids with none yet (to fetch). Checks
/// files: run it away from the window's thread.
pub fn resolve_covers<'a>(
    env: &Env,
    ids: impl IntoIterator<Item = &'a str>,
) -> (HashMap<String, PathBuf>, Vec<String>) {
    let mut found = HashMap::new();
    let mut missing = Vec::new();
    for id in ids {
        if found.contains_key(id) || missing.iter().any(|m| m == id) {
            continue;
        }
        match cover::local(env, id) {
            cover::Local::Have(p) => {
                found.insert(id.to_string(), p);
            }
            cover::Local::Placeholder => {}
            cover::Local::Missing => missing.push(id.to_string()),
        }
    }
    (found, missing)
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
    fn covers_fit_the_width() {
        assert_eq!(columns(1000.0, 160.0, 20.0), 5);
        assert_eq!(columns(159.0, 160.0, 20.0), 1, "always one");
        assert_eq!(columns(340.0, 160.0, 20.0), 2);
    }

    #[test]
    fn covers_on_disk_are_found_and_the_others_are_to_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        let thumb = cover::thumb_path(&env, "c1");
        std::fs::create_dir_all(thumb.parent().unwrap()).unwrap();
        std::fs::write(&thumb, "").unwrap();
        std::fs::write(format!("{}.none", cover::thumb_path(&env, "c3").display()), "").unwrap();
        let (found, missing) = resolve_covers(&env, ["c1", "c2", "c3", "c2", "c1"]);
        assert_eq!(found.get("c1"), Some(&thumb));
        assert_eq!(found.len(), 1);
        assert_eq!(missing, ["c2"], "the server's default image is not fetched again");
    }
}
