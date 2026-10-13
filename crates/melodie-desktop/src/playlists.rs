//! What the sidebar shows of each playlist: the picture the server makes for it (a mosaic of its
//! covers), else the same icon as every other playlist.

use melodie_core::library::Playlist;
use std::collections::HashMap;
use std::path::PathBuf;

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

/// The picture file of a playlist, if it has a cover and the cover is on disk.
pub fn picture<'a>(
    playlist: &Playlist,
    files: &'a HashMap<String, PathBuf>,
) -> Option<&'a PathBuf> {
    playlist.cover.as_ref().and_then(|id| files.get(id))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_playlist_shows_its_picture_once_the_file_is_there() {
        let files = HashMap::from([("pl-p1".to_string(), PathBuf::from("/cache/pl-p1.jpg"))]);
        assert_eq!(
            picture(&playlist("p1", Some("pl-p1")), &files),
            Some(&PathBuf::from("/cache/pl-p1.jpg"))
        );
    }

    #[test]
    fn a_playlist_keeps_its_icon_without_a_picture_or_before_the_file_lands() {
        let files = HashMap::new();
        assert_eq!(picture(&playlist("p1", None), &files), None);
        assert_eq!(picture(&playlist("p2", Some("pl-p2")), &files), None);
    }
}
