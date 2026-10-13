//! What the sidebar shows of each playlist: the picture the server makes for it (a mosaic of its
//! covers), else the same icon as every other playlist.

use crate::covers::{Cover, Covers, Use};
use melodie_core::library::Playlist;

/// The side, in logical pixels, of a playlist's picture in the sidebar.
pub const PICTURE: f32 = 24.0;

/// The cover ids to ask the shared cover queue for, once each, in the order of the playlists.
pub fn wanted_covers(playlists: &[Playlist]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for id in playlists.iter().filter_map(|p| p.cover.as_ref()) {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids
}

/// What the playlists ask the shared cover queue for: their covers at the side of the page's
/// squares (`square`), and again at the sidebar's.
pub fn cover_requests(playlists: &[Playlist], square: f32) -> [(Vec<String>, Use); 2] {
    let ids = wanted_covers(playlists);
    [(ids.clone(), Use::of(square)), (ids, Use::of(PICTURE))]
}

/// The picture of a playlist, if it has a cover and the cover is on disk (at the size the
/// sidebar draws it, or a smaller one meanwhile).
pub fn picture(playlist: &Playlist, covers: &Covers) -> Option<Cover> {
    playlist
        .cover
        .as_ref()
        .and_then(|id| covers.get(id, PICTURE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::covers::{Files, resolve};
    use melodie_core::cover::{self, Tier, Want};
    use melodie_core::env::Env;
    use std::sync::Arc;
    use std::time::Instant;

    fn playlist(id: &str, cover: Option<&str>) -> Playlist {
        Playlist {
            id: id.into(),
            name: format!("Playlist {id}"),
            songs: 3,
            duration: 600,
            owner: None,
            cover: cover.map(Into::into),
        }
    }

    #[test]
    fn the_sidebar_asks_for_the_cover_of_each_playlist() {
        let list = [
            playlist("p1", Some("pl-p1")),
            playlist("p2", None),
            playlist("p3", Some("pl-p3")),
        ];
        assert_eq!(wanted_covers(&list), ["pl-p1", "pl-p3"]);
    }

    #[test]
    fn a_cover_shared_by_two_playlists_is_asked_once() {
        let list = [playlist("p1", Some("pl-a")), playlist("p2", Some("pl-a"))];
        assert_eq!(wanted_covers(&list), ["pl-a"]);
    }

    #[test]
    fn the_sidebar_asks_the_shared_queue_at_its_own_size() {
        let list = [playlist("p1", Some("pl-p1")), playlist("p2", None)];
        let asked = cover_requests(&list, 180.0);
        let ids = vec!["pl-p1".to_string()];
        assert!(
            asked.contains(&(ids.clone(), Use::of(PICTURE))),
            "{asked:?}"
        );
        assert!(asked.contains(&(ids, Use::of(180.0))), "{asked:?}");
    }

    /// The covers known after the cover queue found `ids` at the sidebar's size on disk.
    fn covers_on_disk(env: &Env, ids: &[&str]) -> Covers {
        for id in ids {
            let file = cover::tier_path(env, id, Tier::for_draw(PICTURE, 1.0));
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "").unwrap();
        }
        let wants: Vec<Want> = ids
            .iter()
            .map(|id| Want::new(*id, Tier::for_draw(PICTURE, 1.0)))
            .collect();
        let (found, _) = resolve(env, &wants);
        let mut files = Files::default();
        files.merge(found, Instant::now());
        Covers::new(Arc::new(files), 1.0)
    }

    #[test]
    fn a_playlist_shows_its_picture_once_the_file_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        let covers = covers_on_disk(&env, &["pl-p1"]);
        let shown = picture(&playlist("p1", Some("pl-p1")), &covers).unwrap();
        assert_eq!(
            shown.path,
            cover::tier_path(&env, "pl-p1", Tier::for_draw(PICTURE, 1.0))
        );
    }

    #[test]
    fn a_playlist_keeps_its_icon_without_a_picture_or_before_the_file_lands() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        let covers = covers_on_disk(&env, &["pl-p1"]);
        assert_eq!(picture(&playlist("p1", None), &covers), None);
        assert_eq!(picture(&playlist("p2", Some("pl-p2")), &covers), None);
    }
}
